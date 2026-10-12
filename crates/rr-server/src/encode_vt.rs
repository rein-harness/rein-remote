//! macOS: H.264 and HEVC in hardware with VideoToolbox (the media engine on Apple silicon, Quick Sync
//! on Intel Macs). The frame's BGRA memory goes in as it is (the hardware converts the color), each
//! frame is encoded at once (real time, no reordering), and the output is the same Annex B stream the
//! software encoder makes (start codes, the parameter sets before each keyframe: SPS and PPS, and
//! HEVC's VPS).

use crate::encode::{Encoded, VideoEncoder};
use core_foundation::base::{CFRelease, CFTypeRef, TCFType};
use core_foundation::boolean::CFBoolean;
use core_foundation::dictionary::CFDictionary;
use core_foundation::number::CFNumber;
use core_foundation::string::{CFString, CFStringRef};
use rr_proto::Codec;
use std::collections::VecDeque;
use std::ffi::c_void;
use std::sync::{Condvar, Mutex};

type Session = *mut c_void;
type SampleBuffer = *mut c_void;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct CMTime {
    value: i64,
    timescale: i32,
    flags: u32,
    epoch: i64,
}
const VALID: u32 = 1;
const INVALID_TIME: CMTime = CMTime {
    value: 0,
    timescale: 0,
    flags: 0,
    epoch: 0,
};

const H264: u32 = u32::from_be_bytes(*b"avc1");
const HEVC: u32 = u32::from_be_bytes(*b"hvc1");
const BGRA: u32 = u32::from_be_bytes(*b"BGRA");

#[allow(clippy::duplicated_attributes)] // one #[link] per framework
#[link(name = "VideoToolbox", kind = "framework")]
#[link(name = "CoreMedia", kind = "framework")]
#[link(name = "CoreVideo", kind = "framework")]
extern "C" {
    fn VTCompressionSessionCreate(
        alloc: *const c_void,
        width: i32,
        height: i32,
        codec: u32,
        encoder_spec: *const c_void,
        source_attrs: *const c_void,
        compressed_alloc: *const c_void,
        callback: extern "C" fn(*mut c_void, *mut c_void, i32, u32, SampleBuffer),
        refcon: *mut c_void,
        out: *mut Session,
    ) -> i32;
    fn VTCopyVideoEncoderList(options: *const c_void, out: *mut *const c_void) -> i32;
    static kVTVideoEncoderList_CodecType: CFStringRef;
    static kVTVideoEncoderList_IsHardwareAccelerated: CFStringRef;
    fn VTSessionSetProperty(session: Session, key: CFStringRef, value: CFTypeRef) -> i32;
    fn VTCompressionSessionPrepareToEncodeFrames(session: Session) -> i32;
    fn VTCompressionSessionEncodeFrame(
        session: Session,
        image: *mut c_void,
        pts: CMTime,
        duration: CMTime,
        props: *const c_void,
        refcon: *mut c_void,
        flags_out: *mut u32,
    ) -> i32;
    fn VTCompressionSessionCompleteFrames(session: Session, until: CMTime) -> i32;
    fn VTCompressionSessionInvalidate(session: Session);

    static kVTCompressionPropertyKey_RealTime: CFStringRef;
    static kVTCompressionPropertyKey_AllowFrameReordering: CFStringRef;
    static kVTCompressionPropertyKey_ProfileLevel: CFStringRef;
    static kVTCompressionPropertyKey_AverageBitRate: CFStringRef;
    static kVTCompressionPropertyKey_ExpectedFrameRate: CFStringRef;
    static kVTCompressionPropertyKey_MaxKeyFrameInterval: CFStringRef;
    static kVTProfileLevel_H264_Main_AutoLevel: CFStringRef;
    static kVTProfileLevel_HEVC_Main_AutoLevel: CFStringRef;
    static kVTEncodeFrameOptionKey_ForceKeyFrame: CFStringRef;
    static kVTVideoEncoderSpecification_EnableLowLatencyRateControl: CFStringRef;

    fn CVPixelBufferCreateWithBytes(
        alloc: *const c_void,
        width: usize,
        height: usize,
        format: u32,
        base: *mut c_void,
        bytes_per_row: usize,
        release: *const c_void,
        release_ref: *mut c_void,
        attrs: *const c_void,
        out: *mut *mut c_void,
    ) -> i32;
    fn VTCompressionSessionGetPixelBufferPool(session: Session) -> *mut c_void;
    fn CVPixelBufferPoolCreatePixelBuffer(alloc: *const c_void, pool: *mut c_void, out: *mut *mut c_void) -> i32;
    fn CVPixelBufferLockBaseAddress(buf: *mut c_void, flags: u64) -> i32;
    fn CVPixelBufferUnlockBaseAddress(buf: *mut c_void, flags: u64) -> i32;
    fn CVPixelBufferGetBaseAddress(buf: *mut c_void) -> *mut u8;
    fn CVPixelBufferGetBytesPerRow(buf: *mut c_void) -> usize;
    static kCVPixelBufferPixelFormatTypeKey: CFStringRef;
    static kCVPixelBufferWidthKey: CFStringRef;
    static kCVPixelBufferHeightKey: CFStringRef;
    static kCVPixelBufferIOSurfacePropertiesKey: CFStringRef;
    fn CMSampleBufferGetDataBuffer(sample: SampleBuffer) -> *mut c_void;
    fn CMBlockBufferGetDataPointer(block: *mut c_void, offset: usize, len_at: *mut usize, total: *mut usize, ptr: *mut *mut u8) -> i32;
    fn CMSampleBufferGetFormatDescription(sample: SampleBuffer) -> *mut c_void;
    fn CMVideoFormatDescriptionGetH264ParameterSetAtIndex(
        desc: *mut c_void,
        index: usize,
        ptr: *mut *const u8,
        size: *mut usize,
        count: *mut usize,
        nal_len: *mut i32,
    ) -> i32;
    fn CMVideoFormatDescriptionGetHEVCParameterSetAtIndex(
        desc: *mut c_void,
        index: usize,
        ptr: *mut *const u8,
        size: *mut usize,
        count: *mut usize,
        nal_len: *mut i32,
    ) -> i32;
    fn CMSampleBufferGetSampleAttachmentsArray(sample: SampleBuffer, create: bool) -> *const c_void;
    fn CFArrayGetCount(array: *const c_void) -> isize;
    fn CFArrayGetValueAtIndex(array: *const c_void, i: isize) -> *const c_void;
    fn CFDictionaryGetValue(dict: *const c_void, key: *const c_void) -> *const c_void;
    static kCMSampleAttachmentKey_NotSync: CFStringRef;
    static kCFBooleanTrue: *const c_void;
}

/// What the callback hands back, frame by frame as each is done (Annex B, keyframe or not, its
/// tag), and how many it has seen (dropped ones too).
struct Out {
    done: Mutex<(VecDeque<Result<Encoded, String>>, u64)>,
    ready: Condvar,
    hevc: bool,
}

/// Frames the hardware works on at once (`RR_PIPELINE`, 1 by default). The server doesn't wait for a
/// frame to be encoded: it reads the next while the hardware works and sends each the moment it's
/// done, so the rate isn't bound by one frame's encode time. More than one at once measured slower
/// (they share the same engine).
fn max_in_flight() -> u64 {
    std::env::var("RR_PIPELINE").ok().and_then(|v| v.parse().ok()).unwrap_or(1).clamp(1, 4)
}

extern "C" fn on_frame(refcon: *mut c_void, tag: *mut c_void, status: i32, _flags: u32, sample: SampleBuffer) {
    let out = unsafe { &*(refcon as *const Out) };
    let result = if status != 0 || sample.is_null() {
        // A dropped frame has no sample (status 0): nothing to send.
        (status != 0).then(|| Err(format!("VideoToolbox: encode failed ({status})")))
    } else {
        Some(unsafe { annex_b(sample, out.hevc) }.map(|e| Encoded { tag: tag as u64, ..e }))
    };
    let mut done = out.done.lock().unwrap();
    done.0.extend(result);
    done.1 += 1;
    out.ready.notify_all();
}

/// A sample (lengths before each NAL unit) as Annex B (start codes), the parameter sets first on a keyframe.
unsafe fn annex_b(sample: SampleBuffer, hevc: bool) -> Result<Encoded, String> {
    let attachments = CMSampleBufferGetSampleAttachmentsArray(sample, false);
    let keyframe = attachments.is_null() || CFArrayGetCount(attachments) == 0 || {
        let first = CFArrayGetValueAtIndex(attachments, 0);
        CFDictionaryGetValue(first, kCMSampleAttachmentKey_NotSync as *const c_void) != kCFBooleanTrue
    };
    let mut data = Vec::new();
    let mut nal_len = 4i32;
    if keyframe {
        let desc = CMSampleBufferGetFormatDescription(sample);
        let get = if hevc {
            CMVideoFormatDescriptionGetHEVCParameterSetAtIndex
        } else {
            CMVideoFormatDescriptionGetH264ParameterSetAtIndex
        };
        let mut count = 0usize;
        get(desc, 0, std::ptr::null_mut(), std::ptr::null_mut(), &mut count, &mut nal_len);
        for i in 0..count {
            let (mut ptr, mut size) = (std::ptr::null(), 0usize);
            if get(desc, i, &mut ptr, &mut size, std::ptr::null_mut(), std::ptr::null_mut()) == 0 {
                data.extend_from_slice(&[0, 0, 0, 1]);
                data.extend_from_slice(std::slice::from_raw_parts(ptr, size));
            }
        }
    }
    let block = CMSampleBufferGetDataBuffer(sample);
    let (mut total, mut ptr) = (0usize, std::ptr::null_mut());
    if block.is_null() || CMBlockBufferGetDataPointer(block, 0, std::ptr::null_mut(), &mut total, &mut ptr) != 0 {
        return Err("VideoToolbox: no data in the frame".into());
    }
    let bytes = std::slice::from_raw_parts(ptr, total);
    let n = nal_len.clamp(1, 4) as usize;
    let mut at = 0;
    while at + n <= bytes.len() {
        let len = bytes[at..at + n].iter().fold(0usize, |v, b| (v << 8) | *b as usize);
        at += n;
        if at + len > bytes.len() {
            break;
        }
        data.extend_from_slice(&[0, 0, 0, 1]);
        data.extend_from_slice(&bytes[at..at + len]);
        at += len;
    }
    Ok(Encoded { data, keyframe, tag: 0 })
}

/// Whether this Mac lists a hardware encoder for `codec` (without opening one: that takes a second).
pub fn listed(codec: Codec) -> bool {
    let want = match codec {
        Codec::H264 => H264,
        Codec::H265 => HEVC,
        _ => return false,
    };
    unsafe {
        let mut list = std::ptr::null();
        if VTCopyVideoEncoderList(std::ptr::null(), &mut list) != 0 || list.is_null() {
            return false;
        }
        let found = (0..CFArrayGetCount(list)).any(|i| {
            let d = CFArrayGetValueAtIndex(list, i);
            let ty = CFDictionaryGetValue(d, kVTVideoEncoderList_CodecType as *const c_void);
            let hw = CFDictionaryGetValue(d, kVTVideoEncoderList_IsHardwareAccelerated as *const c_void);
            !ty.is_null() && CFNumber::wrap_under_get_rule(ty as _).to_i64() == Some(want as i64) && hw == kCFBooleanTrue
        });
        CFRelease(list);
        found
    }
}

pub struct VideoToolbox {
    session: Session,
    /// Asks the session to finish each frame as soon as it's in (VideoToolbox otherwise keeps the last
    /// frame until the next one comes: on a still screen, for good). On a thread of its own, so the
    /// server goes on while the hardware works.
    flush: Option<(std::sync::mpsc::Sender<CMTime>, std::thread::JoinHandle<()>)>,
    codec: Codec,
    width: usize,
    height: usize,
    keyframe_next: bool,
    frame_no: i64,
    /// Frames handed to the session (the callback counts those done).
    submitted: u64,
    fps: i32,
    out: Box<Out>,
}

// The session is only used by whoever holds the encoder.
unsafe impl Send for VideoToolbox {}

impl VideoToolbox {
    pub fn new(codec: Codec, width: usize, height: usize, fps: f32, bitrate_kbps: u32) -> Result<Self, String> {
        let (width, height) = (width & !1, height & !1);
        let (fourcc, profile, what) = match codec {
            Codec::H264 => (H264, unsafe { kVTProfileLevel_H264_Main_AutoLevel }, "H.264"),
            Codec::H265 => (HEVC, unsafe { kVTProfileLevel_HEVC_Main_AutoLevel }, "HEVC"),
            c => return Err(format!("VideoToolbox: no {c:?} encoder here")),
        };
        let out: Box<Out> = Box::new(Out {
            done: Mutex::new((VecDeque::new(), 0)),
            ready: Condvar::new(),
            hevc: codec == Codec::H265,
        });
        let mut session: Session = std::ptr::null_mut();
        unsafe {
            // The low-latency rate control (no lookahead, each frame out as soon as it's in).
            let spec = CFDictionary::from_CFType_pairs(&[(
                CFString::wrap_under_get_rule(kVTVideoEncoderSpecification_EnableLowLatencyRateControl),
                CFBoolean::true_value().as_CFType(),
            )]);
            // Frames from memory go into buffers from the session's own pool (GPU memory, IOSurfaces), the
            // form the encoder reads without converting them first.
            let source = CFDictionary::from_CFType_pairs(&[
                (
                    CFString::wrap_under_get_rule(kCVPixelBufferPixelFormatTypeKey),
                    CFNumber::from(BGRA as i64).as_CFType(),
                ),
                (CFString::wrap_under_get_rule(kCVPixelBufferWidthKey), CFNumber::from(width as i64).as_CFType()),
                (
                    CFString::wrap_under_get_rule(kCVPixelBufferHeightKey),
                    CFNumber::from(height as i64).as_CFType(),
                ),
                (
                    CFString::wrap_under_get_rule(kCVPixelBufferIOSurfacePropertiesKey),
                    CFDictionary::<CFString, CFNumber>::from_CFType_pairs(&[]).as_CFType(),
                ),
            ]);
            let status = VTCompressionSessionCreate(
                std::ptr::null(),
                width as i32,
                height as i32,
                fourcc,
                spec.as_concrete_TypeRef() as *const c_void,
                source.as_concrete_TypeRef() as *const c_void,
                std::ptr::null(),
                on_frame,
                &*out as *const Out as *mut c_void,
                &mut session,
            );
            if status != 0 || session.is_null() {
                return Err(format!("VideoToolbox: no {what} encoder ({status})"));
            }
            let set = |k: CFStringRef, v: CFTypeRef| VTSessionSetProperty(session, k, v);
            set(kVTCompressionPropertyKey_RealTime, CFBoolean::true_value().as_CFTypeRef());
            set(kVTCompressionPropertyKey_AllowFrameReordering, CFBoolean::false_value().as_CFTypeRef());
            set(kVTCompressionPropertyKey_ProfileLevel, profile as CFTypeRef);
            set(
                kVTCompressionPropertyKey_AverageBitRate,
                CFNumber::from(bitrate_kbps as i64 * 1000).as_CFTypeRef(),
            );
            set(kVTCompressionPropertyKey_ExpectedFrameRate, CFNumber::from(fps as f64).as_CFTypeRef());
            // A keyframe only when someone needs one (the client asks; the server sends one after a backlog).
            set(kVTCompressionPropertyKey_MaxKeyFrameInterval, CFNumber::from(i32::MAX as i64).as_CFTypeRef());
            VTCompressionSessionPrepareToEncodeFrames(session);
        }
        let flush = {
            let (tx, rx) = std::sync::mpsc::channel::<CMTime>();
            let s = session as usize;
            let t = std::thread::Builder::new()
                .name("vt-flush".into())
                .spawn(move || {
                    for until in rx {
                        unsafe { VTCompressionSessionCompleteFrames(s as Session, until) };
                    }
                })
                .map_err(|e| e.to_string())?;
            Some((tx, t))
        };
        Ok(Self {
            session,
            flush,
            codec,
            width,
            height,
            keyframe_next: true,
            frame_no: 0,
            submitted: 0,
            fps: fps.max(1.0) as i32,
            out,
        })
    }

    pub fn size(&self) -> (usize, usize) {
        (self.width, self.height)
    }

    /// A new target bitrate, taken at once (no keyframe).
    pub fn set_bitrate(&mut self, kbps: u32) -> Result<(), String> {
        let n = CFNumber::from(kbps as i64 * 1000);
        match unsafe { VTSessionSetProperty(self.session, kVTCompressionPropertyKey_AverageBitRate, n.as_CFTypeRef()) } {
            0 => Ok(()),
            s => Err(format!("VideoToolbox: can't set the bitrate ({s})")),
        }
    }

    pub fn keyframe(&mut self) {
        self.keyframe_next = true;
    }

    pub fn wants_keyframe(&self) -> bool {
        self.keyframe_next
    }

    pub fn encode(&mut self, bgra: &[u8], stride: usize, tag: u64) -> Result<Vec<Encoded>, String> {
        if bgra.len() < stride * self.height {
            return Err("VideoToolbox: the frame is smaller than the stream".into());
        }
        unsafe {
            // Into a buffer from the session's pool (copied row by row: GPU memory the encoder reads as
            // it is); the frame's own memory wrapped as it is when there's no pool.
            let pool = VTCompressionSessionGetPixelBufferPool(self.session);
            let mut pixels: *mut c_void = std::ptr::null_mut();
            if !pool.is_null() && CVPixelBufferPoolCreatePixelBuffer(std::ptr::null(), pool, &mut pixels) == 0 && !pixels.is_null() {
                CVPixelBufferLockBaseAddress(pixels, 0);
                let (dst, dst_stride) = (CVPixelBufferGetBaseAddress(pixels), CVPixelBufferGetBytesPerRow(pixels));
                let row = (self.width * 4).min(stride).min(dst_stride);
                for y in 0..self.height {
                    std::ptr::copy_nonoverlapping(bgra.as_ptr().add(y * stride), dst.add(y * dst_stride), row);
                }
                CVPixelBufferUnlockBaseAddress(pixels, 0);
            } else {
                // The frame's own memory, valid until the encode returns: it waits for this frame.
                let status = CVPixelBufferCreateWithBytes(
                    std::ptr::null(),
                    self.width,
                    self.height,
                    BGRA,
                    bgra.as_ptr() as *mut c_void,
                    stride,
                    std::ptr::null(),
                    std::ptr::null_mut(),
                    std::ptr::null(),
                    &mut pixels,
                );
                if status != 0 || pixels.is_null() {
                    return Err(format!("VideoToolbox: can't wrap the frame ({status})"));
                }
                let mut r = self.encode_image(pixels, tag, 1);
                CFRelease(pixels as CFTypeRef);
                VTCompressionSessionCompleteFrames(self.session, INVALID_TIME);
                if let Ok(v) = &mut r {
                    v.extend(self.poll()?);
                }
                return r;
            }
            let r = self.encode_image(pixels, tag, max_in_flight());
            CFRelease(pixels as CFTypeRef);
            r
        }
    }

    /// A frame already in the system's own form (a CVPixelBuffer from ScreenCaptureKit), read where
    /// it is: no copy.
    /// (The session holds on to the buffer for as long as it needs it.)
    pub fn encode_surface(&mut self, surface: &crate::capture::Surface, tag: u64) -> Result<Vec<Encoded>, String> {
        unsafe { self.encode_image(surface.0, tag, max_in_flight()) }
    }

    pub fn in_flight(&self) -> usize {
        (self.submitted - self.out.done.lock().unwrap().1) as usize
    }

    /// The frames done so far, without waiting.
    pub fn poll(&mut self) -> Result<Vec<Encoded>, String> {
        self.out.done.lock().unwrap().0.drain(..).collect()
    }

    /// Hands the frame to the session once fewer than `limit` are being encoded, and gives back what's
    /// done by then.
    unsafe fn encode_image(&mut self, pixels: *mut c_void, tag: u64, limit: u64) -> Result<Vec<Encoded>, String> {
        {
            let mut done = self.out.done.lock().unwrap();
            while self.submitted - done.1 >= limit {
                let (d, timeout) = self.out.ready.wait_timeout(done, std::time::Duration::from_secs(2)).unwrap();
                done = d;
                if timeout.timed_out() {
                    return Err("VideoToolbox: the encoder stopped answering".into());
                }
            }
        }
        let force = std::mem::take(&mut self.keyframe_next);
        let props = force.then(|| {
            CFDictionary::from_CFType_pairs(&[(
                CFString::wrap_under_get_rule(kVTEncodeFrameOptionKey_ForceKeyFrame),
                CFBoolean::true_value().as_CFType(),
            )])
        });
        self.frame_no += 1;
        let pts = CMTime {
            value: self.frame_no,
            timescale: self.fps,
            flags: VALID,
            epoch: 0,
        };
        let status = VTCompressionSessionEncodeFrame(
            self.session,
            pixels,
            pts,
            INVALID_TIME,
            props.as_ref().map_or(std::ptr::null(), |p| p.as_concrete_TypeRef() as *const c_void),
            tag as *mut c_void,
            std::ptr::null_mut(),
        );
        if status != 0 {
            return Err(format!("VideoToolbox: encode failed ({status})"));
        }
        self.submitted += 1;
        if let Some((tx, _)) = &self.flush {
            let _ = tx.send(pts);
        }
        self.poll()
    }
}

impl VideoEncoder for VideoToolbox {
    fn name(&self) -> String {
        format!("{} in hardware (VideoToolbox)", if self.codec == Codec::H265 { "HEVC" } else { "H.264" })
    }
    fn hardware(&self) -> bool {
        true
    }
    fn size(&self) -> (usize, usize) {
        VideoToolbox::size(self)
    }
    fn keyframe(&mut self) {
        VideoToolbox::keyframe(self)
    }
    fn wants_keyframe(&self) -> bool {
        VideoToolbox::wants_keyframe(self)
    }
    fn set_bitrate(&mut self, kbps: u32) -> Result<(), String> {
        VideoToolbox::set_bitrate(self, kbps)
    }
    fn encode(&mut self, bgra: &[u8], stride: usize, tag: u64) -> Result<Vec<Encoded>, String> {
        VideoToolbox::encode(self, bgra, stride, tag)
    }
    fn takes_surfaces(&self) -> bool {
        true
    }
    fn encode_surface(&mut self, surface: &crate::capture::Surface, tag: u64) -> Result<Vec<Encoded>, String> {
        VideoToolbox::encode_surface(self, surface, tag)
    }
    fn in_flight(&self) -> usize {
        VideoToolbox::in_flight(self)
    }
    fn poll(&mut self) -> Result<Vec<Encoded>, String> {
        VideoToolbox::poll(self)
    }
}

impl Drop for VideoToolbox {
    fn drop(&mut self) {
        if let Some((tx, t)) = self.flush.take() {
            drop(tx);
            let _ = t.join();
        }
        unsafe {
            VTCompressionSessionCompleteFrames(self.session, INVALID_TIME);
            VTCompressionSessionInvalidate(self.session);
            CFRelease(self.session as CFTypeRef);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One frame in, waited for: the frame out.
    fn one(enc: &mut VideoToolbox, frame: &[u8], stride: usize, tag: u64) -> Option<Encoded> {
        let mut out = enc.encode(frame, stride, tag).unwrap();
        while enc.in_flight() > 0 {
            std::thread::sleep(std::time::Duration::from_millis(1));
            out.extend(enc.poll().unwrap());
        }
        out.into_iter().find(|e| e.tag == tag)
    }

    #[test]
    fn encodes_a_keyframe_then_deltas_in_annex_b() {
        let (w, h) = (320, 240);
        let Ok(mut enc) = VideoToolbox::new(Codec::H264, w, h, 30.0, 2000) else {
            return;
        }; // a Mac without an H.264 encoder
        let mut frame = vec![40u8; w * h * 4];
        let first = one(&mut enc, &frame, w * 4, 1).expect("a first frame");
        assert!(first.keyframe);
        assert_eq!(&first.data[..4], &[0, 0, 0, 1]);
        assert!(
            first.data.windows(5).any(|w| w[..4] == [0, 0, 0, 1] && w[4] & 0x1f == 7),
            "SPS before the keyframe"
        );
        for px in frame[..w * 40 * 4].chunks_mut(4) {
            px.copy_from_slice(&[255, 255, 255, 255]);
        }
        let delta = one(&mut enc, &frame, w * 4, 2).expect("a delta frame");
        assert!(!delta.keyframe);
        enc.keyframe();
        assert!(one(&mut enc, &frame, w * 4, 3).unwrap().keyframe);
    }

    #[test]
    fn hevc_has_vps_sps_pps_before_the_keyframe() {
        let (w, h) = (320, 240);
        let Ok(mut enc) = VideoToolbox::new(Codec::H265, w, h, 30.0, 2000) else {
            return;
        }; // no HEVC encoder
        let frame = vec![90u8; w * h * 4];
        let first = one(&mut enc, &frame, w * 4, 4).expect("a first frame");
        assert!(first.keyframe);
        // HEVC NAL type is bits 1..7 of the first byte: 32 VPS, 33 SPS, 34 PPS.
        let types: Vec<u8> = first.data.windows(5).filter(|w| w[..4] == [0, 0, 0, 1]).map(|w| (w[4] >> 1) & 0x3f).collect();
        assert_eq!(&types[..3], &[32, 33, 34], "{types:?}");
        assert!(!one(&mut enc, &frame, w * 4, 5).is_some_and(|d| d.keyframe));
    }

    #[test]
    fn lists_hardware_encoders_like_it_opens_them() {
        for codec in [Codec::H264, Codec::H265] {
            if listed(codec) {
                assert!(VideoToolbox::new(codec, 320, 240, 30.0, 1000).is_ok(), "{codec:?} listed but doesn't open");
            }
        }
    }

    #[test]
    fn pipelined_frames_come_back_in_order_with_their_tags() {
        let (w, h) = (640, 360);
        let Ok(mut enc) = VideoToolbox::new(Codec::H264, w, h, 60.0, 4000) else {
            return;
        };
        let mut frame = vec![0u8; w * h * 4];
        let mut out = Vec::new();
        for n in 1..=20u64 {
            let at = (n as usize * 997) % frame.len();
            frame[at] ^= 0xff;
            out.extend(enc.encode(&frame, w * 4, n).unwrap());
            assert!(enc.in_flight() <= 2);
        }
        while enc.in_flight() > 0 {
            std::thread::sleep(std::time::Duration::from_millis(1));
            out.extend(enc.poll().unwrap());
        }
        let tags: Vec<u64> = out.iter().map(|e| e.tag).collect();
        assert!(tags.windows(2).all(|t| t[0] < t[1]), "{tags:?}");
        assert_eq!(tags.first(), Some(&1));
        assert!(out[0].keyframe);
    }
}
