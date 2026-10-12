//! Video decoding: each platform's hardware decoder first (VideoToolbox on macOS, Media Foundation
//! on Windows, VA-API on Linux), openh264 in software as the fallback everywhere (H.264). Every
//! decoder gives the same thing: the picture in BGRA, ready to be scaled into the window.
//!
//! The stream is the server's: Annex B (start codes), the parameter sets before each keyframe, no
//! B-frames (each frame is shown the moment it's decoded, nothing to reorder).

#[cfg(windows)]
mod mf;
mod software;
#[cfg(target_os = "linux")]
mod vaapi;
#[cfg(target_os = "macos")]
mod vt;
pub mod yuv;

use rr_proto::{Codec, CodecCap};

/// A decoded picture: `w`×`h` pixels, 0RGB in a `u32` each (BGRA in memory on every platform this
/// runs on, which is what the decoders write: see `bytes_mut`).
#[derive(Default)]
pub struct Picture {
    pub w: usize,
    pub h: usize,
    pub px: Vec<u32>,
    /// The frame's number and when it came (µs, this machine's clock), for feedback.
    pub frame: u32,
    pub received_us: u64,
    /// The test pattern's capture time, read from the picture (ms, mod 2^32), when measuring.
    pub barcode: Option<u32>,
}

impl Picture {
    /// Sized for `w`×`h` (keeping its memory when it's the same size).
    pub fn reshape(&mut self, w: usize, h: usize) {
        self.w = w;
        self.h = h;
        self.px.resize(w * h, 0);
    }

    /// The pixels as BGRA bytes, `w * 4` a row.
    pub fn bytes(&self) -> &[u8] {
        // SAFETY: a u32's memory is four valid bytes, and u8 needs no alignment.
        unsafe { std::slice::from_raw_parts(self.px.as_ptr().cast(), self.px.len() * 4) }
    }

    pub fn bytes_mut(&mut self) -> &mut [u8] {
        // SAFETY: as above; any bytes written make a valid u32.
        unsafe { std::slice::from_raw_parts_mut(self.px.as_mut_ptr().cast(), self.px.len() * 4) }
    }
}

/// Which decoders to try.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Prefer {
    /// The hardware's, else software.
    Auto,
    /// Only the hardware's (an error where there's none).
    Hardware,
    /// Only openh264.
    Software,
}

pub trait Decoder: Send {
    /// What it is, for the title bar and the report ("VideoToolbox H.264 (hardware)").
    fn name(&self) -> String;
    fn hardware(&self) -> bool;
    /// One frame in; the picture out, into `out` (true), or none yet (false). An error means the
    /// decoder lost its place: ask for a keyframe and wait for it.
    fn decode(&mut self, data: &[u8], keyframe: bool, out: &mut Picture) -> Result<bool, String>;
}

/// What this machine decodes, for the Hello (the server picks from these, hardware first).
pub fn capabilities(prefer: Prefer) -> Vec<CodecCap> {
    let mut caps = Vec::new();
    if prefer != Prefer::Software {
        for codec in [Codec::H265, Codec::H264] {
            if hardware_decodes(codec) {
                caps.push(CodecCap { codec, hardware: true });
            }
        }
    }
    if prefer != Prefer::Hardware && !caps.iter().any(|c| c.codec == Codec::H264) {
        caps.push(CodecCap {
            codec: Codec::H264,
            hardware: false,
        });
    }
    caps
}

/// Whether the hardware decodes `codec` here (asked of the system, without opening a decoder for
/// a stream).
fn hardware_decodes(codec: Codec) -> bool {
    #[cfg(target_os = "macos")]
    return vt::supported(codec);
    #[cfg(windows)]
    return mf::supported(codec);
    #[cfg(target_os = "linux")]
    return vaapi::supported(codec);
    #[allow(unreachable_code)]
    {
        let _ = codec;
        false
    }
}

/// A decoder for `codec`: the hardware's first (unless `prefer` says software), then openh264. The
/// notes say what was tried and why it wasn't used.
pub fn open(codec: Codec, w: usize, h: usize, prefer: Prefer, notes: &mut Vec<String>) -> Result<Box<dyn Decoder>, String> {
    if prefer != Prefer::Software {
        match open_hardware(codec, w, h) {
            Ok(d) => return Ok(d),
            Err(e) if prefer == Prefer::Hardware => return Err(e),
            Err(e) => notes.push(format!("no hardware decoder ({e}): software")),
        }
    }
    match codec {
        Codec::H264 => Ok(Box::new(software::OpenH264::new()?)),
        c => Err(format!("{c:?} decodes only in hardware here, and there's none")),
    }
}

fn open_hardware(codec: Codec, w: usize, h: usize) -> Result<Box<dyn Decoder>, String> {
    let _ = (w, h);
    #[cfg(target_os = "macos")]
    return Ok(Box::new(vt::VideoToolbox::new(codec)?));
    #[cfg(windows)]
    return Ok(Box::new(mf::MediaFoundation::new(codec, w, h)?));
    #[cfg(target_os = "linux")]
    return Ok(Box::new(vaapi::VaApi::new(codec)?));
    #[allow(unreachable_code)]
    Err(format!("no hardware decoder for {codec:?} on this platform"))
}

/// The NAL units of an Annex B stream: each one's bytes, without its start code.
pub fn nal_units(data: &[u8]) -> impl Iterator<Item = &[u8]> {
    let mut at = 0;
    std::iter::from_fn(move || {
        // The next start code (00 00 01, or 00 00 00 01 whose last three bytes are the same).
        let start = find_start(data, at)? + 3;
        let end = find_start(data, start).map_or(data.len(), |s| {
            // A four-byte start code's leading zero belongs to it, not to this unit.
            if s > start && data[s - 1] == 0 {
                s - 1
            } else {
                s
            }
        });
        at = end;
        Some(&data[start..end])
    })
}

fn find_start(data: &[u8], from: usize) -> Option<usize> {
    (from..data.len().saturating_sub(2)).find(|&i| data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn annex_b_splits_into_units() {
        let s = [0, 0, 0, 1, 0x67, 1, 2, 0, 0, 1, 0x68, 3, 0, 0, 0, 1, 0x65, 4, 5];
        let units: Vec<&[u8]> = nal_units(&s).collect();
        assert_eq!(units, vec![&[0x67, 1, 2][..], &[0x68, 3], &[0x65, 4, 5]]);
        assert_eq!(nal_units(&[1, 2, 3]).count(), 0);
    }
}
