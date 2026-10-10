//! Video encoding. Phase 1: software H.264 (openh264, BSD-licensed), tuned for screens in real time:
//! no B-frames, bitrate rate control, a keyframe on request. Hardware encoders come per platform in
//! later phases behind the same `encode` call.

use openh264::encoder::{Encoder, EncoderConfig, FrameType, RateControlMode, UsageType};
use openh264::formats::YUVSlices;
use openh264::OpenH264API;

pub struct H264 {
    enc: Encoder,
    width: usize,
    height: usize,
    keyframe_next: bool,
    yuv: crate::yuv::I420,
}

pub struct Encoded {
    pub data: Vec<u8>,
    pub keyframe: bool,
}

impl H264 {
    pub fn new(width: usize, height: usize, fps: f32, bitrate_kbps: u32) -> Result<Self, String> {
        // H.264 needs even dimensions.
        let (width, height) = (width & !1, height & !1);
        let config = EncoderConfig::new()
            .usage_type(UsageType::ScreenContentRealTime)
            .rate_control_mode(RateControlMode::Bitrate)
            .set_bitrate_bps(bitrate_kbps.saturating_mul(1000))
            .max_frame_rate(fps)
            // A still screen sends almost nothing.
            .enable_skip_frame(true);
        let enc = Encoder::with_api_config(OpenH264API::from_source(), config).map_err(|e| format!("openh264: {e}"))?;
        Ok(Self {
            enc,
            width,
            height,
            keyframe_next: true,
            yuv: crate::yuv::I420::new(width, height),
        })
    }

    pub fn size(&self) -> (usize, usize) {
        (self.width, self.height)
    }

    /// The next frame is a keyframe (a client joined, or lost a reference frame).
    pub fn keyframe(&mut self) {
        self.keyframe_next = true;
    }

    pub fn wants_keyframe(&self) -> bool {
        self.keyframe_next
    }

    /// One frame of BGRA pixels (`stride` bytes a row, at least `width * 4`). `None`: the encoder
    /// skipped it (nothing changed enough to send).
    pub fn encode(&mut self, bgra: &[u8], stride: usize) -> Result<Option<Encoded>, String> {
        // Into the encoder's YUV, fast (see yuv.rs); a frame one pixel wider or taller than the even
        // size just leaves its last column or row out.
        self.yuv.convert(bgra, stride);
        let (w, h) = (self.width, self.height);
        let yuv = YUVSlices::new((&self.yuv.y, &self.yuv.u, &self.yuv.v), (w, h), (w, w / 2, w / 2));
        if std::mem::take(&mut self.keyframe_next) {
            self.enc.force_intra_frame();
        }
        let bits = self.enc.encode(&yuv).map_err(|e| format!("openh264: {e}"))?;
        let frame_type = bits.frame_type();
        if matches!(frame_type, FrameType::Skip | FrameType::Invalid) {
            return Ok(None);
        }
        Ok(Some(Encoded {
            data: bits.to_vec(),
            keyframe: matches!(frame_type, FrameType::IDR | FrameType::I),
        }))
    }
}

/// The encoder a stream uses: the machine's hardware one where there is one (VideoToolbox on macOS),
/// else software. `RR_ENCODER=software` picks software anyway.
pub enum Video {
    Software(Box<H264>),
    #[cfg(target_os = "macos")]
    Hardware(crate::encode_vt::VideoToolbox),
}

impl Video {
    pub fn new(width: usize, height: usize, fps: f32, bitrate_kbps: u32) -> Result<Self, String> {
        #[cfg(target_os = "macos")]
        if std::env::var("RR_ENCODER").as_deref() != Ok("software") {
            match crate::encode_vt::VideoToolbox::new(width, height, fps, bitrate_kbps) {
                Ok(v) => return Ok(Self::Hardware(v)),
                Err(e) => eprintln!("rein-remote: {e}; encoding in software"),
            }
        }
        Ok(Self::Software(Box::new(H264::new(width, height, fps, bitrate_kbps)?)))
    }

    pub fn name(&self) -> &'static str {
        match self {
            Self::Software(_) => "H.264 in software (openh264)",
            #[cfg(target_os = "macos")]
            Self::Hardware(_) => "H.264 in hardware (VideoToolbox)",
        }
    }

    pub fn size(&self) -> (usize, usize) {
        match self {
            Self::Software(e) => e.size(),
            #[cfg(target_os = "macos")]
            Self::Hardware(e) => e.size(),
        }
    }

    pub fn keyframe(&mut self) {
        match self {
            Self::Software(e) => e.keyframe(),
            #[cfg(target_os = "macos")]
            Self::Hardware(e) => e.keyframe(),
        }
    }

    pub fn wants_keyframe(&self) -> bool {
        match self {
            Self::Software(e) => e.wants_keyframe(),
            #[cfg(target_os = "macos")]
            Self::Hardware(e) => e.wants_keyframe(),
        }
    }

    pub fn encode(&mut self, bgra: &[u8], stride: usize) -> Result<Option<Encoded>, String> {
        match self {
            Self::Software(e) => e.encode(bgra, stride),
            #[cfg(target_os = "macos")]
            Self::Hardware(e) => e.encode(bgra, stride),
        }
    }

    /// Whether this encoder reads the system's own frames in place (zero copy).
    pub fn takes_surfaces(&self) -> bool {
        !matches!(self, Self::Software(_))
    }

    #[cfg_attr(not(target_os = "macos"), allow(unused_variables))]
    pub fn encode_surface(&mut self, surface: &crate::capture::Surface) -> Result<Option<Encoded>, String> {
        match self {
            Self::Software(_) => Err("the software encoder reads frames from memory".into()),
            #[cfg(target_os = "macos")]
            Self::Hardware(e) => e.encode_surface(surface),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_a_keyframe_then_small_deltas() {
        let (w, h) = (320, 240);
        let mut enc = H264::new(w, h, 30.0, 2000).unwrap();
        let mut frame = vec![0u8; w * h * 4];
        let first = enc.encode(&frame, w * 4).unwrap().expect("a first frame");
        assert!(first.keyframe);
        // Annex B: starts with a start code, and carries the SPS (NAL type 7) for the decoder.
        assert_eq!(&first.data[..4], &[0, 0, 0, 1]);
        assert!(first.data.windows(5).any(|w| w[..4] == [0, 0, 0, 1] && w[4] & 0x1f == 7));
        // A small change: a delta frame, much smaller than the keyframe.
        for px in frame[..w * 40 * 4].chunks_mut(4) {
            px.copy_from_slice(&[255, 255, 255, 255]);
        }
        let delta = enc.encode(&frame, w * 4).unwrap().expect("a delta frame");
        assert!(!delta.keyframe);
        enc.keyframe();
        assert!(enc.encode(&frame, w * 4).unwrap().unwrap().keyframe);
    }
}

#[cfg(test)]
mod bench {
    use super::*;
    use crate::yuv::I420;
    use openh264::formats::{BgraSliceU8, YUVBuffer};
    use std::time::Instant;

    /// `cargo test --release -p rr-server -- --ignored bench --nocapture`: where the time goes.
    #[test]
    #[ignore]
    fn bench_1280x720() {
        let (w, h) = (1280, 720);
        let mut frame = vec![0u8; w * h * 4];
        for (i, b) in frame.iter_mut().enumerate() {
            *b = (i * 7 % 251) as u8;
        }
        let t = Instant::now();
        for _ in 0..30 {
            let _ = std::hint::black_box(YUVBuffer::from_rgb_source(BgraSliceU8::new(&frame, (w, h))));
        }
        println!("bgra→yuv (openh264's): {:.1} ms", t.elapsed().as_secs_f64() * 1000.0 / 30.0);
        let mut f = I420::new(w, h);
        let t = Instant::now();
        for _ in 0..30 {
            f.convert(&frame, w * 4);
        }
        println!("bgra→yuv (ours): {:.2} ms", t.elapsed().as_secs_f64() * 1000.0 / 30.0);
        let mut enc = H264::new(w, h, 60.0, 8000).unwrap();
        let t = Instant::now();
        for n in 0..60 {
            let at = (n * 4096) % frame.len();
            frame[at] ^= 0xff;
            enc.encode(&frame, w * 4).unwrap();
        }
        println!("encode (incl. conversion): {:.1} ms", t.elapsed().as_secs_f64() * 1000.0 / 60.0);
    }

    /// 1920×1080 with real motion (every row shifts each frame), software against the hardware encoder.
    #[cfg(unix)]
    #[test]
    #[ignore]
    fn bench_1080p_software_and_hardware() {
        let (w, h) = (1920, 1080);
        let frames: Vec<Vec<u8>> = (0..30)
            .map(|n| {
                let mut f = vec![0u8; w * h * 4];
                for (i, px) in f.chunks_mut(4).enumerate() {
                    let (x, y) = (i % w, i / w);
                    px.copy_from_slice(&[((x + n * 8) % 256) as u8, ((y * 3 + n * 5) % 256) as u8, ((x ^ y) % 256) as u8, 255]);
                }
                f
            })
            .collect();
        std::env::set_var("RR_ENCODER", "software");
        let mut soft = Video::new(w, h, 60.0, 8000).unwrap();
        std::env::remove_var("RR_ENCODER");
        let mut hard = Video::new(w, h, 60.0, 8000).unwrap();
        // This process's CPU time (user + system), to see what each costs the machine.
        let cpu = || {
            let mut u: libc::rusage = unsafe { std::mem::zeroed() };
            unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut u) };
            let t = |v: libc::timeval| v.tv_sec as f64 * 1000.0 + v.tv_usec as f64 / 1000.0;
            t(u.ru_utime) + t(u.ru_stime)
        };
        for enc in [&mut soft, &mut hard] {
            let (t, c) = (Instant::now(), cpu());
            for f in &frames {
                enc.encode(f, w * 4).unwrap();
            }
            let n = frames.len() as f64;
            println!(
                "{}: {:.1} ms a frame, {:.1} ms of CPU a frame",
                enc.name(),
                t.elapsed().as_secs_f64() * 1000.0 / n,
                (cpu() - c) / n
            );
        }
    }
}
