//! Windows: what the machine plays, from WASAPI's loopback on the default output device (the mix
//! as the speakers get it). In the mix format (often 48 kHz float, sometimes 44.1 kHz or more than two
//! channels), made 48 kHz stereo here. Loopback sends nothing while nothing plays: that's a gap,
//! which the packets' times carry (the client hears silence).
//!
//! No output device (a server, a CI runner): the reason is returned, and the video goes on alone.

use super::{now_us, Block, Capture, Resampler, Sink};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::Duration;
use windows::core::GUID;
use windows::Win32::Media::Audio::{
    eConsole, eRender, IAudioCaptureClient, IAudioClient, IMMDeviceEnumerator, MMDeviceEnumerator, AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_SHAREMODE_SHARED,
    AUDCLNT_STREAMFLAGS_LOOPBACK, WAVEFORMATEX, WAVEFORMATEXTENSIBLE,
};
use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CoTaskMemFree, CLSCTX_ALL, COINIT_MULTITHREADED};
use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};

const WAVE_FORMAT_PCM: u16 = 1;
const WAVE_FORMAT_IEEE_FLOAT: u16 = 3;
const WAVE_FORMAT_EXTENSIBLE: u16 = 0xfffe;
const SUBTYPE_PCM: GUID = GUID::from_u128(0x00000001_0000_0010_8000_00aa00389b71);
const SUBTYPE_FLOAT: GUID = GUID::from_u128(0x00000003_0000_0010_8000_00aa00389b71);

/// How the mix's samples are stored.
#[derive(Clone, Copy)]
enum Kind {
    F32,
    I16,
    I32,
}

pub struct Loopback {
    stop: Arc<AtomicBool>,
}

impl Capture for Loopback {}

impl Drop for Loopback {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

impl Loopback {
    pub fn start(sink: Sink) -> Result<Self, String> {
        let stop = Arc::new(AtomicBool::new(false));
        let done = stop.clone();
        let (ok_tx, ok_rx) = mpsc::channel();
        // COM objects stay on the thread that made them.
        std::thread::spawn(move || {
            if let Err(e) = unsafe { run(sink, &done, &ok_tx) } {
                let _ = ok_tx.send(Err(e));
            }
        });
        match ok_rx.recv_timeout(Duration::from_secs(5)) {
            Ok(Ok(())) => Ok(Self { stop }),
            Ok(Err(e)) => Err(e),
            Err(_) => {
                stop.store(true, Ordering::Relaxed);
                Err("WASAPI didn't answer in 5 s".into())
            }
        }
    }
}

unsafe fn run(sink: Sink, stop: &AtomicBool, ok: &mpsc::Sender<Result<(), String>>) -> Result<(), String> {
    let e = |what: &str| {
        let what = what.to_string();
        move |err: windows::core::Error| format!("{what}: {}", err.message())
    };
    let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    let devices: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).map_err(e("the audio devices"))?;
    let device = devices.GetDefaultAudioEndpoint(eRender, eConsole).map_err(e("no audio output device"))?;
    let client: IAudioClient = device.Activate(CLSCTX_ALL, None).map_err(e("the audio output"))?;
    let fmt = client.GetMixFormat().map_err(e("its mix format"))?;
    let wf: WAVEFORMATEX = std::ptr::read_unaligned(fmt);
    let kind = match (wf.wFormatTag, wf.wBitsPerSample) {
        (WAVE_FORMAT_IEEE_FLOAT, 32) => Some(Kind::F32),
        (WAVE_FORMAT_PCM, 16) => Some(Kind::I16),
        (WAVE_FORMAT_PCM, 32) => Some(Kind::I32),
        (WAVE_FORMAT_EXTENSIBLE, bits) => {
            let x: WAVEFORMATEXTENSIBLE = std::ptr::read_unaligned(fmt as *const WAVEFORMATEXTENSIBLE);
            let sub = x.SubFormat;
            match (sub == SUBTYPE_FLOAT, sub == SUBTYPE_PCM, bits) {
                (true, _, 32) => Some(Kind::F32),
                (_, true, 16) => Some(Kind::I16),
                (_, true, 32) => Some(Kind::I32),
                _ => None,
            }
        }
        _ => None,
    };
    let (channels, rate, block_align) = (wf.nChannels as usize, wf.nSamplesPerSec, wf.nBlockAlign as usize);
    // 20 ms of buffer (in 100 ns units): read every 5 ms, it never fills.
    let init = client.Initialize(AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_LOOPBACK, 200_000, 0, fmt, None);
    CoTaskMemFree(Some(fmt as *const _));
    init.map_err(e("loopback on the output"))?;
    let Some(kind) = kind else {
        let (bits, tag) = (wf.wBitsPerSample, wf.wFormatTag);
        return Err(format!("a mix format it doesn't read ({bits} bits, tag {tag:#x})"));
    };
    let capture: IAudioCaptureClient = client.GetService().map_err(e("the loopback"))?;
    client.Start().map_err(e("starting the loopback"))?;
    let _ = ok.send(Ok(()));
    let mut freq = 0i64;
    let _ = QueryPerformanceFrequency(&mut freq);
    let mut resampler = Resampler::new(rate);
    while !stop.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_millis(5));
        while capture.GetNextPacketSize().unwrap_or(0) > 0 {
            let (mut data, mut frames, mut flags, mut qpc) = (std::ptr::null_mut(), 0u32, 0u32, 0u64);
            if capture.GetBuffer(&mut data, &mut frames, &mut flags, None, Some(&mut qpc as *mut u64)).is_err() {
                break;
            }
            let n = frames as usize * channels;
            let pcm: Vec<f32> = if flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0 || data.is_null() {
                vec![0.0; n]
            } else {
                let bytes = std::slice::from_raw_parts(data, frames as usize * block_align);
                match kind {
                    Kind::F32 => bytes.chunks_exact(4).take(n).map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect(),
                    Kind::I16 => bytes
                        .chunks_exact(2)
                        .take(n)
                        .map(|b| i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0)
                        .collect(),
                    Kind::I32 => bytes
                        .chunks_exact(4)
                        .take(n)
                        .map(|b| i32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f32 / 2147483648.0)
                        .collect(),
                }
            };
            let _ = capture.ReleaseBuffer(frames);
            // When its first sample played: its performance-counter time (100 ns units) against now.
            let mut now = 0i64;
            let _ = QueryPerformanceCounter(&mut now);
            let now_100ns = (now as u128 * 10_000_000 / freq.max(1) as u128) as u64;
            let at_us = now_us().saturating_sub(now_100ns.saturating_sub(qpc) / 10);
            let samples = resampler.process(&pcm, channels);
            if sink.send(Block { at_us, samples }).is_err() {
                let _ = client.Stop();
                return Ok(());
            }
        }
    }
    let _ = client.Stop();
    Ok(())
}
