//! macOS: the sound ScreenCaptureKit hears for the content the video shows. With a display, every
//! app's sound; with a window or an app, that app's alone (ScreenCaptureKit filters audio by the
//! apps in the filter). rein-remote's own sound is left out. Needs the same Screen Recording
//! permission as the video, nothing more.
//!
//! It's a stream of its own next to the video's (2×2 pixels, a frame a second, nobody looks at
//! them): sound starts and stops with its listeners, and the video's stream is left as it is.

use super::{Block, Capture, Resampler, Sink, RATE};
use crate::capture::mac::{content, error_text, pick_display, Sendable};
use block2::RcBlock;
use dispatch2::DispatchQueue;
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{define_class, msg_send, AllocAnyThread, DefinedClass};
use objc2_core_media::{CMSampleBuffer, CMTime, CMTimeFlags};
use objc2_foundation::{NSArray, NSError, NSObject, NSObjectProtocol};
use objc2_screen_capture_kit::{SCContentFilter, SCStream, SCStreamConfiguration, SCStreamOutput, SCStreamOutputType, SCWindow};
use std::ffi::c_void;
use std::sync::{mpsc, Mutex};
use std::time::Duration;

/// What to hear: the video's own content filter, and whether it's one app's.
pub struct Filter {
    filter: Sendable<Retained<SCContentFilter>>,
    pub app: bool,
}

// A content filter isn't changed once made; it's only read (to start a stream) from any thread.
unsafe impl Sync for Filter {}

impl Filter {
    pub fn new(filter: Retained<SCContentFilter>, app: bool) -> Self {
        Self { filter: Sendable(filter), app }
    }

    /// The main display's: everything the machine plays.
    pub fn main_display() -> Result<Self, String> {
        let c = content()?;
        let display = pick_display(&c, None)?;
        let none: Retained<NSArray<SCWindow>> = NSArray::new();
        let filter = unsafe { SCContentFilter::initWithDisplay_excludingWindows(SCContentFilter::alloc(), &display, &none) };
        Ok(Self::new(filter, false))
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Time {
    value: i64,
    timescale: i32,
    flags: u32,
    epoch: i64,
}

impl Time {
    fn secs(&self) -> f64 {
        self.value as f64 / self.timescale.max(1) as f64
    }
}

#[repr(C)]
struct AudioBuffer {
    channels: u32,
    size: u32,
    data: *const c_void,
}

#[repr(C)]
struct AudioBufferList {
    count: u32,
    buffers: [AudioBuffer; 8],
}

/// AudioStreamBasicDescription.
#[repr(C)]
struct Asbd {
    rate: f64,
    format: u32,
    flags: u32,
    bytes_per_packet: u32,
    frames_per_packet: u32,
    bytes_per_frame: u32,
    channels: u32,
    bits: u32,
    reserved: u32,
}

const FLAG_FLOAT: u32 = 1;
const FLAG_NON_INTERLEAVED: u32 = 1 << 5;

#[link(name = "CoreMedia", kind = "framework")]
extern "C" {
    fn CMSampleBufferGetFormatDescription(sb: *mut c_void) -> *mut c_void;
    fn CMAudioFormatDescriptionGetStreamBasicDescription(fd: *mut c_void) -> *const Asbd;
    fn CMSampleBufferGetAudioBufferListWithRetainedBlockBuffer(
        sb: *mut c_void,
        size_needed: *mut usize,
        list: *mut AudioBufferList,
        size: usize,
        allocator: *const c_void,
        block_allocator: *const c_void,
        flags: u32,
        block: *mut *const c_void,
    ) -> i32;
    fn CMSampleBufferGetPresentationTimeStamp(sb: *mut c_void) -> Time;
    fn CMClockGetHostTimeClock() -> *const c_void;
    fn CMClockGetTime(clock: *const c_void) -> Time;
    fn CFRelease(p: *const c_void);
}

/// Where the sound goes from ScreenCaptureKit's queue, and the resampler when it isn't 48 kHz.
struct Out {
    sink: Sink,
    resampler: Option<(u32, Resampler)>,
    warned: bool,
}

define_class!(
    // Receives the sound (and the tiny frames, ignored) on ScreenCaptureKit's queues.
    #[unsafe(super(NSObject))]
    #[name = "ReinRemoteAudioOutput"]
    #[ivars = Mutex<Out>]
    struct Output;

    unsafe impl NSObjectProtocol for Output {}

    unsafe impl SCStreamOutput for Output {
        #[unsafe(method(stream:didOutputSampleBuffer:ofType:))]
        unsafe fn did_output(&self, _stream: &SCStream, sample: &CMSampleBuffer, kind: SCStreamOutputType) {
            if kind == SCStreamOutputType::Audio {
                let mut out = self.ivars().lock().unwrap();
                if let Err(e) = unsafe { take(sample as *const CMSampleBuffer as *mut c_void, &mut out) } {
                    if !out.warned {
                        out.warned = true;
                        eprintln!("rein-remote: sound: {e}");
                    }
                }
            }
        }
    }
);

/// One buffer of sound: 48 kHz stereo, interleaved, timed on the video's clock.
unsafe fn take(sb: *mut c_void, out: &mut Out) -> Result<(), String> {
    let asbd = CMAudioFormatDescriptionGetStreamBasicDescription(CMSampleBufferGetFormatDescription(sb));
    let Some(asbd) = asbd.as_ref() else { return Ok(()) };
    if asbd.flags & FLAG_FLOAT == 0 || asbd.bits != 32 {
        return Err(format!("a format it doesn't read ({} bits, flags {:#x})", asbd.bits, asbd.flags));
    }
    // How big its buffer list is, then the list (in memory aligned for it, as big as it says).
    let mut size = 0usize;
    let mut block: *const c_void = std::ptr::null();
    CMSampleBufferGetAudioBufferListWithRetainedBlockBuffer(
        sb,
        &mut size,
        std::ptr::null_mut(),
        0,
        std::ptr::null(),
        std::ptr::null(),
        0,
        std::ptr::null_mut(),
    );
    let mut mem = vec![0u64; size.max(std::mem::size_of::<AudioBufferList>()).div_ceil(8)];
    let status = CMSampleBufferGetAudioBufferListWithRetainedBlockBuffer(
        sb,
        std::ptr::null_mut(),
        mem.as_mut_ptr().cast(),
        size,
        std::ptr::null(),
        std::ptr::null(),
        0,
        &mut block,
    );
    if status != 0 {
        return Err(format!("its buffers ({status})"));
    }
    let list = &*(mem.as_ptr() as *const AudioBufferList);
    let count = (list.count as usize).min(list.buffers.len());
    let channels = asbd.channels.max(1) as usize;
    let floats = |b: &AudioBuffer| std::slice::from_raw_parts(b.data as *const f32, b.size as usize / 4);
    // Interleaved, whichever way it came: planar (one buffer a channel, ScreenCaptureKit's way) or not.
    let (pcm, n) = if count == 0 {
        (vec![], 2)
    } else if asbd.flags & FLAG_NON_INTERLEAVED != 0 {
        let left = floats(&list.buffers[0]);
        let right = if count >= 2 { floats(&list.buffers[1]) } else { left };
        let n = left.len().min(right.len());
        ((0..n).flat_map(|i| [left[i], right[i]]).collect::<Vec<f32>>(), 2)
    } else {
        (floats(&list.buffers[0]).to_vec(), channels)
    };
    if !block.is_null() {
        CFRelease(block);
    }
    // When its first sample was heard: its time on the host clock, against that clock now.
    let pts = CMSampleBufferGetPresentationTimeStamp(sb);
    let now = CMClockGetTime(CMClockGetHostTimeClock());
    let age_us = ((now.secs() - pts.secs()) * 1e6).max(0.0) as u64;
    let at_us = super::now_us().saturating_sub(age_us);
    if std::env::var_os("RR_DEBUG_AUDIO").is_some() {
        eprintln!(
            "sck audio: {} frames at {} Hz, {} ch, age {} us",
            pcm.len() / n,
            asbd.rate,
            asbd.channels,
            age_us
        );
    }
    let rate = asbd.rate.round() as u32;
    let samples = if rate == RATE && n == 2 {
        pcm
    } else {
        if out.resampler.as_ref().is_none_or(|(r, _)| *r != rate) {
            out.resampler = Some((rate, Resampler::new(rate)));
        }
        out.resampler.as_mut().unwrap().1.process(&pcm, n)
    };
    // Full (the encoder's behind): this buffer is dropped, the next is timed on its own.
    let _ = out.sink.try_send(Block { at_us, samples });
    Ok(())
}

impl Output {
    fn new(sink: Sink) -> Retained<Self> {
        let this = Self::alloc().set_ivars(Mutex::new(Out {
            sink,
            resampler: None,
            warned: false,
        }));
        unsafe { msg_send![super(this), init] }
    }
}

pub struct MacAudio {
    stream: Retained<SCStream>,
    _output: Retained<Output>,
}

// Only stopped (dropped) by the thread that holds it.
unsafe impl Send for MacAudio {}
impl Capture for MacAudio {}

impl MacAudio {
    pub fn start(filter: &Filter, sink: Sink) -> Result<Self, String> {
        let config = unsafe { SCStreamConfiguration::new() };
        unsafe {
            config.setCapturesAudio(true);
            config.setSampleRate(RATE as isize);
            config.setChannelCount(2);
            // Not what rein-remote itself plays (nothing, but a Rein that embeds it might).
            config.setExcludesCurrentProcessAudio(true);
            // The picture isn't used: as small and as rare as it goes.
            config.setWidth(2);
            config.setHeight(2);
            config.setMinimumFrameInterval(CMTime {
                value: 1,
                timescale: 1,
                flags: CMTimeFlags::Valid,
                epoch: 0,
            });
            config.setQueueDepth(3);
        }
        let output = Output::new(sink);
        let stream = unsafe { SCStream::initWithFilter_configuration_delegate(SCStream::alloc(), &filter.filter.0, &config, None) };
        let audio_queue = DispatchQueue::new("rein-remote.sound", None);
        let frames_queue = DispatchQueue::new("rein-remote.sound-frames", None);
        unsafe {
            stream
                .addStreamOutput_type_sampleHandlerQueue_error(ProtocolObject::from_ref(&*output), SCStreamOutputType::Audio, Some(&audio_queue))
                .map_err(|e| e.localizedDescription().to_string())?;
            // A stream with no output for its frames complains about each one it drops.
            stream
                .addStreamOutput_type_sampleHandlerQueue_error(ProtocolObject::from_ref(&*output), SCStreamOutputType::Screen, Some(&frames_queue))
                .map_err(|e| e.localizedDescription().to_string())?;
        }
        let (tx, rx) = mpsc::channel();
        let done = RcBlock::new(move |err: *mut NSError| {
            let _ = tx.send(if err.is_null() { Ok(()) } else { Err(error_text(err)) });
        });
        unsafe { stream.startCaptureWithCompletionHandler(Some(&done)) };
        rx.recv_timeout(Duration::from_secs(10))
            .map_err(|_| "ScreenCaptureKit didn't start".to_string())?
            .map_err(|e| format!("ScreenCaptureKit: {e}"))?;
        Ok(Self { stream, _output: output })
    }
}

impl Drop for MacAudio {
    fn drop(&mut self) {
        unsafe { self.stream.stopCaptureWithCompletionHandler(None) };
    }
}
