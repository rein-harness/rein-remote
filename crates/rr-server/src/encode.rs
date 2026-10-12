//! Video encoding: software H.264 (openh264, BSD-licensed), tuned for screens in real time (no
//! B-frames, bitrate rate control, a keyframe on request), and each platform's hardware encoders
//! behind the same `VideoEncoder` trait, found at run time with software as the fallback.

use openh264_sys2::*;
use rr_proto::{Codec, CodecCap};
use std::ptr::null_mut;

/// H.264 in software: openh264's own interface, so its bitrate changes live (no keyframe) and a
/// frame is cut into slices encoded on several cores at once.
pub struct H264 {
    api: DynamicAPI,
    enc: *mut ISVCEncoder,
    width: usize,
    height: usize,
    keyframe_next: bool,
    yuv: crate::yuv::I420,
    kbps: u32,
    bs: Box<SFrameBSInfo>,
    started: std::time::Instant,
}

// The encoder is only used by whoever holds it.
unsafe impl Send for H264 {}

pub struct Encoded {
    pub data: Vec<u8>,
    pub keyframe: bool,
    /// What the caller tagged the frame with when it went in (its capture time): a pipelined
    /// encoder gives frames back later than they went in.
    pub tag: u64,
}

impl H264 {
    pub fn new(width: usize, height: usize, fps: f32, bitrate_kbps: u32) -> Result<Self, String> {
        // H.264 needs even dimensions.
        let (width, height) = (width & !1, height & !1);
        let api = DynamicAPI::from_source();
        let mut enc: *mut ISVCEncoder = null_mut();
        // Slices, one a core (up to 4): a frame is encoded in parallel.
        let threads = std::thread::available_parallelism().map_or(1, |n| n.get()).clamp(1, 4) as u16;
        unsafe {
            if api.WelsCreateSVCEncoder(&mut enc) != 0 || enc.is_null() {
                return Err("openh264: can't create an encoder".into());
            }
            let vt = &**enc;
            let mut p: SEncParamExt = std::mem::zeroed();
            vt.GetDefaultParams.ok_or("openh264")?(enc, &mut p);
            let bps = bitrate_kbps.saturating_mul(1000) as i32;
            p.iUsageType = SCREEN_CONTENT_REAL_TIME;
            p.iPicWidth = width as i32;
            p.iPicHeight = height as i32;
            p.iTargetBitrate = bps;
            p.iMaxBitrate = bps;
            p.iRCMode = RC_BITRATE_MODE;
            p.fMaxFrameRate = fps;
            // A still screen sends almost nothing; a keyframe only when one is needed.
            p.bEnableFrameSkip = true;
            p.uiIntraPeriod = 0;
            p.eSpsPpsIdStrategy = CONSTANT_ID;
            p.bEnableBackgroundDetection = false;
            p.iComplexityMode = LOW_COMPLEXITY;
            p.iMultipleThreadIdc = threads;
            p.iSpatialLayerNum = 1;
            let l = &mut p.sSpatialLayers[0];
            l.iVideoWidth = width as i32;
            l.iVideoHeight = height as i32;
            l.fFrameRate = fps;
            l.iSpatialBitrate = bps;
            l.iMaxSpatialBitrate = bps;
            l.sSliceArgument.uiSliceMode = if threads > 1 { SM_FIXEDSLCNUM_SLICE } else { SM_SINGLE_SLICE };
            l.sSliceArgument.uiSliceNum = threads as u32;
            let mut quiet = WELS_LOG_QUIET;
            vt.SetOption.ok_or("openh264")?(enc, ENCODER_OPTION_TRACE_LEVEL, &mut quiet as *mut i32 as *mut _);
            if vt.InitializeExt.ok_or("openh264")?(enc, &p) != 0 {
                api.WelsDestroySVCEncoder(enc);
                return Err("openh264: can't start the encoder with these settings".into());
            }
        }
        Ok(Self {
            api,
            enc,
            width,
            height,
            keyframe_next: true,
            yuv: crate::yuv::I420::new(width, height),
            kbps: bitrate_kbps,
            bs: Box::new(unsafe { std::mem::zeroed() }),
            started: std::time::Instant::now(),
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

    /// A new target bitrate, taken at once.
    pub fn set_bitrate(&mut self, kbps: u32) -> Result<(), String> {
        let mut info = SBitrateInfo {
            iLayer: SPATIAL_LAYER_ALL,
            iBitrate: kbps.saturating_mul(1000) as i32,
        };
        unsafe {
            let vt = &**self.enc;
            let set = vt.SetOption.ok_or("openh264")?;
            // The cap first when going down (the target can't be above it), last when going up.
            let order = if kbps < self.kbps {
                [ENCODER_OPTION_MAX_BITRATE, ENCODER_OPTION_BITRATE]
            } else {
                [ENCODER_OPTION_BITRATE, ENCODER_OPTION_MAX_BITRATE]
            };
            for o in order {
                set(self.enc, o, &mut info as *mut SBitrateInfo as *mut _);
            }
        }
        self.kbps = kbps;
        Ok(())
    }

    /// One frame of BGRA pixels (`stride` bytes a row, at least `width * 4`). `None`: the encoder
    /// skipped it (nothing changed enough to send, or the bitrate is spent).
    pub fn encode(&mut self, bgra: &[u8], stride: usize) -> Result<Option<Encoded>, String> {
        // Into the encoder's YUV, fast (see yuv.rs); a frame one pixel wider or taller than the even
        // size just leaves its last column or row out.
        self.yuv.convert(bgra, stride);
        let (w, h) = (self.width, self.height);
        let mut pic: SSourcePicture = unsafe { std::mem::zeroed() };
        pic.iColorFormat = videoFormatI420;
        pic.iStride = [w as i32, (w / 2) as i32, (w / 2) as i32, 0];
        pic.pData = [self.yuv.y.as_mut_ptr(), self.yuv.u.as_mut_ptr(), self.yuv.v.as_mut_ptr(), null_mut()];
        pic.iPicWidth = w as i32;
        pic.iPicHeight = h as i32;
        // Real time: the rate control spends the bitrate by the clock, whatever the frame rate.
        pic.uiTimeStamp = self.started.elapsed().as_millis() as i64;
        unsafe {
            let vt = &**self.enc;
            if std::mem::take(&mut self.keyframe_next) {
                vt.ForceIntraFrame.ok_or("openh264")?(self.enc, true);
            }
            if vt.EncodeFrame.ok_or("openh264")?(self.enc, &pic, &mut *self.bs) != 0 {
                return Err("openh264: encode failed".into());
            }
        }
        let bs = &*self.bs;
        if bs.eFrameType == videoFrameTypeSkip || bs.eFrameType == videoFrameTypeInvalid {
            return Ok(None);
        }
        let mut data = Vec::with_capacity(bs.iFrameSizeInBytes.max(0) as usize);
        for layer in &bs.sLayerInfo[..bs.iLayerNum.max(0) as usize] {
            let lens = unsafe { std::slice::from_raw_parts(layer.pNalLengthInByte, layer.iNalCount.max(0) as usize) };
            let size: usize = lens.iter().map(|&n| n.max(0) as usize).sum();
            data.extend_from_slice(unsafe { std::slice::from_raw_parts(layer.pBsBuf, size) });
        }
        Ok(Some(Encoded {
            tag: 0,
            data,
            keyframe: bs.eFrameType == videoFrameTypeIDR || bs.eFrameType == videoFrameTypeI,
        }))
    }
}

impl Drop for H264 {
    fn drop(&mut self) {
        unsafe {
            if let Some(un) = (**self.enc).Uninitialize {
                un(self.enc);
            }
            self.api.WelsDestroySVCEncoder(self.enc);
        }
    }
}

/// A video encoder: software (openh264) or one of the machine's hardware encoders.
pub trait VideoEncoder: Send {
    /// What it is, for the log ("H.264 in hardware (VideoToolbox)").
    fn name(&self) -> String;
    fn hardware(&self) -> bool;
    /// The size it encodes (even: H.264 and HEVC need it).
    fn size(&self) -> (usize, usize);
    /// The next frame is a keyframe (a client joined, or lost a reference frame).
    fn keyframe(&mut self);
    fn wants_keyframe(&self) -> bool;
    /// A new target bitrate, taken at once (no keyframe).
    fn set_bitrate(&mut self, kbps: u32) -> Result<(), String>;
    /// One frame of BGRA pixels (`stride` bytes a row), tagged (its capture time). Gives back the
    /// frames done: this one (an encoder that works one frame at a time), or earlier ones (a
    /// pipelined hardware encoder: see `poll`); none when it skipped the frame.
    fn encode(&mut self, bgra: &[u8], stride: usize, tag: u64) -> Result<Vec<Encoded>, String>;
    /// Whether it reads the system's own frames in place (zero copy): see `encode_surface`.
    fn takes_surfaces(&self) -> bool {
        false
    }
    /// Whether it scales a surface bigger than its own size itself (in hardware).
    fn scales_surfaces(&self) -> bool {
        false
    }
    /// A frame as the system's own image (`Source::surface`), read where it is.
    fn encode_surface(&mut self, _surface: &crate::capture::Surface, _tag: u64) -> Result<Vec<Encoded>, String> {
        Err("this encoder reads frames from memory".into())
    }
    /// Frames still being encoded (a pipelined encoder): `poll` gives them back once done.
    fn in_flight(&self) -> usize {
        0
    }
    /// The frames done since the last call (a pipelined encoder), without waiting.
    fn poll(&mut self) -> Result<Vec<Encoded>, String> {
        Ok(Vec::new())
    }
}

impl VideoEncoder for H264 {
    fn name(&self) -> String {
        "H.264 in software (openh264)".into()
    }
    fn hardware(&self) -> bool {
        false
    }
    fn size(&self) -> (usize, usize) {
        H264::size(self)
    }
    fn keyframe(&mut self) {
        H264::keyframe(self)
    }
    fn wants_keyframe(&self) -> bool {
        H264::wants_keyframe(self)
    }
    fn set_bitrate(&mut self, kbps: u32) -> Result<(), String> {
        H264::set_bitrate(self, kbps)
    }
    fn encode(&mut self, bgra: &[u8], stride: usize, tag: u64) -> Result<Vec<Encoded>, String> {
        Ok(H264::encode(self, bgra, stride)?.map(|e| Encoded { tag, ..e }).into_iter().collect())
    }
}

/// Which encoders to use: `RR_ENCODER` = `auto` (the default: hardware where it works, else
/// software), `software`, `hardware` (no fallback), or one backend by name (`videotoolbox`,
/// `mediafoundation`, `vaapi`).
fn choice() -> String {
    std::env::var("RR_ENCODER").unwrap_or_default().to_ascii_lowercase()
}

/// The machine's hardware encoders for `codec`, best first: each is tried in turn when a stream starts.
#[allow(unused_variables, unused_mut)]
fn hardware(codec: Codec, width: usize, height: usize, fps: f32, kbps: u32) -> Vec<Result<Box<dyn VideoEncoder>, String>> {
    let want = choice();
    let named = |n: &str| matches!(want.as_str(), "" | "auto" | "hardware") || want == n;
    let mut out: Vec<Result<Box<dyn VideoEncoder>, String>> = Vec::new();
    #[cfg(target_os = "macos")]
    if named("videotoolbox") {
        out.push(crate::encode_vt::VideoToolbox::new(codec, width, height, fps, kbps).map(|e| Box::new(e) as Box<dyn VideoEncoder>));
    }
    out
}

/// An encoder for a stream: the machine's hardware one where it works, else software (H.264 only).
pub fn open(codec: Codec, width: usize, height: usize, fps: f32, kbps: u32) -> Result<Box<dyn VideoEncoder>, String> {
    let want = choice();
    let mut why = Vec::new();
    if want != "software" {
        for e in hardware(codec, width, height, fps, kbps) {
            match e {
                Ok(e) => return Ok(e),
                Err(e) => why.push(e),
            }
        }
    }
    let soft = !matches!(want.as_str(), "hardware" | "videotoolbox" | "mediafoundation" | "vaapi");
    if soft && codec == Codec::H264 {
        if !why.is_empty() {
            eprintln!("rein-remote: no hardware encoder ({}): encoding in software", why.join("; "));
        }
        return Ok(Box::new(H264::new(width, height, fps, kbps)?));
    }
    Err(if why.is_empty() {
        format!("no encoder for {codec:?} (RR_ENCODER={want})")
    } else {
        why.join("; ")
    })
}

/// Whether a hardware encoder for `codec` opens here: asked of the system where it can say without
/// opening one (macOS lists its encoders), else by opening each, small.
fn has_hardware(codec: Codec) -> bool {
    #[cfg(target_os = "macos")]
    if matches!(choice().as_str(), "" | "auto" | "hardware" | "videotoolbox") {
        return crate::encode_vt::listed(codec);
    }
    hardware(codec, 256, 144, 30.0, 500).into_iter().any(|e| e.is_ok())
}

/// What this machine encodes, each codec once, hardware first (for the server's Hello): found the
/// first time it's asked (the server asks as it starts).
pub fn capabilities() -> &'static [CodecCap] {
    static CAPS: std::sync::OnceLock<Vec<CodecCap>> = std::sync::OnceLock::new();
    CAPS.get_or_init(|| {
        let mut caps = Vec::new();
        for codec in [Codec::H264, Codec::H265] {
            if choice() != "software" && has_hardware(codec) {
                caps.push(CodecCap { codec, hardware: true });
            } else if codec == Codec::H264 && !matches!(choice().as_str(), "hardware" | "videotoolbox" | "mediafoundation" | "vaapi") {
                caps.push(CodecCap { codec, hardware: false });
            }
        }
        caps
    })
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
        let mut soft = open(Codec::H264, w, h, 60.0, 8000).unwrap();
        std::env::remove_var("RR_ENCODER");
        let mut hard = open(Codec::H264, w, h, 60.0, 8000).unwrap();
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
                enc.encode(f, w * 4, 0).unwrap();
            }
            while enc.in_flight() > 0 {
                enc.poll().unwrap();
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

#[cfg(test)]
mod rc {
    use super::*;
    /// Does the software encoder keep to a low bitrate on full-screen motion? (sizes per frame)
    #[test]
    #[ignore]
    fn sizes_at_low_bitrate() {
        let (w, h) = (1280, 720);
        let mut enc = H264::new(w, h, 5.0, 250).unwrap();
        let mut sizes = vec![];
        for n in 0..20 {
            let mut f = vec![0u8; w * h * 4];
            for (i, px) in f.chunks_mut(4).enumerate() {
                let (x, y) = (i % w, i / w);
                let v = (((x / 8) * 31 + (y / 12 + n * 3) * 17) % 251) as u8;
                px.copy_from_slice(&[v, v, v, 255]);
            }
            std::thread::sleep(std::time::Duration::from_millis(200));
            sizes.push(enc.encode(&f, w * 4).unwrap().map_or(0, |e| e.data.len()));
        }
        println!("bytes per frame at 250 kbit/s, 5 fps: {sizes:?}");
    }
}
