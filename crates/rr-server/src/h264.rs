//! H.264 headers written bit by bit, for the VA-API encoder: it hands drivers that take them its own
//! SPS, PPS and slice headers, so the stream says exactly what the client needs (no reordering, so a
//! decoder shows each frame the moment it's in; the colours as they were converted), whatever the
//! driver would have written.
//!
//! The stream is IPPP: one reference frame, frame_num counting from each IDR, picture order from
//! frame_num (pic_order_cnt_type 2: no B-frames, so nothing to reorder and nothing to send for it).

/// Bits, most significant first, as the standard writes them.
#[derive(Default)]
pub struct Bits {
    bytes: Vec<u8>,
    /// Bits used in the last byte (0: it's full, or there's none).
    used: u32,
}

impl Bits {
    pub fn new() -> Self {
        Self::default()
    }

    /// `n` bits of `v` (n ≤ 32), high first: the standard's u(n).
    pub fn u(&mut self, n: u32, v: u32) {
        for i in (0..n).rev() {
            if self.used == 0 {
                self.bytes.push(0);
            }
            let bit = (v >> i) & 1;
            *self.bytes.last_mut().unwrap() |= (bit as u8) << (7 - self.used);
            self.used = (self.used + 1) % 8;
        }
    }

    pub fn flag(&mut self, b: bool) {
        self.u(1, b as u32);
    }

    /// Unsigned exp-Golomb, ue(v): as many zeros as v+1 has bits after its first, then v+1.
    pub fn ue(&mut self, v: u32) {
        let v1 = v as u64 + 1;
        let len = 64 - v1.leading_zeros();
        self.u(len - 1, 0);
        // v+1 can be 33 bits (v = u32::MAX): its top bit first, then the rest.
        if len > 32 {
            self.u(1, 1);
        }
        self.u(len.min(32), v1 as u32);
    }

    /// Signed exp-Golomb, se(v): 1, −1, 2, −2… as 1, 2, 3, 4…
    pub fn se(&mut self, v: i32) {
        let k = if v > 0 { 2 * v as i64 - 1 } else { -2 * v as i64 };
        self.ue(k as u32);
    }

    /// rbsp_trailing_bits: a one, then zeros to the byte.
    pub fn trailing(&mut self) {
        self.flag(true);
        while self.used != 0 {
            self.flag(false);
        }
    }

    #[cfg(test)]
    pub fn bit_len(&self) -> usize {
        self.bytes.len() * 8 - if self.used == 0 { 0 } else { (8 - self.used) as usize }
    }
}

/// One NAL unit in Annex B: a start code, the header, and the payload with emulation prevention (a
/// 0x03 after any two zero bytes that a 0, 1, 2 or 3 follows, so the payload never looks like a start
/// code). Gives the bytes and how many bits of them count: a slice header ends mid-byte, and the
/// driver writes the slice data from that bit on.
pub fn nal(ref_idc: u8, kind: u8, rbsp: &Bits) -> (Vec<u8>, usize) {
    let mut out = vec![0, 0, 0, 1, (ref_idc & 3) << 5 | (kind & 31)];
    let mut zeros = 0;
    for &b in &rbsp.bytes {
        if zeros >= 2 && b <= 3 {
            out.push(3);
            zeros = 0;
        }
        out.push(b);
        zeros = if b == 0 { zeros + 1 } else { 0 };
    }
    let unused = if rbsp.used == 0 { 0 } else { 8 - rbsp.used as usize };
    let bits = out.len() * 8 - unused;
    (out, bits)
}

pub const NAL_SLICE: u8 = 1;
pub const NAL_IDR: u8 = 5;
pub const NAL_SPS: u8 = 7;
pub const NAL_PPS: u8 = 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Profile {
    /// CAVLC, what every decoder takes.
    ConstrainedBaseline,
    /// CABAC: the same picture in fewer bits.
    Main,
}

/// What the headers describe: the stream's size and coding tools. Everything else is fixed: one SPS
/// and one PPS (id 0), one slice a frame, one reference frame, QP 26 before rate control's change.
#[derive(Clone, Debug)]
pub struct Params {
    pub profile: Profile,
    pub level_idc: u8,
    /// The picture's size (even); the coded size is whole macroblocks, the rest cropped.
    pub width: u32,
    pub height: u32,
}

/// frame_num's size in bits: it wraps at 65536 (it only has to tell the frames since an IDR apart).
pub const LOG2_MAX_FRAME_NUM: u32 = 16;
pub const PIC_INIT_QP: i32 = 26;

impl Params {
    pub fn width_mbs(&self) -> u32 {
        self.width.div_ceil(16)
    }

    pub fn height_mbs(&self) -> u32 {
        self.height.div_ceil(16)
    }

    pub fn cabac(&self) -> bool {
        self.profile == Profile::Main
    }

    /// profile_idc and the constraint flags (byte 2 of the SPS: the client's codec string reads it).
    pub fn profile_idc(&self) -> (u8, u8) {
        match self.profile {
            // constraint_set0 and set1: baseline that a main decoder takes too.
            Profile::ConstrainedBaseline => (66, 0xc0),
            Profile::Main => (77, 0),
        }
    }

    /// Cropping in chroma units (2 pixels for 4:2:0): right and bottom.
    pub fn crop(&self) -> (u32, u32) {
        ((self.width_mbs() * 16 - self.width) / 2, (self.height_mbs() * 16 - self.height) / 2)
    }

    /// The sequence parameter set, a whole NAL unit.
    pub fn sps(&self) -> Vec<u8> {
        let mut b = Bits::new();
        let (profile_idc, constraints) = self.profile_idc();
        b.u(8, profile_idc as u32);
        b.u(8, constraints as u32);
        b.u(8, self.level_idc as u32);
        b.ue(0); // seq_parameter_set_id
        b.ue(LOG2_MAX_FRAME_NUM - 4);
        b.ue(2); // pic_order_cnt_type: output order is decode order
        b.ue(1); // max_num_ref_frames
        b.flag(false); // gaps_in_frame_num_value_allowed_flag
        b.ue(self.width_mbs() - 1);
        b.ue(self.height_mbs() - 1); // pic_height_in_map_units_minus1 (frames only)
        b.flag(true); // frame_mbs_only_flag
        b.flag(true); // direct_8x8_inference_flag
        let (right, bottom) = self.crop();
        b.flag(right != 0 || bottom != 0);
        if right != 0 || bottom != 0 {
            b.ue(0);
            b.ue(right);
            b.ue(0);
            b.ue(bottom);
        }
        b.flag(true); // vui_parameters_present_flag
        b.flag(false); // aspect_ratio_info_present_flag
        b.flag(false); // overscan_info_present_flag
                       // The colours as converted: limited range, BT.601's matrix (as yuv.rs and the GPU's
                       // conversion do it), from sRGB (BT.709's primaries, sRGB's transfer): a screen's.
        b.flag(true); // video_signal_type_present_flag
        b.u(3, 5); // video_format: unspecified
        b.flag(false); // video_full_range_flag
        b.flag(true); // colour_description_present_flag
        b.u(8, 1); // colour_primaries: BT.709
        b.u(8, 13); // transfer_characteristics: sRGB
        b.u(8, 6); // matrix_coefficients: BT.601
        b.flag(false); // chroma_loc_info_present_flag
        b.flag(false); // timing_info_present_flag
        b.flag(false); // nal_hrd_parameters_present_flag
        b.flag(false); // vcl_hrd_parameters_present_flag
        b.flag(false); // pic_struct_present_flag
                       // No frame waits for a later one: a decoder that reads this shows each as it's decoded.
        b.flag(true); // bitstream_restriction_flag
        b.flag(true); // motion_vectors_over_pic_boundaries_flag
        b.ue(2); // max_bytes_per_pic_denom (the default)
        b.ue(1); // max_bits_per_mb_denom (the default)
        b.ue(15); // log2_max_mv_length_horizontal
        b.ue(15); // log2_max_mv_length_vertical
        b.ue(0); // max_num_reorder_frames
        b.ue(1); // max_dec_frame_buffering
        b.trailing();
        nal(3, NAL_SPS, &b).0
    }

    /// The picture parameter set, a whole NAL unit.
    pub fn pps(&self) -> Vec<u8> {
        let mut b = Bits::new();
        b.ue(0); // pic_parameter_set_id
        b.ue(0); // seq_parameter_set_id
        b.flag(self.cabac()); // entropy_coding_mode_flag
        b.flag(false); // bottom_field_pic_order_in_frame_present_flag
        b.ue(0); // num_slice_groups_minus1
        b.ue(0); // num_ref_idx_l0_default_active_minus1
        b.ue(0); // num_ref_idx_l1_default_active_minus1
        b.flag(false); // weighted_pred_flag
        b.u(2, 0); // weighted_bipred_idc
        b.se(PIC_INIT_QP - 26); // pic_init_qp_minus26
        b.se(0); // pic_init_qs_minus26
        b.se(0); // chroma_qp_index_offset
        b.flag(true); // deblocking_filter_control_present_flag
        b.flag(false); // constrained_intra_pred_flag
        b.flag(false); // redundant_pic_cnt_present_flag
        b.trailing();
        nal(3, NAL_PPS, &b).0
    }

    /// A slice header (the whole frame is one slice), as a NAL unit that stops where the slice data
    /// starts: the bytes and how many bits of them count. A keyframe is an IDR (frame_num 0); the
    /// others are P-frames that refer to the frame before.
    pub fn slice_header(&self, idr: bool, frame_num: u32, idr_pic_id: u32) -> (Vec<u8>, usize) {
        let b = self.slice_header_bits(idr, frame_num, idr_pic_id);
        nal(if idr { 3 } else { 2 }, if idr { NAL_IDR } else { NAL_SLICE }, &b)
    }

    fn slice_header_bits(&self, idr: bool, frame_num: u32, idr_pic_id: u32) -> Bits {
        let mut b = Bits::new();
        b.ue(0); // first_mb_in_slice
        b.ue(if idr { SLICE_I } else { SLICE_P } as u32);
        b.ue(0); // pic_parameter_set_id
        b.u(LOG2_MAX_FRAME_NUM, frame_num & ((1 << LOG2_MAX_FRAME_NUM) - 1));
        if idr {
            b.ue(idr_pic_id);
        }
        // (pic_order_cnt_type 2: no picture order fields.)
        if !idr {
            b.flag(false); // num_ref_idx_active_override_flag: the PPS's one reference
            b.flag(false); // ref_pic_list_modification_flag_l0
        }
        // dec_ref_pic_marking (every frame is a reference)
        if idr {
            b.flag(false); // no_output_of_prior_pics_flag
            b.flag(false); // long_term_reference_flag
        } else {
            b.flag(false); // adaptive_ref_pic_marking_mode_flag: sliding window
        }
        if self.cabac() && !idr {
            b.ue(0); // cabac_init_idc
        }
        b.se(0); // slice_qp_delta (rate control sets the QP)
        b.ue(0); // disable_deblocking_filter_idc: deblocking on
        b.se(0); // slice_alpha_c0_offset_div2
        b.se(0); // slice_beta_offset_div2
        b
    }
}

/// slice_type as VA-API and the slice header give it.
pub const SLICE_P: u8 = 0;
pub const SLICE_I: u8 = 2;

/// The lowest level that holds this size and rate (frames a second, macroblocks a second, the bitrate),
/// from the standard's table A-1: a decoder sizes itself from it.
pub fn level(width: u32, height: u32, fps: f32, kbps: u32) -> u8 {
    let mbs = width.div_ceil(16) as u64 * height.div_ceil(16) as u64;
    let mbps = (mbs as f64 * fps.max(1.0) as f64).ceil() as u64;
    // (level_idc, max frame size in MBs, max MBs a second, max bitrate in kbit/s for Main)
    const LEVELS: [(u8, u64, u64, u64); 19] = [
        (10, 99, 1_485, 64),
        (11, 396, 3_000, 192),
        (12, 396, 6_000, 384),
        (13, 396, 11_880, 768),
        (20, 396, 11_880, 2_000),
        (21, 792, 19_800, 4_000),
        (22, 1_620, 20_250, 4_000),
        (30, 1_620, 40_500, 10_000),
        (31, 3_600, 108_000, 14_000),
        (32, 5_120, 216_000, 20_000),
        (40, 8_192, 245_760, 20_000),
        (41, 8_192, 245_760, 50_000),
        (42, 8_704, 522_240, 50_000),
        (50, 22_080, 589_824, 135_000),
        (51, 36_864, 983_040, 240_000),
        (52, 36_864, 2_073_600, 240_000),
        (60, 139_264, 4_177_920, 240_000),
        (61, 139_264, 8_355_840, 480_000),
        (62, 139_264, 16_711_680, 800_000),
    ];
    LEVELS
        .iter()
        .find(|&&(_, fs, rate, br)| mbs <= fs && mbps <= rate && kbps as u64 <= br)
        .map_or(62, |l| l.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Reads bits back, as a decoder does (the emulation prevention bytes taken out first).
    struct Reader {
        bytes: Vec<u8>,
        at: usize,
    }

    impl Reader {
        /// A NAL unit from `nal()`: past the start code and the header byte.
        fn of_nal(n: &[u8]) -> (u8, u8, Self) {
            assert_eq!(&n[..4], &[0, 0, 0, 1]);
            let mut bytes = Vec::new();
            let mut zeros = 0;
            for &b in &n[5..] {
                if zeros >= 2 && b == 3 {
                    zeros = 0;
                    continue;
                }
                bytes.push(b);
                zeros = if b == 0 { zeros + 1 } else { 0 };
            }
            ((n[4] >> 5) & 3, n[4] & 31, Self { bytes, at: 0 })
        }
        fn u(&mut self, n: u32) -> u32 {
            let mut v = 0;
            for _ in 0..n {
                let bit = (self.bytes[self.at / 8] >> (7 - self.at % 8)) & 1;
                v = (v << 1) | bit as u32;
                self.at += 1;
            }
            v
        }
        fn flag(&mut self) -> bool {
            self.u(1) == 1
        }
        fn ue(&mut self) -> u32 {
            let mut zeros = 0;
            while !self.flag() {
                zeros += 1;
            }
            ((1u64 << zeros) - 1 + self.u(zeros) as u64) as u32
        }
        fn se(&mut self) -> i32 {
            let k = self.ue() as i64;
            (if k % 2 == 1 { (k + 1) / 2 } else { -k / 2 }) as i32
        }
        /// rbsp_trailing_bits, and nothing after.
        fn trailing(&mut self) {
            assert!(self.flag(), "the stop bit");
            while self.at % 8 != 0 {
                assert!(!self.flag(), "zeros to the byte");
            }
            assert_eq!(self.at, self.bytes.len() * 8, "nothing after the trailing bits");
        }
    }

    fn bits_of(f: impl FnOnce(&mut Bits)) -> String {
        let mut b = Bits::new();
        f(&mut b);
        let s: String = b.bytes.iter().map(|x| format!("{x:08b}")).collect();
        s[..b.bit_len()].to_string()
    }

    #[test]
    fn exp_golomb_codes_are_the_standards() {
        // Table 9-2 of the standard.
        for (v, code) in [
            (0, "1"),
            (1, "010"),
            (2, "011"),
            (3, "00100"),
            (4, "00101"),
            (6, "00111"),
            (7, "0001000"),
            (14, "0001111"),
        ] {
            assert_eq!(bits_of(|b| b.ue(v)), code, "ue({v})");
        }
        // 9.1.1: 1 → 1, −1 → 2, 2 → 3, −2 → 4.
        for (v, code) in [(0, "1"), (1, "010"), (-1, "011"), (2, "00100"), (-2, "00101"), (3, "00110")] {
            assert_eq!(bits_of(|b| b.se(v)), code, "se({v})");
        }
        // The largest: 32 zeros, then 33 bits of 2^32.
        let max = bits_of(|b| b.ue(u32::MAX));
        assert_eq!(max.len(), 65);
        assert_eq!(&max[..33], &format!("{:033b}", 1));
        assert!(max[33..].chars().all(|c| c == '0'));
        let mut r = {
            let mut b = Bits::new();
            for v in [0, 1, 2, 3, 100, 65535, 1 << 20, u32::MAX - 1] {
                b.ue(v);
            }
            for v in [0, 1, -1, 26, -26, 1000, -1000] {
                b.se(v);
            }
            b.trailing();
            Reader { bytes: b.bytes, at: 0 }
        };
        for v in [0, 1, 2, 3, 100, 65535, 1 << 20, u32::MAX - 1] {
            assert_eq!(r.ue(), v);
        }
        for v in [0, 1, -1, 26, -26, 1000, -1000] {
            assert_eq!(r.se(), v);
        }
        r.trailing();
    }

    #[test]
    fn fixed_width_bits_and_trailing_bits() {
        assert_eq!(bits_of(|b| b.u(3, 5)), "101");
        assert_eq!(bits_of(|b| b.u(16, 0xabcd)), "1010101111001101");
        assert_eq!(bits_of(|b| b.u(0, 1)), "");
        assert_eq!(bits_of(|b| b.trailing()), "10000000");
        assert_eq!(
            bits_of(|b| {
                b.u(3, 7);
                b.trailing()
            }),
            "11110000"
        );
        // Already at a byte boundary: a whole byte of trailing bits.
        assert_eq!(
            bits_of(|b| {
                b.u(8, 0);
                b.trailing()
            }),
            "0000000010000000"
        );
    }

    #[test]
    fn emulation_prevention_bytes_go_after_two_zeros() {
        let payload = |bytes: &[u8]| {
            let mut b = Bits::new();
            for &x in bytes {
                b.u(8, x as u32);
            }
            nal(3, NAL_SPS, &b).0[5..].to_vec()
        };
        assert_eq!(payload(&[0, 0, 0]), [0, 0, 3, 0]);
        assert_eq!(payload(&[0, 0, 1]), [0, 0, 3, 1]);
        assert_eq!(payload(&[0, 0, 2]), [0, 0, 3, 2]);
        assert_eq!(payload(&[0, 0, 3]), [0, 0, 3, 3]);
        assert_eq!(payload(&[0, 0, 4]), [0, 0, 4]);
        assert_eq!(payload(&[0, 1, 0, 0]), [0, 1, 0, 0]);
        // After an inserted byte the count starts again: 00 00 03 00 00 03 00.
        assert_eq!(payload(&[0, 0, 0, 0, 0]), [0, 0, 3, 0, 0, 3, 0]);
        // The header byte: forbidden bit 0, nal_ref_idc, nal_unit_type.
        assert_eq!(nal(3, NAL_SPS, &Bits::new()).0, [0, 0, 0, 1, 0x67]);
        assert_eq!(nal(2, NAL_SLICE, &Bits::new()).0, [0, 0, 0, 1, 0x41]);
        assert_eq!(nal(3, NAL_IDR, &Bits::new()).0, [0, 0, 0, 1, 0x65]);
    }

    #[test]
    fn bit_length_stops_where_the_slice_data_starts() {
        let mut b = Bits::new();
        b.u(3, 0b101);
        let (bytes, bits) = nal(2, NAL_SLICE, &b);
        assert_eq!(bytes.len(), 6);
        assert_eq!(bits, 5 * 8 + 3);
        // An emulation prevention byte counts: 00 00 then a 0 bit pads into a third zero byte.
        let mut b = Bits::new();
        b.u(16, 0);
        b.u(1, 0);
        let (bytes, bits) = nal(2, NAL_SLICE, &b);
        assert_eq!(&bytes[5..], &[0, 0, 3, 0]);
        assert_eq!(bits, (5 + 3) * 8 + 1);
    }

    fn params(profile: Profile, width: u32, height: u32) -> Params {
        Params {
            profile,
            level_idc: level(width, height, 60.0, 8000),
            width,
            height,
        }
    }

    /// The SPS read back field by field, as section 7.3.2.1 orders them.
    fn read_sps(p: &Params) {
        let sps = p.sps();
        let (ref_idc, kind, mut r) = Reader::of_nal(&sps);
        assert_eq!((ref_idc, kind), (3, NAL_SPS));
        let (profile_idc, constraints) = p.profile_idc();
        assert_eq!(r.u(8), profile_idc as u32);
        assert_eq!(r.u(8), constraints as u32);
        assert_eq!(r.u(8), p.level_idc as u32);
        assert_eq!(r.ue(), 0, "seq_parameter_set_id");
        assert_eq!(r.ue() + 4, LOG2_MAX_FRAME_NUM);
        assert_eq!(r.ue(), 2, "pic_order_cnt_type");
        assert_eq!(r.ue(), 1, "max_num_ref_frames");
        assert!(!r.flag(), "gaps_in_frame_num_value_allowed_flag");
        assert_eq!(r.ue() + 1, p.width.div_ceil(16));
        assert_eq!(r.ue() + 1, p.height.div_ceil(16));
        assert!(r.flag(), "frame_mbs_only_flag");
        assert!(r.flag(), "direct_8x8_inference_flag");
        let cropped = r.flag();
        let (mut w, mut h) = (p.width.div_ceil(16) * 16, p.height.div_ceil(16) * 16);
        assert_eq!(cropped, (w, h) != (p.width, p.height));
        if cropped {
            let (left, right, top, bottom) = (r.ue(), r.ue(), r.ue(), r.ue());
            w -= 2 * (left + right);
            h -= 2 * (top + bottom);
        }
        assert_eq!((w, h), (p.width, p.height), "the picture after cropping");
        assert!(r.flag(), "vui_parameters_present_flag");
        assert!(!r.flag() && !r.flag(), "aspect ratio, overscan");
        assert!(r.flag(), "video_signal_type_present_flag");
        assert_eq!(r.u(3), 5);
        assert!(!r.flag(), "limited range");
        assert!(r.flag(), "colour_description_present_flag");
        assert_eq!((r.u(8), r.u(8), r.u(8)), (1, 13, 6));
        assert!(
            !r.flag() && !r.flag() && !r.flag() && !r.flag() && !r.flag(),
            "chroma loc, timing, HRD, pic_struct"
        );
        assert!(r.flag(), "bitstream_restriction_flag");
        assert!(r.flag());
        assert_eq!((r.ue(), r.ue(), r.ue(), r.ue()), (2, 1, 15, 15));
        assert_eq!(r.ue(), 0, "max_num_reorder_frames");
        assert_eq!(r.ue(), 1, "max_dec_frame_buffering");
        r.trailing();
    }

    fn read_pps(p: &Params) {
        let (ref_idc, kind, mut r) = Reader::of_nal(&p.pps());
        assert_eq!((ref_idc, kind), (3, NAL_PPS));
        assert_eq!((r.ue(), r.ue()), (0, 0), "pps and sps ids");
        assert_eq!(r.flag(), p.cabac(), "entropy_coding_mode_flag");
        assert!(!r.flag());
        assert_eq!((r.ue(), r.ue(), r.ue()), (0, 0, 0), "slice groups, default reference counts");
        assert!(!r.flag());
        assert_eq!(r.u(2), 0);
        assert_eq!(r.se() + 26, PIC_INIT_QP);
        assert_eq!((r.se(), r.se()), (0, 0));
        assert!(r.flag(), "deblocking_filter_control_present_flag");
        assert!(!r.flag() && !r.flag());
        r.trailing();
    }

    /// A slice header read back (7.3.3), with what the reader stops at: the bit the data starts.
    fn read_slice_header(p: &Params, idr: bool, frame_num: u32, idr_pic_id: u32) {
        let (bytes, bits) = p.slice_header(idr, frame_num, idr_pic_id);
        let (ref_idc, kind, mut r) = Reader::of_nal(&bytes);
        assert_eq!(kind, if idr { NAL_IDR } else { NAL_SLICE });
        assert!(ref_idc > 0, "every frame is a reference");
        assert_eq!(r.ue(), 0, "first_mb_in_slice");
        assert_eq!(r.ue(), if idr { 2 } else { 0 }, "slice_type");
        assert_eq!(r.ue(), 0, "pic_parameter_set_id");
        assert_eq!(r.u(LOG2_MAX_FRAME_NUM), frame_num % (1 << LOG2_MAX_FRAME_NUM));
        if idr {
            assert_eq!(r.ue(), idr_pic_id);
        } else {
            assert!(!r.flag(), "num_ref_idx_active_override_flag");
            assert!(!r.flag(), "ref_pic_list_modification_flag_l0");
        }
        if idr {
            assert!(!r.flag() && !r.flag());
        } else {
            assert!(!r.flag(), "adaptive_ref_pic_marking_mode_flag");
        }
        if p.cabac() && !idr {
            assert_eq!(r.ue(), 0, "cabac_init_idc");
        }
        assert_eq!(r.se(), 0, "slice_qp_delta");
        assert_eq!(r.ue(), 0, "disable_deblocking_filter_idc");
        assert_eq!((r.se(), r.se()), (0, 0));
        // Exactly where the bit length says (no emulation bytes in these small headers).
        assert_eq!(r.at, bits - 5 * 8);
        assert!(bytes.len() * 8 - bits < 8);
    }

    #[test]
    fn headers_read_back_as_written() {
        for profile in [Profile::ConstrainedBaseline, Profile::Main] {
            for (w, h) in [(1920, 1080), (1280, 720), (256, 144), (2560, 1600), (1366, 768), (3840, 2160), (72, 40)] {
                let p = params(profile, w, h);
                read_sps(&p);
                read_pps(&p);
                read_slice_header(&p, true, 0, 0);
                read_slice_header(&p, true, 0, 1);
                read_slice_header(&p, false, 1, 0);
                read_slice_header(&p, false, 65535, 0);
                read_slice_header(&p, false, 65536 + 7, 0);
            }
        }
    }

    #[test]
    fn known_sps_bytes() {
        // 1920×1080 Main, level 4.2 (60 fps): 120×68 macroblocks, 8 rows cropped.
        let p = params(Profile::Main, 1920, 1080);
        assert_eq!(p.level_idc, 42);
        assert_eq!(p.crop(), (0, 4));
        let sps = p.sps();
        // Start code, header, profile 77, constraints 0, level 42; then by hand from the fields:
        // ue(0) 1, ue(12) 0001101, ue(2) 011, ue(1) 010, 0, ue(119) 0000001111000, ...
        assert_eq!(&sps[..8], &[0, 0, 0, 1, 0x67, 77, 0, 42]);
        assert_eq!(sps[8], 0b1000_1101);
        assert_eq!(sps[9], 0b0110_1000);
        // Baseline: profile 66, constraint_set0 and set1.
        let b = params(Profile::ConstrainedBaseline, 1280, 720).sps();
        assert_eq!(&b[4..8], &[0x67, 66, 0xc0, 32]);
    }

    #[test]
    fn levels_from_size_rate_and_bitrate() {
        assert_eq!(level(1280, 720, 30.0, 8000), 31);
        assert_eq!(level(1280, 720, 60.0, 8000), 32);
        assert_eq!(level(1920, 1080, 30.0, 8000), 40);
        assert_eq!(level(1920, 1080, 60.0, 8000), 42);
        assert_eq!(level(1920, 1080, 120.0, 8000), 51);
        assert_eq!(level(1920, 1080, 240.0, 8000), 52);
        assert_eq!(level(3840, 2160, 60.0, 30000), 52);
        // 1.2 by size and rate, but 500 kbit/s is over its 384.
        assert_eq!(level(256, 144, 30.0, 500), 13);
        // The bitrate can need a higher level than the size does.
        assert_eq!(level(1280, 720, 30.0, 30000), 41);
        assert_eq!(level(7680, 4320, 240.0, 900_000), 62);
    }

    /// A whole stream from these headers that any decoder can play, without an encoder: an IDR of
    /// I_PCM macroblocks (raw samples) and a P-frame of skipped ones. Each header field the decoder
    /// misreads shows as an error or a wrong picture. With RR_H264_OUT=path it writes the stream there
    /// and the pixels it should decode to at path.yuv (CI decodes it with ffmpeg and compares).
    #[test]
    fn a_stream_from_these_headers_decodes() {
        let p = params(Profile::ConstrainedBaseline, 72, 40);
        let (wm, hm) = (p.width_mbs() as usize, p.height_mbs() as usize);
        let (cw, ch) = (wm * 16, hm * 16);
        // A picture worth checking: gradients, and zeros (emulation prevention in the samples).
        let luma = |x: usize, y: usize| if x < 8 { 0 } else { ((x * 3 + y * 5) % 256) as u8 };
        let cb = |x: usize, y: usize| ((x * 7 + 64 + y) % 256) as u8;
        let cr = |x: usize, y: usize| ((200 + y * 9 - x) % 256) as u8;
        let mut stream = [p.sps(), p.pps()].concat();
        // The IDR: its header, then its data (CAVLC): each macroblock mb_type I_PCM (25), zeros to
        // the byte, its 256 luma samples and 64 of each chroma.
        let mut idr = p.slice_header_bits(true, 0, 0);
        for my in 0..hm {
            for mx in 0..wm {
                idr.ue(25);
                while idr.used != 0 {
                    idr.flag(false); // pcm_alignment_zero_bit
                }
                for y in 0..16 {
                    for x in 0..16 {
                        idr.u(8, luma(mx * 16 + x, my * 16 + y) as u32);
                    }
                }
                for plane in [cb, cr] {
                    for y in 0..8 {
                        for x in 0..8 {
                            idr.u(8, plane(mx * 8 + x, my * 8 + y) as u32);
                        }
                    }
                }
            }
        }
        idr.trailing();
        stream.extend(nal(3, NAL_IDR, &idr).0);
        // The P-frame: every macroblock skipped (a copy of the IDR).
        let mut pf = p.slice_header_bits(false, 1, 0);
        pf.ue((wm * hm) as u32); // mb_skip_run
        pf.trailing();
        stream.extend(nal(2, NAL_SLICE, &pf).0);
        // What it decodes to (I420, cropped), twice.
        let mut yuv = Vec::new();
        for y in 0..p.height as usize {
            yuv.extend((0..p.width as usize).map(|x| luma(x, y)));
        }
        for plane in [cb, cr] {
            for y in 0..p.height as usize / 2 {
                yuv.extend((0..p.width as usize / 2).map(|x| plane(x, y)));
            }
        }
        let yuv = [yuv.clone(), yuv].concat();
        assert!(cw >= p.width as usize && ch >= p.height as usize);
        // The emulation prevention did its work: no start code inside a NAL unit.
        let starts = stream.windows(3).filter(|w| w == &[0, 0, 1]).count();
        assert_eq!(starts, 4, "SPS, PPS, IDR, P: one start code each");
        if let Ok(path) = std::env::var("RR_H264_OUT") {
            std::fs::write(&path, &stream).unwrap();
            std::fs::write(format!("{path}.yuv"), &yuv).unwrap();
            // Main's headers too (CABAC: no raw macroblocks to fill it with by hand), for a parser to read
            // back (ffmpeg's trace_headers): each slice header followed by a few bytes of data.
            let m = params(Profile::Main, 1920, 1080);
            let slice = |idr, n| {
                let mut b = m.slice_header_bits(idr, n, 0);
                b.trailing();
                b.u(32, 0x5a5a_5a5a);
                nal(if idr { 3 } else { 2 }, if idr { NAL_IDR } else { NAL_SLICE }, &b).0
            };
            std::fs::write(format!("{path}.main"), [m.sps(), m.pps(), slice(true, 0), slice(false, 1)].concat()).unwrap();
        }
    }
}
