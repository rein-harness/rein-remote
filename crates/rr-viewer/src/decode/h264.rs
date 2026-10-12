//! H.264 headers read, for a decoder that's given them parsed (VA-API): the SPS, the PPS, each
//! slice's header, picture order counts, and the reference pictures kept from frame to frame (the
//! decoded picture buffer's marking, with long-term references, which openh264's screen mode uses).
//! Frames only (no fields, no interlacing) and P and I slices only: what every Rein Remote server
//! sends (no B-frames, nothing reordered). Numbers in comments are the standard's sections.

// Only VA-API (Linux) uses it; elsewhere it's built for its tests.
#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

use std::collections::HashMap;

/// A NAL unit's payload as the standard reads it (RBSP: the 0x03 that keeps a start code from
/// appearing taken out), with where those bytes were, so a position can be given in either.
pub struct Rbsp {
    pub data: Vec<u8>,
    /// For each byte taken out, the RBSP byte index it came before.
    pub removed: Vec<usize>,
}

impl Rbsp {
    pub fn new(nal: &[u8]) -> Self {
        let (mut data, mut removed) = (Vec::with_capacity(nal.len()), Vec::new());
        let mut zeros = 0;
        for &b in nal {
            if zeros >= 2 && b == 3 {
                removed.push(data.len());
                zeros = 0;
                continue;
            }
            zeros = if b == 0 { zeros + 1 } else { 0 };
            data.push(b);
        }
        Self { data, removed }
    }

    /// Bytes taken out before RBSP bit `bits` (to give a position in the NAL as it was sent).
    pub fn removed_before(&self, bits: usize) -> usize {
        self.removed.iter().filter(|&&at| at * 8 < bits).count()
    }
}

/// Bits, most significant first.
pub struct Bits<'a> {
    d: &'a [u8],
    pos: usize,
}

impl<'a> Bits<'a> {
    pub fn new(d: &'a [u8]) -> Self {
        Self { d, pos: 0 }
    }

    /// Bits read so far.
    pub fn pos(&self) -> usize {
        self.pos
    }

    fn bit(&mut self) -> Result<u32, String> {
        let byte = *self.d.get(self.pos / 8).ok_or("H.264: a header ends early")?;
        let b = (byte >> (7 - self.pos % 8)) & 1;
        self.pos += 1;
        Ok(b as u32)
    }

    pub fn u(&mut self, n: u32) -> Result<u32, String> {
        (0..n).try_fold(0u32, |v, _| Ok(v << 1 | self.bit()?))
    }

    pub fn flag(&mut self) -> Result<bool, String> {
        Ok(self.bit()? == 1)
    }

    /// ue(v): exp-Golomb.
    pub fn ue(&mut self) -> Result<u32, String> {
        let mut zeros = 0;
        while self.bit()? == 0 {
            zeros += 1;
            if zeros > 31 {
                return Err("H.264: a number too long".into());
            }
        }
        Ok(((1u64 << zeros) - 1 + self.u(zeros)? as u64) as u32)
    }

    /// se(v): 1, −1, 2, −2…
    pub fn se(&mut self) -> Result<i32, String> {
        let k = self.ue()? as i64;
        Ok(if k % 2 == 1 { (k + 1) / 2 } else { -(k / 2) } as i32)
    }

    /// Whether there's more than the trailing bits left (7.2: more_rbsp_data).
    pub fn more_data(&self) -> bool {
        let Some(last) = self.d.iter().rposition(|&b| b != 0) else { return false };
        let stop = last * 8 + 7 - self.d[last].trailing_zeros() as usize;
        self.pos < stop
    }
}

/// Scaling lists, as a decoder takes them: in raster order (not the zig-zag they're sent in).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scaling {
    pub l4: [[u8; 16]; 6],
    pub l8: [[u8; 64]; 6],
}

impl Scaling {
    pub const FLAT: Scaling = Scaling {
        l4: [[16; 16]; 6],
        l8: [[16; 64]; 6],
    };
}

const ZIGZAG4: [usize; 16] = [0, 1, 4, 8, 5, 2, 3, 6, 9, 12, 13, 10, 7, 11, 14, 15];
const ZIGZAG8: [usize; 64] = [
    0, 1, 8, 16, 9, 2, 3, 10, 17, 24, 32, 25, 18, 11, 4, 5, 12, 19, 26, 33, 40, 48, 41, 34, 27, 20, 13, 6, 7, 14, 21, 28, 35, 42, 49, 56, 57, 50, 43, 36, 29,
    22, 15, 23, 30, 37, 44, 51, 58, 59, 52, 45, 38, 31, 39, 46, 53, 60, 61, 54, 47, 55, 62, 63,
];
// Table 7-3 and 7-4, in zig-zag order.
const DEFAULT4_INTRA: [u8; 16] = [6, 13, 13, 20, 20, 20, 28, 28, 28, 28, 32, 32, 32, 37, 37, 42];
const DEFAULT4_INTER: [u8; 16] = [10, 14, 14, 20, 20, 20, 24, 24, 24, 24, 27, 27, 27, 30, 30, 34];
const DEFAULT8_INTRA: [u8; 64] = [
    6, 10, 10, 13, 11, 13, 16, 16, 16, 16, 18, 18, 18, 18, 18, 23, 23, 23, 23, 23, 23, 25, 25, 25, 25, 25, 25, 25, 27, 27, 27, 27, 27, 27, 27, 27, 29, 29, 29,
    29, 29, 29, 29, 31, 31, 31, 31, 31, 31, 33, 33, 33, 33, 33, 36, 36, 36, 36, 38, 38, 38, 40, 40, 42,
];
const DEFAULT8_INTER: [u8; 64] = [
    9, 13, 13, 15, 13, 15, 17, 17, 17, 17, 19, 19, 19, 19, 19, 21, 21, 21, 21, 21, 21, 22, 22, 22, 22, 22, 22, 22, 24, 24, 24, 24, 24, 24, 24, 24, 25, 25, 25,
    25, 25, 25, 25, 27, 27, 27, 27, 27, 27, 28, 28, 28, 28, 28, 30, 30, 30, 30, 32, 32, 32, 33, 33, 35,
];

fn raster<const N: usize>(zz: &[u8; N]) -> [u8; N] {
    let scan: &[usize] = if N == 16 { &ZIGZAG4 } else { &ZIGZAG8 };
    let mut out = [0u8; N];
    for (i, &v) in zz.iter().enumerate() {
        out[scan[i]] = v;
    }
    out
}

/// One list as sent (7.3.2.1.1.1): None when it says to use the default.
fn scaling_list<const N: usize>(b: &mut Bits) -> Result<Option<[u8; N]>, String> {
    let (mut last, mut next) = (8i32, 8i32);
    let mut zz = [0u8; N];
    for (j, v) in zz.iter_mut().enumerate() {
        if next != 0 {
            next = (last + b.se()? + 256) % 256;
            if j == 0 && next == 0 {
                return Ok(None);
            }
        }
        *v = if next == 0 { last } else { next } as u8;
        last = *v as i32;
    }
    Ok(Some(raster(&zz)))
}

/// The lists of an SPS or a PPS (`n` of them), each one sent, its default, or what the fall-back
/// rule gives: rule A (an SPS) falls back to the defaults, rule B (a PPS) to the sequence's lists.
fn scaling_lists(b: &mut Bits, n: usize, fallback: Option<&Scaling>) -> Result<Scaling, String> {
    let mut s = Scaling::FLAT;
    for i in 0..n {
        let present = b.flag()?;
        if i < 6 {
            let sent = if present { Some(scaling_list::<16>(b)?) } else { None };
            s.l4[i] = match sent {
                Some(Some(l)) => l,
                Some(None) => raster(if i < 3 { &DEFAULT4_INTRA } else { &DEFAULT4_INTER }),
                None if i == 0 || i == 3 => fallback.map_or_else(|| raster(if i == 0 { &DEFAULT4_INTRA } else { &DEFAULT4_INTER }), |f| f.l4[i]),
                None => s.l4[i - 1],
            };
        } else {
            let k = i - 6;
            let sent = if present { Some(scaling_list::<64>(b)?) } else { None };
            let intra = k % 2 == 0;
            s.l8[k] = match sent {
                Some(Some(l)) => l,
                Some(None) => raster(if intra { &DEFAULT8_INTRA } else { &DEFAULT8_INTER }),
                None if k < 2 => fallback.map_or_else(|| raster(if intra { &DEFAULT8_INTRA } else { &DEFAULT8_INTER }), |f| f.l8[k]),
                None => s.l8[k - 2],
            };
        }
    }
    // Lists not sent at all (8×8 with 8×8 transforms off) as the fall-back would have them.
    for k in n.saturating_sub(6)..6 {
        s.l8[k] = match fallback {
            Some(f) => f.l8[k],
            None if k < 2 => raster(if k % 2 == 0 { &DEFAULT8_INTRA } else { &DEFAULT8_INTER }),
            None => s.l8[k - 2],
        };
    }
    Ok(s)
}

#[derive(Debug, Clone)]
pub struct Sps {
    pub id: u32,
    pub profile_idc: u8,
    pub constraint_flags: u8,
    pub level_idc: u8,
    pub chroma_format_idc: u32,
    pub bit_depth_luma_minus8: u32,
    pub bit_depth_chroma_minus8: u32,
    /// The sequence's scaling lists, when it has its own (else flat).
    pub scaling: Option<Scaling>,
    pub log2_max_frame_num: u32,
    pub poc_type: u32,
    pub log2_max_poc_lsb: u32,
    pub delta_pic_order_always_zero: bool,
    pub offset_for_non_ref_pic: i32,
    pub offset_for_top_to_bottom_field: i32,
    pub offsets_for_ref_frame: Vec<i32>,
    pub max_num_ref_frames: u32,
    pub gaps_allowed: bool,
    pub width_mbs: u32,
    pub height_mbs: u32,
    pub frame_mbs_only: bool,
    pub direct_8x8_inference: bool,
    /// Left, right, top, bottom, in pixels.
    pub crop: (u32, u32, u32, u32),
}

impl Sps {
    pub fn max_frame_num(&self) -> u32 {
        1 << self.log2_max_frame_num
    }

    /// The picture's size, cropped.
    pub fn size(&self) -> (usize, usize) {
        let (l, r, t, b) = self.crop;
        ((self.width_mbs * 16 - l - r) as usize, (self.height_mbs * 16 - t - b) as usize)
    }

    pub fn high(&self) -> bool {
        matches!(self.profile_idc, 100 | 110 | 122 | 244 | 44 | 83 | 86 | 118 | 128 | 138 | 139 | 134 | 135)
    }
}

/// 7.3.2.1.1. `nal` is the whole unit, its header byte first.
pub fn parse_sps(nal: &[u8]) -> Result<Sps, String> {
    let rbsp = Rbsp::new(nal);
    let mut b = Bits::new(&rbsp.data);
    b.u(8)?;
    let profile_idc = b.u(8)? as u8;
    let constraint_flags = b.u(8)? as u8;
    let level_idc = b.u(8)? as u8;
    let id = b.ue()?;
    let mut sps = Sps {
        id,
        profile_idc,
        constraint_flags,
        level_idc,
        chroma_format_idc: 1,
        bit_depth_luma_minus8: 0,
        bit_depth_chroma_minus8: 0,
        scaling: None,
        log2_max_frame_num: 4,
        poc_type: 0,
        log2_max_poc_lsb: 4,
        delta_pic_order_always_zero: false,
        offset_for_non_ref_pic: 0,
        offset_for_top_to_bottom_field: 0,
        offsets_for_ref_frame: Vec::new(),
        max_num_ref_frames: 0,
        gaps_allowed: false,
        width_mbs: 0,
        height_mbs: 0,
        frame_mbs_only: true,
        direct_8x8_inference: false,
        crop: (0, 0, 0, 0),
    };
    if sps.high() {
        sps.chroma_format_idc = b.ue()?;
        if sps.chroma_format_idc == 3 {
            b.flag()?; // separate_colour_plane_flag
        }
        sps.bit_depth_luma_minus8 = b.ue()?;
        sps.bit_depth_chroma_minus8 = b.ue()?;
        b.flag()?; // qpprime_y_zero_transform_bypass_flag
        if b.flag()? {
            let n = if sps.chroma_format_idc == 3 { 12 } else { 8 };
            sps.scaling = Some(scaling_lists(&mut b, n, None)?);
        }
    }
    if sps.chroma_format_idc != 1 || sps.bit_depth_luma_minus8 != 0 || sps.bit_depth_chroma_minus8 != 0 {
        return Err("H.264: only 8-bit 4:2:0 is decoded here".into());
    }
    sps.log2_max_frame_num = b.ue()? + 4;
    sps.poc_type = b.ue()?;
    match sps.poc_type {
        0 => sps.log2_max_poc_lsb = b.ue()? + 4,
        1 => {
            sps.delta_pic_order_always_zero = b.flag()?;
            sps.offset_for_non_ref_pic = b.se()?;
            sps.offset_for_top_to_bottom_field = b.se()?;
            let n = b.ue()?;
            if n > 255 {
                return Err("H.264: a bad SPS (POC cycle)".into());
            }
            for _ in 0..n {
                sps.offsets_for_ref_frame.push(b.se()?);
            }
        }
        2 => {}
        _ => return Err("H.264: a bad SPS (POC type)".into()),
    }
    sps.max_num_ref_frames = b.ue()?;
    sps.gaps_allowed = b.flag()?;
    sps.width_mbs = b.ue()? + 1;
    sps.height_mbs = b.ue()? + 1;
    sps.frame_mbs_only = b.flag()?;
    if !sps.frame_mbs_only {
        return Err("H.264: interlaced streams aren't decoded here".into());
    }
    sps.direct_8x8_inference = b.flag()?;
    if b.flag()? {
        // In units of 2 pixels each way (4:2:0, frames).
        let (l, r, t, bo) = (b.ue()?, b.ue()?, b.ue()?, b.ue()?);
        sps.crop = (l * 2, r * 2, t * 2, bo * 2);
        if (l + r) * 2 >= sps.width_mbs * 16 || (t + bo) * 2 >= sps.height_mbs * 16 {
            return Err("H.264: a bad SPS (cropping)".into());
        }
    }
    if sps.max_num_ref_frames > 16 || sps.width_mbs > 1024 || sps.height_mbs > 1024 {
        return Err("H.264: a bad SPS".into());
    }
    Ok(sps)
}

#[derive(Debug, Clone)]
pub struct Pps {
    pub id: u32,
    pub sps_id: u32,
    pub entropy_coding_mode: bool,
    pub bottom_field_pic_order_present: bool,
    pub num_ref_idx_l0_default: u32,
    pub num_ref_idx_l1_default: u32,
    pub weighted_pred: bool,
    pub weighted_bipred_idc: u32,
    pub pic_init_qp_minus26: i32,
    pub pic_init_qs_minus26: i32,
    pub chroma_qp_index_offset: i32,
    pub deblocking_filter_control_present: bool,
    pub constrained_intra_pred: bool,
    pub redundant_pic_cnt_present: bool,
    pub transform_8x8: bool,
    pub second_chroma_qp_index_offset: i32,
    /// The lists in force for pictures that use this PPS (its own, else the sequence's, else flat).
    pub scaling: Scaling,
}

/// 7.3.2.2.
pub fn parse_pps(nal: &[u8], spss: &HashMap<u32, Sps>) -> Result<Pps, String> {
    let rbsp = Rbsp::new(nal);
    let mut b = Bits::new(&rbsp.data);
    b.u(8)?;
    let id = b.ue()?;
    let sps_id = b.ue()?;
    let sps = spss.get(&sps_id).ok_or("H.264: a PPS for an SPS not seen")?;
    let entropy_coding_mode = b.flag()?;
    let bottom_field_pic_order_present = b.flag()?;
    if b.ue()? != 0 {
        return Err("H.264: slice groups (FMO) aren't decoded here".into());
    }
    let mut pps = Pps {
        id,
        sps_id,
        entropy_coding_mode,
        bottom_field_pic_order_present,
        num_ref_idx_l0_default: b.ue()? + 1,
        num_ref_idx_l1_default: b.ue()? + 1,
        weighted_pred: b.flag()?,
        weighted_bipred_idc: b.u(2)?,
        pic_init_qp_minus26: b.se()?,
        pic_init_qs_minus26: b.se()?,
        chroma_qp_index_offset: b.se()?,
        deblocking_filter_control_present: b.flag()?,
        constrained_intra_pred: b.flag()?,
        redundant_pic_cnt_present: b.flag()?,
        transform_8x8: false,
        second_chroma_qp_index_offset: 0,
        scaling: sps.scaling.clone().unwrap_or(Scaling::FLAT),
    };
    pps.second_chroma_qp_index_offset = pps.chroma_qp_index_offset;
    if b.more_data() {
        pps.transform_8x8 = b.flag()?;
        if b.flag()? {
            let n = 6 + if sps.chroma_format_idc == 3 { 6 } else { 2 } * pps.transform_8x8 as usize;
            // Rule B: what isn't sent falls back to the sequence's (flat when it has none).
            let seq = sps.scaling.clone().unwrap_or(Scaling::FLAT);
            let fallback = if sps.scaling.is_some() { Some(&seq) } else { None };
            pps.scaling = scaling_lists(&mut b, n, fallback)?;
        }
        pps.second_chroma_qp_index_offset = b.se()?;
    }
    if pps.num_ref_idx_l0_default > 32 || pps.num_ref_idx_l1_default > 32 {
        return Err("H.264: a bad PPS".into());
    }
    Ok(pps)
}

/// One step of a reference list's modification (7.3.3.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Modify {
    /// A short-term picture this much (abs_diff_pic_num_minus1 + 1) below the last.
    Sub(u32),
    Add(u32),
    Long(u32),
}

/// A memory management control operation (7.3.3.3, 8.2.5.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mmco {
    /// 1: a short-term picture (difference_of_pic_nums_minus1) no longer a reference.
    ForgetShort(u32),
    /// 2: a long-term one (long_term_pic_num).
    ForgetLong(u32),
    /// 3: a short-term one becomes long-term, at this index.
    ShortToLong(u32, u32),
    /// 4: max_long_term_frame_idx_plus1.
    MaxLong(u32),
    /// 5: none are references any more.
    ForgetAll,
    /// 6: this picture becomes long-term, at this index.
    CurrentToLong(u32),
}

/// Explicit weighted prediction for list 0 (7.3.3.2): each reference's (weight, offset), luma
/// and the two chromas, with the defaults where none were sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Weights {
    pub luma_log2_denom: u32,
    pub chroma_log2_denom: u32,
    pub luma_flag: bool,
    pub chroma_flag: bool,
    pub luma: Vec<(i32, i32)>,
    pub chroma: Vec<[(i32, i32); 2]>,
}

pub const SLICE_P: u32 = 0;
pub const SLICE_B: u32 = 1;
pub const SLICE_I: u32 = 2;
pub const SLICE_SP: u32 = 3;
pub const SLICE_SI: u32 = 4;

#[derive(Debug, Clone)]
pub struct SliceHeader {
    pub nal_ref_idc: u8,
    /// An IDR picture (NAL unit type 5).
    pub idr: bool,
    pub first_mb: u32,
    /// 0 P, 1 B, 2 I, 3 SP, 4 SI (the sent value mod 5).
    pub slice_type: u32,
    pub pps_id: u32,
    pub frame_num: u32,
    pub idr_pic_id: u32,
    pub poc_lsb: u32,
    pub delta_poc_bottom: i32,
    pub delta_poc: [i32; 2],
    pub num_ref_idx_l0_active: u32,
    pub modify_l0: Vec<Modify>,
    pub weights: Option<Weights>,
    pub long_term_reference: bool,
    /// The adaptive marking's operations; None: the sliding window.
    pub mmcos: Option<Vec<Mmco>>,
    pub cabac_init_idc: u32,
    pub slice_qp_delta: i32,
    pub disable_deblocking_filter_idc: u32,
    pub slice_alpha_c0_offset_div2: i32,
    pub slice_beta_offset_div2: i32,
    /// Where the slice's data starts: bits from the NAL unit's first byte, in the RBSP (the
    /// emulation-prevention bytes not counted, which is how VA-API's drivers count them), and how
    /// many such bytes the header had (to give the position in the unit as sent).
    pub header_bits: usize,
    pub header_emulation_bytes: usize,
}

/// 7.3.3, for frames. `nal` is the whole unit, its header byte first.
pub fn parse_slice(nal: &[u8], spss: &HashMap<u32, Sps>, ppss: &HashMap<u32, Pps>) -> Result<SliceHeader, String> {
    let rbsp = Rbsp::new(nal);
    let mut b = Bits::new(&rbsp.data);
    let head = b.u(8)?;
    let (nal_ref_idc, nal_type) = ((head >> 5 & 3) as u8, head & 31);
    let first_mb = b.ue()?;
    let slice_type = b.ue()? % 5;
    let pps_id = b.ue()?;
    let pps = ppss.get(&pps_id).ok_or("H.264: a slice for a PPS not seen")?;
    let sps = spss.get(&pps.sps_id).ok_or("H.264: a slice for an SPS not seen")?;
    if slice_type == SLICE_B {
        return Err("H.264: B-frames aren't decoded here (no Rein Remote server sends them)".into());
    }
    let mut sh = SliceHeader {
        nal_ref_idc,
        idr: nal_type == 5,
        first_mb,
        slice_type,
        pps_id,
        frame_num: b.u(sps.log2_max_frame_num)?,
        idr_pic_id: 0,
        poc_lsb: 0,
        delta_poc_bottom: 0,
        delta_poc: [0; 2],
        num_ref_idx_l0_active: pps.num_ref_idx_l0_default,
        modify_l0: Vec::new(),
        weights: None,
        long_term_reference: false,
        mmcos: None,
        cabac_init_idc: 0,
        slice_qp_delta: 0,
        disable_deblocking_filter_idc: 0,
        slice_alpha_c0_offset_div2: 0,
        slice_beta_offset_div2: 0,
        header_bits: 0,
        header_emulation_bytes: 0,
    };
    if sh.idr {
        sh.idr_pic_id = b.ue()?;
    }
    if sps.poc_type == 0 {
        sh.poc_lsb = b.u(sps.log2_max_poc_lsb)?;
        if pps.bottom_field_pic_order_present {
            sh.delta_poc_bottom = b.se()?;
        }
    }
    if sps.poc_type == 1 && !sps.delta_pic_order_always_zero {
        sh.delta_poc[0] = b.se()?;
        if pps.bottom_field_pic_order_present {
            sh.delta_poc[1] = b.se()?;
        }
    }
    if pps.redundant_pic_cnt_present {
        b.ue()?;
    }
    let inter = matches!(slice_type, SLICE_P | SLICE_SP);
    if inter && b.flag()? {
        sh.num_ref_idx_l0_active = b.ue()? + 1;
        if sh.num_ref_idx_l0_active > 32 {
            return Err("H.264: a bad slice header (reference count)".into());
        }
    }
    if inter && b.flag()? {
        loop {
            match b.ue()? {
                0 => sh.modify_l0.push(Modify::Sub(b.ue()? + 1)),
                1 => sh.modify_l0.push(Modify::Add(b.ue()? + 1)),
                2 => sh.modify_l0.push(Modify::Long(b.ue()?)),
                3 => break,
                _ => return Err("H.264: a bad slice header (list modification)".into()),
            }
            if sh.modify_l0.len() > 64 {
                return Err("H.264: a bad slice header (list modification)".into());
            }
        }
    }
    if pps.weighted_pred && inter {
        let luma_log2_denom = b.ue()?;
        let chroma_log2_denom = b.ue()?;
        let mut w = Weights {
            luma_log2_denom,
            chroma_log2_denom,
            luma_flag: false,
            chroma_flag: false,
            luma: Vec::new(),
            chroma: Vec::new(),
        };
        for _ in 0..sh.num_ref_idx_l0_active {
            w.luma.push(if b.flag()? {
                w.luma_flag = true;
                (b.se()?, b.se()?)
            } else {
                (1 << luma_log2_denom, 0)
            });
            w.chroma.push(if b.flag()? {
                w.chroma_flag = true;
                [(b.se()?, b.se()?), (b.se()?, b.se()?)]
            } else {
                [(1 << chroma_log2_denom, 0); 2]
            });
        }
        sh.weights = Some(w);
    }
    if nal_ref_idc != 0 {
        if sh.idr {
            b.flag()?; // no_output_of_prior_pics_flag
            sh.long_term_reference = b.flag()?;
        } else if b.flag()? {
            let mut ops = Vec::new();
            loop {
                ops.push(match b.ue()? {
                    0 => break,
                    1 => Mmco::ForgetShort(b.ue()?),
                    2 => Mmco::ForgetLong(b.ue()?),
                    3 => Mmco::ShortToLong(b.ue()?, b.ue()?),
                    4 => Mmco::MaxLong(b.ue()?),
                    5 => Mmco::ForgetAll,
                    6 => Mmco::CurrentToLong(b.ue()?),
                    _ => return Err("H.264: a bad slice header (marking)".into()),
                });
                if ops.len() > 64 {
                    return Err("H.264: a bad slice header (marking)".into());
                }
            }
            sh.mmcos = Some(ops);
        }
    }
    if pps.entropy_coding_mode && !matches!(slice_type, SLICE_I | SLICE_SI) {
        sh.cabac_init_idc = b.ue()?;
    }
    sh.slice_qp_delta = b.se()?;
    if matches!(slice_type, SLICE_SP | SLICE_SI) {
        if slice_type == SLICE_SP {
            b.flag()?; // sp_for_switch_flag
        }
        b.se()?; // slice_qs_delta
    }
    if pps.deblocking_filter_control_present {
        sh.disable_deblocking_filter_idc = b.ue()?;
        if sh.disable_deblocking_filter_idc != 1 {
            sh.slice_alpha_c0_offset_div2 = b.se()?;
            sh.slice_beta_offset_div2 = b.se()?;
        }
    }
    sh.header_bits = b.pos();
    sh.header_emulation_bytes = rbsp.removed_before(b.pos());
    Ok(sh)
}

/// A picture kept as a reference.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RefPic {
    /// Whatever the decoder holds it in (a VA surface).
    pub surface: u32,
    pub frame_num: u32,
    /// Its LongTermFrameIdx, when it's a long-term reference.
    pub long_term: Option<u32>,
    pub poc: i32,
}

/// The picture being decoded, as the reference marking and the decoder see it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Current {
    pub frame_num: u32,
    pub frame_num_offset: i32,
    pub poc_msb: i32,
    pub top: i32,
    pub bottom: i32,
}

impl Current {
    #[cfg(test)]
    pub fn poc(&self) -> i32 {
        self.top.min(self.bottom)
    }
}

/// The reference pictures and what's carried from picture to picture (8.2.1, 8.2.4, 8.2.5).
#[derive(Debug, Default)]
pub struct Dpb {
    pub refs: Vec<RefPic>,
    max_long_idx: Option<u32>,
    prev_ref_frame_num: u32,
    prev_poc_msb: i32,
    prev_poc_lsb: i32,
    prev_frame_num: u32,
    prev_frame_num_offset: i32,
    /// No picture yet (or lost track): only an IDR can start.
    started: bool,
}

impl Dpb {
    /// Forgets everything: the next picture must be an IDR.
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// The picture's numbers before it's decoded (8.2.1), or an error when it can't be decoded from
    /// what's here (no IDR yet, frames missing).
    pub fn begin(&self, sps: &Sps, sh: &SliceHeader) -> Result<Current, String> {
        let max = sps.max_frame_num();
        if !sh.idr {
            if !self.started {
                return Err("H.264: waiting for a keyframe".into());
            }
            if sh.frame_num != self.prev_ref_frame_num && sh.frame_num != (self.prev_ref_frame_num + 1) % max {
                return Err(format!("H.264: frames missing (frame_num {} after {})", sh.frame_num, self.prev_ref_frame_num));
            }
        }
        let frame_num_offset = if sh.idr {
            0
        } else if self.prev_frame_num > sh.frame_num {
            self.prev_frame_num_offset + max as i32
        } else {
            self.prev_frame_num_offset
        };
        let mut c = Current {
            frame_num: sh.frame_num,
            frame_num_offset,
            poc_msb: 0,
            top: 0,
            bottom: 0,
        };
        match sps.poc_type {
            0 => {
                let (prev_msb, prev_lsb) = if sh.idr { (0, 0) } else { (self.prev_poc_msb, self.prev_poc_lsb) };
                let max_lsb = 1i32 << sps.log2_max_poc_lsb;
                let lsb = sh.poc_lsb as i32;
                c.poc_msb = if lsb < prev_lsb && prev_lsb - lsb >= max_lsb / 2 {
                    prev_msb + max_lsb
                } else if lsb > prev_lsb && lsb - prev_lsb > max_lsb / 2 {
                    prev_msb - max_lsb
                } else {
                    prev_msb
                };
                c.top = c.poc_msb + lsb;
                c.bottom = c.top + sh.delta_poc_bottom;
            }
            1 => {
                let n = sps.offsets_for_ref_frame.len() as i32;
                let mut abs = if n != 0 { frame_num_offset + sh.frame_num as i32 } else { 0 };
                if sh.nal_ref_idc == 0 && abs > 0 {
                    abs -= 1;
                }
                let mut expected = 0;
                if abs > 0 {
                    let per_cycle: i32 = sps.offsets_for_ref_frame.iter().sum();
                    let (cycle, in_cycle) = ((abs - 1) / n, (abs - 1) % n);
                    expected = cycle * per_cycle + sps.offsets_for_ref_frame[..=in_cycle as usize].iter().sum::<i32>();
                }
                if sh.nal_ref_idc == 0 {
                    expected += sps.offset_for_non_ref_pic;
                }
                c.top = expected + sh.delta_poc[0];
                c.bottom = c.top + sps.offset_for_top_to_bottom_field + sh.delta_poc[1];
            }
            _ => {
                let t = if sh.idr {
                    0
                } else if sh.nal_ref_idc == 0 {
                    2 * (frame_num_offset + sh.frame_num as i32) - 1
                } else {
                    2 * (frame_num_offset + sh.frame_num as i32)
                };
                c.top = t;
                c.bottom = t;
            }
        }
        Ok(c)
    }

    /// A short-term reference's PicNum (8.2.4.1), seen from the current picture.
    fn pic_num(r: &RefPic, cur: &Current, max: u32) -> i32 {
        if r.frame_num > cur.frame_num {
            r.frame_num as i32 - max as i32
        } else {
            r.frame_num as i32
        }
    }

    /// RefPicList0 for a P slice (8.2.4.2.1, then 8.2.4.3's modifications), `num_ref_idx_l0_active`
    /// long; None where the stream refers to nothing.
    pub fn list0(&self, sps: &Sps, sh: &SliceHeader, cur: &Current) -> Result<Vec<Option<RefPic>>, String> {
        let max = sps.max_frame_num();
        let mut short: Vec<RefPic> = self.refs.iter().filter(|r| r.long_term.is_none()).copied().collect();
        short.sort_by_key(|r| std::cmp::Reverse(Self::pic_num(r, cur, max)));
        let mut long: Vec<RefPic> = self.refs.iter().filter(|r| r.long_term.is_some()).copied().collect();
        long.sort_by_key(|r| r.long_term);
        let n = sh.num_ref_idx_l0_active as usize;
        let mut list: Vec<Option<RefPic>> = short.into_iter().chain(long).map(Some).collect();
        list.resize(n.max(list.len()), None);
        list.truncate(n);
        let mut pred = cur.frame_num as i32;
        for (idx, m) in sh.modify_l0.iter().enumerate() {
            if idx >= n {
                break;
            }
            let pic = match *m {
                Modify::Sub(d) | Modify::Add(d) => {
                    let d = d as i32;
                    let mut no_wrap = if matches!(m, Modify::Sub(_)) { pred - d } else { pred + d };
                    if no_wrap < 0 {
                        no_wrap += max as i32;
                    } else if no_wrap >= max as i32 {
                        no_wrap -= max as i32;
                    }
                    pred = no_wrap;
                    let num = if no_wrap > cur.frame_num as i32 { no_wrap - max as i32 } else { no_wrap };
                    self.refs
                        .iter()
                        .find(|r| r.long_term.is_none() && Self::pic_num(r, cur, max) == num)
                        .ok_or("H.264: the list names a short-term picture not kept")?
                }
                Modify::Long(num) => self
                    .refs
                    .iter()
                    .find(|r| r.long_term == Some(num))
                    .ok_or("H.264: the list names a long-term picture not kept")?,
            };
            // Put it at idx, and take it out further down.
            list.insert(idx, Some(*pic));
            if let Some(j) = (idx + 1..list.len()).find(|&j| list[j] == Some(*pic)) {
                list.remove(j);
            }
            list.truncate(n);
        }
        Ok(list)
    }

    /// After the picture is decoded into `surface`: the references marked (8.2.5), and what the
    /// next picture's numbers start from.
    pub fn finish(&mut self, sps: &Sps, sh: &SliceHeader, cur: &Current, surface: u32) {
        let max = sps.max_frame_num();
        let mut mmco5 = false;
        let mut current_long = None;
        if sh.idr {
            self.refs.clear();
            if sh.long_term_reference {
                current_long = Some(0);
                self.max_long_idx = Some(0);
            } else {
                self.max_long_idx = None;
            }
        } else if sh.nal_ref_idc != 0 {
            match &sh.mmcos {
                Some(ops) => {
                    for op in ops {
                        let pic_num_x = |d: u32| cur.frame_num as i32 - (d as i32 + 1);
                        match *op {
                            Mmco::ForgetShort(d) => {
                                let x = pic_num_x(d);
                                self.refs.retain(|r| !(r.long_term.is_none() && Self::pic_num(r, cur, max) == x));
                            }
                            Mmco::ForgetLong(n) => self.refs.retain(|r| r.long_term != Some(n)),
                            Mmco::ShortToLong(d, idx) => {
                                let x = pic_num_x(d);
                                self.refs.retain(|r| r.long_term != Some(idx) || Self::pic_num(r, cur, max) == x);
                                if let Some(r) = self.refs.iter_mut().find(|r| r.long_term.is_none() && Self::pic_num(r, cur, max) == x) {
                                    r.long_term = Some(idx);
                                }
                            }
                            Mmco::MaxLong(plus1) => {
                                self.max_long_idx = plus1.checked_sub(1);
                                let m = self.max_long_idx;
                                self.refs.retain(|r| r.long_term.is_none_or(|i| m.is_some_and(|m| i <= m)));
                            }
                            Mmco::ForgetAll => {
                                self.refs.clear();
                                self.max_long_idx = None;
                                mmco5 = true;
                            }
                            Mmco::CurrentToLong(idx) => {
                                self.refs.retain(|r| r.long_term != Some(idx));
                                current_long = Some(idx);
                            }
                        }
                    }
                }
                None => {
                    // The sliding window (8.2.5.3): the oldest short-term one goes when it's full.
                    let cap = sps.max_num_ref_frames.max(1) as usize;
                    while self.refs.len() >= cap {
                        let oldest = self
                            .refs
                            .iter()
                            .enumerate()
                            .filter(|(_, r)| r.long_term.is_none())
                            .min_by_key(|(_, r)| Self::pic_num(r, cur, max))
                            .map(|(i, _)| i);
                        match oldest {
                            Some(i) => {
                                self.refs.remove(i);
                            }
                            None => break,
                        }
                    }
                }
            }
        }
        // Memory management operation 5 makes the picture frame 0 with its order counts from 0.
        let (frame_num, top, bottom) = if mmco5 {
            let t = cur.top.min(cur.bottom);
            (0, cur.top - t, cur.bottom - t)
        } else {
            (cur.frame_num, cur.top, cur.bottom)
        };
        if sh.nal_ref_idc != 0 {
            self.refs.push(RefPic {
                surface,
                frame_num,
                long_term: current_long,
                poc: top.min(bottom),
            });
            // A stream that keeps more than it said it would: the oldest short-term ones go.
            while self.refs.len() > sps.max_num_ref_frames.max(1) as usize {
                let Some(i) = self.refs.iter().position(|r| r.long_term.is_none()) else {
                    break;
                };
                self.refs.remove(i);
            }
            self.prev_ref_frame_num = frame_num;
            if mmco5 {
                (self.prev_poc_msb, self.prev_poc_lsb) = (0, top);
            } else {
                (self.prev_poc_msb, self.prev_poc_lsb) = (cur.poc_msb, sh.poc_lsb as i32);
            }
        }
        self.prev_frame_num = frame_num;
        self.prev_frame_num_offset = if mmco5 { 0 } else { cur.frame_num_offset };
        self.started = true;
    }
}

/// Prints each slice header of an Annex B file (RR_H264_TRACE=FILE in this module's tests), for a
/// comparison with another parser (ffmpeg's trace_headers).
#[cfg(test)]
pub fn trace(stream: &[u8]) -> Result<Vec<String>, String> {
    let (mut spss, mut ppss) = (HashMap::new(), HashMap::new());
    let mut out = Vec::new();
    for nal in super::nal_units(stream).filter(|n| !n.is_empty()) {
        match nal[0] & 31 {
            7 => {
                let s = parse_sps(nal)?;
                out.push(format!(
                    "sps id {} profile {} level {} {}x{} mbs crop {:?} max_frame_num 2^{} poc_type {} max_refs {}",
                    s.id, s.profile_idc, s.level_idc, s.width_mbs, s.height_mbs, s.crop, s.log2_max_frame_num, s.poc_type, s.max_num_ref_frames
                ));
                spss.insert(s.id, s);
            }
            8 => {
                let p = parse_pps(nal, &spss)?;
                out.push(format!(
                    "pps id {} cabac {} 8x8 {} qp {} weighted {}",
                    p.id,
                    p.entropy_coding_mode as u8,
                    p.transform_8x8 as u8,
                    p.pic_init_qp_minus26 + 26,
                    p.weighted_pred as u8
                ));
                ppss.insert(p.id, p);
            }
            1 | 5 => {
                let h = parse_slice(nal, &spss, &ppss)?;
                out.push(format!(
                    "slice first_mb {} type {} frame_num {} poc_lsb {} refs {} mod {:?} long_term {} mmco {:?} qp_delta {} deblock {} header_bits {}",
                    h.first_mb,
                    h.slice_type,
                    h.frame_num,
                    h.poc_lsb,
                    h.num_ref_idx_l0_active,
                    h.modify_l0,
                    h.long_term_reference as u8,
                    h.mmcos,
                    h.slice_qp_delta,
                    h.disable_deblocking_filter_idc,
                    h.header_bits
                ));
            }
            _ => {}
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const VT: &[u8] = include_bytes!("../../tests-data/vt-320x240.h264");
    const OPENH264: &[u8] = include_bytes!("../../tests-data/openh264-320x240.h264");

    /// Each picture of a stream decoded as far as the headers go: its slices' headers, its numbers,
    /// and the references list 0 would hold for its first slice.
    /// A picture's first slice header, its numbers, its list 0 and the references kept then.
    type Walked = (SliceHeader, Current, Vec<Option<RefPic>>, Vec<RefPic>);

    fn walk(stream: &[u8]) -> Vec<Walked> {
        let (mut spss, mut ppss) = (HashMap::new(), HashMap::new());
        let mut dpb = Dpb::default();
        let mut out = Vec::new();
        let mut surface = 0;
        let mut open: Option<(SliceHeader, Current)> = None;
        let units: Vec<&[u8]> = super::super::nal_units(stream).filter(|n| !n.is_empty()).collect();
        for nal in units {
            match nal[0] & 31 {
                7 => {
                    let s = parse_sps(nal).unwrap();
                    spss.insert(s.id, s);
                }
                8 => {
                    let p = parse_pps(nal, &spss).unwrap();
                    ppss.insert(p.id, p);
                }
                1 | 5 => {
                    let h = parse_slice(nal, &spss, &ppss).unwrap();
                    let sps = &spss[&ppss[&h.pps_id].sps_id];
                    if h.first_mb == 0 {
                        if let Some((ph, pc)) = open.take() {
                            dpb.finish(sps, &ph, &pc, surface);
                            surface += 1;
                        }
                        let cur = dpb.begin(sps, &h).unwrap();
                        let list = if h.slice_type == SLICE_P { dpb.list0(sps, &h, &cur).unwrap() } else { vec![] };
                        out.push((h.clone(), cur, list, dpb.refs.clone()));
                        open = Some((h, cur));
                    }
                }
                _ => {}
            }
        }
        out
    }

    #[test]
    fn a_videotoolbox_stream_reads() {
        let (mut spss, mut ppss) = (HashMap::new(), HashMap::new());
        let units: Vec<&[u8]> = super::super::nal_units(VT).collect();
        let sps = parse_sps(units[0]).unwrap();
        assert_eq!((sps.profile_idc, sps.size(), sps.frame_mbs_only), (77, (320, 240), true));
        spss.insert(sps.id, sps);
        let pps = parse_pps(units[1], &spss).unwrap();
        assert!(pps.entropy_coding_mode, "Main profile: CABAC");
        ppss.insert(pps.id, pps);
        let pics = walk(VT);
        // (The encoder gave nine of the ten frames it was given by the time the file closed.)
        assert_eq!(pics.len(), 9);
        assert!(pics[0].0.idr && pics[0].0.slice_type == SLICE_I);
        for (i, (h, cur, list, _)) in pics.iter().enumerate() {
            assert_eq!(h.frame_num as usize, i % 16, "frame_num counts up");
            if i > 0 {
                assert_eq!(h.slice_type, SLICE_P);
                // The frame before is the first reference.
                assert_eq!(list[0].map(|r| r.frame_num), Some(h.frame_num - 1));
                assert!(cur.poc() > pics[i - 1].1.poc(), "order counts go up");
            }
        }
    }

    #[test]
    fn an_openh264_screen_stream_reads_with_its_long_term_references() {
        let pics = walk(OPENH264);
        assert_eq!(pics.len(), 30, "four slices a frame, one picture each");
        let sps = parse_sps(super::super::nal_units(OPENH264).next().unwrap()).unwrap();
        assert_eq!(sps.size(), (320, 240));
        // Screen mode marks long-term references (at the IDR, or later with operations 3 and 6).
        let long = pics
            .iter()
            .any(|(h, _, _, refs)| h.long_term_reference || h.mmcos.is_some() || refs.iter().any(|r| r.long_term.is_some()));
        assert!(long, "openh264's screen content uses long-term references");
        for (h, _, list, refs) in &pics[1..] {
            assert_eq!(h.slice_type, SLICE_P);
            // Every reference the list holds is kept, and it names one.
            assert!(list.iter().all(|r| r.is_some_and(|r| refs.contains(&r))), "{list:?} of {refs:?}");
        }
        if let Ok(path) = std::env::var("RR_H264_TRACE") {
            let data = std::fs::read(path).unwrap();
            for l in trace(&data).unwrap() {
                println!("{l}");
            }
        }
    }

    #[test]
    fn the_header_ends_where_its_bits_say_with_emulation_bytes_counted_apart() {
        // A slice header whose frame_num is zero bits: 00 00 0x makes an emulation byte.
        let (mut spss, mut ppss) = (HashMap::new(), HashMap::new());
        let sps = parse_sps(super::super::nal_units(VT).next().unwrap()).unwrap();
        let pps = parse_pps(super::super::nal_units(VT).nth(1).unwrap(), &HashMap::from([(sps.id, sps.clone())])).unwrap();
        spss.insert(sps.id, sps.clone());
        ppss.insert(pps.id, pps.clone());
        // first_mb_in_slice = 0 ("1"), slice_type 5 (P, "00110"), pps 0 ("1"), then frame_num and
        // POC lsb as zeros, written so two zero bytes come first.
        let mut w = Writer::default();
        w.u(8, 0x41); // nal_ref_idc 2, type 1
        w.ue(0);
        w.ue(5);
        w.ue(pps.id);
        w.u(sps.log2_max_frame_num, 1);
        w.u(sps.log2_max_poc_lsb, 2);
        w.flag(false); // num_ref_idx_active_override
        w.flag(false); // ref_pic_list_modification_flag_l0
        w.flag(false); // adaptive_ref_pic_marking_mode_flag
        w.ue(0); // cabac_init_idc
        w.se(0); // slice_qp_delta
        if pps.deblocking_filter_control_present {
            w.ue(1);
        }
        let bits = w.n;
        let rbsp = w.bytes();
        // Insert an emulation byte the way an encoder would, for a run of zeros: none here, so
        // build one: the same header behind 00 00 01 inside the payload gets 03.
        let mut raw = rbsp.clone();
        raw.extend_from_slice(&[0, 0, 1, 0xff]);
        let mut sent = raw[..rbsp.len()].to_vec();
        sent.extend_from_slice(&[0, 0, 3, 1, 0xff]);
        let h = parse_slice(&sent, &spss, &ppss).unwrap();
        assert_eq!(h.header_bits, bits);
        assert_eq!(h.header_emulation_bytes, 0, "the escape is after the header");
        // And one inside: frame_num of zeros across a byte boundary.
        let r = Rbsp::new(&[0x41, 0, 0, 3, 1, 0x80]);
        assert_eq!(r.data, vec![0x41, 0, 0, 1, 0x80]);
        assert_eq!((r.removed_before(24), r.removed_before(25)), (0, 1));
    }

    #[test]
    fn marking_with_long_term_operations_gives_the_lists_the_standard_does() {
        let sps = Sps {
            max_num_ref_frames: 4,
            ..parse_sps(super::super::nal_units(VT).next().unwrap()).unwrap()
        };
        let mut dpb = Dpb::default();
        let header = |frame_num: u32, idr: bool, mmcos: Option<Vec<Mmco>>, modify: Vec<Modify>| SliceHeader {
            nal_ref_idc: 1,
            idr,
            first_mb: 0,
            slice_type: if idr { SLICE_I } else { SLICE_P },
            pps_id: 0,
            frame_num,
            idr_pic_id: 0,
            poc_lsb: frame_num * 2,
            delta_poc_bottom: 0,
            delta_poc: [0; 2],
            num_ref_idx_l0_active: 4,
            modify_l0: modify,
            weights: None,
            long_term_reference: idr,
            mmcos,
            cabac_init_idc: 0,
            slice_qp_delta: 0,
            disable_deblocking_filter_idc: 0,
            slice_alpha_c0_offset_div2: 0,
            slice_beta_offset_div2: 0,
            header_bits: 0,
            header_emulation_bytes: 0,
        };
        let step = |dpb: &mut Dpb, h: SliceHeader, surface: u32| {
            let cur = dpb.begin(&sps, &h).unwrap();
            let list = if h.idr { vec![] } else { dpb.list0(&sps, &h, &cur).unwrap() };
            dpb.finish(&sps, &h, &cur, surface);
            (list, dpb.refs.clone())
        };
        let surf = |l: &[Option<RefPic>]| l.iter().map(|r| r.map_or(99, |r| r.surface)).collect::<Vec<_>>();
        // An IDR kept long-term (index 0), as openh264's screen mode does.
        step(&mut dpb, header(0, true, None, vec![]), 0);
        // Two short-term ones by the sliding window.
        step(&mut dpb, header(1, false, None, vec![]), 1);
        let (list, _) = step(&mut dpb, header(2, false, None, vec![]), 2);
        assert_eq!(surf(&list), vec![1, 0, 99, 99], "short-term first (newest first), then long-term");
        // Frame 3 makes frame 2 long-term at index 1 (operation 3: picNum 2 = 3 − (0 + 1)).
        let (list, refs) = step(&mut dpb, header(3, false, Some(vec![Mmco::ShortToLong(0, 1)]), vec![]), 3);
        assert_eq!(surf(&list), vec![2, 1, 0, 99]);
        assert_eq!(refs.iter().filter(|r| r.long_term.is_some()).count(), 2);
        // Frame 4: the list reordered to put long-term 1 first, then short-term frame 1 (4 − 3).
        let (list, _) = step(
            &mut dpb,
            header(
                4,
                false,
                Some(vec![Mmco::ForgetShort(2), Mmco::CurrentToLong(2)]),
                vec![Modify::Long(1), Modify::Sub(3)],
            ),
            4,
        );
        assert_eq!(surf(&list), vec![2, 1, 3, 0]);
        // After it: frame 1 forgotten (operation 1: picNum 4 − 3 = 1), frame 4 long-term at 2.
        assert!(!dpb.refs.iter().any(|r| r.surface == 1));
        assert_eq!(dpb.refs.iter().find(|r| r.surface == 4).unwrap().long_term, Some(2));
        // Operation 4 with 1: only long-term index 0 stays long-term.
        step(&mut dpb, header(5, false, Some(vec![Mmco::MaxLong(1)]), vec![]), 5);
        assert_eq!(
            dpb.refs.iter().filter(|r| r.long_term.is_some()).map(|r| r.surface).collect::<Vec<_>>(),
            vec![0]
        );
        // Operation 5: everything goes, the picture becomes frame 0.
        step(&mut dpb, header(6, false, Some(vec![Mmco::ForgetAll]), vec![]), 6);
        assert_eq!(dpb.refs.len(), 1);
        assert_eq!(dpb.refs[0].frame_num, 0);
        // And a frame missing is an error (the viewer asks for a keyframe).
        assert!(dpb.begin(&sps, &header(5, false, None, vec![])).is_err());
    }

    #[test]
    fn order_counts_of_types_1_and_2() {
        let base = parse_sps(super::super::nal_units(VT).next().unwrap()).unwrap();
        let two = Sps { poc_type: 2, ..base.clone() };
        let mut dpb = Dpb::default();
        let mut h = parse_slice_stub(0, true);
        let c = dpb.begin(&two, &h).unwrap();
        assert_eq!(c.poc(), 0);
        dpb.finish(&two, &h, &c, 0);
        h = parse_slice_stub(1, false);
        let c = dpb.begin(&two, &h).unwrap();
        assert_eq!(c.poc(), 2);
        dpb.finish(&two, &h, &c, 1);
        let one = Sps {
            poc_type: 1,
            offsets_for_ref_frame: vec![2],
            ..base
        };
        let mut dpb = Dpb::default();
        let h = parse_slice_stub(0, true);
        let c = dpb.begin(&one, &h).unwrap();
        dpb.finish(&one, &h, &c, 0);
        let h = parse_slice_stub(1, false);
        assert_eq!(dpb.begin(&one, &h).unwrap().poc(), 2);
    }

    fn parse_slice_stub(frame_num: u32, idr: bool) -> SliceHeader {
        SliceHeader {
            nal_ref_idc: 1,
            idr,
            first_mb: 0,
            slice_type: SLICE_P,
            pps_id: 0,
            frame_num,
            idr_pic_id: 0,
            poc_lsb: 0,
            delta_poc_bottom: 0,
            delta_poc: [0; 2],
            num_ref_idx_l0_active: 1,
            modify_l0: vec![],
            weights: None,
            long_term_reference: false,
            mmcos: None,
            cabac_init_idc: 0,
            slice_qp_delta: 0,
            disable_deblocking_filter_idc: 0,
            slice_alpha_c0_offset_div2: 0,
            slice_beta_offset_div2: 0,
            header_bits: 0,
            header_emulation_bytes: 0,
        }
    }

    #[test]
    fn scaling_lists_fall_back_by_the_rules() {
        // An SPS-like list set: only list 0 sent (all 8s: delta 0 then nothing), the rest by rule A.
        let mut w = Writer::default();
        w.flag(true);
        w.se(0); // 8, then next stays 8 for all 16
        for _ in 1..16 {
            w.se(0);
        }
        for _ in 1..8 {
            w.flag(false);
        }
        let bytes = w.bytes();
        let s = scaling_lists(&mut Bits::new(&bytes), 8, None).unwrap();
        assert_eq!(s.l4[0], [8; 16]);
        assert_eq!(s.l4[1], [8; 16], "rule A: the list before");
        assert_eq!(s.l4[3], raster(&DEFAULT4_INTER));
        assert_eq!(s.l8[0], raster(&DEFAULT8_INTRA));
        assert_eq!(raster(&DEFAULT4_INTRA)[4], 13, "raster order: (1,0) is zig-zag 2");
    }

    /// Writes bits, for building headers in tests.
    #[derive(Default)]
    struct Writer {
        out: Vec<u8>,
        n: usize,
    }

    impl Writer {
        fn u(&mut self, bits: u32, v: u32) {
            for i in (0..bits).rev() {
                if self.n & 7 == 0 {
                    self.out.push(0);
                }
                *self.out.last_mut().unwrap() |= (((v >> i) & 1) as u8) << (7 - self.n % 8);
                self.n += 1;
            }
        }
        fn flag(&mut self, b: bool) {
            self.u(1, b as u32);
        }
        fn ue(&mut self, v: u32) {
            let v1 = v as u64 + 1;
            let len = 64 - v1.leading_zeros();
            self.u(len - 1, 0);
            self.u(len, v1 as u32);
        }
        fn se(&mut self, v: i32) {
            self.ue(if v > 0 { 2 * v as u32 - 1 } else { (-2 * v) as u32 });
        }
        /// The bytes, with the stop bit.
        fn bytes(mut self) -> Vec<u8> {
            self.flag(true);
            self.out
        }
    }
}
