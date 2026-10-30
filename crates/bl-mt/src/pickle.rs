//! A safe reader for the Python pickles `torch.save` writes.
//!
//! A pickle is a little program for a stack machine; Python's own reader
//! can call any function it names, which is why loading an unknown
//! `.bin` model with Python is dangerous. This reader runs no code at
//! all: it only builds plain values (numbers, strings, tuples, lists,
//! dicts). The few names a state dict needs are accepted from a fixed
//! list (`OrderedDict`, torch's tensor rebuild function and the float
//! storage types) and become markers; every other name is an error.
//! "Calling" a marker builds a value ([`Value::Dict`] or
//! [`Value::Tensor`]) after checking its arguments.
//!
//! Every length is checked against what is left of the input, and there
//! are limits on the stack, the memo, the nesting depth and the total
//! number of values built, so a hostile file can only cause an error.
//!
//! Values are copied out of the memo (Python shares them), so a value
//! changed after it was memoised keeps its old content in the memo. Torch
//! never relies on that for the data we read.

use std::collections::HashMap;
use std::io::Read;

/// Deepest nesting of tuples, lists and dicts.
const MAX_DEPTH: usize = 32;
/// Most values on the stack.
const MAX_STACK: usize = 1 << 20;
/// Most open marks.
const MAX_MARKS: usize = 1 << 10;
/// Most memo entries.
const MAX_MEMO: usize = 1 << 20;
/// Most values built in all (copies from the memo count in full).
const MAX_NODES: u64 = 1 << 24;
/// Longest string (bytes).
const MAX_STR: usize = 1 << 16;
/// Longest module or class name of a global.
const MAX_NAME: usize = 128;
/// Most dimensions of a tensor.
const MAX_DIMS: usize = 8;

/// Element types of tensor storages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DType {
    F32,
    F16,
    BF16,
}

impl DType {
    /// Bytes per element.
    pub fn size(self) -> usize {
        match self {
            DType::F32 => 4,
            DType::F16 | DType::BF16 => 2,
        }
    }
}

/// The names a pickle may refer to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Global {
    /// `collections.OrderedDict`
    OrderedDict,
    /// `torch._utils._rebuild_tensor_v2`
    RebuildTensor,
    /// `torch.FloatStorage`, `torch.HalfStorage`, `torch.BFloat16Storage`
    Storage(DType),
}

impl Global {
    fn lookup(module: &str, name: &str) -> Option<Global> {
        Some(match (module, name) {
            ("collections", "OrderedDict") => Global::OrderedDict,
            ("torch._utils", "_rebuild_tensor_v2") => Global::RebuildTensor,
            ("torch", "FloatStorage") => Global::Storage(DType::F32),
            ("torch", "HalfStorage") => Global::Storage(DType::F16),
            ("torch", "BFloat16Storage") => Global::Storage(DType::BF16),
            _ => return None,
        })
    }
}

/// A storage (a flat array of values in the file) named by a persistent
/// id: `('storage', type, key, location, count)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorageRef {
    pub key: String,
    pub dtype: DType,
    /// Number of elements.
    pub count: u64,
}

/// A contiguous tensor: `shape` values from element `offset` of a storage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TensorRef {
    pub storage: StorageRef,
    pub offset: u64,
    pub shape: Vec<usize>,
}

impl TensorRef {
    /// Number of elements (checked when the tensor was read).
    pub fn numel(&self) -> u64 {
        self.shape.iter().map(|&d| d as u64).product()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    None,
    Bool(bool),
    Int(i64),
    /// An integer too large for i64: little-endian two's complement bytes.
    BigInt(Vec<u8>),
    Str(String),
    Tuple(Vec<Value>),
    List(Vec<Value>),
    /// Entries in insertion order (also `OrderedDict`).
    Dict(Vec<(Value, Value)>),
    Global(Global),
    Storage(StorageRef),
    Tensor(TensorRef),
}

impl Value {
    /// Nesting depth (1 for plain values).
    fn depth(&self) -> usize {
        match self {
            Value::Tuple(v) | Value::List(v) => 1 + v.iter().map(Value::depth).max().unwrap_or(0),
            Value::Dict(v) => {
                1 + v
                    .iter()
                    .map(|(k, v)| k.depth().max(v.depth()))
                    .max()
                    .unwrap_or(0)
            }
            _ => 1,
        }
    }

    /// Number of values it consists of.
    fn nodes(&self) -> u64 {
        match self {
            Value::Tuple(v) | Value::List(v) => 1 + v.iter().map(Value::nodes).sum::<u64>(),
            Value::Dict(v) => 1 + v.iter().map(|(k, v)| k.nodes() + v.nodes()).sum::<u64>(),
            _ => 1,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Str(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_int(&self) -> Option<i64> {
        match self {
            Value::Int(i) => Some(*i),
            Value::Bool(b) => Some(*b as i64),
            _ => None,
        }
    }
}

/// Reads one pickle (up to its STOP) from `r`. The caller limits how much
/// `r` may deliver (e.g. with `Read::take`).
pub fn read(r: &mut dyn Read) -> Result<Value, String> {
    Machine::default().run(r)
}

/// Reads one pickle from the start of `bytes`.
pub fn read_bytes(bytes: &[u8]) -> Result<Value, String> {
    let mut r = bytes;
    read(&mut r)
}

#[derive(Default)]
struct Machine {
    stack: Vec<Value>,
    marks: Vec<usize>,
    memo: HashMap<u32, Value>,
    nodes: u64,
}

fn byte(r: &mut dyn Read) -> Result<u8, String> {
    let mut b = [0u8; 1];
    r.read_exact(&mut b).map_err(|_| truncated())?;
    Ok(b[0])
}

fn array<const N: usize>(r: &mut dyn Read) -> Result<[u8; N], String> {
    let mut b = [0u8; N];
    r.read_exact(&mut b).map_err(|_| truncated())?;
    Ok(b)
}

fn truncated() -> String {
    "pickle: truncated".to_string()
}

/// Reads `n` bytes, growing the buffer as data arrives (a false length
/// fails at the end of the input instead of allocating up front).
fn bytes(r: &mut dyn Read, n: usize) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    r.take(n as u64)
        .read_to_end(&mut out)
        .map_err(|e| format!("pickle: {e}"))?;
    if out.len() != n {
        return Err(truncated());
    }
    Ok(out)
}

fn string(r: &mut dyn Read, n: usize) -> Result<Value, String> {
    if n > MAX_STR {
        return Err("pickle: string too long".into());
    }
    String::from_utf8(bytes(r, n)?)
        .map(Value::Str)
        .map_err(|_| "pickle: string is not UTF-8".to_string())
}

/// A line of a GLOBAL opcode (module or name), without the newline.
fn name_line(r: &mut dyn Read) -> Result<String, String> {
    let mut s = String::new();
    loop {
        let b = byte(r)?;
        if b == b'\n' {
            return Ok(s);
        }
        if !(b.is_ascii_graphic()) || s.len() >= MAX_NAME {
            return Err("pickle: bad global name".into());
        }
        s.push(b as char);
    }
}

fn global(module: &str, name: &str) -> Result<Value, String> {
    Global::lookup(module, name)
        .map(Value::Global)
        .ok_or_else(|| format!("pickle: refusing global {module}.{name}"))
}

impl Machine {
    fn run(&mut self, r: &mut dyn Read) -> Result<Value, String> {
        loop {
            let op = byte(r)?;
            match op {
                // PROTO
                0x80 => {
                    if byte(r)? > 5 {
                        return Err("pickle: unknown protocol".into());
                    }
                }
                // FRAME (protocol 4): only a hint for buffering.
                0x95 => {
                    array::<8>(r)?;
                }
                // STOP
                b'.' => {
                    if self.stack.len() != 1 || !self.marks.is_empty() {
                        return Err("pickle: unbalanced stack at STOP".into());
                    }
                    return self.pop();
                }
                // GLOBAL
                b'c' => {
                    let module = name_line(r)?;
                    let name = name_line(r)?;
                    self.push(global(&module, &name)?)?;
                }
                // STACK_GLOBAL
                0x93 => {
                    let name = self.pop()?;
                    let module = self.pop()?;
                    match (module.as_str(), name.as_str()) {
                        (Some(m), Some(n)) => self.push(global(m, n)?)?,
                        _ => return Err("pickle: bad STACK_GLOBAL".into()),
                    }
                }
                // BINPUT, LONG_BINPUT, MEMOIZE
                b'q' | b'r' | 0x94 => {
                    let idx = match op {
                        b'q' => byte(r)? as u32,
                        b'r' => u32::from_le_bytes(array(r)?),
                        _ => self.memo.len() as u32,
                    };
                    let top = self.stack.last().ok_or("pickle: empty stack")?.clone();
                    self.count(top.nodes())?;
                    if self.memo.len() >= MAX_MEMO && !self.memo.contains_key(&idx) {
                        return Err("pickle: memo too large".into());
                    }
                    self.memo.insert(idx, top);
                }
                // BINGET, LONG_BINGET
                b'h' | b'j' => {
                    let idx = if op == b'h' {
                        byte(r)? as u32
                    } else {
                        u32::from_le_bytes(array(r)?)
                    };
                    let v = self.memo.get(&idx).ok_or("pickle: unknown memo entry")?;
                    let v = v.clone();
                    self.count(v.nodes())?;
                    self.stack_push(v)?;
                }
                // MARK
                b'(' => {
                    if self.marks.len() >= MAX_MARKS {
                        return Err("pickle: too many marks".into());
                    }
                    self.marks.push(self.stack.len());
                }
                b'N' => self.push(Value::None)?,
                0x88 => self.push(Value::Bool(true))?,
                0x89 => self.push(Value::Bool(false))?,
                // BININT, BININT1, BININT2
                b'J' => self.push(Value::Int(i32::from_le_bytes(array(r)?) as i64))?,
                b'K' => self.push(Value::Int(byte(r)? as i64))?,
                b'M' => self.push(Value::Int(u16::from_le_bytes(array(r)?) as i64))?,
                // LONG1
                0x8a => {
                    let n = byte(r)? as usize;
                    if n > 32 {
                        return Err("pickle: integer too long".into());
                    }
                    let b = bytes(r, n)?;
                    self.push(long(b))?;
                }
                // BINUNICODE, SHORT_BINUNICODE
                b'X' => {
                    let n = u32::from_le_bytes(array(r)?) as usize;
                    let s = string(r, n)?;
                    self.push(s)?;
                }
                0x8c => {
                    let n = byte(r)? as usize;
                    let s = string(r, n)?;
                    self.push(s)?;
                }
                b')' => self.push(Value::Tuple(Vec::new()))?,
                b']' => self.push(Value::List(Vec::new()))?,
                b'}' => self.push(Value::Dict(Vec::new()))?,
                // TUPLE
                b't' => {
                    let items = self.pop_mark()?;
                    self.push(container(Value::Tuple(items))?)?;
                }
                // TUPLE1..3
                0x85..=0x87 => {
                    let n = (op - 0x84) as usize;
                    if self.top_len() < n {
                        return Err("pickle: stack underflow".into());
                    }
                    let items = self.stack.split_off(self.stack.len() - n);
                    self.push(container(Value::Tuple(items))?)?;
                }
                // APPEND, APPENDS
                b'a' | b'e' => {
                    let items = if op == b'a' {
                        vec![self.pop()?]
                    } else {
                        self.pop_mark()?
                    };
                    check_items(items.iter())?;
                    match self.stack.last_mut() {
                        Some(Value::List(list)) => list.extend(items),
                        _ => return Err("pickle: APPEND to a non-list".into()),
                    }
                }
                // SETITEM, SETITEMS
                b's' | b'u' => {
                    let items = if op == b's' {
                        let v = self.pop()?;
                        let k = self.pop()?;
                        vec![k, v]
                    } else {
                        self.pop_mark()?
                    };
                    if items.len() % 2 != 0 {
                        return Err("pickle: odd SETITEMS".into());
                    }
                    check_items(items.iter())?;
                    match self.stack.last_mut() {
                        Some(Value::Dict(dict)) => {
                            let mut it = items.into_iter();
                            while let (Some(k), Some(v)) = (it.next(), it.next()) {
                                match dict.iter_mut().find(|(old, _)| *old == k) {
                                    Some(slot) => slot.1 = v,
                                    None => dict.push((k, v)),
                                }
                            }
                        }
                        _ => return Err("pickle: SETITEM on a non-dict".into()),
                    }
                }
                // REDUCE
                b'R' => {
                    let args = self.pop()?;
                    let f = self.pop()?;
                    let v = reduce(f, args)?;
                    self.push(v)?;
                }
                // BUILD: only the attribute state of an OrderedDict (the
                // `_metadata` of a state dict), which is not needed.
                b'b' => {
                    let _state = self.pop()?;
                    if !matches!(self.stack.last(), Some(Value::Dict(_))) {
                        return Err("pickle: BUILD on something else than a dict".into());
                    }
                }
                // BINPERSID
                b'Q' => {
                    let pid = self.pop()?;
                    let v = Value::Storage(storage(pid)?);
                    self.push(v)?;
                }
                _ => return Err(format!("pickle: unsupported opcode 0x{op:02x}")),
            }
        }
    }

    fn count(&mut self, n: u64) -> Result<(), String> {
        self.nodes += n;
        if self.nodes > MAX_NODES {
            return Err("pickle: too many values".into());
        }
        Ok(())
    }

    fn push(&mut self, v: Value) -> Result<(), String> {
        self.count(1)?;
        self.stack_push(v)
    }

    fn stack_push(&mut self, v: Value) -> Result<(), String> {
        if self.stack.len() >= MAX_STACK {
            return Err("pickle: stack too large".into());
        }
        self.stack.push(v);
        Ok(())
    }

    /// Values above the innermost mark (only these may be popped).
    fn top_len(&self) -> usize {
        self.stack.len() - self.marks.last().copied().unwrap_or(0)
    }

    fn pop(&mut self) -> Result<Value, String> {
        if self.top_len() == 0 {
            return Err("pickle: stack underflow".into());
        }
        self.stack
            .pop()
            .ok_or_else(|| "pickle: stack underflow".into())
    }

    fn pop_mark(&mut self) -> Result<Vec<Value>, String> {
        let m = self.marks.pop().ok_or("pickle: no mark")?;
        Ok(self.stack.split_off(m))
    }
}

/// Little-endian two's complement bytes as an integer.
fn long(b: Vec<u8>) -> Value {
    if b.len() <= 8 {
        let fill = if b.last().is_some_and(|&x| x & 0x80 != 0) {
            0xff
        } else {
            0
        };
        let mut a = [fill; 8];
        a[..b.len()].copy_from_slice(&b);
        Value::Int(i64::from_le_bytes(a))
    } else {
        Value::BigInt(b)
    }
}

/// Checks that the items may go into a container.
fn check_items<'a>(items: impl Iterator<Item = &'a Value>) -> Result<(), String> {
    for v in items {
        if v.depth() >= MAX_DEPTH {
            return Err("pickle: nested too deeply".into());
        }
    }
    Ok(())
}

fn container(v: Value) -> Result<Value, String> {
    if v.depth() > MAX_DEPTH {
        return Err("pickle: nested too deeply".into());
    }
    Ok(v)
}

/// "Calls" a whitelisted global.
fn reduce(f: Value, args: Value) -> Result<Value, String> {
    let Value::Tuple(args) = args else {
        return Err("pickle: REDUCE arguments are not a tuple".into());
    };
    match f {
        Value::Global(Global::OrderedDict) if args.is_empty() => Ok(Value::Dict(Vec::new())),
        Value::Global(Global::RebuildTensor) => rebuild_tensor(args).map(Value::Tensor),
        _ => Err("pickle: REDUCE of something else than a known global".into()),
    }
}

/// The persistent id of a storage: `('storage', type, key, location,
/// count)`, in the legacy format followed by `None` (no view).
fn storage(pid: Value) -> Result<StorageRef, String> {
    let bad = || "pickle: bad storage id".to_string();
    let Value::Tuple(t) = pid else {
        return Err(bad());
    };
    if !(t.len() == 5 || (t.len() == 6 && t[5] == Value::None)) || t[0].as_str() != Some("storage")
    {
        return Err(bad());
    }
    let Value::Global(Global::Storage(dtype)) = t[1] else {
        return Err(bad());
    };
    let key = t[2].as_str().ok_or_else(bad)?;
    t[3].as_str().ok_or_else(bad)?;
    let count = t[4].as_int().ok_or_else(bad)?;
    if key.is_empty() || key.len() > 64 {
        return Err(bad());
    }
    Ok(StorageRef {
        key: key.to_string(),
        dtype,
        count: u64::try_from(count).map_err(|_| bad())?,
    })
}

/// A list of small non-negative integers (a size or stride tuple).
fn dims(v: &Value) -> Option<Vec<u64>> {
    let Value::Tuple(t) = v else { return None };
    if t.len() > MAX_DIMS {
        return None;
    }
    t.iter()
        .map(|d| d.as_int().and_then(|i| u64::try_from(i).ok()))
        .collect()
}

/// `_rebuild_tensor_v2(storage, offset, size, stride, requires_grad,
/// backward_hooks[, metadata])`, for contiguous tensors only.
fn rebuild_tensor(args: Vec<Value>) -> Result<TensorRef, String> {
    let bad = |what: &str| format!("pickle: tensor with bad {what}");
    if !(6..=7).contains(&args.len()) {
        return Err(bad("arguments"));
    }
    let mut args = args.into_iter();
    let Some(Value::Storage(storage)) = args.next() else {
        return Err(bad("storage"));
    };
    let offset = args
        .next()
        .and_then(|v| v.as_int())
        .and_then(|i| u64::try_from(i).ok())
        .ok_or_else(|| bad("offset"))?;
    let size = args
        .next()
        .as_ref()
        .and_then(dims)
        .ok_or_else(|| bad("size"))?;
    let stride = args
        .next()
        .as_ref()
        .and_then(dims)
        .ok_or_else(|| bad("stride"))?;
    if stride.len() != size.len() {
        return Err(bad("stride"));
    }
    // Contiguous (row-major): each stride is the product of the later
    // sizes (dimensions of size 1 may have any stride).
    let mut expected: u64 = 1;
    for (&s, &st) in size.iter().zip(&stride).rev() {
        if s != 1 && st != expected {
            return Err("pickle: tensor is not contiguous".into());
        }
        expected = expected.checked_mul(s).ok_or_else(|| bad("size"))?;
    }
    let numel = expected;
    let end = offset.checked_add(numel).ok_or_else(|| bad("offset"))?;
    if end > storage.count {
        return Err("pickle: tensor outside its storage".into());
    }
    let shape = size
        .iter()
        .map(|&d| usize::try_from(d).map_err(|_| bad("size")))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(TensorRef {
        storage,
        offset,
        shape,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A small pickle writer for the tests.
    #[derive(Default)]
    struct P(Vec<u8>);

    impl P {
        fn op(mut self, b: &[u8]) -> P {
            self.0.extend_from_slice(b);
            self
        }
        fn s(mut self, s: &str) -> P {
            self.0.push(b'X');
            self.0.extend_from_slice(&(s.len() as u32).to_le_bytes());
            self.0.extend_from_slice(s.as_bytes());
            self
        }
        fn int(mut self, i: i32) -> P {
            self.0.push(b'J');
            self.0.extend_from_slice(&i.to_le_bytes());
            self
        }
        fn global(self, m: &str, n: &str) -> P {
            self.op(format!("c{m}\n{n}\n").as_bytes())
        }
        /// `_rebuild_tensor_v2(storage key, offset, size, stride)`.
        fn tensor(self, key: &str, count: i32, offset: i32, size: &[i32], stride: &[i32]) -> P {
            let mut p = self
                .global("torch._utils", "_rebuild_tensor_v2")
                .op(b"((")
                .s("storage")
                .global("torch", "FloatStorage")
                .s(key)
                .s("cpu")
                .int(count)
                .op(b"NtQ")
                .int(offset)
                .op(b"(");
            for &d in size {
                p = p.int(d);
            }
            p = p.op(b"t(");
            for &d in stride {
                p = p.int(d);
            }
            p.op(b"t\x89")
                .global("collections", "OrderedDict")
                .op(b")R")
                .op(b"tR")
        }
    }

    fn state_dict() -> Vec<u8> {
        P::default()
            .op(b"\x80\x02")
            .global("collections", "OrderedDict")
            .op(b"q\x00)Rq\x01(")
            .s("a.weight")
            .tensor("10", 6, 0, &[2, 3], &[3, 1])
            .s("a.bias")
            .tensor("11", 4, 1, &[3], &[1])
            .op(b"u}q\x02X\x09\x00\x00\x00_metadatah\x00)Rsb.")
            .0
    }

    #[test]
    fn reads_a_state_dict() {
        let v = read_bytes(&state_dict()).unwrap();
        let Value::Dict(items) = v else {
            panic!("not a dict")
        };
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].0, Value::Str("a.weight".into()));
        let Value::Tensor(t) = &items[0].1 else {
            panic!("not a tensor")
        };
        assert_eq!(t.shape, vec![2, 3]);
        assert_eq!(t.storage.key, "10");
        assert_eq!(t.storage.count, 6);
        assert_eq!(t.storage.dtype, DType::F32);
        let Value::Tensor(t) = &items[1].1 else {
            panic!("not a tensor")
        };
        assert_eq!((t.offset, t.numel()), (1, 3));
    }

    #[test]
    fn reads_plain_values() {
        // The magic number of the legacy format (LONG1, 10 bytes).
        let mut b = vec![0x80, 2, 0x8a, 10];
        b.extend_from_slice(&[0x6c, 0xfc, 0x9c, 0x46, 0xf9, 0x20, 0x6a, 0xa8, 0x50, 0x19]);
        b.push(b'.');
        assert!(matches!(read_bytes(&b).unwrap(), Value::BigInt(v) if v.len() == 10));
        let p = P::default()
            .op(b"\x80\x02]q\x00(")
            .s("x")
            .op(b"K\x05M\x00\x01\x8a\x01\xff\x88N")
            .op(b"e.");
        assert_eq!(
            read_bytes(&p.0).unwrap(),
            Value::List(vec![
                Value::Str("x".into()),
                Value::Int(5),
                Value::Int(256),
                Value::Int(-1),
                Value::Bool(true),
                Value::None,
            ])
        );
        // Memo copies.
        let p = P::default().op(b"K\x07q\x03h\x03\x86.");
        assert_eq!(
            read_bytes(&p.0).unwrap(),
            Value::Tuple(vec![Value::Int(7), Value::Int(7)])
        );
    }

    #[test]
    fn refuses_other_globals() {
        for (m, n) in [
            ("os", "system"),
            ("builtins", "eval"),
            ("torch", "load"),
            ("collections", "defaultdict"),
            ("torch._utils", "_rebuild_parameter"),
        ] {
            let p = P::default()
                .op(b"\x80\x02")
                .global(m, n)
                .s("echo hi")
                .op(b"\x85R.");
            let e = read_bytes(&p.0).unwrap_err();
            assert!(e.contains("refusing global"), "{e}");
        }
        // Also through STACK_GLOBAL.
        let p = P::default().s("os").s("system").op(b"\x93.");
        assert!(read_bytes(&p.0).unwrap_err().contains("refusing"));
        // Calling something that is not a global, or a storage type.
        let p = P::default().op(b"K\x01)R.");
        assert!(read_bytes(&p.0).is_err());
        let p = P::default().global("torch", "FloatStorage").op(b")R.");
        assert!(read_bytes(&p.0).is_err());
    }

    #[test]
    fn rejects_truncated_and_broken_input() {
        let full = state_dict();
        for n in 0..full.len() {
            assert!(read_bytes(&full[..n]).is_err(), "prefix of {n} bytes");
        }
        let bad: &[&[u8]] = &[
            b"0.",                         // POP: unsupported
            b"t.",                         // TUPLE without mark
            b"K\x01K\x02.",                // two values left
            b"(K\x01.",                    // open mark
            b"h\x00.",                     // unknown memo entry
            b"X\xff\xff\xff\x7fabc.",      // length beyond the input
            b"K\x01\x86.",                 // TUPLE2 with one value
            b"(K\x01\x85.",                // TUPLE1 reaching below the mark
            b"K\x01K\x02s.",               // SETITEM on a non-dict
            b"]K\x01K\x02s.",              // SETITEM on a list
            b"X\x02\x00\x00\x00\xff\xfe.", // not UTF-8
            b"\x80\x09.",                  // unknown protocol
        ];
        for b in bad {
            assert!(read_bytes(b).is_err(), "{b:?}");
        }
    }

    #[test]
    fn rejects_bad_tensors() {
        let wrap = |p: P| {
            let mut v = b"\x80\x02".to_vec();
            v.extend(p.0);
            v.push(b'.');
            v
        };
        // Fine.
        assert!(read_bytes(&wrap(P::default().tensor("1", 6, 0, &[2, 3], &[3, 1]))).is_ok());
        // Transposed (not contiguous).
        let e = read_bytes(&wrap(P::default().tensor("1", 6, 0, &[2, 3], &[1, 2])));
        assert!(e.unwrap_err().contains("contiguous"));
        // Beyond its storage.
        let e = read_bytes(&wrap(P::default().tensor("1", 6, 1, &[2, 3], &[3, 1])));
        assert!(e.unwrap_err().contains("outside"));
        // Negative size.
        let e = read_bytes(&wrap(P::default().tensor("1", 6, 0, &[-2, 3], &[3, 1])));
        assert!(e.is_err());
        // Huge sizes that overflow.
        let e = read_bytes(&wrap(P::default().tensor(
            "1",
            6,
            0,
            &[i32::MAX, i32::MAX, i32::MAX, i32::MAX],
            &[0, 0, 0, 1],
        )));
        assert!(e.is_err());
    }

    #[test]
    fn limits_nesting_and_copies() {
        // Tuples nested 100 deep.
        let mut b = b"N".to_vec();
        b.extend([0x85; 100]);
        b.push(b'.');
        assert!(read_bytes(&b).unwrap_err().contains("deeply"));
        // A tuple doubled through the memo again and again ("billion
        // laughs"): stopped by the value count.
        let mut b = b"(K\x01K\x02tq\x00".to_vec();
        for _ in 0..25 {
            b.extend_from_slice(b"h\x00h\x00\x86q\x00");
        }
        b.push(b'.');
        assert!(read_bytes(&b).unwrap_err().contains("too many values"));
    }
}
