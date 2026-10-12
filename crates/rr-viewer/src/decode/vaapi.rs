//! Linux: VA-API. In progress.

use super::{Decoder, Picture};
use rr_proto::Codec;

pub fn supported(_codec: Codec) -> bool {
    false
}

pub struct VaApi;

impl VaApi {
    pub fn new(_codec: Codec) -> Result<Self, String> {
        Err("VA-API: not yet".into())
    }
}

impl Decoder for VaApi {
    fn name(&self) -> String {
        String::new()
    }
    fn hardware(&self) -> bool {
        true
    }
    fn decode(&mut self, _data: &[u8], _keyframe: bool, _out: &mut Picture) -> Result<bool, String> {
        Err("VA-API: not yet".into())
    }
}
