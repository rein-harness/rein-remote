//! Session recording (`--record DIR`): each recorded session's video, as it was sent (no second
//! encode), and its sound, in a fragmented MP4 that plays afterwards in any player (ffplay, VLC,
//! QuickTime for H.264, a browser). H.264 goes in as `avc3` and HEVC as `hev1`: their parameter sets
//! stay in the stream, so a session whose picture changes size mid-way (the link adapting) still
//! plays. Sound is Opus (`Opus` in MP4, as ffmpeg and browsers read it).
//!
//! A fragment is written for each video frame (with the sound that came meanwhile), so a recording
//! cut short (the machine lost power) plays up to its last frame. Files are made only this user may
//! read (Unix), named `<UTC time>-<user>-<session id>.mp4`.

use rr_proto::Codec;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::mpsc;

const VIDEO_TIMESCALE: u32 = 90_000;
const AUDIO_TIMESCALE: u32 = 48_000;
/// Samples in each Opus packet (10 ms at 48 kHz: what `audio` makes).
const OPUS_SAMPLES: u32 = 480;
/// libopus's encoder delay at 48 kHz.
const OPUS_PRE_SKIP: u16 = 312;

enum Item {
    Video { data: Vec<u8>, keyframe: bool, ts_us: u64 },
    Audio { data: Vec<u8>, ts_us: u64 },
}

/// A recording in progress: frames go to a writer thread (the stream never waits on the disk).
pub struct Recorder {
    tx: Option<mpsc::SyncSender<Item>>,
    done: Option<std::thread::JoinHandle<Result<u64, String>>>,
    pub path: PathBuf,
}

/// A file name part from a user name: letters, digits, `.`, `-`, `_`, `@` (the rest `_`), 64 at most.
fn safe(s: &str) -> String {
    let s: String = s
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || "._-@".contains(c) { c } else { '_' })
        .take(64)
        .collect();
    s.trim_start_matches('.').to_string()
}

impl Recorder {
    /// Starts a recording in `dir` for `user`'s session `id`: `codec` at `width`×`height`, with sound when
    /// `audio`.
    pub fn start(dir: &Path, user: &str, id: &str, codec: Codec, width: u16, height: u16, audio: bool) -> Result<Self, String> {
        if !matches!(codec, Codec::H264 | Codec::H265) {
            return Err(format!("recording {codec:?} isn't supported (H.264 and HEVC are)"));
        }
        std::fs::create_dir_all(dir).map_err(|e| format!("--record {}: {e}", dir.display()))?;
        let stamp = crate::audit::rfc3339(std::time::SystemTime::now()).replace([':', '.'], "-");
        let path = dir.join(format!("{stamp}-{}-{id}.mp4", safe(user)));
        let mut o = std::fs::OpenOptions::new();
        o.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut o, 0o600);
        let file = o.open(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        // A few seconds of frames queued at most: a disk that can't keep up loses frames, not the stream.
        let (tx, rx) = mpsc::sync_channel::<Item>(600);
        let done = std::thread::spawn(move || {
            let mut m = Mp4::new(std::io::BufWriter::new(file), codec, width, height, audio);
            for item in rx {
                match item {
                    Item::Video { data, keyframe, ts_us } => m.video(&data, keyframe, ts_us)?,
                    Item::Audio { data, ts_us } => m.audio(data, ts_us),
                }
            }
            m.finish()
        });
        Ok(Self {
            tx: Some(tx),
            done: Some(done),
            path,
        })
    }

    /// A video frame as sent (Annex B), its capture time in microseconds.
    pub fn video(&self, data: &[u8], keyframe: bool, ts_us: u64) {
        if let Some(tx) = &self.tx {
            let _ = tx.try_send(Item::Video {
                data: data.to_vec(),
                keyframe,
                ts_us,
            });
        }
    }

    /// An Opus packet, its capture time in microseconds (the same clock as the video's).
    pub fn audio(&self, data: &[u8], ts_us: u64) {
        if let Some(tx) = &self.tx {
            let _ = tx.try_send(Item::Audio { data: data.to_vec(), ts_us });
        }
    }

    /// Ends the recording (what's queued is written first): the bytes written. Once; then 0.
    pub fn finish(&mut self) -> Result<u64, String> {
        self.tx = None;
        self.done
            .take()
            .map_or(Ok(0), |d| d.join().map_err(|_| "the recording's writer failed".to_string())?)
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        let _ = self.finish();
    }
}

/// The NAL units of an Annex B stream (without their start codes).
pub fn nals(data: &[u8]) -> Vec<&[u8]> {
    let mut out = vec![];
    let mut i = 0;
    let mut start = None;
    while i + 3 <= data.len() {
        if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
            if let Some(s) = start {
                // A 4-byte start code's leading zero belongs to it, not to the NAL before.
                let mut end = i;
                while end > s && data[end - 1] == 0 {
                    end -= 1;
                }
                out.push(&data[s..end]);
            }
            i += 3;
            start = Some(i);
        } else {
            i += 1;
        }
    }
    if let Some(s) = start {
        if s < data.len() {
            out.push(&data[s..]);
        }
    }
    out
}

/// Emulation prevention bytes (00 00 03) removed: the bits as the syntax reads them.
fn rbsp(nal: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(nal.len());
    let mut zeros = 0;
    for &b in nal {
        if zeros >= 2 && b == 3 {
            zeros = 0;
            continue;
        }
        zeros = if b == 0 { zeros + 1 } else { 0 };
        out.push(b);
    }
    out
}

fn bx(kind: &[u8; 4], parts: &[&[u8]]) -> Vec<u8> {
    let len: usize = 8 + parts.iter().map(|p| p.len()).sum::<usize>();
    let mut b = Vec::with_capacity(len);
    b.extend_from_slice(&(len as u32).to_be_bytes());
    b.extend_from_slice(kind);
    for p in parts {
        b.extend_from_slice(p);
    }
    b
}

fn full(kind: &[u8; 4], version: u8, flags: u32, parts: &[&[u8]]) -> Vec<u8> {
    let vf = ((version as u32) << 24 | flags).to_be_bytes();
    let mut all: Vec<&[u8]> = vec![&vf];
    all.extend_from_slice(parts);
    bx(kind, &all)
}

fn u16b(v: u16) -> [u8; 2] {
    v.to_be_bytes()
}
fn u32b(v: u32) -> [u8; 4] {
    v.to_be_bytes()
}

const MATRIX: [u32; 9] = [0x0001_0000, 0, 0, 0, 0x0001_0000, 0, 0, 0, 0x4000_0000];

fn matrix() -> Vec<u8> {
    MATRIX.iter().flat_map(|v| v.to_be_bytes()).collect()
}

/// The decoder configuration (`avcC` or `hvcC`) from a keyframe's parameter sets.
fn decoder_config(codec: Codec, units: &[&[u8]]) -> Option<Vec<u8>> {
    match codec {
        Codec::H264 => {
            let sps = units.iter().find(|n| !n.is_empty() && n[0] & 0x1f == 7)?;
            let pps = units.iter().find(|n| !n.is_empty() && n[0] & 0x1f == 8)?;
            if sps.len() < 4 {
                return None;
            }
            let mut c = vec![1, sps[1], sps[2], sps[3], 0xff, 0xe1];
            c.extend_from_slice(&u16b(sps.len() as u16));
            c.extend_from_slice(sps);
            c.push(1);
            c.extend_from_slice(&u16b(pps.len() as u16));
            c.extend_from_slice(pps);
            Some(bx(b"avcC", &[&c]))
        }
        Codec::H265 => {
            let of = |t: u8| units.iter().find(|n| n.len() > 2 && (n[0] >> 1) & 0x3f == t).copied();
            let (vps, sps, pps) = (of(32)?, of(33)?, of(34)?);
            // The SPS's profile_tier_level: after its 2-byte header and one byte (vps id, sub-layers).
            let r = rbsp(sps);
            if r.len() < 15 {
                return None;
            }
            let ptl = &r[3..15];
            let mut c = vec![1, ptl[0]];
            c.extend_from_slice(&ptl[1..5]); // general_profile_compatibility_flags
            c.extend_from_slice(&ptl[5..11]); // general_constraint_indicator_flags
            c.push(ptl[11]); // general_level_idc
            c.extend_from_slice(&[0xf0, 0x00, 0xfc, 0xfd, 0xf8, 0xf8, 0x00, 0x00]);
            // constantFrameRate 0, one temporal layer, nested, 4-byte lengths.
            c.push(0x0f);
            c.push(3);
            for (t, n) in [(32u8, vps), (33, sps), (34, pps)] {
                c.push(0x80 | t);
                c.extend_from_slice(&u16b(1));
                c.extend_from_slice(&u16b(n.len() as u16));
                c.extend_from_slice(n);
            }
            Some(bx(b"hvcC", &[&c]))
        }
        _ => None,
    }
}

struct Sample {
    data: Vec<u8>,
    keyframe: bool,
    ts_us: u64,
}

/// A fragmented MP4 written as it goes: the header at the first keyframe, then a fragment a frame.
struct Mp4<W: Write> {
    out: W,
    codec: Codec,
    width: u16,
    height: u16,
    audio: bool,
    started: bool,
    /// The first keyframe's capture time: time zero.
    t0: u64,
    seq: u32,
    /// The last video frame, written when the next comes (its duration is the gap to it).
    held: Option<Sample>,
    sound: Vec<(Vec<u8>, u64)>,
    bytes: u64,
}

impl<W: Write> Mp4<W> {
    fn new(out: W, codec: Codec, width: u16, height: u16, audio: bool) -> Self {
        Self {
            out,
            codec,
            width,
            height,
            audio,
            started: false,
            t0: 0,
            seq: 0,
            held: None,
            sound: vec![],
            bytes: 0,
        }
    }

    fn write(&mut self, b: &[u8]) -> Result<(), String> {
        self.bytes += b.len() as u64;
        self.out.write_all(b).map_err(|e| format!("the recording: {e}"))
    }

    fn header(&mut self, config: Vec<u8>) -> Result<(), String> {
        let ftyp = bx(b"ftyp", &[b"iso6", &u32b(0), b"iso6", b"isom", b"iso5", b"mp41"]);
        let tracks = if self.audio { 2 } else { 1 };
        let mvhd = full(
            b"mvhd",
            0,
            0,
            &[
                &u32b(0),
                &u32b(0),
                &u32b(1000),
                &u32b(0),
                &u32b(0x0001_0000),
                &u16b(0x0100),
                &[0; 10],
                &matrix(),
                &[0; 24],
                &u32b(tracks + 1),
            ],
        );
        let (w, h) = (self.width as u32, self.height as u32);
        // The video track.
        let entry = {
            let kind: &[u8; 4] = if self.codec == Codec::H264 { b"avc3" } else { b"hev1" };
            let mut name = [0u8; 32];
            name[0] = 11;
            name[1..12].copy_from_slice(b"rein-remote");
            bx(
                kind,
                &[
                    &[0; 6],
                    &u16b(1),
                    &[0; 16],
                    &u16b(self.width),
                    &u16b(self.height),
                    &u32b(0x0048_0000),
                    &u32b(0x0048_0000),
                    &u32b(0),
                    &u16b(1),
                    &name,
                    &u16b(0x18),
                    &u16b(0xffff),
                    &config,
                ],
            )
        };
        let video = self.trak(1, VIDEO_TIMESCALE, b"vide", entry, w << 16, h << 16);
        let mut moov_parts = vec![mvhd, video];
        let mut trex = vec![full(b"trex", 0, 0, &[&u32b(1), &u32b(1), &u32b(0), &u32b(0), &u32b(0)])];
        if self.audio {
            let dops = bx(b"dOps", &[&[0, 2], &u16b(OPUS_PRE_SKIP), &u32b(48_000), &u16b(0), &[0]]);
            let entry = bx(
                b"Opus",
                &[&[0; 6], &u16b(1), &[0; 8], &u16b(2), &u16b(16), &u16b(0), &u16b(0), &u32b(48_000 << 16), &dops],
            );
            moov_parts.push(self.trak(2, AUDIO_TIMESCALE, b"soun", entry, 0, 0));
            trex.push(full(b"trex", 0, 0, &[&u32b(2), &u32b(1), &u32b(0), &u32b(0), &u32b(0)]));
        }
        let mvex = bx(b"mvex", &trex.iter().map(Vec::as_slice).collect::<Vec<_>>());
        moov_parts.push(mvex);
        let moov = bx(b"moov", &moov_parts.iter().map(Vec::as_slice).collect::<Vec<_>>());
        self.write(&ftyp)?;
        self.write(&moov)
    }

    fn trak(&self, id: u32, timescale: u32, handler: &[u8; 4], entry: Vec<u8>, w: u32, h: u32) -> Vec<u8> {
        let video = handler == b"vide";
        let tkhd = full(
            b"tkhd",
            0,
            3,
            &[
                &u32b(0),
                &u32b(0),
                &u32b(id),
                &u32b(0),
                &u32b(0),
                &[0; 8],
                &u16b(0),
                &u16b(if video { 0 } else { 1 }),
                &u16b(if video { 0 } else { 0x0100 }),
                &u16b(0),
                &matrix(),
                &u32b(w),
                &u32b(h),
            ],
        );
        let mdhd = full(b"mdhd", 0, 0, &[&u32b(0), &u32b(0), &u32b(timescale), &u32b(0), &u16b(0x55c4), &u16b(0)]);
        let name: &[u8] = if video { b"Video\0" } else { b"Sound\0" };
        let hdlr = full(b"hdlr", 0, 0, &[&u32b(0), handler, &[0; 12], name]);
        let header = if video {
            full(b"vmhd", 0, 1, &[&[0; 8]])
        } else {
            full(b"smhd", 0, 0, &[&[0; 4]])
        };
        let dinf = bx(b"dinf", &[&full(b"dref", 0, 0, &[&u32b(1), &full(b"url ", 0, 1, &[])])]);
        let stbl = bx(
            b"stbl",
            &[
                &full(b"stsd", 0, 0, &[&u32b(1), &entry]),
                &full(b"stts", 0, 0, &[&u32b(0)]),
                &full(b"stsc", 0, 0, &[&u32b(0)]),
                &full(b"stsz", 0, 0, &[&u32b(0), &u32b(0)]),
                &full(b"stco", 0, 0, &[&u32b(0)]),
            ],
        );
        let minf = bx(b"minf", &[&header, &dinf, &stbl]);
        let mdia = bx(b"mdia", &[&mdhd, &hdlr, &minf]);
        bx(b"trak", &[&tkhd, &mdia])
    }

    fn video(&mut self, data: &[u8], keyframe: bool, ts_us: u64) -> Result<(), String> {
        if !self.started {
            if !keyframe {
                return Ok(());
            }
            let Some(config) = decoder_config(self.codec, &nals(data)) else {
                return Ok(());
            };
            self.header(config)?;
            self.started = true;
            self.t0 = ts_us;
        }
        let next = Sample {
            data: data.to_vec(),
            keyframe,
            ts_us: ts_us.max(self.t0),
        };
        if let Some(prev) = self.held.replace(next) {
            let dur = self.held.as_ref().unwrap().ts_us.saturating_sub(prev.ts_us);
            self.fragment(prev, dur)?;
        }
        Ok(())
    }

    fn audio(&mut self, data: Vec<u8>, ts_us: u64) {
        // Sound from before the first picture has nothing to go with.
        if self.audio && self.started && ts_us >= self.t0 {
            self.sound.push((data, ts_us));
        }
    }

    /// One fragment: a video frame lasting `dur_us`, and the sound held since the last.
    fn fragment(&mut self, v: Sample, dur_us: u64) -> Result<(), String> {
        self.seq += 1;
        let to = |us: u64, scale: u32| (us as u128 * scale as u128 / 1_000_000) as u64;
        // Length-prefixed NAL units, as MP4 has them.
        let mut vdata = Vec::with_capacity(v.data.len() + 16);
        for n in nals(&v.data) {
            vdata.extend_from_slice(&u32b(n.len() as u32));
            vdata.extend_from_slice(n);
        }
        let vdur = (to(dur_us, VIDEO_TIMESCALE) as u32).max(1);
        let vflags = if v.keyframe { 0x0200_0000 } else { 0x0101_0000 };
        let sound = std::mem::take(&mut self.sound);
        let adata: Vec<u8> = sound.iter().flat_map(|(d, _)| d.iter().copied()).collect();
        // Twice: the first time to learn the moof's size (the data offsets depend on it).
        let mut moof_len = 0usize;
        let mut moof = vec![];
        for _ in 0..2 {
            let vtrun_offset = (moof_len + 8) as u32;
            let mut vtrun_entries = Vec::with_capacity(12);
            vtrun_entries.extend_from_slice(&u32b(vdur));
            vtrun_entries.extend_from_slice(&u32b(vdata.len() as u32));
            vtrun_entries.extend_from_slice(&u32b(vflags));
            let vtraf = bx(
                b"traf",
                &[
                    &full(b"tfhd", 0, 0x02_0000, &[&u32b(1)]),
                    &full(b"tfdt", 1, 0, &[&to(v.ts_us - self.t0, VIDEO_TIMESCALE).to_be_bytes()]),
                    &full(b"trun", 0, 0x0701, &[&u32b(1), &u32b(vtrun_offset), &vtrun_entries]),
                ],
            );
            let mut parts = vec![full(b"mfhd", 0, 0, &[&u32b(self.seq)]), vtraf];
            if !sound.is_empty() {
                let aoffset = vtrun_offset + vdata.len() as u32;
                let mut entries = Vec::with_capacity(sound.len() * 4);
                for (d, _) in &sound {
                    entries.extend_from_slice(&u32b(d.len() as u32));
                }
                parts.push(bx(
                    b"traf",
                    &[
                        // Every packet 10 ms long, a key sample.
                        &full(b"tfhd", 0, 0x02_0000 | 0x08 | 0x20, &[&u32b(2), &u32b(OPUS_SAMPLES), &u32b(0x0200_0000)]),
                        &full(b"tfdt", 1, 0, &[&to(sound[0].1 - self.t0, AUDIO_TIMESCALE).to_be_bytes()]),
                        &full(b"trun", 0, 0x0201, &[&u32b(sound.len() as u32), &u32b(aoffset), &entries]),
                    ],
                ));
            }
            moof = bx(b"moof", &parts.iter().map(Vec::as_slice).collect::<Vec<_>>());
            moof_len = moof.len();
        }
        let mdat_head = [&u32b((8 + vdata.len() + adata.len()) as u32)[..], b"mdat"].concat();
        self.write(&moof)?;
        self.write(&mdat_head)?;
        self.write(&vdata)?;
        self.write(&adata)
    }

    fn finish(mut self) -> Result<u64, String> {
        if let Some(last) = self.held.take() {
            self.fragment(last, 33_333)?;
        }
        self.out.flush().map_err(|e| format!("the recording: {e}"))?;
        Ok(self.bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn annex_b_splits_into_nal_units() {
        let s = [0, 0, 0, 1, 0x67, 1, 2, 0, 0, 1, 0x68, 3, 0, 0, 0, 1, 0x65, 9, 9];
        assert_eq!(nals(&s), vec![&[0x67, 1, 2][..], &[0x68, 3][..], &[0x65, 9, 9][..]]);
        assert_eq!(rbsp(&[1, 0, 0, 3, 1, 0, 0, 3]), vec![1, 0, 0, 1, 0, 0]);
        assert_eq!(safe("../alice@corp.example/x y"), "_alice@corp.example_x_y");
    }

    /// A session's worth of frames from the encoder a stream uses and real Opus packets, recorded;
    /// then, where ffmpeg is installed (CI on Linux: RR_REQUIRE_FFPROBE=1), decoded by it from end to
    /// end: every frame and packet read, nothing it complains about.
    #[test]
    fn a_recording_plays() {
        let dir = std::env::temp_dir().join(format!("rr-record-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let (w, h) = (320usize, 240usize);
        let mut enc = crate::encode::open(Codec::H264, w, h, 30.0, 1000).unwrap();
        let mut src = crate::capture::TestPattern::new(w, h);
        let mut rec = Recorder::start(&dir, "alice@corp.example", "abcd", Codec::H264, w as u16, h as u16, true).unwrap();
        let mut opus = crate::audio::opus::Encoder::new().unwrap();
        let (mut frames, mut packets) = (0, 0);
        let t0 = 1_000_000_000u64;
        for i in 0..60u64 {
            use crate::capture::Source;
            let ts = t0 + i * 33_333;
            let f = src.grab().unwrap();
            let mut out = enc.encode(f.bgra, f.stride, ts).unwrap();
            while enc.in_flight() > 0 {
                out.extend(enc.poll().unwrap());
            }
            for e in out {
                rec.video(&e.data, e.keyframe, e.tag);
                frames += 1;
            }
            // 10 ms packets of a 440 Hz tone, on the same clock.
            for k in 0..3u64 {
                let at = ts + k * 10_000;
                let pcm: Vec<f32> = (0..480 * 2)
                    .map(|n| ((at as f64 / 1e6 + (n / 2) as f64 / 48_000.0) * 440.0 * std::f64::consts::TAU).sin() as f32 * 0.3)
                    .collect();
                rec.audio(&opus.encode(&pcm).unwrap(), at);
                packets += 1;
            }
        }
        let path = rec.path.clone();
        let bytes = rec.finish().unwrap();
        assert!(bytes > 1000 && std::fs::metadata(&path).unwrap().len() == bytes);
        assert!(path.file_name().unwrap().to_string_lossy().ends_with("-alice@corp.example-abcd.mp4"));
        let probe = std::process::Command::new("ffprobe")
            .args([
                "-v",
                "error",
                "-count_frames",
                "-show_entries",
                "stream=codec_name,nb_read_frames,width,height",
                "-of",
                "csv=p=0",
            ])
            .arg(&path)
            .output();
        match probe {
            Ok(o) => {
                let text = String::from_utf8_lossy(&o.stdout).to_string();
                let errs = String::from_utf8_lossy(&o.stderr).to_string();
                eprintln!("ffprobe: {text} {errs}");
                assert!(o.status.success() && errs.trim().is_empty(), "{errs}");
                assert!(text.contains(&format!("h264,{w},{h},{frames}")), "{text} (sent {frames} frames)");
                // Packets before the first frame aren't kept; every one after is.
                assert!(
                    text.lines().any(|l| l.starts_with("opus,") && l.ends_with(&format!(",{packets}"))),
                    "{text} (sent {packets})"
                );
                let decode = std::process::Command::new("ffmpeg")
                    .args(["-v", "error", "-xerror", "-i"])
                    .arg(&path)
                    // Frames are timed by when they were captured (a still screen sends none): passed
                    // through as they are, not converted to a constant rate.
                    .args(["-fps_mode", "passthrough", "-f", "null", "-"])
                    .output()
                    .unwrap();
                assert!(decode.status.success(), "{}", String::from_utf8_lossy(&decode.stderr));
                assert!(
                    String::from_utf8_lossy(&decode.stderr).trim().is_empty(),
                    "{}",
                    String::from_utf8_lossy(&decode.stderr)
                );
            }
            Err(_) if std::env::var_os("RR_REQUIRE_FFPROBE").is_none() => eprintln!("no ffprobe here: the recording's playback isn't checked"),
            Err(e) => panic!("ffprobe: {e}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
