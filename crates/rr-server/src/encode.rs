//! Video encoding. Phase 1: software H.264 (openh264, BSD-licensed), tuned for screens in real time:
//! no B-frames, bitrate rate control, a keyframe on request. Hardware encoders come per platform in
//! later phases behind the same `encode` call.

use openh264::encoder::{Encoder, EncoderConfig, FrameType, RateControlMode, UsageType};
use openh264::formats::{BgraSliceU8, YUVBuffer};
use openh264::OpenH264API;

pub struct H264 {
    enc: Encoder,
    width: usize,
    height: usize,
    keyframe_next: bool,
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
        })
    }

    pub fn size(&self) -> (usize, usize) {
        (self.width, self.height)
    }

    /// The next frame is a keyframe (a client joined, or lost a reference frame).
    pub fn keyframe(&mut self) {
        self.keyframe_next = true;
    }

    /// One frame of BGRA pixels (`stride` bytes a row, at least `width * 4`). `None`: the encoder
    /// skipped it (nothing changed enough to send).
    pub fn encode(&mut self, bgra: &[u8], stride: usize) -> Result<Option<Encoded>, String> {
        let packed;
        let pixels = if stride == self.width * 4 && bgra.len() >= self.width * self.height * 4 {
            &bgra[..self.width * self.height * 4]
        } else {
            // Rows padded, or the frame one pixel wider/taller than the even size: repack.
            let mut p = Vec::with_capacity(self.width * self.height * 4);
            for row in 0..self.height {
                p.extend_from_slice(&bgra[row * stride..row * stride + self.width * 4]);
            }
            packed = p;
            &packed
        };
        let yuv = YUVBuffer::from_rgb_source(BgraSliceU8::new(pixels, (self.width, self.height)));
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
