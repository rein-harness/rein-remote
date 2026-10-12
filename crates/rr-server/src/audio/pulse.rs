//! Linux: what the machine plays, from the default output's monitor, through PulseAudio's simple
//! API (PipeWire answers it too, through pipewire-pulse). libpulse-simple is opened when sound is
//! first wanted, not linked: the binary runs where it isn't installed (a server, a container), and
//! only the sound is missing there.
//!
//! The source is `@DEFAULT_MONITOR@` (whatever the default output is, followed when it changes), or
//! REIN_REMOTE_AUDIO_SOURCE (a name from `pactl list short sources`).

use super::{now_us, Block, Capture, Sink, FRAME, RATE};
use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::Duration;

#[repr(C)]
struct SampleSpec {
    format: c_int,
    rate: u32,
    channels: u8,
}

#[repr(C)]
struct BufferAttr {
    maxlength: u32,
    tlength: u32,
    prebuf: u32,
    minreq: u32,
    fragsize: u32,
}

const PA_STREAM_RECORD: c_int = 2;
const PA_SAMPLE_FLOAT32LE: c_int = 5;

type New = unsafe extern "C" fn(
    server: *const c_char,
    name: *const c_char,
    dir: c_int,
    dev: *const c_char,
    stream_name: *const c_char,
    ss: *const SampleSpec,
    map: *const c_void,
    attr: *const BufferAttr,
    error: *mut c_int,
) -> *mut c_void;
type Read = unsafe extern "C" fn(s: *mut c_void, data: *mut c_void, bytes: usize, error: *mut c_int) -> c_int;
type Latency = unsafe extern "C" fn(s: *mut c_void, error: *mut c_int) -> u64;
type Free = unsafe extern "C" fn(s: *mut c_void);
type Strerror = unsafe extern "C" fn(error: c_int) -> *const c_char;

struct Lib {
    new: New,
    read: Read,
    latency: Latency,
    free: Free,
    strerror: Option<Strerror>,
}

/// libpulse-simple's functions, if it's on this machine.
fn load() -> Result<Lib, String> {
    unsafe {
        let mut lib = std::ptr::null_mut();
        for name in [c"libpulse-simple.so.0", c"libpulse-simple.so"] {
            lib = libc::dlopen(name.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL);
            if !lib.is_null() {
                break;
            }
        }
        if lib.is_null() {
            return Err("libpulse-simple isn't installed (PulseAudio or PipeWire's pulse server, e.g. the libpulse0 package)".into());
        }
        let sym = |name: &CStr| {
            let p = libc::dlsym(lib, name.as_ptr());
            (!p.is_null()).then_some(p)
        };
        let missing = || "libpulse-simple is missing a function".to_string();
        Ok(Lib {
            new: std::mem::transmute::<*mut c_void, New>(sym(c"pa_simple_new").ok_or_else(missing)?),
            read: std::mem::transmute::<*mut c_void, Read>(sym(c"pa_simple_read").ok_or_else(missing)?),
            latency: std::mem::transmute::<*mut c_void, Latency>(sym(c"pa_simple_get_latency").ok_or_else(missing)?),
            free: std::mem::transmute::<*mut c_void, Free>(sym(c"pa_simple_free").ok_or_else(missing)?),
            // In libpulse, which libpulse-simple loads (dlsym looks there too).
            strerror: sym(c"pa_strerror").map(|p| std::mem::transmute::<*mut c_void, Strerror>(p)),
        })
    }
}

fn error(lib: &Lib, code: c_int) -> String {
    match lib.strerror {
        Some(f) => unsafe {
            let p = f(code);
            if p.is_null() {
                format!("error {code}")
            } else {
                CStr::from_ptr(p).to_string_lossy().into_owned()
            }
        },
        None => format!("error {code}"),
    }
}

struct Stream(*mut c_void);
unsafe impl Send for Stream {}

pub struct Pulse {
    stop: Arc<AtomicBool>,
}

impl Capture for Pulse {}

impl Drop for Pulse {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

impl Pulse {
    pub fn start(sink: Sink) -> Result<Self, String> {
        let lib = load()?;
        let source = std::env::var("REIN_REMOTE_AUDIO_SOURCE").unwrap_or_else(|_| "@DEFAULT_MONITOR@".into());
        let dev = CString::new(source.clone()).map_err(|_| "REIN_REMOTE_AUDIO_SOURCE has a NUL")?;
        let spec = SampleSpec {
            format: PA_SAMPLE_FLOAT32LE,
            rate: RATE,
            channels: 2,
        };
        // Handed over every 10 ms (the server's default would be ~2 s for a recording).
        let attr = BufferAttr {
            maxlength: u32::MAX,
            tlength: u32::MAX,
            prebuf: u32::MAX,
            minreq: u32::MAX,
            fragsize: (FRAME * 2 * 4) as u32,
        };
        let mut err = 0;
        let s = unsafe {
            (lib.new)(
                std::ptr::null(),
                c"rein-remote".as_ptr(),
                PA_STREAM_RECORD,
                dev.as_ptr(),
                c"what this machine plays".as_ptr(),
                &spec,
                std::ptr::null(),
                &attr,
                &mut err,
            )
        };
        if s.is_null() {
            return Err(format!("PulseAudio, {source}: {}", error(&lib, err)));
        }
        let stream = Stream(s);
        let stop = Arc::new(AtomicBool::new(false));
        let done = stop.clone();
        // The first read says whether it works, before this returns.
        let (ok_tx, ok_rx) = mpsc::channel();
        std::thread::spawn(move || {
            let stream = stream;
            let mut buf = vec![0f32; FRAME * 2];
            let mut first = true;
            while !done.load(Ordering::Relaxed) {
                let mut err = 0;
                if unsafe { (lib.read)(stream.0, buf.as_mut_ptr().cast(), buf.len() * 4, &mut err) } < 0 {
                    let e = format!("PulseAudio: {}", error(&lib, err));
                    if first {
                        let _ = ok_tx.send(Err(e));
                    } else {
                        eprintln!("rein-remote: sound: {e}");
                    }
                    break;
                }
                if first {
                    first = false;
                    let _ = ok_tx.send(Ok(()));
                }
                // The block's first sample: what's still buffered, and the block itself, before now.
                let latency = unsafe { (lib.latency)(stream.0, &mut err) };
                let latency = if latency > 1_000_000 { 0 } else { latency };
                let at_us = now_us().saturating_sub(latency + (FRAME as u64 * 1_000_000 / RATE as u64));
                if sink.send(Block { at_us, samples: buf.clone() }).is_err() {
                    break;
                }
            }
            unsafe { (lib.free)(stream.0) };
        });
        match ok_rx.recv_timeout(Duration::from_secs(5)) {
            Ok(Ok(())) => Ok(Self { stop }),
            Ok(Err(e)) => Err(e),
            Err(_) => {
                stop.store(true, Ordering::Relaxed);
                Err("PulseAudio sent nothing in 5 s".into())
            }
        }
    }
}
