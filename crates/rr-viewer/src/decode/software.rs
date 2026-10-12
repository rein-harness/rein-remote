//! H.264 in software with openh264's decoder (BSD-licensed, built in: the same library the server
//! encodes with), the fallback on every platform. Each frame is decoded the moment it's given
//! (`DecodeFrameNoDelay`: no frame held back for reordering, and the server sends none to reorder).

use super::yuv::{to_bgra, Chroma, Plane};
use super::{Decoder, Picture};
use openh264_sys2::*;
use std::ptr::null_mut;

pub struct OpenH264 {
    api: DynamicAPI,
    dec: *mut ISVCDecoder,
}

// The decoder is only used by whoever holds it.
unsafe impl Send for OpenH264 {}

impl OpenH264 {
    pub fn new() -> Result<Self, String> {
        let api = DynamicAPI::from_source();
        let mut dec: *mut ISVCDecoder = null_mut();
        unsafe {
            if api.WelsCreateDecoder(&mut dec) != 0 || dec.is_null() {
                return Err("openh264: can't create a decoder".into());
            }
            let vt = &**dec;
            let mut p = SDecodingParam::default();
            p.sVideoProperty.eVideoBsType = VIDEO_BITSTREAM_AVC;
            // Every layer (a plain H.264 stream has one; 0 here would mean none).
            p.uiTargetDqLayer = u8::MAX;
            // No concealment: a frame with a lost reference is an error, so the viewer asks for a
            // keyframe, rather than showing a smeared guess until the next one.
            p.eEcActiveIdc = ERROR_CON_DISABLE;
            let mut quiet = WELS_LOG_QUIET;
            vt.SetOption.ok_or("openh264")?(dec, DECODER_OPTION_TRACE_LEVEL, &mut quiet as *mut i32 as *mut _);
            if vt.Initialize.ok_or("openh264")?(dec, &p) != 0 {
                api.WelsDestroyDecoder(dec);
                return Err("openh264: can't start the decoder".into());
            }
        }
        Ok(Self { api, dec })
    }
}

impl Drop for OpenH264 {
    fn drop(&mut self) {
        unsafe {
            if let Some(u) = (**self.dec).Uninitialize {
                u(self.dec);
            }
            self.api.WelsDestroyDecoder(self.dec);
        }
    }
}

impl Decoder for OpenH264 {
    fn name(&self) -> String {
        "openh264 H.264 (software)".into()
    }

    fn hardware(&self) -> bool {
        false
    }

    fn decode(&mut self, data: &[u8], _keyframe: bool, out: &mut Picture) -> Result<bool, String> {
        let baseline;
        let data = match as_baseline(data) {
            Some(d) => {
                baseline = d;
                &baseline[..]
            }
            None => data,
        };
        let mut planes: [*mut u8; 3] = [null_mut(); 3];
        let mut info: SBufferInfo = unsafe { std::mem::zeroed() };
        let state = unsafe { (**self.dec).DecodeFrameNoDelay.ok_or("openh264")?(self.dec, data.as_ptr(), data.len() as i32, planes.as_mut_ptr(), &mut info) };
        if state != dsErrorFree {
            return Err(format!("openh264: decoding state {state:#x}"));
        }
        if info.iBufferStatus != 1 {
            return Ok(false);
        }
        let b = unsafe { info.UsrData.sSystemBuffer };
        let (w, h) = (b.iWidth as usize, b.iHeight as usize);
        let (ys, cs) = (b.iStride[0] as usize, b.iStride[1] as usize);
        // The planes stay the decoder's until the next call: converted now.
        let (y, u, v) = unsafe {
            (
                std::slice::from_raw_parts(info.pDst[0], ys * h),
                std::slice::from_raw_parts(info.pDst[1], cs * h.div_ceil(2)),
                std::slice::from_raw_parts(info.pDst[2], cs * h.div_ceil(2)),
            )
        };
        out.reshape(w, h);
        to_bgra(
            Plane { data: y, stride: ys },
            Chroma::I420(Plane { data: u, stride: cs }, Plane { data: v, stride: cs }),
            w,
            h,
            out.bytes_mut(),
        );
        Ok(true)
    }
}

/// The stream with its SPS saying a baseline profile, when it says another (a copy; None when it
/// already does, or has no SPS).
///
/// openh264 holds back every picture of a Main or High profile stream until the next has come, in
/// case B-frames want them reordered: a frame late, at any frame rate. Hardware encoders write Main
/// or High; the server never sends B-frames, so there is nothing to reorder. openh264 reorders by
/// the profile alone, and decodes the stream's tools (CABAC, 8×8 transforms) from the PPS whatever
/// the profile says, so saying Baseline (66) for Main and Extended, whose SPS reads the same, and
/// Scalable Baseline (83, which it takes as baseline too) for the High profiles, whose SPS has the
/// same extra fields, gives each frame the moment it's decoded.
fn as_baseline(data: &[u8]) -> Option<Vec<u8>> {
    let mut out: Option<Vec<u8>> = None;
    for nal in super::nal_units(data) {
        if nal.len() < 2 || nal[0] & 0x1f != 7 {
            continue;
        }
        let to = match nal[1] {
            77 | 88 => 66,
            100 | 110 | 122 | 244 | 44 | 86 | 118 | 128 | 138 | 139 | 134 | 135 => 83,
            _ => continue,
        };
        // The profile byte's place in `data` (the unit is a slice of it).
        let at = nal.as_ptr() as usize - data.as_ptr() as usize + 1;
        out.get_or_insert_with(|| data.to_vec())[at] = to;
    }
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn main_and_high_profiles_read_as_baseline() {
        let s = [0, 0, 0, 1, 0x67, 77, 0x40, 0x1f, 0, 0, 1, 0x68, 0xee, 0, 0, 1, 0x65, 0x88];
        let b = super::as_baseline(&s).unwrap();
        assert_eq!((b[5], b.len()), (66, s.len()));
        let high = [0, 0, 1, 0x67, 100, 0, 0x28];
        assert_eq!(super::as_baseline(&high).unwrap()[4], 83);
        assert!(super::as_baseline(&[0, 0, 1, 0x67, 66, 0xc0]).is_none());
        assert!(super::as_baseline(&[0, 0, 1, 0x41, 77]).is_none());
    }
}
