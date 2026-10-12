//! The sound the machine plays, as Opus, timed on the same clock as the video.
//!
//! A capture (macOS: ScreenCaptureKit, the same display, window or app the video shows; Linux: the
//! default output's monitor through PulseAudio or PipeWire; Windows: WASAPI loopback on the default
//! output; or a test tone) hands over blocks of 48 kHz stereo PCM with the capture time of their first
//! sample, in microseconds on the clock video frames are stamped with (`SystemTime`, as `now_us`).
//! One encoder makes 10 ms Opus packets and every viewer that plays sound gets the same ones.
//!
//! It runs only while a viewer listens: started by the first, stopped a moment after the last goes.
//! No device, no permission, no PulseAudio: the reason is logged once a session, the video goes on.

#[cfg(target_os = "macos")]
pub mod mac;
pub mod opus;
#[cfg(target_os = "linux")]
pub mod pulse;
pub mod tone;
#[cfg(windows)]
pub mod wasapi;

use std::sync::mpsc::{self, Receiver, SyncSender};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

/// Samples a second, per channel: Opus's own rate (browsers decode to it).
pub const RATE: u32 = 48_000;
/// Samples per channel in a packet: 10 ms (Opus's low-delay mode adds 2.5 ms to that).
pub const FRAME: usize = 480;
/// What a packet carries at most (96 kbit/s is ~120 bytes a packet; the rest is headroom).
const MAX_PACKET: usize = 1500;

/// PCM as a capture hands it over: 48 kHz, stereo, interleaved, and when its first sample was
/// captured (µs, the video's clock).
pub struct Block {
    pub at_us: u64,
    pub samples: Vec<f32>,
}

/// Where blocks go; a capture runs until it's dropped.
pub type Sink = SyncSender<Block>;
pub trait Capture: Send {}

/// What sound goes with the video.
pub enum Sound {
    /// None (`--audio off`, or a source whose sound isn't this machine's: a VNC display).
    Off,
    /// Everything this machine plays.
    System,
    /// A tone that beeps when the test pattern flashes (for measuring sync).
    Tone,
    /// What ScreenCaptureKit hears for the content the video shows: a display's (every app's), or
    /// one app's (`--window`, `--app`).
    #[cfg(target_os = "macos")]
    Mac(mac::Filter),
    /// A capture that can't start (tests: no device, no permission).
    #[cfg(test)]
    Broken,
}

impl Sound {
    fn open(&self, sink: Sink) -> Result<Box<dyn Capture>, String> {
        match self {
            Sound::Off => Err("audio is off".into()),
            #[cfg(test)]
            Sound::Broken => Err("no audio output device (a test)".into()),
            Sound::Tone => Ok(Box::new(tone::Tone::start(sink))),
            #[cfg(target_os = "macos")]
            Sound::Mac(f) => Ok(Box::new(mac::MacAudio::start(f, sink)?)),
            #[cfg(target_os = "macos")]
            Sound::System => Ok(Box::new(mac::MacAudio::start(&mac::Filter::main_display()?, sink)?)),
            #[cfg(target_os = "linux")]
            Sound::System => Ok(Box::new(pulse::Pulse::start(sink)?)),
            #[cfg(windows)]
            Sound::System => Ok(Box::new(wasapi::Loopback::start(sink)?)),
            #[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
            Sound::System => Err("no audio capture on this platform".into()),
        }
    }
}

/// What /stats shows: packets made, and how loud the last second was (so a check can tell sound
/// from silence without listening).
#[derive(Default, Clone)]
pub struct Stats {
    pub packets: u64,
    pub bytes: u64,
    /// Packets louder than -60 dBFS.
    pub loud_packets: u64,
    /// The loudest packet of the last second, dBFS (RMS); -120 when silent.
    pub level_db: f32,
    pub listeners: usize,
    /// Why there's no sound, when there isn't.
    pub error: String,
}

/// The sound for every session: started when the first listens.
pub struct Audio {
    sound: Sound,
    state: Mutex<State>,
    pub stats: Mutex<Stats>,
}

/// A listener's queue, and whether it's still there (its `Listener` not dropped).
type Queue = (SyncSender<Arc<Vec<u8>>>, Weak<()>);

#[derive(Default)]
struct State {
    listeners: Vec<Queue>,
    running: bool,
}

/// One session's audio: whole protocol frames, from when it started listening. Dropped: it stops
/// listening.
pub struct Listener {
    pub packets: Receiver<Arc<Vec<u8>>>,
    _alive: Arc<()>,
}

impl Audio {
    pub fn new(sound: Sound) -> Option<Arc<Self>> {
        if matches!(sound, Sound::Off) {
            return None;
        }
        Some(Arc::new(Self {
            sound,
            state: Mutex::default(),
            stats: Mutex::new(Stats {
                level_db: -120.0,
                ..Default::default()
            }),
        }))
    }

    /// Audio packets for one session (whole protocol frames), from now on. The capture starts in
    /// the background the first time; if it can't, nothing comes and the reason is logged.
    pub fn listen(self: &Arc<Self>) -> Listener {
        // A second of packets: a session that stops reading (stuck) loses its own, no one else's.
        let (tx, rx) = mpsc::sync_channel(100);
        let alive = Arc::new(());
        let mut st = self.state.lock().unwrap();
        st.listeners.push((tx, Arc::downgrade(&alive)));
        if !st.running {
            st.running = true;
            let me = self.clone();
            std::thread::spawn(move || me.run());
        }
        Listener { packets: rx, _alive: alive }
    }

    /// Capture and encode while anyone listens.
    fn run(self: Arc<Self>) {
        let (sink, blocks) = mpsc::sync_channel::<Block>(64);
        let capture = match self.sound.open(sink) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("rein-remote: no sound ({e}): the video streams on its own");
                self.stats.lock().unwrap().error = e;
                let mut st = self.state.lock().unwrap();
                st.listeners.clear();
                st.running = false;
                return;
            }
        };
        eprintln!("rein-remote: sound: {}", describe(&self.sound));
        self.stats.lock().unwrap().error.clear();
        let mut enc = match opus::Encoder::new() {
            Ok(e) => e,
            Err(e) => {
                eprintln!("rein-remote: no sound (the Opus encoder: {e})");
                self.state.lock().unwrap().running = false;
                return;
            }
        };
        let mut packets = Packetizer::default();
        let mut alone_since: Option<Instant> = None;
        let mut loudest = (-120.0f32, Instant::now());
        loop {
            let block = match blocks.recv_timeout(Duration::from_millis(250)) {
                Ok(b) => Some(b),
                Err(mpsc::RecvTimeoutError::Timeout) => None,
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    eprintln!("rein-remote: the sound stopped");
                    // Its listeners get no more (the next one to come starts it again).
                    let mut st = self.state.lock().unwrap();
                    st.running = false;
                    st.listeners.clear();
                    break;
                }
            };
            for (at_us, pcm) in block.map(|b| packets.push(b)).unwrap_or_default() {
                let level = level_db(&pcm);
                let Ok(data) = enc.encode(&pcm) else { continue };
                let frame = Arc::new(rr_proto::Message::Audio(rr_proto::Audio { timestamp_us: at_us, data }).encode());
                {
                    let mut s = self.stats.lock().unwrap();
                    s.packets += 1;
                    s.bytes += frame.len() as u64;
                    s.loud_packets += (level > -60.0) as u64;
                    if level > loudest.0 || loudest.1.elapsed() > Duration::from_secs(1) {
                        loudest = (level, Instant::now());
                    }
                    s.level_db = loudest.0;
                }
                // Each listener gets it, or (backed up) misses it.
                for (l, _) in &self.state.lock().unwrap().listeners {
                    let _ = l.try_send(frame.clone());
                }
            }
            // Nobody left: stop a moment later (a page reloading comes straight back).
            let mut st = self.state.lock().unwrap();
            st.listeners.retain(|(_, alive)| alive.strong_count() > 0);
            self.stats.lock().unwrap().listeners = st.listeners.len();
            if st.listeners.is_empty() {
                if alone_since.get_or_insert_with(Instant::now).elapsed() > Duration::from_secs(3) {
                    st.running = false;
                    break;
                }
            } else {
                alone_since = None;
            }
        }
        // (`running` went false under the lock above: a listener that comes now starts a new capture,
        // which this one stopping doesn't touch.)
        drop(capture);
    }
}

fn describe(s: &Sound) -> &'static str {
    match s {
        Sound::Off => "off",
        Sound::System => "what this machine plays",
        Sound::Tone => "a test tone, a beep when the pattern flashes",
        #[cfg(test)]
        Sound::Broken => "broken",
        #[cfg(target_os = "macos")]
        Sound::Mac(f) if f.app => "what the app shown plays",
        #[cfg(target_os = "macos")]
        Sound::Mac(_) => "what this machine plays",
    }
}

/// RMS level of a packet in dBFS (-120 for silence).
pub fn level_db(pcm: &[f32]) -> f32 {
    if pcm.is_empty() {
        return -120.0;
    }
    let ms = pcm.iter().map(|x| x * x).sum::<f32>() / pcm.len() as f32;
    (10.0 * ms.max(1e-12).log10()).max(-120.0)
}

/// Blocks of any length into packets of exactly `FRAME` samples per channel, each with the capture
/// time of its first sample. Within a run, times come from the sample count (packets exactly 10 ms
/// apart, as the client expects; a block's own time jitters by a few ms with the device's buffering,
/// which isn't the sound moving). A block that starts more than 20 ms from where the count says (a
/// gap: WASAPI sends nothing while nothing plays; or the device's clock drifted that far from the
/// system's) closes the packet in progress with silence and starts counting again from its own time.
#[derive(Default)]
pub struct Packetizer {
    pending: Vec<f32>,
    /// The capture time of `pending[0]`.
    at_us: f64,
}

impl Packetizer {
    pub fn push(&mut self, b: Block) -> Vec<(u64, Vec<f32>)> {
        let mut out = vec![];
        let here = self.at_us + (self.pending.len() / 2) as f64 * 1e6 / RATE as f64;
        if self.pending.is_empty() {
            self.at_us = b.at_us as f64;
        } else if (b.at_us as f64 - here).abs() > 20_000.0 {
            self.pending.resize(FRAME * 2, 0.0);
            out.push((self.at_us as u64, std::mem::take(&mut self.pending)));
            self.at_us = b.at_us as f64;
        }
        self.pending.extend_from_slice(&b.samples);
        while self.pending.len() >= FRAME * 2 {
            let rest = self.pending.split_off(FRAME * 2);
            out.push((self.at_us.round() as u64, std::mem::replace(&mut self.pending, rest)));
            self.at_us += FRAME as f64 * 1e6 / RATE as f64;
        }
        out
    }
}

/// Any rate and channel count into 48 kHz stereo (linear interpolation: the mix formats it meets
/// are 44.1 or 48 kHz, where that's inaudible next to Opus). Mono is copied to both sides; more than
/// two channels keep the front left and right.
// (PulseAudio converts to 48 kHz stereo itself: on Linux only the tests use it.)
#[cfg_attr(target_os = "linux", allow(dead_code))]
pub struct Resampler {
    from: u32,
    /// Where the next output sample falls, in input samples from the start of the next block (it can
    /// be negative: between the last block's last sample and this one's first).
    pos: f64,
    last: [f32; 2],
}

#[cfg_attr(target_os = "linux", allow(dead_code))]
impl Resampler {
    pub fn new(from: u32) -> Self {
        Self {
            from,
            pos: 0.0,
            last: [0.0; 2],
        }
    }

    pub fn process(&mut self, input: &[f32], channels: usize) -> Vec<f32> {
        let channels = channels.max(1);
        let frames = input.len() / channels;
        let at = |i: usize| -> [f32; 2] {
            let f = &input[i * channels..];
            [f[0], if channels > 1 { f[1] } else { f[0] }]
        };
        if self.from == RATE {
            return (0..frames).flat_map(at).collect();
        }
        let step = self.from as f64 / RATE as f64;
        let mut out = Vec::with_capacity((frames as f64 / step) as usize * 2 + 4);
        while frames > 0 && self.pos < frames as f64 - 1.0 {
            let i = self.pos.floor();
            let t = (self.pos - i) as f32;
            let a = if i < 0.0 { self.last } else { at(i as usize) };
            let b = at((i + 1.0) as usize);
            out.push(a[0] + (b[0] - a[0]) * t);
            out.push(a[1] + (b[1] - a[1]) * t);
            self.pos += step;
        }
        if frames > 0 {
            self.last = at(frames - 1);
            self.pos -= frames as f64;
        }
        out
    }
}

pub fn now_us() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_micros() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks_become_10ms_packets_timed_by_their_first_sample() {
        let mut p = Packetizer::default();
        // 7 ms then 7 ms then 7 ms, back to back from t = 1 s.
        let block = |at_us| Block {
            at_us,
            samples: vec![0.5; 336 * 2],
        };
        assert!(p.push(block(1_000_000)).is_empty());
        let out = p.push(block(1_007_000));
        assert_eq!(out.len(), 1);
        assert_eq!((out[0].0, out[0].1.len()), (1_000_000, FRAME * 2));
        // A block a few ms off where the count says (the device's buffering jitters): the count wins.
        let out = p.push(block(1_017_500));
        assert_eq!(out[0].0, 1_010_000);
        // A gap of a second (nothing played): what was pending goes out padded, and counting starts
        // again at the new block's own time.
        let out = p.push(block(2_100_000));
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].0, 1_020_000);
        assert_eq!(out[0].1[out[0].1.len() - 1], 0.0);
        let out = p.push(block(2_107_000));
        assert_eq!(out[0].0, 2_100_000);
    }

    #[test]
    fn resamples_44k1_to_48k_and_keeps_a_sine() {
        let mut r = Resampler::new(44_100);
        let mut out = vec![];
        // 1 kHz, mono, in blocks of 441 (10 ms).
        for b in 0..10 {
            let block: Vec<f32> = (0..441)
                .map(|i| ((b * 441 + i) as f32 * 2.0 * std::f32::consts::PI * 1000.0 / 44_100.0).sin())
                .collect();
            out.extend(r.process(&block, 1));
        }
        // 100 ms in, ~100 ms out (a sample of lag at most), both sides equal.
        assert!((out.len() as i64 / 2 - 4800).abs() <= 2, "{}", out.len() / 2);
        for (i, f) in out.chunks(2).enumerate() {
            let want = (i as f32 * 2.0 * std::f32::consts::PI * 1000.0 / 48_000.0).sin();
            assert!((f[0] - want).abs() < 0.03, "sample {i}: {} vs {want}", f[0]);
            assert_eq!(f[0], f[1]);
        }
    }

    #[test]
    fn levels() {
        assert_eq!(level_db(&[0.0; 960]), -120.0);
        let sine: Vec<f32> = (0..960).map(|i| (i as f32 * 0.13).sin()).collect();
        assert!((level_db(&sine) + 3.0).abs() < 0.2);
    }
}
