//! macOS: H.264 in hardware with VideoToolbox (the media engine on Apple silicon, Quick Sync on Intel
//! Macs). The frame's BGRA memory goes in as it is (the hardware converts the color), each frame is
//! encoded at once (real time, no reordering), and the output is the same Annex B stream the
//! software encoder makes (start codes, SPS and PPS before each keyframe).

use crate::encode::Encoded;
use core_foundation::base::{CFRelease, CFTypeRef, TCFType};
use core_foundation::boolean::CFBoolean;
use core_foundation::dictionary::CFDictionary;
use core_foundation::number::CFNumber;
use core_foundation::string::{CFString, CFStringRef};
use std::ffi::c_void;
use std::sync::Mutex;

type Session = *mut c_void;
type SampleBuffer = *mut c_void;

#[repr(C)]
#[derive(Clone, Copy)]
struct CMTime {
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
    fn CMSampleBufferGetSampleAttachmentsArray(sample: SampleBuffer, create: bool) -> *const c_void;
    fn CFArrayGetCount(array: *const c_void) -> isize;
    fn CFArrayGetValueAtIndex(array: *const c_void, i: isize) -> *const c_void;
    fn CFDictionaryGetValue(dict: *const c_void, key: *const c_void) -> *const c_void;
    static kCMSampleAttachmentKey_NotSync: CFStringRef;
    static kCFBooleanTrue: *const c_void;
}

/// What the callback hands back: the frame as Annex B, and whether it's a keyframe.
type Out = Mutex<Option<Result<Encoded, String>>>;

extern "C" fn on_frame(refcon: *mut c_void, _src: *mut c_void, status: i32, _flags: u32, sample: SampleBuffer) {
    let out = unsafe { &*(refcon as *const Out) };
    let result = if status != 0 || sample.is_null() {
        // A dropped frame has no sample (status 0): nothing to send.
        (status != 0).then(|| Err(format!("VideoToolbox: encode failed ({status})")))
    } else {
        Some(unsafe { annex_b(sample) })
    };
    *out.lock().unwrap() = result;
}

/// A sample (lengths before each NAL unit) as Annex B (start codes), SPS and PPS first on a keyframe.
unsafe fn annex_b(sample: SampleBuffer) -> Result<Encoded, String> {
    let attachments = CMSampleBufferGetSampleAttachmentsArray(sample, false);
    let keyframe = attachments.is_null() || CFArrayGetCount(attachments) == 0 || {
        let first = CFArrayGetValueAtIndex(attachments, 0);
        CFDictionaryGetValue(first, kCMSampleAttachmentKey_NotSync as *const c_void) != kCFBooleanTrue
    };
    let mut data = Vec::new();
    let mut nal_len = 4i32;
    if keyframe {
        let desc = CMSampleBufferGetFormatDescription(sample);
        let mut count = 0usize;
        CMVideoFormatDescriptionGetH264ParameterSetAtIndex(desc, 0, std::ptr::null_mut(), std::ptr::null_mut(), &mut count, &mut nal_len);
        for i in 0..count {
            let (mut ptr, mut size) = (std::ptr::null(), 0usize);
            if CMVideoFormatDescriptionGetH264ParameterSetAtIndex(desc, i, &mut ptr, &mut size, std::ptr::null_mut(), std::ptr::null_mut()) == 0 {
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
    Ok(Encoded { data, keyframe })
}

pub struct VideoToolbox {
    session: Session,
    width: usize,
    height: usize,
    keyframe_next: bool,
    frame_no: i64,
    fps: i32,
    out: Box<Out>,
}

// The session is only used by whoever holds the encoder.
unsafe impl Send for VideoToolbox {}

impl VideoToolbox {
    pub fn new(width: usize, height: usize, fps: f32, bitrate_kbps: u32) -> Result<Self, String> {
        let (width, height) = (width & !1, height & !1);
        let out: Box<Out> = Box::new(Mutex::new(None));
        let mut session: Session = std::ptr::null_mut();
        unsafe {
            // The low-latency rate control (no lookahead, each frame out as soon as it's in).
            let spec = CFDictionary::from_CFType_pairs(&[(
                CFString::wrap_under_get_rule(kVTVideoEncoderSpecification_EnableLowLatencyRateControl),
                CFBoolean::true_value().as_CFType(),
            )]);
            let status = VTCompressionSessionCreate(
                std::ptr::null(),
                width as i32,
                height as i32,
                H264,
                spec.as_concrete_TypeRef() as *const c_void,
                std::ptr::null(),
                std::ptr::null(),
                on_frame,
                &*out as *const Out as *mut c_void,
                &mut session,
            );
            if status != 0 || session.is_null() {
                return Err(format!("VideoToolbox: no H.264 encoder ({status})"));
            }
            let set = |k: CFStringRef, v: CFTypeRef| VTSessionSetProperty(session, k, v);
            set(kVTCompressionPropertyKey_RealTime, CFBoolean::true_value().as_CFTypeRef());
            set(kVTCompressionPropertyKey_AllowFrameReordering, CFBoolean::false_value().as_CFTypeRef());
            set(kVTCompressionPropertyKey_ProfileLevel, kVTProfileLevel_H264_Main_AutoLevel as CFTypeRef);
            set(
                kVTCompressionPropertyKey_AverageBitRate,
                CFNumber::from(bitrate_kbps as i64 * 1000).as_CFTypeRef(),
            );
            set(kVTCompressionPropertyKey_ExpectedFrameRate, CFNumber::from(fps as f64).as_CFTypeRef());
            // A keyframe only when someone needs one (the client asks; the server sends one after a backlog).
            set(kVTCompressionPropertyKey_MaxKeyFrameInterval, CFNumber::from(i32::MAX as i64).as_CFTypeRef());
            VTCompressionSessionPrepareToEncodeFrames(session);
        }
        Ok(Self {
            session,
            width,
            height,
            keyframe_next: true,
            frame_no: 0,
            fps: fps.max(1.0) as i32,
            out,
        })
    }

    pub fn size(&self) -> (usize, usize) {
        (self.width, self.height)
    }

    pub fn keyframe(&mut self) {
        self.keyframe_next = true;
    }

    pub fn wants_keyframe(&self) -> bool {
        self.keyframe_next
    }

    pub fn encode(&mut self, bgra: &[u8], stride: usize) -> Result<Option<Encoded>, String> {
        if bgra.len() < stride * self.height {
            return Err("VideoToolbox: the frame is smaller than the stream".into());
        }
        unsafe {
            let mut pixels: *mut c_void = std::ptr::null_mut();
            // The frame's own memory, valid until the encode returns (it waits for the frame).
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
            let r = self.encode_image(pixels);
            CFRelease(pixels as CFTypeRef);
            r
        }
    }

    /// A frame already in the system's own form (a CVPixelBuffer from ScreenCaptureKit), read where
    /// it is: no copy.
    pub fn encode_surface(&mut self, surface: &crate::capture::Surface) -> Result<Option<Encoded>, String> {
        unsafe { self.encode_image(surface.0) }
    }

    unsafe fn encode_image(&mut self, pixels: *mut c_void) -> Result<Option<Encoded>, String> {
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
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        );
        VTCompressionSessionCompleteFrames(self.session, INVALID_TIME);
        if status != 0 {
            return Err(format!("VideoToolbox: encode failed ({status})"));
        }
        self.out.lock().unwrap().take().transpose()
    }
}

impl Drop for VideoToolbox {
    fn drop(&mut self) {
        unsafe {
            VTCompressionSessionInvalidate(self.session);
            CFRelease(self.session as CFTypeRef);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_a_keyframe_then_deltas_in_annex_b() {
        let (w, h) = (320, 240);
        let Ok(mut enc) = VideoToolbox::new(w, h, 30.0, 2000) else { return }; // a Mac without an H.264 encoder
        let mut frame = vec![40u8; w * h * 4];
        let first = enc.encode(&frame, w * 4).unwrap().expect("a first frame");
        assert!(first.keyframe);
        assert_eq!(&first.data[..4], &[0, 0, 0, 1]);
        assert!(
            first.data.windows(5).any(|w| w[..4] == [0, 0, 0, 1] && w[4] & 0x1f == 7),
            "SPS before the keyframe"
        );
        for px in frame[..w * 40 * 4].chunks_mut(4) {
            px.copy_from_slice(&[255, 255, 255, 255]);
        }
        let delta = enc.encode(&frame, w * 4).unwrap().expect("a delta frame");
        assert!(!delta.keyframe);
        enc.keyframe();
        assert!(enc.encode(&frame, w * 4).unwrap().unwrap().keyframe);
    }
}
