//! A minimal Vulkan compute layer: one GPU, buffers in its memory, the
//! compute kernels of this crate and command lists that run them.
//!
//! Vulkan is loaded at run time (see `loader`); without it, or without a
//! suitable device, [`Gpu::new`] returns `None` and the caller computes
//! on the CPU. The API is safe: all `unsafe` code is in this crate, every
//! size and offset is checked before it reaches Vulkan, every command
//! list is waited for before its buffers can be dropped, and the device
//! is created with robust buffer access, so a kernel given wrong sizes
//! reads zeros or its own buffers instead of other memory. Only the
//! kernels compiled into this crate can be run (from `shaders/`, checked
//! with `spirv-val` when built).
//!
//! Errors from the device (lost, out of memory) are returned; after a
//! lost device every call fails, and the caller drops the [`Gpu`].

mod loader;
mod vk;

use std::ffi::{c_void, CStr};
use std::fmt;
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// Why a GPU operation failed.
#[derive(Debug, Clone)]
pub struct Error {
    msg: String,
}

impl Error {
    fn new(msg: impl Into<String>) -> Error {
        Error { msg: msg.into() }
    }

    fn vk(what: &str, code: vk::Result) -> Error {
        Error::new(format!("{what} failed (Vulkan error {code})"))
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.msg)
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

fn check(what: &str, code: vk::Result) -> Result<()> {
    if code == vk::SUCCESS {
        Ok(())
    } else {
        Err(Error::vk(what, code))
    }
}

/// The kind of device, as Vulkan reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceKind {
    Discrete,
    Integrated,
    Virtual,
    /// A software renderer (such as Mesa's lavapipe).
    Cpu,
    Other,
}

impl DeviceKind {
    fn from_vk(t: i32) -> DeviceKind {
        match t {
            vk::PHYSICAL_DEVICE_TYPE_DISCRETE_GPU => DeviceKind::Discrete,
            vk::PHYSICAL_DEVICE_TYPE_INTEGRATED_GPU => DeviceKind::Integrated,
            vk::PHYSICAL_DEVICE_TYPE_VIRTUAL_GPU => DeviceKind::Virtual,
            vk::PHYSICAL_DEVICE_TYPE_CPU => DeviceKind::Cpu,
            _ => DeviceKind::Other,
        }
    }

    /// Preference when several devices are there (higher is better).
    fn rank(self) -> u32 {
        match self {
            DeviceKind::Discrete => 4,
            DeviceKind::Integrated => 3,
            DeviceKind::Virtual => 2,
            DeviceKind::Other => 1,
            DeviceKind::Cpu => 0,
        }
    }
}

/// The compute kernels of this crate (GLSL sources in `shaders/`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Shader {
    /// `y = x Wᵀ + b` (optionally GELU, optionally added to y), W as
    /// f32. Buffers: x, W, bias, y. Push: n, k, m, flags (1: bias, 2:
    /// GELU, 4: add to y). Workgroups: (⌈m/64⌉, ⌈n/64⌉, 1).
    MatmulF32,
    /// As `MatmulF32`, W as f16.
    MatmulF16,
    /// As `MatmulF32`, W in ggml's q8_0 blocks (k a multiple of 32).
    MatmulQ8_0,
    /// As `MatmulF32`, W in ggml's q5_0 blocks.
    MatmulQ5_0,
    /// As `MatmulF32`, W in ggml's q5_1 blocks.
    MatmulQ5_1,
    /// As `MatmulF32`, W in ggml's q4_0 blocks.
    MatmulQ4_0,
    /// As `MatmulF32`, W in ggml's q4_1 blocks.
    MatmulQ4_1,
    /// Layer norm of rows. Buffers: x, weight, bias, y. Push: n, d.
    /// Workgroups: (n, 1, 1).
    LayerNorm,
    /// Attention without mask, heads of 64 values. Buffers: q, k, v,
    /// out. Push: query rows, key rows, row width. Workgroups:
    /// (⌈rows/64⌉, heads, 1).
    Attention,
    /// Patches of a kernel-3 convolution. Buffers: input, patches. Push:
    /// output rows, input rows, input channels, stride, channel-major
    /// input (0/1). 256 values per workgroup, workgroups numbered x
    /// first, then y.
    Im2col,
}

/// What Vulkan needs to know about a kernel.
struct ShaderInfo {
    spirv: &'static [u8],
    bindings: u32,
    push_words: u32,
    local_size: [u32; 3],
    shared_bytes: u32,
}

impl Shader {
    fn info(self) -> ShaderInfo {
        let mm = |spirv| ShaderInfo {
            spirv,
            bindings: 4,
            push_words: 4,
            local_size: [16, 16, 1],
            shared_bytes: 2 * 32 * 64 * 4,
        };
        match self {
            Shader::MatmulF32 => mm(include_bytes!("../shaders/matmul_f32.spv")),
            Shader::MatmulF16 => mm(include_bytes!("../shaders/matmul_f16.spv")),
            Shader::MatmulQ8_0 => mm(include_bytes!("../shaders/matmul_q8_0.spv")),
            Shader::MatmulQ5_0 => mm(include_bytes!("../shaders/matmul_q5_0.spv")),
            Shader::MatmulQ5_1 => mm(include_bytes!("../shaders/matmul_q5_1.spv")),
            Shader::MatmulQ4_0 => mm(include_bytes!("../shaders/matmul_q4_0.spv")),
            Shader::MatmulQ4_1 => mm(include_bytes!("../shaders/matmul_q4_1.spv")),
            Shader::LayerNorm => ShaderInfo {
                spirv: include_bytes!("../shaders/layer_norm.spv"),
                bindings: 4,
                push_words: 2,
                local_size: [256, 1, 1],
                shared_bytes: 256 * 4,
            },
            Shader::Attention => ShaderInfo {
                spirv: include_bytes!("../shaders/attention.spv"),
                bindings: 4,
                push_words: 3,
                local_size: [64, 1, 1],
                shared_bytes: 2 * 32 * 64 * 4,
            },
            Shader::Im2col => ShaderInfo {
                spirv: include_bytes!("../shaders/im2col.spv"),
                bindings: 2,
                push_words: 5,
                local_size: [256, 1, 1],
                shared_bytes: 0,
            },
        }
    }
}

/// The SPIR-V words of a module (little-endian file, magic number
/// first).
fn spirv_words(bytes: &[u8]) -> Result<Vec<u32>> {
    let words: Vec<u32> = bytes
        .chunks_exact(4)
        .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect();
    if bytes.len() % 4 != 0 || words.len() < 5 || words[0] != 0x0723_0203 {
        return Err(Error::new("invalid SPIR-V module"));
    }
    Ok(words)
}

/// Limits of the device that the checks need.
#[derive(Clone, Copy)]
struct Limits {
    max_storage_buffer_range: u64,
    max_push_constants_size: u32,
    max_shared_memory: u32,
    max_group_count: [u32; 3],
    max_group_invocations: u32,
    max_group_size: [u32; 3],
}

/// The logical device and what belongs to it for its whole life. Shared
/// (`Arc`) by everything created on it, so it is destroyed last.
struct Device {
    instance: vk::Instance,
    ifn: vk::InstanceFns,
    device: vk::Device,
    dfn: vk::DeviceFns,
    queue: vk::Queue,
    queue_family: u32,
    memory: vk::PhysicalDeviceMemoryProperties,
    limits: Limits,
    name: String,
    kind: DeviceKind,
    lost: AtomicBool,
}

// SAFETY: Vulkan handles may be used from any thread. The objects that
// need external synchronisation are the queue (used only by `Exec` under
// its mutex) and objects being destroyed (only in `Drop`, with exclusive
// access); creating and destroying other objects on a device is thread
// safe.
unsafe impl Send for Device {}
// SAFETY: as for Send; shared access only creates and destroys objects
// or reads the plain fields.
unsafe impl Sync for Device {}

impl Device {
    fn check_lost(&self) -> Result<()> {
        if self.lost.load(Ordering::Acquire) {
            Err(Error::new("GPU device lost"))
        } else {
            Ok(())
        }
    }

    /// A memory type allowed by `bits` with all `need` flags, preferably
    /// also `prefer`.
    fn memory_type(&self, bits: u32, need: u32, prefer: u32) -> Option<u32> {
        let count = (self.memory.memory_type_count as usize).min(32);
        let types = &self.memory.memory_types[..count];
        let find = |want: u32| {
            types
                .iter()
                .enumerate()
                .find(|(i, t)| bits & (1 << i) != 0 && t.property_flags & want == want)
                .map(|(i, _)| i as u32)
        };
        find(need | prefer).or_else(|| find(need))
    }

    /// A buffer with its own memory (`need`/`prefer`: memory property
    /// flags).
    fn raw_buffer(&self, size: u64, usage: u32, need: u32, prefer: u32) -> Result<RawBuffer> {
        let info = vk::BufferCreateInfo {
            s_type: vk::ST_BUFFER_CREATE_INFO,
            p_next: ptr::null(),
            flags: 0,
            size,
            usage,
            sharing_mode: vk::SHARING_MODE_EXCLUSIVE,
            queue_family_index_count: 0,
            queue_family_indices: ptr::null(),
        };
        let mut buffer = vk::NULL_HANDLE;
        // SAFETY: `info` is a valid create info (size > 0 checked by the
        // callers) and `buffer` a place for the handle.
        check("vkCreateBuffer", unsafe {
            (self.dfn.create_buffer)(self.device, &info, ptr::null(), &mut buffer)
        })?;
        let mut raw = RawBuffer {
            buffer,
            memory: vk::NULL_HANDLE,
        };
        let mut req = vk::MemoryRequirements::default();
        // SAFETY: `buffer` was just created on this device.
        unsafe { (self.dfn.get_buffer_memory_requirements)(self.device, buffer, &mut req) };
        let Some(type_index) = self.memory_type(req.memory_type_bits, need, prefer) else {
            self.destroy(&raw);
            return Err(Error::new("no suitable GPU memory type"));
        };
        let alloc = vk::MemoryAllocateInfo {
            s_type: vk::ST_MEMORY_ALLOCATE_INFO,
            p_next: ptr::null(),
            allocation_size: req.size,
            memory_type_index: type_index,
        };
        // SAFETY: valid allocate info for a memory type of this device.
        let r = unsafe {
            (self.dfn.allocate_memory)(self.device, &alloc, ptr::null(), &mut raw.memory)
        };
        if let Err(e) = check("vkAllocateMemory", r) {
            self.destroy(&raw);
            return Err(e);
        }
        // SAFETY: the memory was allocated for this buffer's requirements
        // (size and type), offset 0 satisfies any alignment.
        let r = unsafe { (self.dfn.bind_buffer_memory)(self.device, buffer, raw.memory, 0) };
        if let Err(e) = check("vkBindBufferMemory", r) {
            self.destroy(&raw);
            return Err(e);
        }
        Ok(raw)
    }

    /// Destroys a buffer and frees its memory (null handles are allowed).
    fn destroy(&self, raw: &RawBuffer) {
        // SAFETY: both handles belong to this device (or are null) and no
        // command using them is pending: every submission is waited for
        // before its buffers can be dropped.
        unsafe {
            (self.dfn.destroy_buffer)(self.device, raw.buffer, ptr::null());
            (self.dfn.free_memory)(self.device, raw.memory, ptr::null());
        }
    }
}

impl Drop for Device {
    fn drop(&mut self) {
        // SAFETY: this is the last reference: every object created on the
        // device (holding an Arc to it) is gone, so the device and then
        // the instance can be destroyed.
        unsafe {
            (self.dfn.device_wait_idle)(self.device);
            (self.dfn.destroy_device)(self.device, ptr::null());
            (self.ifn.destroy_instance)(self.instance, ptr::null());
        }
    }
}

struct RawBuffer {
    buffer: vk::Handle,
    memory: vk::Handle,
}

/// A buffer in GPU memory (storage buffer for the kernels).
pub struct Buffer {
    dev: Arc<Device>,
    raw: RawBuffer,
    size: u64,
}

impl Buffer {
    /// Size in bytes.
    pub fn size(&self) -> u64 {
        self.size
    }
}

impl Drop for Buffer {
    fn drop(&mut self) {
        self.dev.destroy(&self.raw);
    }
}

/// A compute pipeline for one of the crate's shaders.
pub struct Kernel {
    dev: Arc<Device>,
    shader: Shader,
    set_layout: vk::Handle,
    layout: vk::Handle,
    pipeline: vk::Handle,
    bindings: u32,
    push_words: u32,
}

impl Kernel {
    pub fn shader(&self) -> Shader {
        self.shader
    }
}

impl Drop for Kernel {
    fn drop(&mut self) {
        let (d, f) = (self.dev.device, &self.dev.dfn);
        // SAFETY: the handles belong to this device (null if creation
        // stopped early) and no submission using them is pending.
        unsafe {
            (f.destroy_pipeline)(d, self.pipeline, ptr::null());
            (f.destroy_pipeline_layout)(d, self.layout, ptr::null());
            (f.destroy_descriptor_set_layout)(d, self.set_layout, ptr::null());
        }
    }
}

/// Plain values that can be copied to and from GPU buffers.
pub trait Plain: Copy + sealed::Sealed {
    #[doc(hidden)]
    const SIZE: usize;
    #[doc(hidden)]
    fn put(self, out: &mut [u8]);
    #[doc(hidden)]
    fn get(b: &[u8]) -> Self;
}

mod sealed {
    pub trait Sealed {}
}

macro_rules! plain {
    ($($t:ty),*) => {$(
        impl sealed::Sealed for $t {}
        impl Plain for $t {
            const SIZE: usize = std::mem::size_of::<$t>();
            fn put(self, out: &mut [u8]) {
                out.copy_from_slice(&self.to_ne_bytes());
            }
            fn get(b: &[u8]) -> Self {
                let mut a = [0u8; std::mem::size_of::<$t>()];
                a.copy_from_slice(b);
                <$t>::from_ne_bytes(a)
            }
        }
    )*};
}
plain!(u8, u16, u32, f32);

enum Op<'a> {
    Dispatch {
        kernel: &'a Kernel,
        buffers: Vec<&'a Buffer>,
        push: Vec<u32>,
        groups: [u32; 3],
    },
    Copy {
        src: &'a Buffer,
        dst: &'a Buffer,
        region: vk::BufferCopy,
    },
}

/// A list of kernel runs and copies, run in order by [`Gpu::run`] (each
/// sees the results of the ones before). The buffers stay borrowed until
/// the list is dropped.
#[derive(Default)]
pub struct Commands<'a> {
    ops: Vec<Op<'a>>,
}

impl<'a> Commands<'a> {
    pub fn new() -> Commands<'a> {
        Commands { ops: Vec::new() }
    }

    /// Adds a run of `kernel` over `groups` workgroups, with `buffers`
    /// bound in order and `push` as its parameters (see [`Shader`]).
    pub fn dispatch(
        &mut self,
        kernel: &'a Kernel,
        buffers: &[&'a Buffer],
        push: &[u32],
        groups: [u32; 3],
    ) -> Result<()> {
        if buffers.len() != kernel.bindings as usize || push.len() != kernel.push_words as usize {
            return Err(Error::new("wrong number of kernel buffers or parameters"));
        }
        if buffers.iter().any(|b| !Arc::ptr_eq(&b.dev, &kernel.dev)) {
            return Err(Error::new("buffer of another GPU"));
        }
        let max = kernel.dev.limits.max_group_count;
        if groups.iter().zip(max).any(|(&g, m)| g == 0 || g > m) {
            return Err(Error::new("workgroup count out of range"));
        }
        self.ops.push(Op::Dispatch {
            kernel,
            buffers: buffers.to_vec(),
            push: push.to_vec(),
            groups,
        });
        Ok(())
    }

    /// Adds a copy of `bytes` bytes from `src` at `src_offset` to `dst`
    /// at `dst_offset`.
    pub fn copy(
        &mut self,
        src: &'a Buffer,
        src_offset: u64,
        dst: &'a Buffer,
        dst_offset: u64,
        bytes: u64,
    ) -> Result<()> {
        let fits = |off: u64, b: &Buffer| off.checked_add(bytes).is_some_and(|e| e <= b.size);
        if bytes == 0 || !fits(src_offset, src) || !fits(dst_offset, dst) {
            return Err(Error::new("copy outside the buffers"));
        }
        if !Arc::ptr_eq(&src.dev, &dst.dev) {
            return Err(Error::new("buffer of another GPU"));
        }
        if ptr::eq(src, dst) && src_offset < dst_offset + bytes && dst_offset < src_offset + bytes {
            return Err(Error::new("overlapping copy"));
        }
        self.ops.push(Op::Copy {
            src,
            dst,
            region: vk::BufferCopy {
                src_offset,
                dst_offset,
                size: bytes,
            },
        });
        Ok(())
    }

    pub fn is_empty(&self) -> bool {
        self.ops.is_empty()
    }
}

/// The staging buffer for transfers between the program and the GPU
/// (host-visible, coherent, mapped for its whole life).
struct Staging {
    raw: RawBuffer,
    ptr: *mut u8,
    size: usize,
}

/// Bytes of the staging buffer; larger transfers go in parts.
const STAGING_BYTES: usize = 16 << 20;

/// What submitting work needs: used under the mutex of [`Gpu`].
struct Exec {
    dev: Arc<Device>,
    pool: vk::Handle,
    cmd: vk::CommandBuffer,
    fence: vk::Handle,
    staging: Option<Staging>,
}

// SAFETY: the raw pointers (command buffer, mapped staging memory) are
// only used by the thread holding the mutex around `Exec`.
unsafe impl Send for Exec {}

impl Drop for Exec {
    fn drop(&mut self) {
        let (d, f) = (self.dev.device, &self.dev.dfn);
        // SAFETY: nothing is pending (every submission is waited for), so
        // the fence, the pool (with its command buffer) and the staging
        // memory (unmapped when freed) can go.
        unsafe {
            (f.destroy_fence)(d, self.fence, ptr::null());
            (f.destroy_command_pool)(d, self.pool, ptr::null());
        }
        if let Some(s) = &self.staging {
            self.dev.destroy(&s.raw);
        }
    }
}

impl Exec {
    /// The staging buffer, created on first use.
    fn staging(&mut self) -> Result<&mut Staging> {
        if self.staging.is_none() {
            let raw = self.dev.raw_buffer(
                STAGING_BYTES as u64,
                vk::BUFFER_USAGE_TRANSFER_SRC_BIT | vk::BUFFER_USAGE_TRANSFER_DST_BIT,
                vk::MEMORY_PROPERTY_HOST_VISIBLE_BIT | vk::MEMORY_PROPERTY_HOST_COHERENT_BIT,
                vk::MEMORY_PROPERTY_HOST_CACHED_BIT,
            )?;
            let mut p: *mut c_void = ptr::null_mut();
            // SAFETY: the memory is host-visible, not mapped yet, and the
            // range (whole allocation, at least STAGING_BYTES) is valid.
            let r = unsafe {
                (self.dev.dfn.map_memory)(self.dev.device, raw.memory, 0, u64::MAX, 0, &mut p)
            };
            if r != vk::SUCCESS || p.is_null() {
                self.dev.destroy(&raw);
                return Err(Error::vk("vkMapMemory", r));
            }
            let ptr = p.cast::<u8>();
            // SAFETY: the mapping covers STAGING_BYTES writable bytes;
            // zeroing them once means they are initialised from then on.
            unsafe { ptr::write_bytes(ptr, 0, STAGING_BYTES) };
            self.staging = Some(Staging {
                raw,
                ptr,
                size: STAGING_BYTES,
            });
        }
        Ok(self.staging.as_mut().expect("staging buffer just created"))
    }

    /// Records commands with `record` into the command buffer, submits
    /// them and waits until they are done.
    fn submit(&mut self, record: impl FnOnce(&vk::DeviceFns, vk::CommandBuffer)) -> Result<()> {
        self.dev.check_lost()?;
        let (d, f) = (self.dev.device, &self.dev.dfn);
        let cmd = self.cmd;
        // SAFETY: the pool's command buffer is not pending (the previous
        // submission was waited for), so it can be reset and recorded.
        check("vkResetCommandPool", unsafe {
            (f.reset_command_pool)(d, self.pool, 0)
        })?;
        let begin = vk::CommandBufferBeginInfo {
            s_type: vk::ST_COMMAND_BUFFER_BEGIN_INFO,
            p_next: ptr::null(),
            flags: vk::COMMAND_BUFFER_USAGE_ONE_TIME_SUBMIT_BIT,
            inheritance_info: ptr::null(),
        };
        // SAFETY: `cmd` is in the initial state after the reset.
        check("vkBeginCommandBuffer", unsafe {
            (f.begin_command_buffer)(cmd, &begin)
        })?;
        record(f, cmd);
        // SAFETY: `cmd` is recording.
        check("vkEndCommandBuffer", unsafe { (f.end_command_buffer)(cmd) })?;
        let submit = vk::SubmitInfo {
            s_type: vk::ST_SUBMIT_INFO,
            p_next: ptr::null(),
            wait_semaphore_count: 0,
            wait_semaphores: ptr::null(),
            wait_dst_stage_mask: ptr::null(),
            command_buffer_count: 1,
            command_buffers: &cmd,
            signal_semaphore_count: 0,
            signal_semaphores: ptr::null(),
        };
        // SAFETY: `cmd` is executable, the queue is used only under the
        // mutex (held through `&mut self`) and the fence is unsignalled.
        let r = unsafe { (f.queue_submit)(self.dev.queue, 1, &submit, self.fence) };
        if r != vk::SUCCESS {
            return Err(self.fail("vkQueueSubmit", r));
        }
        // SAFETY: the fence belongs to this device.
        let r = unsafe { (f.wait_for_fences)(d, 1, &self.fence, 1, u64::MAX) };
        if r != vk::SUCCESS {
            return Err(self.fail("vkWaitForFences", r));
        }
        // SAFETY: the fence is signalled and no longer used by the queue.
        check("vkResetFences", unsafe {
            (f.reset_fences)(d, 1, &self.fence)
        })
    }

    /// After a failed submission: waits until the device is idle (or
    /// lost), so nothing still uses the buffers, and marks it lost.
    fn fail(&self, what: &str, code: vk::Result) -> Error {
        // SAFETY: waiting for the device has no preconditions; the queue
        // is not used concurrently (mutex held).
        unsafe { (self.dev.dfn.device_wait_idle)(self.dev.device) };
        self.dev.lost.store(true, Ordering::Release);
        Error::vk(what, code)
    }
}

/// A full memory dependency between consecutive commands (kernels and
/// copies, in both directions).
fn barrier(f: &vk::DeviceFns, cmd: vk::CommandBuffer, dst_stage: u32, dst_access: u32) {
    let b = vk::MemoryBarrier {
        s_type: vk::ST_MEMORY_BARRIER,
        p_next: ptr::null(),
        src_access_mask: vk::ACCESS_SHADER_WRITE_BIT | vk::ACCESS_TRANSFER_WRITE_BIT,
        dst_access_mask: dst_access,
    };
    // SAFETY: `cmd` is recording; the barrier struct lives for the call.
    unsafe {
        (f.cmd_pipeline_barrier)(
            cmd,
            vk::PIPELINE_STAGE_COMPUTE_SHADER_BIT | vk::PIPELINE_STAGE_TRANSFER_BIT,
            dst_stage,
            0,
            1,
            &b,
            0,
            ptr::null(),
            0,
            ptr::null(),
        )
    };
}

fn barrier_all(f: &vk::DeviceFns, cmd: vk::CommandBuffer) {
    barrier(
        f,
        cmd,
        vk::PIPELINE_STAGE_COMPUTE_SHADER_BIT | vk::PIPELINE_STAGE_TRANSFER_BIT,
        vk::ACCESS_SHADER_READ_BIT
            | vk::ACCESS_SHADER_WRITE_BIT
            | vk::ACCESS_TRANSFER_READ_BIT
            | vk::ACCESS_TRANSFER_WRITE_BIT,
    );
}

/// A GPU, opened for compute work.
pub struct Gpu {
    dev: Arc<Device>,
    exec: Mutex<Exec>,
}

impl Gpu {
    /// The best GPU of the system, or `None` without Vulkan or without a
    /// suitable device. Software renderers are used only when the
    /// environment variable `BL_GPU_ALLOW_CPU=1` is set (for tests).
    pub fn new() -> Option<Gpu> {
        let allow_cpu = std::env::var("BL_GPU_ALLOW_CPU").is_ok_and(|v| v == "1");
        Gpu::open(allow_cpu).ok()
    }

    /// Like [`new`](Self::new), with the reason when there is no GPU;
    /// `allow_cpu` admits software renderers.
    pub fn open(allow_cpu: bool) -> Result<Gpu> {
        let gipa = loader::get_instance_proc_addr().ok_or_else(|| Error::new("no Vulkan"))?;
        // SAFETY: vkGetInstanceProcAddr with a null instance returns the
        // global functions (vkCreateInstance) or null.
        let entry =
            unsafe { vk::EntryFns::load(&mut |name: &CStr| gipa(ptr::null_mut(), name.as_ptr())) }
                .ok_or_else(|| Error::new("incomplete Vulkan loader"))?;
        let app = vk::ApplicationInfo {
            s_type: vk::ST_APPLICATION_INFO,
            p_next: ptr::null(),
            application_name: c"Beatlumen".as_ptr(),
            application_version: 0,
            engine_name: c"bl-gpu".as_ptr(),
            engine_version: 0,
            api_version: vk::API_VERSION_1_0,
        };
        let info = vk::InstanceCreateInfo {
            s_type: vk::ST_INSTANCE_CREATE_INFO,
            p_next: ptr::null(),
            flags: 0,
            application_info: &app,
            enabled_layer_count: 0,
            enabled_layer_names: ptr::null(),
            enabled_extension_count: 0,
            enabled_extension_names: ptr::null(),
        };
        let mut instance: vk::Instance = ptr::null_mut();
        // SAFETY: valid create info (no layers or extensions), pointers
        // live for the call.
        check("vkCreateInstance", unsafe {
            (entry.create_instance)(&info, ptr::null(), &mut instance)
        })?;
        // SAFETY: vkGetInstanceProcAddr with a live instance returns its
        // functions or null.
        let ifn =
            unsafe { vk::InstanceFns::load(&mut |name: &CStr| gipa(instance, name.as_ptr())) };
        let Some(ifn) = ifn else {
            // Without vkDestroyInstance the instance cannot be freed; it
            // is left (this does not happen with a working loader).
            return Err(Error::new("incomplete Vulkan loader"));
        };
        match Gpu::open_device(instance, ifn, allow_cpu) {
            Ok(g) => Ok(g),
            Err((Some(ifn), e)) => {
                // SAFETY: no device was created, so the instance has no
                // children and can be destroyed.
                unsafe { (ifn.destroy_instance)(instance, ptr::null()) };
                Err(e)
            }
            Err((None, e)) => Err(e),
        }
    }

    /// Picks a device of `instance` and creates the logical device, its
    /// queue, command pool and fence. On error the instance functions are
    /// handed back if the caller is to destroy the instance.
    fn open_device(
        instance: vk::Instance,
        ifn: vk::InstanceFns,
        allow_cpu: bool,
    ) -> std::result::Result<Gpu, (Option<vk::InstanceFns>, Error)> {
        let picked = match pick_device(instance, &ifn, allow_cpu) {
            Ok(p) => p,
            Err(e) => return Err((Some(ifn), e)),
        };
        let priority = 1.0f32;
        let queue_info = vk::DeviceQueueCreateInfo {
            s_type: vk::ST_DEVICE_QUEUE_CREATE_INFO,
            p_next: ptr::null(),
            flags: 0,
            queue_family_index: picked.queue_family,
            queue_count: 1,
            queue_priorities: &priority,
        };
        // Only robust buffer access: kernel accesses outside a bound
        // buffer stay inside it or read zeros.
        let mut features = vk::PhysicalDeviceFeatures([0; 55]);
        features.0[0] = 1;
        let info = vk::DeviceCreateInfo {
            s_type: vk::ST_DEVICE_CREATE_INFO,
            p_next: ptr::null(),
            flags: 0,
            queue_create_info_count: 1,
            queue_create_infos: &queue_info,
            enabled_layer_count: 0,
            enabled_layer_names: ptr::null(),
            enabled_extension_count: 0,
            enabled_extension_names: ptr::null(),
            enabled_features: &features,
        };
        let mut device: vk::Device = ptr::null_mut();
        // SAFETY: the physical device belongs to the instance, the queue
        // family exists, robustBufferAccess is supported (checked when
        // picking); all pointers live for the call.
        let r = unsafe { (ifn.create_device)(picked.physical, &info, ptr::null(), &mut device) };
        if r != vk::SUCCESS {
            return Err((Some(ifn), Error::vk("vkCreateDevice", r)));
        }
        let gdpa = ifn.get_device_proc_addr;
        // SAFETY: vkGetDeviceProcAddr with a live device returns its
        // functions or null.
        let dfn = unsafe { vk::DeviceFns::load(&mut |name: &CStr| gdpa(device, name.as_ptr())) };
        let Some(dfn) = dfn else {
            // As with the instance: nothing to destroy the device with, so
            // both are left (a conformant driver has all these functions).
            return Err((None, Error::new("incomplete Vulkan driver")));
        };
        let mut queue: vk::Queue = ptr::null_mut();
        // SAFETY: queue 0 of the family was requested at creation.
        unsafe { (dfn.get_device_queue)(device, picked.queue_family, 0, &mut queue) };
        let dev = Arc::new(Device {
            instance,
            ifn,
            device,
            dfn,
            queue,
            queue_family: picked.queue_family,
            memory: picked.memory,
            limits: picked.limits,
            name: picked.name,
            kind: picked.kind,
            lost: AtomicBool::new(false),
        });
        // From here dropping `dev` destroys device and instance.
        let exec = new_exec(&dev).map_err(|e| (None, e))?;
        Ok(Gpu {
            dev,
            exec: Mutex::new(exec),
        })
    }

    /// The device's name.
    pub fn name(&self) -> &str {
        &self.dev.name
    }

    pub fn kind(&self) -> DeviceKind {
        self.dev.kind
    }

    /// The largest buffer the kernels can use (bytes).
    pub fn max_buffer(&self) -> u64 {
        self.dev.limits.max_storage_buffer_range
    }

    fn exec(&self) -> Result<std::sync::MutexGuard<'_, Exec>> {
        self.exec
            .lock()
            .map_err(|_| Error::new("GPU state poisoned"))
    }

    /// A buffer of `bytes` bytes (rounded up to whole 4-byte words) in
    /// GPU memory; its contents are undefined until written.
    pub fn buffer(&self, bytes: u64) -> Result<Buffer> {
        self.dev.check_lost()?;
        let size = bytes
            .checked_add(3)
            .map(|b| b & !3)
            .filter(|&b| b > 0 && b <= self.dev.limits.max_storage_buffer_range)
            .ok_or_else(|| Error::new("GPU buffer size out of range"))?;
        let raw = self.dev.raw_buffer(
            size,
            vk::BUFFER_USAGE_STORAGE_BUFFER_BIT
                | vk::BUFFER_USAGE_TRANSFER_SRC_BIT
                | vk::BUFFER_USAGE_TRANSFER_DST_BIT,
            0,
            vk::MEMORY_PROPERTY_DEVICE_LOCAL_BIT,
        )?;
        Ok(Buffer {
            dev: self.dev.clone(),
            raw,
            size,
        })
    }

    /// A buffer holding `data`.
    pub fn buffer_with<T: Plain>(&self, data: &[T]) -> Result<Buffer> {
        let bytes = (data.len() as u64)
            .checked_mul(T::SIZE as u64)
            .ok_or_else(|| Error::new("GPU buffer size out of range"))?;
        let b = self.buffer(bytes)?;
        self.write(&b, 0, data)?;
        Ok(b)
    }

    /// The byte range `offset..offset + len * size` of `b`, if inside.
    fn range<T: Plain>(&self, b: &Buffer, offset: u64, len: usize) -> Result<u64> {
        if !Arc::ptr_eq(&b.dev, &self.dev) {
            return Err(Error::new("buffer of another GPU"));
        }
        (len as u64)
            .checked_mul(T::SIZE as u64)
            .filter(|&bytes| offset.checked_add(bytes).is_some_and(|e| e <= b.size))
            .ok_or_else(|| Error::new("transfer outside the buffer"))
    }

    /// Copies `data` into `dst` from byte `offset` on.
    pub fn write<T: Plain>(&self, dst: &Buffer, offset: u64, data: &[T]) -> Result<()> {
        self.range::<T>(dst, offset, data.len())?;
        let mut exec = self.exec()?;
        let per = exec.staging()?.size / T::SIZE;
        for (i, part) in data.chunks(per).enumerate() {
            let staging = exec.staging()?;
            // SAFETY: the mapping holds `size` initialised bytes and the
            // GPU is not using it (the last submission was waited for and
            // the mutex is held).
            let mem = unsafe { std::slice::from_raw_parts_mut(staging.ptr, staging.size) };
            for (o, &v) in mem.chunks_exact_mut(T::SIZE).zip(part) {
                v.put(o);
            }
            let src = staging.raw.buffer;
            let region = vk::BufferCopy {
                src_offset: 0,
                dst_offset: offset + (i * per * T::SIZE) as u64,
                size: (part.len() * T::SIZE) as u64,
            };
            let dst_buf = dst.raw.buffer;
            exec.submit(|f, cmd| {
                // SAFETY: both buffers are live and the region lies inside
                // both (checked above; part ≤ staging size).
                unsafe { (f.cmd_copy_buffer)(cmd, src, dst_buf, 1, &region) };
            })?;
        }
        Ok(())
    }

    /// Copies from `src` at byte `offset` into `out`.
    pub fn read<T: Plain>(&self, src: &Buffer, offset: u64, out: &mut [T]) -> Result<()> {
        self.range::<T>(src, offset, out.len())?;
        let mut exec = self.exec()?;
        let per = exec.staging()?.size / T::SIZE;
        for (i, part) in out.chunks_mut(per).enumerate() {
            let staging = exec.staging()?;
            let (dst_buf, ptr, size) = (staging.raw.buffer, staging.ptr, staging.size);
            let region = vk::BufferCopy {
                src_offset: offset + (i * per * T::SIZE) as u64,
                dst_offset: 0,
                size: (part.len() * T::SIZE) as u64,
            };
            let src_buf = src.raw.buffer;
            exec.submit(|f, cmd| {
                // SAFETY: both buffers are live and the region lies inside
                // both (checked above; part ≤ staging size).
                unsafe { (f.cmd_copy_buffer)(cmd, src_buf, dst_buf, 1, &region) };
                barrier(
                    f,
                    cmd,
                    vk::PIPELINE_STAGE_HOST_BIT,
                    vk::ACCESS_HOST_READ_BIT,
                );
            })?;
            // SAFETY: as in `write`; the copy is complete and made visible
            // to the host by the barrier and the fence.
            let mem = unsafe { std::slice::from_raw_parts(ptr, size) };
            for (o, b) in part.iter_mut().zip(mem.chunks_exact(T::SIZE)) {
                *o = T::get(b);
            }
        }
        Ok(())
    }

    /// The pipeline for `shader`; fails if the device's limits are too
    /// small for it.
    pub fn kernel(&self, shader: Shader) -> Result<Kernel> {
        self.dev.check_lost()?;
        let info = shader.info();
        let l = &self.dev.limits;
        let invocations: u32 = info.local_size.iter().product();
        if info.push_words * 4 > l.max_push_constants_size
            || info.shared_bytes > l.max_shared_memory
            || invocations > l.max_group_invocations
            || info
                .local_size
                .iter()
                .zip(l.max_group_size)
                .any(|(&s, m)| s > m)
        {
            return Err(Error::new("GPU limits too small for the kernel"));
        }
        let code = spirv_words(info.spirv)?;
        let (d, f) = (self.dev.device, &self.dev.dfn);
        let mut k = Kernel {
            dev: self.dev.clone(),
            shader,
            set_layout: vk::NULL_HANDLE,
            layout: vk::NULL_HANDLE,
            pipeline: vk::NULL_HANDLE,
            bindings: info.bindings,
            push_words: info.push_words,
        };
        let bindings: Vec<vk::DescriptorSetLayoutBinding> = (0..info.bindings)
            .map(|i| vk::DescriptorSetLayoutBinding {
                binding: i,
                descriptor_type: vk::DESCRIPTOR_TYPE_STORAGE_BUFFER,
                descriptor_count: 1,
                stage_flags: vk::SHADER_STAGE_COMPUTE_BIT,
                immutable_samplers: ptr::null(),
            })
            .collect();
        let set_info = vk::DescriptorSetLayoutCreateInfo {
            s_type: vk::ST_DESCRIPTOR_SET_LAYOUT_CREATE_INFO,
            p_next: ptr::null(),
            flags: 0,
            binding_count: info.bindings,
            bindings: bindings.as_ptr(),
        };
        // SAFETY: valid create info; `bindings` lives for the call. On
        // error `k` is dropped and destroys what was created (null
        // handles are ignored).
        check("vkCreateDescriptorSetLayout", unsafe {
            (f.create_descriptor_set_layout)(d, &set_info, ptr::null(), &mut k.set_layout)
        })?;
        let push = vk::PushConstantRange {
            stage_flags: vk::SHADER_STAGE_COMPUTE_BIT,
            offset: 0,
            size: info.push_words * 4,
        };
        let layout_info = vk::PipelineLayoutCreateInfo {
            s_type: vk::ST_PIPELINE_LAYOUT_CREATE_INFO,
            p_next: ptr::null(),
            flags: 0,
            set_layout_count: 1,
            set_layouts: &k.set_layout,
            push_constant_range_count: 1,
            push_constant_ranges: &push,
        };
        // SAFETY: valid create info; the set layout is live; the push
        // range is within the device limit (checked above).
        check("vkCreatePipelineLayout", unsafe {
            (f.create_pipeline_layout)(d, &layout_info, ptr::null(), &mut k.layout)
        })?;
        let module_info = vk::ShaderModuleCreateInfo {
            s_type: vk::ST_SHADER_MODULE_CREATE_INFO,
            p_next: ptr::null(),
            flags: 0,
            code_size: code.len() * 4,
            code: code.as_ptr(),
        };
        let mut module = vk::NULL_HANDLE;
        // SAFETY: the code is one of this crate's modules, valid SPIR-V
        // for Vulkan 1.0 (checked with spirv-val when compiled).
        check("vkCreateShaderModule", unsafe {
            (f.create_shader_module)(d, &module_info, ptr::null(), &mut module)
        })?;
        let pipe_info = vk::ComputePipelineCreateInfo {
            s_type: vk::ST_COMPUTE_PIPELINE_CREATE_INFO,
            p_next: ptr::null(),
            flags: 0,
            stage: vk::PipelineShaderStageCreateInfo {
                s_type: vk::ST_PIPELINE_SHADER_STAGE_CREATE_INFO,
                p_next: ptr::null(),
                flags: 0,
                stage: vk::SHADER_STAGE_COMPUTE_BIT,
                module,
                name: c"main".as_ptr(),
                specialization_info: ptr::null(),
            },
            layout: k.layout,
            base_pipeline_handle: vk::NULL_HANDLE,
            base_pipeline_index: -1,
        };
        // SAFETY: valid create info whose layout matches the shader's
        // bindings and push constants; the module may be destroyed once
        // the pipeline exists.
        let r = unsafe {
            let r = (f.create_compute_pipelines)(
                d,
                vk::NULL_HANDLE,
                1,
                &pipe_info,
                ptr::null(),
                &mut k.pipeline,
            );
            (f.destroy_shader_module)(d, module, ptr::null());
            r
        };
        check("vkCreateComputePipelines", r)?;
        Ok(k)
    }

    /// Runs `cmds` and waits until they are done.
    pub fn run(&self, cmds: &Commands) -> Result<()> {
        if cmds.ops.is_empty() {
            return Ok(());
        }
        let ours = |b: &Buffer| Arc::ptr_eq(&b.dev, &self.dev);
        let mut sets = 0u32;
        let mut descriptors = 0u32;
        for op in &cmds.ops {
            match op {
                Op::Dispatch {
                    kernel, buffers, ..
                } => {
                    if !Arc::ptr_eq(&kernel.dev, &self.dev) || !buffers.iter().all(|b| ours(b)) {
                        return Err(Error::new("kernel or buffer of another GPU"));
                    }
                    sets += 1;
                    descriptors += kernel.bindings;
                }
                Op::Copy { src, dst, .. } => {
                    if !ours(src) || !ours(dst) {
                        return Err(Error::new("buffer of another GPU"));
                    }
                }
            }
        }
        let mut exec = self.exec()?;
        exec.dev.check_lost()?;
        let pool = DescriptorPool::new(&self.dev, sets, descriptors)?;
        let layouts: Vec<vk::Handle> = cmds
            .ops
            .iter()
            .filter_map(|op| match op {
                Op::Dispatch { kernel, .. } => Some(kernel.set_layout),
                Op::Copy { .. } => None,
            })
            .collect();
        let handles = pool.allocate(&layouts)?;
        // Descriptor contents: all buffer infos first (not moved while the
        // writes point into them).
        let mut infos = Vec::with_capacity(descriptors as usize);
        for op in &cmds.ops {
            if let Op::Dispatch { buffers, .. } = op {
                infos.extend(buffers.iter().map(|b| vk::DescriptorBufferInfo {
                    buffer: b.raw.buffer,
                    offset: 0,
                    range: b.size,
                }));
            }
        }
        let mut writes = Vec::with_capacity(descriptors as usize);
        let mut at = 0usize;
        for (set, op) in handles
            .iter()
            .zip(cmds.ops.iter().filter_map(|op| match op {
                Op::Dispatch { buffers, .. } => Some(buffers.len()),
                Op::Copy { .. } => None,
            }))
        {
            for binding in 0..op {
                writes.push(vk::WriteDescriptorSet {
                    s_type: vk::ST_WRITE_DESCRIPTOR_SET,
                    p_next: ptr::null(),
                    dst_set: *set,
                    dst_binding: binding as u32,
                    dst_array_element: 0,
                    descriptor_count: 1,
                    descriptor_type: vk::DESCRIPTOR_TYPE_STORAGE_BUFFER,
                    image_info: ptr::null(),
                    buffer_info: &infos[at],
                    texel_buffer_view: ptr::null(),
                });
                at += 1;
            }
        }
        // SAFETY: each write names a live set of the pool, a binding of
        // its layout (storage buffer) and a live buffer whose whole size
        // is within maxStorageBufferRange (checked at creation); `infos`
        // outlives the call.
        unsafe {
            (self.dev.dfn.update_descriptor_sets)(
                self.dev.device,
                writes.len() as u32,
                writes.as_ptr(),
                0,
                ptr::null(),
            )
        };
        let last = cmds.ops.len() - 1;
        exec.submit(|f, cmd| {
            let mut sets = handles.iter();
            for (i, op) in cmds.ops.iter().enumerate() {
                match op {
                    Op::Dispatch {
                        kernel,
                        push,
                        groups,
                        ..
                    } => {
                        let set = sets.next().expect("one set per dispatch");
                        // SAFETY: `cmd` is recording; pipeline, layout and
                        // set belong together and are live; the push data
                        // has exactly the size of the layout's range; the
                        // group counts are within the limits (checked when
                        // added).
                        unsafe {
                            (f.cmd_bind_pipeline)(
                                cmd,
                                vk::PIPELINE_BIND_POINT_COMPUTE,
                                kernel.pipeline,
                            );
                            (f.cmd_bind_descriptor_sets)(
                                cmd,
                                vk::PIPELINE_BIND_POINT_COMPUTE,
                                kernel.layout,
                                0,
                                1,
                                set,
                                0,
                                ptr::null(),
                            );
                            (f.cmd_push_constants)(
                                cmd,
                                kernel.layout,
                                vk::SHADER_STAGE_COMPUTE_BIT,
                                0,
                                (push.len() * 4) as u32,
                                push.as_ptr().cast(),
                            );
                            (f.cmd_dispatch)(cmd, groups[0], groups[1], groups[2]);
                        }
                    }
                    Op::Copy { src, dst, region } => {
                        // SAFETY: live buffers; the region lies inside both
                        // and does not overlap itself (checked when added).
                        unsafe {
                            (f.cmd_copy_buffer)(cmd, src.raw.buffer, dst.raw.buffer, 1, region)
                        };
                    }
                }
                if i < last {
                    barrier_all(f, cmd);
                }
            }
        })
    }
}

/// Descriptor sets for one run, freed with the pool after it.
struct DescriptorPool<'a> {
    dev: &'a Device,
    pool: vk::Handle,
}

impl<'a> DescriptorPool<'a> {
    fn new(dev: &'a Device, sets: u32, descriptors: u32) -> Result<DescriptorPool<'a>> {
        let size = vk::DescriptorPoolSize {
            descriptor_type: vk::DESCRIPTOR_TYPE_STORAGE_BUFFER,
            descriptor_count: descriptors.max(1),
        };
        let info = vk::DescriptorPoolCreateInfo {
            s_type: vk::ST_DESCRIPTOR_POOL_CREATE_INFO,
            p_next: ptr::null(),
            flags: 0,
            max_sets: sets.max(1),
            pool_size_count: 1,
            pool_sizes: &size,
        };
        let mut pool = vk::NULL_HANDLE;
        // SAFETY: valid create info (counts above zero).
        check("vkCreateDescriptorPool", unsafe {
            (dev.dfn.create_descriptor_pool)(dev.device, &info, ptr::null(), &mut pool)
        })?;
        Ok(DescriptorPool { dev, pool })
    }

    fn allocate(&self, layouts: &[vk::Handle]) -> Result<Vec<vk::Handle>> {
        let mut sets = vec![vk::NULL_HANDLE; layouts.len()];
        if layouts.is_empty() {
            return Ok(sets);
        }
        let info = vk::DescriptorSetAllocateInfo {
            s_type: vk::ST_DESCRIPTOR_SET_ALLOCATE_INFO,
            p_next: ptr::null(),
            descriptor_pool: self.pool,
            descriptor_set_count: layouts.len() as u32,
            set_layouts: layouts.as_ptr(),
        };
        // SAFETY: the pool was sized for exactly these sets; `sets` has
        // room for one handle per layout.
        check("vkAllocateDescriptorSets", unsafe {
            (self.dev.dfn.allocate_descriptor_sets)(self.dev.device, &info, sets.as_mut_ptr())
        })?;
        Ok(sets)
    }
}

impl Drop for DescriptorPool<'_> {
    fn drop(&mut self) {
        // SAFETY: the run using the sets was waited for (or never
        // submitted); destroying the pool frees its sets.
        unsafe { (self.dev.dfn.destroy_descriptor_pool)(self.dev.device, self.pool, ptr::null()) };
    }
}

/// The physical device chosen and what is known about it.
struct Picked {
    physical: vk::PhysicalDevice,
    queue_family: u32,
    memory: vk::PhysicalDeviceMemoryProperties,
    limits: Limits,
    name: String,
    kind: DeviceKind,
}

/// The best device with a compute queue and robust buffer access.
fn pick_device(instance: vk::Instance, ifn: &vk::InstanceFns, allow_cpu: bool) -> Result<Picked> {
    let mut count = 0u32;
    // SAFETY: counting only (null array).
    check("vkEnumeratePhysicalDevices", unsafe {
        (ifn.enumerate_physical_devices)(instance, &mut count, ptr::null_mut())
    })?;
    let mut devices = vec![ptr::null_mut(); count as usize];
    // SAFETY: `devices` has room for `count` handles.
    let r = unsafe { (ifn.enumerate_physical_devices)(instance, &mut count, devices.as_mut_ptr()) };
    if r != vk::SUCCESS && r != vk::INCOMPLETE {
        return Err(Error::vk("vkEnumeratePhysicalDevices", r));
    }
    devices.truncate(count as usize);
    let mut best: Option<Picked> = None;
    for &physical in &devices {
        // SAFETY (this block): `physical` came from the enumeration; the
        // structures are fully written by the calls, the arrays have the
        // counted room.
        let (props, features, families, memory) = unsafe {
            let mut props = std::mem::MaybeUninit::<vk::PhysicalDeviceProperties>::zeroed();
            (ifn.get_physical_device_properties)(physical, props.as_mut_ptr());
            let mut features = vk::PhysicalDeviceFeatures([0; 55]);
            (ifn.get_physical_device_features)(physical, &mut features);
            let mut n = 0u32;
            (ifn.get_physical_device_queue_family_properties)(physical, &mut n, ptr::null_mut());
            let mut families = vec![vk::QueueFamilyProperties::default(); n as usize];
            (ifn.get_physical_device_queue_family_properties)(
                physical,
                &mut n,
                families.as_mut_ptr(),
            );
            families.truncate(n as usize);
            let mut memory = std::mem::MaybeUninit::<vk::PhysicalDeviceMemoryProperties>::zeroed();
            (ifn.get_physical_device_memory_properties)(physical, memory.as_mut_ptr());
            (
                props.assume_init(),
                features,
                families,
                memory.assume_init(),
            )
        };
        let kind = DeviceKind::from_vk(props.device_type);
        if kind == DeviceKind::Cpu && !allow_cpu {
            continue;
        }
        let Some(queue_family) = families
            .iter()
            .position(|f| f.queue_flags & vk::QUEUE_COMPUTE_BIT != 0 && f.queue_count > 0)
        else {
            continue;
        };
        if features.0[0] == 0 {
            continue;
        }
        if best.as_ref().is_some_and(|b| b.kind.rank() >= kind.rank()) {
            continue;
        }
        let name = CStr::from_bytes_until_nul(&props.device_name)
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let l = &props.limits;
        best = Some(Picked {
            physical,
            queue_family: queue_family as u32,
            memory,
            limits: Limits {
                max_storage_buffer_range: l.max_storage_buffer_range as u64,
                max_push_constants_size: l.max_push_constants_size,
                max_shared_memory: l.max_compute_shared_memory_size,
                max_group_count: l.max_compute_work_group_count,
                max_group_invocations: l.max_compute_work_group_invocations,
                max_group_size: l.max_compute_work_group_size,
            },
            name,
            kind,
        });
    }
    best.ok_or_else(|| Error::new("no suitable Vulkan device"))
}

/// The command pool, its command buffer and the fence of a new device.
fn new_exec(dev: &Arc<Device>) -> Result<Exec> {
    let (d, f) = (dev.device, &dev.dfn);
    let mut exec = Exec {
        dev: dev.clone(),
        pool: vk::NULL_HANDLE,
        cmd: ptr::null_mut(),
        fence: vk::NULL_HANDLE,
        staging: None,
    };
    let pool_info = vk::CommandPoolCreateInfo {
        s_type: vk::ST_COMMAND_POOL_CREATE_INFO,
        p_next: ptr::null(),
        flags: 0,
        queue_family_index: dev.queue_family,
    };
    // SAFETY (the three calls): valid create infos for this device; on
    // error `exec` is dropped and destroys what exists (null handles are
    // ignored).
    check("vkCreateCommandPool", unsafe {
        (f.create_command_pool)(d, &pool_info, ptr::null(), &mut exec.pool)
    })?;
    let alloc = vk::CommandBufferAllocateInfo {
        s_type: vk::ST_COMMAND_BUFFER_ALLOCATE_INFO,
        p_next: ptr::null(),
        command_pool: exec.pool,
        level: vk::COMMAND_BUFFER_LEVEL_PRIMARY,
        command_buffer_count: 1,
    };
    check("vkAllocateCommandBuffers", unsafe {
        (f.allocate_command_buffers)(d, &alloc, &mut exec.cmd)
    })?;
    let fence_info = vk::FenceCreateInfo {
        s_type: vk::ST_FENCE_CREATE_INFO,
        p_next: ptr::null(),
        flags: 0,
    };
    check("vkCreateFence", unsafe {
        (f.create_fence)(d, &fence_info, ptr::null(), &mut exec.fence)
    })?;
    Ok(exec)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [Shader; 10] = [
        Shader::MatmulF32,
        Shader::MatmulF16,
        Shader::MatmulQ8_0,
        Shader::MatmulQ5_0,
        Shader::MatmulQ5_1,
        Shader::MatmulQ4_0,
        Shader::MatmulQ4_1,
        Shader::LayerNorm,
        Shader::Attention,
        Shader::Im2col,
    ];

    #[test]
    fn modules_are_spirv() {
        for s in ALL {
            assert!(spirv_words(s.info().spirv).is_ok(), "{s:?}");
        }
        assert!(spirv_words(&[3, 2, 0x23, 7, 0]).is_err());
    }

    /// A software or real device for the tests, if there is one.
    fn gpu() -> Option<Gpu> {
        match Gpu::open(true) {
            Ok(g) => Some(g),
            Err(e) => {
                eprintln!("no Vulkan device, skipped: {e}");
                None
            }
        }
    }

    #[test]
    fn transfers_and_checks() {
        let Some(g) = gpu() else { return };
        let data: Vec<f32> = (0..10_000).map(|i| i as f32 * 0.5).collect();
        let a = g.buffer_with(&data).unwrap();
        let mut back = vec![0f32; 10_000];
        g.read(&a, 0, &mut back).unwrap();
        assert_eq!(back, data);
        // Ranges outside the buffer are refused.
        assert!(g.write(&a, 4, &data).is_err());
        assert!(g.read(&a, u64::MAX - 2, &mut back[..1]).is_err());
        assert!(g.buffer(0).is_err());
        assert!(g.buffer(u64::MAX).is_err());
        // Copies on the GPU.
        let b = g.buffer(40_000).unwrap();
        let mut c = Commands::new();
        c.copy(&a, 400, &b, 0, 40_000 - 400).unwrap();
        assert!(c.copy(&a, 0, &b, 4, 40_000).is_err());
        assert!(c.copy(&a, 0, &a, 4, 8).is_err());
        g.run(&c).unwrap();
        g.read(&b, 0, &mut back[..9_900]).unwrap();
        assert_eq!(back[..9_900], data[100..]);
        // Kernels check their buffers, parameters and workgroups.
        let k = g.kernel(Shader::Im2col).unwrap();
        let mut c = Commands::new();
        assert!(c.dispatch(&k, &[&a], &[0; 5], [1, 1, 1]).is_err());
        assert!(c.dispatch(&k, &[&a, &b], &[0; 4], [1, 1, 1]).is_err());
        assert!(c.dispatch(&k, &[&a, &b], &[0; 5], [0, 1, 1]).is_err());
        assert!(c
            .dispatch(&k, &[&a, &b], &[0; 5], [u32::MAX, 1, 1])
            .is_err());
    }

    #[test]
    fn large_transfers_in_parts() {
        let Some(g) = gpu() else { return };
        let n = STAGING_BYTES / 2 * 3 + 7;
        let data: Vec<u16> = (0..n).map(|i| (i * 7) as u16).collect();
        let a = g.buffer_with(&data).unwrap();
        let mut back = vec![0u16; n];
        g.read(&a, 0, &mut back).unwrap();
        assert!(back == data);
    }

    #[test]
    fn convolution_patches() {
        let Some(g) = gpu() else { return };
        // Two channels, five times, channel-major; stride 2.
        let x: Vec<f32> = (1..=10).map(|v| v as f32).collect();
        let src = g.buffer_with(&x).unwrap();
        let dst = g.buffer(3 * 6 * 4).unwrap();
        let k = g.kernel(Shader::Im2col).unwrap();
        let mut c = Commands::new();
        c.dispatch(&k, &[&src, &dst], &[3, 5, 2, 2, 1], [1, 1, 1])
            .unwrap();
        g.run(&c).unwrap();
        let mut out = vec![0f32; 18];
        g.read(&dst, 0, &mut out).unwrap();
        #[rustfmt::skip]
        assert_eq!(out, [
            0.0, 1.0, 2.0, 0.0, 6.0, 7.0,
            2.0, 3.0, 4.0, 7.0, 8.0, 9.0,
            4.0, 5.0, 0.0, 9.0, 10.0, 0.0,
        ]);
    }
}
