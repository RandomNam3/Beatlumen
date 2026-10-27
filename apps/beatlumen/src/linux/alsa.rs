//! Audio output through ALSA (works with PulseAudio/PipeWire via their ALSA
//! plugins). Falls back to a silent clock-driven sink when no device is
//! available so playback still progresses.

#![allow(non_snake_case)]

use super::dl::Library;
use bl_app::host::{AudioDeviceInfo, AudioSource};
use std::ffi::{c_char, c_int, c_long, c_uint, c_ulong, c_void, CStr, CString};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

const SND_PCM_STREAM_PLAYBACK: c_int = 0;
const SND_PCM_FORMAT_S16_LE: c_int = 2;
const SND_PCM_FORMAT_FLOAT_LE: c_int = 14;
const SND_PCM_ACCESS_RW_INTERLEAVED: c_int = 3;

struct Alsa {
    _lib: Library,
    open: unsafe extern "C" fn(*mut *mut c_void, *const c_char, c_int, c_int) -> c_int,
    set_params: unsafe extern "C" fn(*mut c_void, c_int, c_int, c_uint, c_uint, c_int, c_uint) -> c_int,
    writei: unsafe extern "C" fn(*mut c_void, *const c_void, c_ulong) -> c_long,
    recover: unsafe extern "C" fn(*mut c_void, c_int, c_int) -> c_int,
    delay: unsafe extern "C" fn(*mut c_void, *mut c_long) -> c_int,
    close: unsafe extern "C" fn(*mut c_void) -> c_int,
    strerror: unsafe extern "C" fn(c_int) -> *const c_char,
}

impl Alsa {
    fn load() -> Result<Alsa, String> {
        let lib = Library::open(&["libasound.so.2", "libasound.so"])?;
        unsafe {
            Ok(Alsa {
                open: lib.sym("snd_pcm_open")?,
                set_params: lib.sym("snd_pcm_set_params")?,
                writei: lib.sym("snd_pcm_writei")?,
                recover: lib.sym("snd_pcm_recover")?,
                delay: lib.sym("snd_pcm_delay")?,
                close: lib.sym("snd_pcm_close")?,
                strerror: lib.sym("snd_strerror")?,
                _lib: lib,
            })
        }
    }
    fn err(&self, e: c_int) -> String {
        unsafe { CStr::from_ptr((self.strerror)(e)).to_string_lossy().into_owned() }
    }
}

struct Pcm(*mut c_void);
unsafe impl Send for Pcm {}

pub struct AudioOutput {
    running: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Drop for AudioOutput {
    fn drop(&mut self) {
        self.running.store(false, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

const RATE: u32 = 48000;
const PERIOD: usize = 1024;

/// Starts the audio thread pulling from `source`.
pub fn start(source: Arc<dyn AudioSource>, log: &dyn Fn(&str)) -> (AudioOutput, AudioDeviceInfo) {
    let running = Arc::new(AtomicBool::new(true));
    let info = AudioDeviceInfo { sample_rate: RATE, channels: 2 };
    let device = std::env::var("BEATLUMEN_ALSA_DEVICE").unwrap_or_else(|_| "default".into());
    match open_device(&device) {
        Ok((alsa, pcm, float)) => {
            log(&format!("audio: ALSA device '{device}' ({} Hz, {})", RATE, if float { "float" } else { "s16" }));
            let r = running.clone();
            let thread = std::thread::Builder::new()
                .name("bl-audio".into())
                .spawn(move || alsa_loop(alsa, pcm, float, source, r))
                .ok();
            (AudioOutput { running, thread }, info)
        }
        Err(e) => {
            log(&format!("audio: no ALSA output ({e}); using a silent clock"));
            let r = running.clone();
            let thread = std::thread::Builder::new().name("bl-audio-null".into()).spawn(move || null_loop(source, r)).ok();
            (AudioOutput { running, thread }, info)
        }
    }
}

fn open_device(name: &str) -> Result<(Alsa, Pcm, bool), String> {
    let alsa = Alsa::load()?;
    let cname = CString::new(name).map_err(|e| e.to_string())?;
    let mut pcm: *mut c_void = std::ptr::null_mut();
    let r = unsafe { (alsa.open)(&mut pcm, cname.as_ptr(), SND_PCM_STREAM_PLAYBACK, 0) };
    if r < 0 {
        return Err(format!("snd_pcm_open: {}", alsa.err(r)));
    }
    // 60 ms latency target.
    let latency_us = 60_000;
    for (fmt, float) in [(SND_PCM_FORMAT_FLOAT_LE, true), (SND_PCM_FORMAT_S16_LE, false)] {
        let r = unsafe { (alsa.set_params)(pcm, fmt, SND_PCM_ACCESS_RW_INTERLEAVED, 2, RATE, 1, latency_us) };
        if r >= 0 {
            return Ok((alsa, Pcm(pcm), float));
        }
    }
    unsafe { (alsa.close)(pcm) };
    Err("snd_pcm_set_params failed for float and s16".into())
}

fn alsa_loop(alsa: Alsa, pcm: Pcm, float: bool, source: Arc<dyn AudioSource>, running: Arc<AtomicBool>) {
    let pcm = pcm.0;
    let mut buf = vec![0f32; PERIOD * 2];
    let mut s16 = vec![0i16; PERIOD * 2];
    while running.load(Ordering::Relaxed) {
        let mut delay: c_long = 0;
        if unsafe { (alsa.delay)(pcm, &mut delay) } < 0 {
            delay = 0;
        }
        source.render(&mut buf, delay.max(0) as u32);
        let mut offset = 0usize;
        while offset < PERIOD && running.load(Ordering::Relaxed) {
            let n = unsafe {
                if float {
                    (alsa.writei)(pcm, buf[offset * 2..].as_ptr() as *const c_void, (PERIOD - offset) as c_ulong)
                } else {
                    for (d, s) in s16.iter_mut().zip(&buf) {
                        *d = (s.clamp(-1.0, 1.0) * 32767.0) as i16;
                    }
                    (alsa.writei)(pcm, s16[offset * 2..].as_ptr() as *const c_void, (PERIOD - offset) as c_ulong)
                }
            };
            if n < 0 {
                if unsafe { (alsa.recover)(pcm, n as c_int, 1) } < 0 {
                    std::thread::sleep(std::time::Duration::from_millis(20));
                    break;
                }
            } else {
                offset += n as usize;
            }
        }
    }
    unsafe { (alsa.close)(pcm) };
}

/// Consumes audio in real time without a device.
fn null_loop(source: Arc<dyn AudioSource>, running: Arc<AtomicBool>) {
    let mut buf = vec![0f32; PERIOD * 2];
    let period = std::time::Duration::from_secs_f64(PERIOD as f64 / RATE as f64);
    let mut next = std::time::Instant::now();
    while running.load(Ordering::Relaxed) {
        source.render(&mut buf, 0);
        next += period;
        let now = std::time::Instant::now();
        if next > now {
            std::thread::sleep(next - now);
        } else {
            next = now;
        }
    }
}
