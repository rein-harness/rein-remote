//! Video encoding. Phase 1: software H.264 (openh264, BSD-licensed), tuned for screens in real time:
//! no B-frames, bitrate rate control, a keyframe on request. Hardware encoders come per platform in
//! later phases behind the same `encode` call.

use openh264_sys2::*;
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

    /// A new target bitrate, taken at once (no keyframe).
    pub fn set_bitrate(&mut self, kbps: u32) -> Result<(), String> {
        match self {
            Self::Software(e) => e.set_bitrate(kbps),
            #[cfg(target_os = "macos")]
            Self::Hardware(e) => e.set_bitrate(kbps),
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
