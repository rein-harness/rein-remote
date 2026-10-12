//! Opus (libopus, BSD, built from source into the binary by `opusic-sys`): 48 kHz stereo in 10 ms
//! packets, in the low-delay mode (no speech coder, 2.5 ms of look-ahead instead of 6.5).

use super::{FRAME, MAX_PACKET, RATE};
use opusic_sys as ffi;

pub struct Encoder(*mut ffi::OpusEncoder);

// Used by one thread at a time (the audio thread owns it).
unsafe impl Send for Encoder {}

/// Bitrate: transparent for most sound at 10 ms frames, ~1% of a typical video stream.
pub const BITRATE: i32 = 96_000;

impl Encoder {
    pub fn new() -> Result<Self, String> {
        let mut err = 0;
        let st = unsafe { ffi::opus_encoder_create(RATE as i32, 2, ffi::OPUS_APPLICATION_RESTRICTED_LOWDELAY, &mut err) };
        if st.is_null() || err != ffi::OPUS_OK {
            return Err(format!("opus_encoder_create: {err}"));
        }
        unsafe {
            ffi::opus_encoder_ctl(st, ffi::OPUS_SET_BITRATE_REQUEST, BITRATE);
            // Constant quality: VBR (packets a little bigger when the sound is busier).
            ffi::opus_encoder_ctl(st, ffi::OPUS_SET_VBR_REQUEST, 1);
            // Off: in-band FEC is the speech coder's (not in this mode), and DTX would leave gaps the
            // client couldn't tell from loss.
            ffi::opus_encoder_ctl(st, ffi::OPUS_SET_INBAND_FEC_REQUEST, 0);
            ffi::opus_encoder_ctl(st, ffi::OPUS_SET_DTX_REQUEST, 0);
            ffi::opus_encoder_ctl(st, ffi::OPUS_SET_COMPLEXITY_REQUEST, 8);
        }
        Ok(Self(st))
    }

    /// One packet from `FRAME` samples per channel, interleaved.
    pub fn encode(&mut self, pcm: &[f32]) -> Result<Vec<u8>, String> {
        assert_eq!(pcm.len(), FRAME * 2);
        let mut out = vec![0u8; MAX_PACKET];
        let n = unsafe { ffi::opus_encode_float(self.0, pcm.as_ptr(), FRAME as i32, out.as_mut_ptr(), out.len() as i32) };
        if n < 0 {
            return Err(format!("opus_encode_float: {n}"));
        }
        out.truncate(n as usize);
        Ok(out)
    }
}

impl Drop for Encoder {
    fn drop(&mut self) {
        unsafe { ffi::opus_encoder_destroy(self.0) }
    }
}

/// For tests: packets back into PCM, as a client would.
#[cfg(test)]
pub struct Decoder(*mut ffi::OpusDecoder);

#[cfg(test)]
impl Decoder {
    pub fn new() -> Self {
        let mut err = 0;
        let st = unsafe { ffi::opus_decoder_create(RATE as i32, 2, &mut err) };
        assert!(!st.is_null() && err == ffi::OPUS_OK);
        Self(st)
    }
    pub fn decode(&mut self, packet: &[u8]) -> Vec<f32> {
        let mut out = vec![0f32; 5760 * 2];
        let n = unsafe { ffi::opus_decode_float(self.0, packet.as_ptr(), packet.len() as i32, out.as_mut_ptr(), 5760, 0) };
        assert!(n > 0, "opus_decode_float: {n}");
        out.truncate(n as usize * 2);
        out
    }
}

#[cfg(test)]
impl Drop for Decoder {
    fn drop(&mut self) {
        unsafe { ffi::opus_decoder_destroy(self.0) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tone_survives_opus() {
        // 1 kHz on the left, 440 Hz on the right, 200 ms in 10 ms packets.
        let mut enc = Encoder::new().unwrap();
        let mut dec = Decoder::new();
        let tone = |i: usize, f: f32| 0.5 * (i as f32 * 2.0 * std::f32::consts::PI * f / RATE as f32).sin();
        let (mut sent, mut back, mut bytes) = (vec![], vec![], 0);
        for p in 0..20 {
            let pcm: Vec<f32> = (0..FRAME).flat_map(|i| [tone(p * FRAME + i, 1000.0), tone(p * FRAME + i, 440.0)]).collect();
            let packet = enc.encode(&pcm).unwrap();
            bytes += packet.len();
            back.extend(dec.decode(&packet));
            sent.extend(pcm);
        }
        // ~96 kbit/s.
        let kbps = bytes as f32 * 8.0 / 0.2 / 1000.0;
        assert!((60.0..140.0).contains(&kbps), "{kbps} kbit/s");
        // The decoder's output lags by the codec's delay (2.5 ms in this mode, 120 samples): line them
        // up and compare after the first packets settle.
        let lag = (0..240)
            .min_by(|&a, &b| {
                let err = |l: usize| (4000..12000).map(|i| (back[i + l * 2] - sent[i]).powi(2)).sum::<f32>();
                err(a).partial_cmp(&err(b)).unwrap()
            })
            .unwrap();
        assert!(lag <= 200, "lag {lag}");
        let (mut sig, mut noise) = (0f32, 0f32);
        for i in 2000..(back.len() / 2 - lag).min(sent.len() / 2) {
            for c in 0..2 {
                sig += sent[i * 2 + c].powi(2);
                noise += (back[(i + lag) * 2 + c] - sent[i * 2 + c]).powi(2);
            }
        }
        let snr = 10.0 * (sig / noise).log10();
        assert!(snr > 15.0, "SNR {snr} dB");
    }
}
