//! Windows: Media Foundation's H.264 decoder (DXVA on the GPU). In progress.

use super::{Decoder, Picture};
use rr_proto::Codec;

pub fn supported(_codec: Codec) -> bool {
    false
}

pub struct MediaFoundation;

impl MediaFoundation {
    pub fn new(_codec: Codec, _w: usize, _h: usize) -> Result<Self, String> {
        Err("Media Foundation: not yet".into())
    }
}

impl Decoder for MediaFoundation {
    fn name(&self) -> String {
        String::new()
    }
    fn hardware(&self) -> bool {
        true
    }
    fn decode(&mut self, _data: &[u8], _keyframe: bool, _out: &mut Picture) -> Result<bool, String> {
        Err("Media Foundation: not yet".into())
    }
}
