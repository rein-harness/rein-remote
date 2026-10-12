//! macOS: H.264 and HEVC decoded in hardware with VideoToolbox (the media engine on Apple silicon,
//! Quick Sync on Intel Macs). The parameter sets from each keyframe make the format description; each
//! frame goes in as one sample (its NAL units with lengths in place of start codes) and is decoded at
//! once (no reordering: the server sends none). The session gives BGRA, converted by the hardware, so
//! the CPU only copies the picture out.
//!
//! In a window, nothing is copied at all ("zero copy"): the picture stays the decoder's (an IOSurface,
//! GPU memory) and becomes the contents of a Core Animation layer, which the GPU scales and
//! composites (mac_layer.rs). Headless, it's copied out into memory.

use super::{nal_units, Decoder, Picture};
use core_foundation::base::{CFRelease, TCFType};
use core_foundation::boolean::CFBoolean;
use core_foundation::dictionary::CFDictionary;
use core_foundation::number::CFNumber;
use core_foundation::string::{CFString, CFStringRef};
use rr_proto::Codec;
use std::ffi::c_void;
use std::sync::Mutex;

type Session = *mut c_void;
type FormatDescription = *mut c_void;
type PixelBuffer = *mut c_void;

#[repr(C)]
#[derive(Clone, Copy)]
struct CMTime {
    value: i64,
    timescale: i32,
    flags: u32,
    epoch: i64,
}

#[repr(C)]
struct OutputCallback {
    callback: extern "C" fn(*mut c_void, *mut c_void, i32, u32, PixelBuffer, CMTime, CMTime),
    refcon: *mut c_void,
}

const H264: u32 = u32::from_be_bytes(*b"avc1");
const HEVC: u32 = u32::from_be_bytes(*b"hvc1");
const BGRA: u32 = u32::from_be_bytes(*b"BGRA");
const LOCK_READ_ONLY: u64 = 1;

#[allow(clippy::duplicated_attributes)] // one #[link] per framework
#[link(name = "VideoToolbox", kind = "framework")]
#[link(name = "CoreMedia", kind = "framework")]
#[link(name = "CoreVideo", kind = "framework")]
extern "C" {
    fn VTIsHardwareDecodeSupported(codec: u32) -> bool;
    fn VTDecompressionSessionCreate(
        alloc: *const c_void,
        format: FormatDescription,
        decoder_spec: *const c_void,
        image_attrs: *const c_void,
        callback: *const OutputCallback,
        out: *mut Session,
    ) -> i32;
    fn VTDecompressionSessionDecodeFrame(session: Session, sample: *mut c_void, flags: u32, refcon: *mut c_void, info_out: *mut u32) -> i32;
    fn VTDecompressionSessionWaitForAsynchronousFrames(session: Session) -> i32;
    fn VTDecompressionSessionInvalidate(session: Session);
    static kVTVideoDecoderSpecification_RequireHardwareAcceleratedVideoDecoder: CFStringRef;
    static kVTDecompressionPropertyKey_RealTime: CFStringRef;
    fn VTSessionSetProperty(session: Session, key: CFStringRef, value: *const c_void) -> i32;

    fn CMVideoFormatDescriptionCreateFromH264ParameterSets(
        alloc: *const c_void,
        count: usize,
        sets: *const *const u8,
        sizes: *const usize,
        nal_len: i32,
        out: *mut FormatDescription,
    ) -> i32;
    fn CMVideoFormatDescriptionCreateFromHEVCParameterSets(
        alloc: *const c_void,
        count: usize,
        sets: *const *const u8,
        sizes: *const usize,
        nal_len: i32,
        extensions: *const c_void,
        out: *mut FormatDescription,
    ) -> i32;
    fn CMBlockBufferCreateWithMemoryBlock(
        alloc: *const c_void,
        memory: *mut c_void,
        len: usize,
        block_alloc: *const c_void,
        custom: *const c_void,
        offset: usize,
        data_len: usize,
        flags: u32,
        out: *mut *mut c_void,
    ) -> i32;
    fn CMSampleBufferCreateReady(
        alloc: *const c_void,
        data: *mut c_void,
        format: FormatDescription,
        samples: isize,
        timings: isize,
        timing: *const c_void,
        sizes: isize,
        size: *const usize,
        out: *mut *mut c_void,
    ) -> i32;
    static kCFAllocatorNull: *const c_void;

    fn CVPixelBufferLockBaseAddress(buf: PixelBuffer, flags: u64) -> i32;
    fn CVPixelBufferUnlockBaseAddress(buf: PixelBuffer, flags: u64) -> i32;
    fn CVPixelBufferGetBaseAddress(buf: PixelBuffer) -> *const u8;
    fn CVPixelBufferGetBytesPerRow(buf: PixelBuffer) -> usize;
    fn CVPixelBufferGetWidth(buf: PixelBuffer) -> usize;
    fn CVPixelBufferGetHeight(buf: PixelBuffer) -> usize;
    fn CVPixelBufferRetain(buf: PixelBuffer) -> PixelBuffer;
    fn CVPixelBufferRelease(buf: PixelBuffer);
    fn CVPixelBufferGetIOSurface(buf: PixelBuffer) -> *mut c_void;
    static kCVPixelBufferPixelFormatTypeKey: CFStringRef;
    static kCVPixelBufferIOSurfacePropertiesKey: CFStringRef;
}

fn fourcc(codec: Codec) -> Option<u32> {
    match codec {
        Codec::H264 => Some(H264),
        Codec::H265 => Some(HEVC),
        _ => None,
    }
}

/// Whether this Mac decodes `codec` in hardware.
pub fn supported(codec: Codec) -> bool {
    fourcc(codec).is_some_and(|c| unsafe { VTIsHardwareDecodeSupported(c) })
}

/// What the callback hands back: the picture (retained) or the error, for the frame just given.
#[derive(Default)]
struct Out(Mutex<Option<Result<usize, i32>>>);

extern "C" fn on_frame(refcon: *mut c_void, _frame: *mut c_void, status: i32, _flags: u32, image: PixelBuffer, _pts: CMTime, _dur: CMTime) {
    let out = unsafe { &*(refcon as *const Out) };
    let result = if status != 0 || image.is_null() {
        Err(status)
    } else {
        Ok(unsafe { CVPixelBufferRetain(image) } as usize)
    };
    if let Some(Ok(old)) = out.0.lock().unwrap().replace(result) {
        unsafe { CVPixelBufferRelease(old as PixelBuffer) };
    }
}

/// A decoded picture as the decoder made it (a retained CVPixelBuffer on an IOSurface), held for as
/// long as this is.
pub struct Surface(PixelBuffer);

// A retained, immutable picture: any thread may hold or release it.
unsafe impl Send for Surface {}
unsafe impl Sync for Surface {}

impl Clone for Surface {
    fn clone(&self) -> Self {
        Surface(unsafe { CVPixelBufferRetain(self.0) })
    }
}

impl Drop for Surface {
    fn drop(&mut self) {
        unsafe { CVPixelBufferRelease(self.0) };
    }
}

impl Surface {
    /// Its IOSurface, what a layer shows (valid for as long as this is held).
    pub fn io_surface(&self) -> *mut c_void {
        unsafe { CVPixelBufferGetIOSurface(self.0) }
    }

    /// Row `y`, `w` pixels of it (read through the CPU: for measuring, a row at a time).
    pub fn row(&self, y: usize, w: usize) -> Vec<u32> {
        unsafe {
            CVPixelBufferLockBaseAddress(self.0, LOCK_READ_ONLY);
            let (h, stride, base) = (
                CVPixelBufferGetHeight(self.0),
                CVPixelBufferGetBytesPerRow(self.0),
                CVPixelBufferGetBaseAddress(self.0),
            );
            let row = if base.is_null() || y >= h {
                Vec::new()
            } else {
                let p = base.add(y * stride) as *const u32;
                (0..w.min(stride / 4)).map(|x| p.add(x).read_unaligned()).collect()
            };
            CVPixelBufferUnlockBaseAddress(self.0, LOCK_READ_ONLY);
            row
        }
    }
}

pub struct VideoToolbox {
    codec: Codec,
    session: Session,
    format: FormatDescription,
    /// The parameter sets the session was made with (a keyframe with others makes a new one).
    sets: Vec<Vec<u8>>,
    out: Box<Out>,
    /// The frame as one sample: each NAL unit with its length before it.
    sample: Vec<u8>,
    /// Pictures handed out as they are (a window shows them), not copied.
    zero_copy: bool,
}

// The session is only used by whoever holds the decoder.
unsafe impl Send for VideoToolbox {}

impl VideoToolbox {
    pub fn new(codec: Codec) -> Result<Self, String> {
        if !supported(codec) {
            return Err(format!("VideoToolbox: no {codec:?} decoder in hardware here"));
        }
        Ok(Self {
            codec,
            session: std::ptr::null_mut(),
            format: std::ptr::null_mut(),
            sets: Vec::new(),
            out: Box::default(),
            sample: Vec::new(),
            zero_copy: false,
        })
    }

    fn close(&mut self) {
        unsafe {
            if !self.session.is_null() {
                VTDecompressionSessionWaitForAsynchronousFrames(self.session);
                VTDecompressionSessionInvalidate(self.session);
                CFRelease(self.session as _);
                self.session = std::ptr::null_mut();
            }
            if !self.format.is_null() {
                CFRelease(self.format as _);
                self.format = std::ptr::null_mut();
            }
        }
    }

    /// A session for these parameter sets (H.264: SPS, PPS; HEVC: VPS, SPS, PPS).
    fn open(&mut self, sets: Vec<Vec<u8>>) -> Result<(), String> {
        self.close();
        let ptrs: Vec<*const u8> = sets.iter().map(|s| s.as_ptr()).collect();
        let sizes: Vec<usize> = sets.iter().map(Vec::len).collect();
        let mut format = std::ptr::null_mut();
        let status = unsafe {
            match self.codec {
                Codec::H265 => CMVideoFormatDescriptionCreateFromHEVCParameterSets(
                    std::ptr::null(),
                    sets.len(),
                    ptrs.as_ptr(),
                    sizes.as_ptr(),
                    4,
                    std::ptr::null(),
                    &mut format,
                ),
                _ => CMVideoFormatDescriptionCreateFromH264ParameterSets(std::ptr::null(), sets.len(), ptrs.as_ptr(), sizes.as_ptr(), 4, &mut format),
            }
        };
        if status != 0 || format.is_null() {
            return Err(format!("VideoToolbox: the stream's parameter sets aren't usable ({status})"));
        }
        self.format = format;
        unsafe {
            // The hardware's decoder or none (software decoding is openh264's job here).
            let spec = CFDictionary::from_CFType_pairs(&[(
                CFString::wrap_under_get_rule(kVTVideoDecoderSpecification_RequireHardwareAcceleratedVideoDecoder),
                CFBoolean::true_value().as_CFType(),
            )]);
            // BGRA, converted by the hardware; in IOSurfaces (GPU memory the CPU can read).
            let image = CFDictionary::from_CFType_pairs(&[
                (
                    CFString::wrap_under_get_rule(kCVPixelBufferPixelFormatTypeKey),
                    CFNumber::from(BGRA as i64).as_CFType(),
                ),
                (
                    CFString::wrap_under_get_rule(kCVPixelBufferIOSurfacePropertiesKey),
                    CFDictionary::<CFString, CFNumber>::from_CFType_pairs(&[]).as_CFType(),
                ),
            ]);
            let cb = OutputCallback {
                callback: on_frame,
                refcon: &*self.out as *const Out as *mut c_void,
            };
            let status = VTDecompressionSessionCreate(
                std::ptr::null(),
                format,
                spec.as_concrete_TypeRef() as *const c_void,
                image.as_concrete_TypeRef() as *const c_void,
                &cb,
                &mut self.session,
            );
            if status != 0 || self.session.is_null() {
                self.session = std::ptr::null_mut();
                return Err(format!("VideoToolbox: can't start a {:?} decoder ({status})", self.codec));
            }
            VTSessionSetProperty(self.session, kVTDecompressionPropertyKey_RealTime, CFBoolean::true_value().as_CFTypeRef());
        }
        self.sets = sets;
        Ok(())
    }
}

impl Drop for VideoToolbox {
    fn drop(&mut self) {
        self.close();
        if let Some(Ok(p)) = self.out.0.lock().unwrap().take() {
            unsafe { CVPixelBufferRelease(p as PixelBuffer) };
        }
    }
}

impl Decoder for VideoToolbox {
    fn name(&self) -> String {
        format!("VideoToolbox {} (hardware)", if self.codec == Codec::H265 { "HEVC" } else { "H.264" })
    }

    fn hardware(&self) -> bool {
        true
    }

    fn set_zero_copy(&mut self, on: bool) {
        self.zero_copy = on;
    }

    fn decode(&mut self, data: &[u8], _keyframe: bool, out: &mut Picture) -> Result<bool, String> {
        let hevc = self.codec == Codec::H265;
        // The parameter sets go to the format description; the rest into the sample.
        let mut sets = Vec::new();
        self.sample.clear();
        for nal in nal_units(data).filter(|n| !n.is_empty()) {
            let kind = if hevc { (nal[0] >> 1) & 0x3f } else { nal[0] & 0x1f };
            let is_set = if hevc { (32..=34).contains(&kind) } else { kind == 7 || kind == 8 };
            if is_set {
                sets.push(nal.to_vec());
            } else {
                self.sample.extend_from_slice(&(nal.len() as u32).to_be_bytes());
                self.sample.extend_from_slice(nal);
            }
        }
        if !sets.is_empty() && (sets != self.sets || self.session.is_null()) {
            self.open(sets)?;
        }
        if self.session.is_null() {
            return Err("VideoToolbox: no keyframe yet".into());
        }
        if self.sample.is_empty() {
            return Ok(false);
        }
        unsafe {
            let mut block = std::ptr::null_mut();
            // The sample's memory as it is (kCFAllocatorNull: not copied, not freed): decoding is
            // synchronous, so it's done with before this returns.
            let status = CMBlockBufferCreateWithMemoryBlock(
                std::ptr::null(),
                self.sample.as_mut_ptr().cast(),
                self.sample.len(),
                kCFAllocatorNull,
                std::ptr::null(),
                0,
                self.sample.len(),
                0,
                &mut block,
            );
            if status != 0 {
                return Err(format!("VideoToolbox: no block buffer ({status})"));
            }
            let size = self.sample.len();
            let mut sample = std::ptr::null_mut();
            let status = CMSampleBufferCreateReady(std::ptr::null(), block, self.format, 1, 0, std::ptr::null(), 1, &size, &mut sample);
            CFRelease(block as _);
            if status != 0 {
                return Err(format!("VideoToolbox: no sample buffer ({status})"));
            }
            let mut info = 0u32;
            let status = VTDecompressionSessionDecodeFrame(self.session, sample, 0, std::ptr::null_mut(), &mut info);
            VTDecompressionSessionWaitForAsynchronousFrames(self.session);
            CFRelease(sample as _);
            let got = self.out.0.lock().unwrap().take();
            if status != 0 {
                if let Some(Ok(p)) = got {
                    CVPixelBufferRelease(p as PixelBuffer);
                }
                // The session is gone (the GPU reset, the Mac slept): a new one with the next keyframe.
                if status == -12903 {
                    self.close();
                    self.sets.clear();
                }
                return Err(format!("VideoToolbox: decode failed ({status})"));
            }
            let image = match got {
                Some(Ok(p)) => p as PixelBuffer,
                Some(Err(s)) => return Err(format!("VideoToolbox: decode failed ({s})")),
                None => return Ok(false),
            };
            if self.zero_copy {
                // Handed on as it is: the surface holds the reference.
                out.w = CVPixelBufferGetWidth(image);
                out.h = CVPixelBufferGetHeight(image);
                out.px.clear();
                out.surface = Some(Surface(image));
                return Ok(true);
            }
            out.surface = None;
            CVPixelBufferLockBaseAddress(image, LOCK_READ_ONLY);
            let (w, h, stride) = (CVPixelBufferGetWidth(image), CVPixelBufferGetHeight(image), CVPixelBufferGetBytesPerRow(image));
            let base = CVPixelBufferGetBaseAddress(image);
            out.reshape(w, h);
            if !base.is_null() {
                let src = std::slice::from_raw_parts(base, stride * h);
                let dst = out.bytes_mut();
                for y in 0..h {
                    dst[y * w * 4..(y + 1) * w * 4].copy_from_slice(&src[y * stride..y * stride + w * 4]);
                }
            }
            CVPixelBufferUnlockBaseAddress(image, LOCK_READ_ONLY);
            CVPixelBufferRelease(image);
            Ok(!base.is_null())
        }
    }
}
