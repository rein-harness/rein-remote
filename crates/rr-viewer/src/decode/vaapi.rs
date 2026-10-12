//! Linux: H.264 decoded on the GPU with VA-API (Intel through iHD or i965, AMD through Mesa). libva is
//! loaded when it's first needed (dlopen), so the viewer runs where it isn't installed, decoding with
//! openh264 there.
//!
//! VA-API decodes what it's told: the stream's headers are read here (h264.rs: the SPS, the PPS, each
//! slice's header, the order counts, which pictures are kept as references and in what order the
//! slice refers to them), and handed over with each slice's data. Each frame is decoded at once (no
//! reordering: the servers send no B-frames), read back as NV12 and converted to BGRA on the CPU.
//!
//! H.264 only: HEVC through VA-API would need an HEVC header reader too, which there isn't (a
//! machine whose GPU decodes HEVC gets H.264 from the server instead, decoded the same way).

use super::h264::{self, Current, Dpb, Pps, RefPic, SliceHeader, Sps};
use super::yuv::{to_bgra, Chroma, Plane};
use super::{nal_units, Decoder, Picture};
use rr_proto::Codec;
use std::collections::HashMap;
use std::ffi::{c_char, c_void, CStr};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::sync::OnceLock;

/// What libva's headers declare (va.h, va_drmcommon.h of libva 2.x), only what this file uses, laid
/// out as C lays it out, reserved tails included; the tests check the sizes and offsets.
#[allow(non_camel_case_types, non_snake_case, dead_code)]
mod ffi {
    pub type VADisplay = *mut std::ffi::c_void;
    pub type VAStatus = i32;

    pub const VA_STATUS_SUCCESS: VAStatus = 0;
    pub const VA_INVALID_ID: u32 = 0xffff_ffff;

    // VAProfile, VAEntrypoint
    pub const VA_PROFILE_H264_MAIN: i32 = 6;
    pub const VA_PROFILE_H264_HIGH: i32 = 7;
    pub const VA_PROFILE_H264_CONSTRAINED_BASELINE: i32 = 13;
    pub const VA_ENTRYPOINT_VLD: i32 = 1;

    pub const VA_RT_FORMAT_YUV420: u32 = 0x1;
    pub const VA_PROGRESSIVE: i32 = 0x1;

    // VABufferType
    pub const VA_PICTURE_PARAMETER_BUFFER: i32 = 0;
    pub const VA_IQ_MATRIX_BUFFER: i32 = 1;
    pub const VA_SLICE_PARAMETER_BUFFER: i32 = 4;
    pub const VA_SLICE_DATA_BUFFER: i32 = 5;

    pub const VA_SLICE_DATA_FLAG_ALL: u32 = 0;
    pub const VA_PICTURE_H264_INVALID: u32 = 0x1;
    pub const VA_PICTURE_H264_SHORT_TERM_REFERENCE: u32 = 0x8;
    pub const VA_PICTURE_H264_LONG_TERM_REFERENCE: u32 = 0x10;

    pub const VA_FOURCC_NV12: u32 = u32::from_le_bytes(*b"NV12");

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    pub struct VAImageFormat {
        pub fourcc: u32,
        pub byte_order: u32,
        pub bits_per_pixel: u32,
        pub depth: u32,
        pub red_mask: u32,
        pub green_mask: u32,
        pub blue_mask: u32,
        pub alpha_mask: u32,
        pub va_reserved: [u32; 4],
    }

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    pub struct VAImage {
        pub image_id: u32,
        pub format: VAImageFormat,
        pub buf: u32,
        pub width: u16,
        pub height: u16,
        pub data_size: u32,
        pub num_planes: u32,
        pub pitches: [u32; 3],
        pub offsets: [u32; 3],
        pub num_palette_entries: i32,
        pub entry_bytes: i32,
        pub component_order: [i8; 4],
        pub va_reserved: [u32; 4],
    }

    #[repr(C)]
    #[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
    pub struct VAPictureH264 {
        pub picture_id: u32,
        pub frame_idx: u32,
        pub flags: u32,
        pub TopFieldOrderCnt: i32,
        pub BottomFieldOrderCnt: i32,
        pub va_reserved: [u32; 4],
    }

    impl VAPictureH264 {
        pub const INVALID: Self = Self {
            picture_id: VA_INVALID_ID,
            frame_idx: 0,
            flags: VA_PICTURE_H264_INVALID,
            TopFieldOrderCnt: 0,
            BottomFieldOrderCnt: 0,
            va_reserved: [0; 4],
        };
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct VAPictureParameterBufferH264 {
        pub CurrPic: VAPictureH264,
        pub ReferenceFrames: [VAPictureH264; 16],
        pub picture_width_in_mbs_minus1: u16,
        pub picture_height_in_mbs_minus1: u16,
        pub bit_depth_luma_minus8: u8,
        pub bit_depth_chroma_minus8: u8,
        pub num_ref_frames: u8,
        /// Bit fields, from bit 0: chroma_format_idc (2), residual_colour_transform_flag,
        /// gaps_in_frame_num_value_allowed_flag, frame_mbs_only_flag, mb_adaptive_frame_field_flag,
        /// direct_8x8_inference_flag, MinLumaBiPredSize8x8, log2_max_frame_num_minus4 (4),
        /// pic_order_cnt_type (2), log2_max_pic_order_cnt_lsb_minus4 (4),
        /// delta_pic_order_always_zero_flag.
        pub seq_fields: u32,
        pub num_slice_groups_minus1: u8,
        pub slice_group_map_type: u8,
        pub slice_group_change_rate_minus1: u16,
        pub pic_init_qp_minus26: i8,
        pub pic_init_qs_minus26: i8,
        pub chroma_qp_index_offset: i8,
        pub second_chroma_qp_index_offset: i8,
        /// Bit fields, from bit 0: entropy_coding_mode_flag, weighted_pred_flag,
        /// weighted_bipred_idc (2), transform_8x8_mode_flag, field_pic_flag,
        /// constrained_intra_pred_flag, pic_order_present_flag, deblocking_filter_control_present_flag,
        /// redundant_pic_cnt_present_flag, reference_pic_flag.
        pub pic_fields: u32,
        pub frame_num: u16,
        pub va_reserved: [u32; 8],
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct VAIQMatrixBufferH264 {
        pub ScalingList4x4: [[u8; 16]; 6],
        pub ScalingList8x8: [[u8; 64]; 2],
        pub va_reserved: [u32; 4],
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct VASliceParameterBufferH264 {
        pub slice_data_size: u32,
        pub slice_data_offset: u32,
        pub slice_data_flag: u32,
        pub slice_data_bit_offset: u16,
        pub first_mb_in_slice: u16,
        pub slice_type: u8,
        pub direct_spatial_mv_pred_flag: u8,
        pub num_ref_idx_l0_active_minus1: u8,
        pub num_ref_idx_l1_active_minus1: u8,
        pub cabac_init_idc: u8,
        pub slice_qp_delta: i8,
        pub disable_deblocking_filter_idc: u8,
        pub slice_alpha_c0_offset_div2: i8,
        pub slice_beta_offset_div2: i8,
        pub RefPicList0: [VAPictureH264; 32],
        pub RefPicList1: [VAPictureH264; 32],
        pub luma_log2_weight_denom: u8,
        pub chroma_log2_weight_denom: u8,
        pub luma_weight_l0_flag: u8,
        pub luma_weight_l0: [i16; 32],
        pub luma_offset_l0: [i16; 32],
        pub chroma_weight_l0_flag: u8,
        pub chroma_weight_l0: [[i16; 2]; 32],
        pub chroma_offset_l0: [[i16; 2]; 32],
        pub luma_weight_l1_flag: u8,
        pub luma_weight_l1: [i16; 32],
        pub luma_offset_l1: [i16; 32],
        pub chroma_weight_l1_flag: u8,
        pub chroma_weight_l1: [[i16; 2]; 32],
        pub chroma_offset_l1: [[i16; 2]; 32],
        pub va_reserved: [u32; 4],
    }
}

use ffi::*;

type MessageCallback = Option<unsafe extern "C" fn(*mut c_void, *const c_char)>;

/// The libva functions this file calls, found in libva.so.2 and libva-drm.so.2 at run time.
macro_rules! api {
    ($($name:ident: fn($($arg:ty),*) -> $ret:ty;)*) => {
        #[allow(non_snake_case)]
        struct Va {
            $($name: unsafe extern "C" fn($($arg),*) -> $ret,)*
        }

        impl Va {
            unsafe fn find(libs: &[*mut c_void]) -> Result<Self, String> {
                Ok(Self {
                    $($name: {
                        let name = concat!(stringify!($name), "\0");
                        let p = libs.iter().map(|&l| libc::dlsym(l, name.as_ptr().cast())).find(|p| !p.is_null());
                        let p = p.ok_or(concat!("VA-API: libva has no ", stringify!($name), " (older than libva 2?)"))?;
                        std::mem::transmute::<*mut c_void, unsafe extern "C" fn($($arg),*) -> $ret>(p)
                    },)*
                })
            }
        }
    };
}

api! {
    vaGetDisplayDRM: fn(i32) -> VADisplay;
    vaInitialize: fn(VADisplay, *mut i32, *mut i32) -> VAStatus;
    vaTerminate: fn(VADisplay) -> VAStatus;
    vaSetInfoCallback: fn(VADisplay, MessageCallback, *mut c_void) -> *mut c_void;
    vaErrorStr: fn(VAStatus) -> *const c_char;
    vaQueryVendorString: fn(VADisplay) -> *const c_char;
    vaMaxNumProfiles: fn(VADisplay) -> i32;
    vaQueryConfigProfiles: fn(VADisplay, *mut i32, *mut i32) -> VAStatus;
    vaMaxNumEntrypoints: fn(VADisplay) -> i32;
    vaQueryConfigEntrypoints: fn(VADisplay, i32, *mut i32, *mut i32) -> VAStatus;
    vaCreateConfig: fn(VADisplay, i32, i32, *mut c_void, i32, *mut u32) -> VAStatus;
    vaDestroyConfig: fn(VADisplay, u32) -> VAStatus;
    vaCreateSurfaces: fn(VADisplay, u32, u32, u32, *mut u32, u32, *mut c_void, u32) -> VAStatus;
    vaDestroySurfaces: fn(VADisplay, *mut u32, i32) -> VAStatus;
    vaCreateContext: fn(VADisplay, u32, i32, i32, i32, *mut u32, i32, *mut u32) -> VAStatus;
    vaDestroyContext: fn(VADisplay, u32) -> VAStatus;
    vaCreateBuffer: fn(VADisplay, u32, i32, u32, u32, *mut c_void, *mut u32) -> VAStatus;
    vaDestroyBuffer: fn(VADisplay, u32) -> VAStatus;
    vaMapBuffer: fn(VADisplay, u32, *mut *mut c_void) -> VAStatus;
    vaUnmapBuffer: fn(VADisplay, u32) -> VAStatus;
    vaBeginPicture: fn(VADisplay, u32, u32) -> VAStatus;
    vaRenderPicture: fn(VADisplay, u32, *mut u32, i32) -> VAStatus;
    vaEndPicture: fn(VADisplay, u32) -> VAStatus;
    vaSyncSurface: fn(VADisplay, u32) -> VAStatus;
    vaCreateImage: fn(VADisplay, *mut VAImageFormat, i32, i32, *mut VAImage) -> VAStatus;
    vaDeriveImage: fn(VADisplay, u32, *mut VAImage) -> VAStatus;
    vaGetImage: fn(VADisplay, u32, i32, i32, u32, u32, u32) -> VAStatus;
    vaDestroyImage: fn(VADisplay, u32) -> VAStatus;
}

/// libva, loaded once (never unloaded: drivers it loads keep pointers into it).
fn va() -> Result<&'static Va, String> {
    static VA: OnceLock<Result<Va, String>> = OnceLock::new();
    VA.get_or_init(|| unsafe {
        let open = |name: &CStr| {
            let lib = libc::dlopen(name.as_ptr(), libc::RTLD_NOW | libc::RTLD_GLOBAL);
            if lib.is_null() {
                let why = libc::dlerror();
                let why = if why.is_null() {
                    String::new()
                } else {
                    CStr::from_ptr(why).to_string_lossy().into_owned()
                };
                Err(format!(
                    "VA-API: libva isn't installed ({why}; install libva2 and libva-drm2, and the GPU's driver: intel-media-va-driver or mesa-va-drivers)"
                ))
            } else {
                Ok(lib)
            }
        };
        let libs = [open(c"libva.so.2")?, open(c"libva-drm.so.2")?];
        Va::find(&libs)
    })
    .as_ref()
    .map_err(Clone::clone)
}

fn err_str(va: &Va, s: VAStatus) -> String {
    let p = unsafe { (va.vaErrorStr)(s) };
    if p.is_null() {
        format!("status {s}")
    } else {
        unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
    }
}

/// libva's chatter ("VA-API version…", "Trying to open…") kept out of the log; its errors still show.
unsafe extern "C" fn quiet(_: *mut c_void, _: *const c_char) {}

/// The GPU nodes to try: RR_VAAPI_DEVICE, or every render node.
fn devices() -> Vec<String> {
    if let Ok(d) = std::env::var("RR_VAAPI_DEVICE") {
        return vec![d];
    }
    let mut nodes: Vec<String> = std::fs::read_dir("/dev/dri")
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| n.starts_with("renderD"))
        .map(|n| format!("/dev/dri/{n}"))
        .collect();
    nodes.sort();
    nodes
}

/// An open VA display on one GPU: closed (terminated, then the device) when dropped.
struct Display {
    va: &'static Va,
    dpy: VADisplay,
    vendor: String,
    _fd: OwnedFd,
}

impl Display {
    fn open(path: &str) -> Result<Self, String> {
        let va = va()?;
        let c = std::ffi::CString::new(path).map_err(|_| "VA-API: a bad device path")?;
        let fd = unsafe { libc::open(c.as_ptr(), libc::O_RDWR | libc::O_CLOEXEC) };
        if fd < 0 {
            let e = std::io::Error::last_os_error();
            let hint = if e.raw_os_error() == Some(libc::EACCES) {
                " (add this user to the render group)"
            } else {
                ""
            };
            return Err(format!("{path}: {e}{hint}"));
        }
        let fd = unsafe { OwnedFd::from_raw_fd(fd) };
        unsafe {
            let dpy = (va.vaGetDisplayDRM)(fd.as_raw_fd());
            if dpy.is_null() {
                return Err(format!("{path}: libva can't use it"));
            }
            if std::env::var_os("LIBVA_MESSAGING_LEVEL").is_none() {
                (va.vaSetInfoCallback)(dpy, Some(quiet), std::ptr::null_mut());
            }
            let (mut major, mut minor) = (0, 0);
            let s = (va.vaInitialize)(dpy, &mut major, &mut minor);
            if s != VA_STATUS_SUCCESS {
                // (Terminated anyway: that frees what vaGetDisplayDRM made.)
                (va.vaTerminate)(dpy);
                return Err(format!("{path}: no VA-API driver ({})", err_str(va, s)));
            }
            let v = (va.vaQueryVendorString)(dpy);
            let vendor = if v.is_null() {
                "unknown".into()
            } else {
                CStr::from_ptr(v).to_string_lossy().trim().to_string()
            };
            Ok(Self { va, dpy, vendor, _fd: fd })
        }
    }

    fn check(&self, what: &str, s: VAStatus) -> Result<(), String> {
        if s == VA_STATUS_SUCCESS {
            Ok(())
        } else {
            Err(format!("VA-API: {what} failed ({})", err_str(self.va, s)))
        }
    }

    /// The H.264 profiles this GPU decodes (VLD).
    fn h264_profiles(&self) -> Vec<i32> {
        unsafe {
            let mut list = vec![0; (self.va.vaMaxNumProfiles)(self.dpy).max(1) as usize];
            let mut n = 0;
            if (self.va.vaQueryConfigProfiles)(self.dpy, list.as_mut_ptr(), &mut n) != VA_STATUS_SUCCESS {
                return Vec::new();
            }
            list.truncate(n.max(0) as usize);
            list.retain(|&p| matches!(p, VA_PROFILE_H264_MAIN | VA_PROFILE_H264_HIGH | VA_PROFILE_H264_CONSTRAINED_BASELINE));
            list.retain(|&p| {
                let mut eps = vec![0; (self.va.vaMaxNumEntrypoints)(self.dpy).max(1) as usize];
                let mut n = 0;
                (self.va.vaQueryConfigEntrypoints)(self.dpy, p, eps.as_mut_ptr(), &mut n) == VA_STATUS_SUCCESS
                    && eps[..n.max(0) as usize].contains(&VA_ENTRYPOINT_VLD)
            });
            list
        }
    }
}

impl Drop for Display {
    fn drop(&mut self) {
        unsafe { (self.va.vaTerminate)(self.dpy) };
    }
}

/// The first GPU that decodes H.264, or why there's none.
fn open_display() -> Result<Display, String> {
    let mut why = Vec::new();
    for path in devices() {
        match Display::open(&path) {
            Ok(d) if !d.h264_profiles().is_empty() => return Ok(d),
            Ok(d) => why.push(format!("{path} ({}) doesn't decode H.264", d.vendor)),
            Err(e) => why.push(e),
        }
    }
    if why.is_empty() {
        why.push("no GPU here (/dev/dri/renderD*)".into());
    }
    Err(format!("VA-API: {}", why.join("; ")))
}

/// Whether this machine's GPU decodes `codec` through VA-API (asked once).
pub fn supported(codec: Codec) -> bool {
    codec == Codec::H264 && probe().is_ok()
}

/// Whether there's a VA-API device that decodes H.264, and why not (asked once).
pub fn probe() -> Result<(), String> {
    static PROBE: OnceLock<Result<(), String>> = OnceLock::new();
    PROBE.get_or_init(|| open_display().map(|_| ())).clone()
}

/// What the GPU decodes into: its surfaces (one per reference it may keep, the picture being
/// decoded and one to spare), for a size and profile.
struct Stream {
    config: u32,
    context: u32,
    surfaces: Vec<u32>,
    /// The coded size (whole macroblocks) and the VA profile these were made for.
    key: (u32, u32, i32),
}

pub struct VaApi {
    d: Display,
    profiles: Vec<i32>,
    spss: HashMap<u32, Sps>,
    ppss: HashMap<u32, Pps>,
    dpb: Dpb,
    stream: Option<Stream>,
}

// The display is only used by whoever holds the decoder.
unsafe impl Send for VaApi {}

impl VaApi {
    pub fn new(codec: Codec) -> Result<Self, String> {
        if codec != Codec::H264 {
            return Err(format!("VA-API: {codec:?} isn't decoded here (H.264 only)"));
        }
        let d = open_display()?;
        let profiles = d.h264_profiles();
        Ok(Self {
            d,
            profiles,
            spss: HashMap::new(),
            ppss: HashMap::new(),
            dpb: Dpb::default(),
            stream: None,
        })
    }

    fn close_stream(&mut self) {
        if let Some(mut s) = self.stream.take() {
            let va = self.d.va;
            unsafe {
                (va.vaDestroyContext)(self.d.dpy, s.context);
                (va.vaDestroySurfaces)(self.d.dpy, s.surfaces.as_mut_ptr(), s.surfaces.len() as i32);
                (va.vaDestroyConfig)(self.d.dpy, s.config);
            }
        }
    }

    /// Surfaces and a context for the SPS's size and profile (made again when they change).
    fn ready(&mut self, sps: &Sps) -> Result<(), String> {
        // Constrained Baseline (constraint_set1) decodes as it; Main as Main; the rest as High,
        // which decodes them all. A profile the GPU lacks: the next one up it has.
        let want = match sps.profile_idc {
            66 if sps.constraint_flags & 0x40 != 0 => VA_PROFILE_H264_CONSTRAINED_BASELINE,
            66 | 77 => VA_PROFILE_H264_MAIN,
            _ => VA_PROFILE_H264_HIGH,
        };
        let order = [want, VA_PROFILE_H264_MAIN, VA_PROFILE_H264_HIGH];
        let profile = *order
            .iter()
            .find(|p| self.profiles.contains(p))
            .ok_or_else(|| format!("VA-API: this GPU doesn't decode H.264 profile {}", sps.profile_idc))?;
        let key = (sps.width_mbs, sps.height_mbs, profile);
        let count = (sps.max_num_ref_frames + 2).max(4);
        if self.stream.as_ref().is_some_and(|s| s.key == key && s.surfaces.len() as u32 >= count) {
            return Ok(());
        }
        self.close_stream();
        self.dpb.reset();
        let (va, dpy) = (self.d.va, self.d.dpy);
        let (w, h) = (sps.width_mbs * 16, sps.height_mbs * 16);
        unsafe {
            let mut config = VA_INVALID_ID;
            self.d.check(
                "vaCreateConfig",
                (va.vaCreateConfig)(dpy, profile, VA_ENTRYPOINT_VLD, std::ptr::null_mut(), 0, &mut config),
            )?;
            let mut surfaces = vec![VA_INVALID_ID; count as usize];
            let s = (va.vaCreateSurfaces)(dpy, VA_RT_FORMAT_YUV420, w, h, surfaces.as_mut_ptr(), count, std::ptr::null_mut(), 0);
            if s != VA_STATUS_SUCCESS {
                (va.vaDestroyConfig)(dpy, config);
                return self.d.check("vaCreateSurfaces", s);
            }
            let mut context = VA_INVALID_ID;
            let s = (va.vaCreateContext)(
                dpy,
                config,
                w as i32,
                h as i32,
                VA_PROGRESSIVE,
                surfaces.as_mut_ptr(),
                count as i32,
                &mut context,
            );
            if s != VA_STATUS_SUCCESS {
                (va.vaDestroySurfaces)(dpy, surfaces.as_mut_ptr(), count as i32);
                (va.vaDestroyConfig)(dpy, config);
                return self.d.check("vaCreateContext", s);
            }
            self.stream = Some(Stream {
                config,
                context,
                surfaces,
                key,
            });
        }
        Ok(())
    }

    /// A buffer of `kind` holding `data`, rendered into the picture under way.
    fn render<T>(&self, context: u32, kind: i32, data: &T, size: usize) -> Result<(), String> {
        let (va, dpy) = (self.d.va, self.d.dpy);
        unsafe {
            let mut buf = VA_INVALID_ID;
            self.d.check(
                "vaCreateBuffer",
                (va.vaCreateBuffer)(dpy, context, kind, size as u32, 1, data as *const T as *mut c_void, &mut buf),
            )?;
            let s = (va.vaRenderPicture)(dpy, context, &mut buf, 1);
            (va.vaDestroyBuffer)(dpy, buf);
            self.d.check("vaRenderPicture", s)
        }
    }

    /// Decodes one access unit (a frame's NAL units) into a surface; gives the surface and the SPS.
    fn picture(&mut self, data: &[u8]) -> Result<Option<(u32, u32)>, String> {
        let mut open: Option<(SliceHeader, Current, u32)> = None;
        for nal in nal_units(data).filter(|n| !n.is_empty()) {
            match nal[0] & 31 {
                7 => {
                    let s = h264::parse_sps(nal)?;
                    self.spss.insert(s.id, s);
                }
                8 => {
                    let p = h264::parse_pps(nal, &self.spss)?;
                    self.ppss.insert(p.id, p);
                }
                1 | 5 => {
                    let sh = h264::parse_slice(nal, &self.spss, &self.ppss)?;
                    let pps = self.ppss[&sh.pps_id].clone();
                    let sps = self.spss[&pps.sps_id].clone();
                    if open.is_none() {
                        if sh.first_mb != 0 {
                            return Err("H.264: a frame's first slice is missing".into());
                        }
                        self.ready(&sps)?;
                        let cur = self.dpb.begin(&sps, &sh)?;
                        let surface = self.free_surface()?;
                        self.begin_picture(&sps, &pps, &sh, &cur, surface)?;
                        open = Some((sh.clone(), cur, surface));
                    }
                    let (_, cur, _) = open.as_ref().unwrap();
                    let cur = *cur;
                    self.slice(&sps, &sh, &cur, nal)?;
                }
                _ => {}
            }
        }
        let Some((sh, cur, surface)) = open else { return Ok(None) };
        let context = self.stream.as_ref().map_or(VA_INVALID_ID, |s| s.context);
        self.d.check("vaEndPicture", unsafe { (self.d.va.vaEndPicture)(self.d.dpy, context) })?;
        let sps = self.spss[&self.ppss[&sh.pps_id].sps_id].clone();
        self.dpb.finish(&sps, &sh, &cur, surface);
        Ok(Some((surface, sps.id)))
    }

    /// A surface no reference holds.
    fn free_surface(&self) -> Result<u32, String> {
        let s = self.stream.as_ref().ok_or("VA-API: no stream")?;
        s.surfaces
            .iter()
            .copied()
            .find(|id| !self.dpb.refs.iter().any(|r| r.surface == *id))
            .ok_or_else(|| "VA-API: every surface is a reference (the stream keeps more than it said)".into())
    }

    fn va_ref(r: &RefPic) -> VAPictureH264 {
        VAPictureH264 {
            picture_id: r.surface,
            frame_idx: r.long_term.unwrap_or(r.frame_num),
            flags: if r.long_term.is_some() {
                VA_PICTURE_H264_LONG_TERM_REFERENCE
            } else {
                VA_PICTURE_H264_SHORT_TERM_REFERENCE
            },
            TopFieldOrderCnt: r.poc,
            BottomFieldOrderCnt: r.poc,
            va_reserved: [0; 4],
        }
    }

    fn begin_picture(&mut self, sps: &Sps, pps: &Pps, sh: &SliceHeader, cur: &Current, surface: u32) -> Result<(), String> {
        let context = self.stream.as_ref().map_or(VA_INVALID_ID, |s| s.context);
        let mut refs = [VAPictureH264::INVALID; 16];
        for (slot, r) in refs.iter_mut().zip(&self.dpb.refs) {
            *slot = Self::va_ref(r);
        }
        let seq_fields = sps.chroma_format_idc
            | (sps.gaps_allowed as u32) << 3
            | (sps.frame_mbs_only as u32) << 4
            | (sps.direct_8x8_inference as u32) << 6
            | ((sps.level_idc >= 31) as u32) << 7
            | (sps.log2_max_frame_num - 4) << 8
            | sps.poc_type << 12
            | (sps.log2_max_poc_lsb.saturating_sub(4)) << 14
            | (sps.delta_pic_order_always_zero as u32) << 18;
        let pic_fields = pps.entropy_coding_mode as u32
            | (pps.weighted_pred as u32) << 1
            | pps.weighted_bipred_idc << 2
            | (pps.transform_8x8 as u32) << 4
            | (pps.constrained_intra_pred as u32) << 6
            | (pps.bottom_field_pic_order_present as u32) << 7
            | (pps.deblocking_filter_control_present as u32) << 8
            | (pps.redundant_pic_cnt_present as u32) << 9
            | ((sh.nal_ref_idc != 0) as u32) << 10;
        let pic = VAPictureParameterBufferH264 {
            CurrPic: VAPictureH264 {
                picture_id: surface,
                frame_idx: cur.frame_num,
                flags: 0,
                TopFieldOrderCnt: cur.top,
                BottomFieldOrderCnt: cur.bottom,
                va_reserved: [0; 4],
            },
            ReferenceFrames: refs,
            picture_width_in_mbs_minus1: (sps.width_mbs - 1) as u16,
            picture_height_in_mbs_minus1: (sps.height_mbs - 1) as u16,
            bit_depth_luma_minus8: sps.bit_depth_luma_minus8 as u8,
            bit_depth_chroma_minus8: sps.bit_depth_chroma_minus8 as u8,
            num_ref_frames: sps.max_num_ref_frames as u8,
            seq_fields,
            num_slice_groups_minus1: 0,
            slice_group_map_type: 0,
            slice_group_change_rate_minus1: 0,
            pic_init_qp_minus26: pps.pic_init_qp_minus26 as i8,
            pic_init_qs_minus26: pps.pic_init_qs_minus26 as i8,
            chroma_qp_index_offset: pps.chroma_qp_index_offset as i8,
            second_chroma_qp_index_offset: pps.second_chroma_qp_index_offset as i8,
            pic_fields,
            frame_num: sh.frame_num as u16,
            va_reserved: [0; 8],
        };
        let iq = VAIQMatrixBufferH264 {
            ScalingList4x4: pps.scaling.l4,
            ScalingList8x8: [pps.scaling.l8[0], pps.scaling.l8[1]],
            va_reserved: [0; 4],
        };
        self.d
            .check("vaBeginPicture", unsafe { (self.d.va.vaBeginPicture)(self.d.dpy, context, surface) })?;
        self.render(context, VA_PICTURE_PARAMETER_BUFFER, &pic, std::mem::size_of_val(&pic))?;
        self.render(context, VA_IQ_MATRIX_BUFFER, &iq, std::mem::size_of_val(&iq))
    }

    fn slice(&mut self, sps: &Sps, sh: &SliceHeader, cur: &Current, nal: &[u8]) -> Result<(), String> {
        let context = self.stream.as_ref().map_or(VA_INVALID_ID, |s| s.context);
        // SAFETY: plain data, zeroes are valid.
        let mut p: VASliceParameterBufferH264 = unsafe { std::mem::zeroed() };
        p.slice_data_size = nal.len() as u32;
        p.slice_data_offset = 0;
        p.slice_data_flag = VA_SLICE_DATA_FLAG_ALL;
        p.slice_data_bit_offset = sh.header_bits as u16;
        p.first_mb_in_slice = sh.first_mb as u16;
        p.slice_type = sh.slice_type as u8;
        p.num_ref_idx_l0_active_minus1 = sh.num_ref_idx_l0_active.saturating_sub(1) as u8;
        p.cabac_init_idc = sh.cabac_init_idc as u8;
        p.slice_qp_delta = sh.slice_qp_delta as i8;
        p.disable_deblocking_filter_idc = sh.disable_deblocking_filter_idc as u8;
        p.slice_alpha_c0_offset_div2 = sh.slice_alpha_c0_offset_div2 as i8;
        p.slice_beta_offset_div2 = sh.slice_beta_offset_div2 as i8;
        p.RefPicList0 = [VAPictureH264::INVALID; 32];
        p.RefPicList1 = [VAPictureH264::INVALID; 32];
        if matches!(sh.slice_type, h264::SLICE_P | h264::SLICE_SP) {
            let list = self.dpb.list0(sps, sh, cur)?;
            for (slot, r) in p.RefPicList0.iter_mut().zip(&list) {
                if let Some(r) = r {
                    *slot = Self::va_ref(r);
                }
            }
        }
        if let Some(w) = &sh.weights {
            p.luma_log2_weight_denom = w.luma_log2_denom as u8;
            p.chroma_log2_weight_denom = w.chroma_log2_denom as u8;
            p.luma_weight_l0_flag = w.luma_flag as u8;
            p.chroma_weight_l0_flag = w.chroma_flag as u8;
            for (i, &(wt, off)) in w.luma.iter().enumerate().take(32) {
                p.luma_weight_l0[i] = wt as i16;
                p.luma_offset_l0[i] = off as i16;
            }
            for (i, c) in w.chroma.iter().enumerate().take(32) {
                for (j, &(wt, off)) in c.iter().enumerate() {
                    p.chroma_weight_l0[i][j] = wt as i16;
                    p.chroma_offset_l0[i][j] = off as i16;
                }
            }
        }
        self.render(context, VA_SLICE_PARAMETER_BUFFER, &p, std::mem::size_of_val(&p))?;
        // The slice's data: the whole unit as it came (its header byte first), its emulation bytes
        // in (the driver takes them out itself).
        let (va, dpy) = (self.d.va, self.d.dpy);
        unsafe {
            let mut buf = VA_INVALID_ID;
            self.d.check(
                "vaCreateBuffer",
                (va.vaCreateBuffer)(dpy, context, VA_SLICE_DATA_BUFFER, nal.len() as u32, 1, nal.as_ptr() as *mut c_void, &mut buf),
            )?;
            let s = (va.vaRenderPicture)(dpy, context, &mut buf, 1);
            (va.vaDestroyBuffer)(dpy, buf);
            self.d.check("vaRenderPicture", s)
        }
    }

    /// The surface's picture (NV12), cropped to the SPS's size, into `out` as BGRA.
    fn read(&self, surface: u32, sps: &Sps, out: &mut Picture) -> Result<(), String> {
        let (va, dpy) = (self.d.va, self.d.dpy);
        let (w, h) = sps.size();
        let (cl, _, ct, _) = sps.crop;
        let (cw, ch) = (sps.width_mbs * 16, sps.height_mbs * 16);
        unsafe {
            self.d.check("vaSyncSurface", (va.vaSyncSurface)(dpy, surface))?;
            let mut image = VAImage::default();
            // The surface's own memory where the driver allows it; else a copy into an image.
            if (va.vaDeriveImage)(dpy, surface, &mut image) != VA_STATUS_SUCCESS || image.format.fourcc != VA_FOURCC_NV12 {
                if image.image_id != 0 && image.image_id != VA_INVALID_ID {
                    (va.vaDestroyImage)(dpy, image.image_id);
                }
                let mut fmt = VAImageFormat {
                    fourcc: VA_FOURCC_NV12,
                    byte_order: 1,
                    bits_per_pixel: 12,
                    ..Default::default()
                };
                image = VAImage::default();
                self.d
                    .check("vaCreateImage", (va.vaCreateImage)(dpy, &mut fmt, cw as i32, ch as i32, &mut image))?;
                let s = (va.vaGetImage)(dpy, surface, 0, 0, cw, ch, image.image_id);
                if s != VA_STATUS_SUCCESS {
                    (va.vaDestroyImage)(dpy, image.image_id);
                    return self.d.check("vaGetImage", s);
                }
            }
            let mut p = std::ptr::null_mut();
            let s = (va.vaMapBuffer)(dpy, image.buf, &mut p);
            if s != VA_STATUS_SUCCESS {
                (va.vaDestroyImage)(dpy, image.image_id);
                return self.d.check("vaMapBuffer", s);
            }
            let base = p as *const u8;
            let (yp, uvp) = (image.pitches[0] as usize, image.pitches[1] as usize);
            let (cl, ct) = (cl as usize, ct as usize);
            let y = std::slice::from_raw_parts(base.add(image.offsets[0] as usize + ct * yp + cl), yp * (h - 1) + w);
            let uv = std::slice::from_raw_parts(
                base.add(image.offsets[1] as usize + ct / 2 * uvp + cl),
                uvp * (h.div_ceil(2) - 1) + w.div_ceil(2) * 2,
            );
            out.reshape(w, h);
            to_bgra(
                Plane { data: y, stride: yp },
                Chroma::Nv12(Plane { data: uv, stride: uvp }),
                w,
                h,
                out.bytes_mut(),
            );
            (va.vaUnmapBuffer)(dpy, image.buf);
            (va.vaDestroyImage)(dpy, image.image_id);
        }
        Ok(())
    }
}

impl Drop for VaApi {
    fn drop(&mut self) {
        self.close_stream();
    }
}

impl Decoder for VaApi {
    fn name(&self) -> String {
        format!("VA-API H.264 (hardware, {})", self.d.vendor)
    }

    fn hardware(&self) -> bool {
        true
    }

    fn decode(&mut self, data: &[u8], _keyframe: bool, out: &mut Picture) -> Result<bool, String> {
        match self.picture(data) {
            Ok(Some((surface, sps_id))) => {
                let sps = self.spss[&sps_id].clone();
                self.read(surface, &sps, out)?;
                Ok(true)
            }
            Ok(None) => Ok(false),
            Err(e) => {
                // Whatever was kept may be wrong now: start again from the next keyframe.
                self.dpb.reset();
                Err(e)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::ffi::*;
    use std::mem::{offset_of, size_of};

    /// Sizes and offsets from libva 2.x's va.h on 64-bit Linux.
    #[test]
    fn structs_are_laid_out_as_libva_has_them() {
        assert_eq!(size_of::<VAPictureH264>(), 36);
        assert_eq!(size_of::<VAImageFormat>(), 48);
        assert_eq!(size_of::<VAImage>(), 120);
        assert_eq!(size_of::<VAPictureParameterBufferH264>(), 672);
        assert_eq!(offset_of!(VAPictureParameterBufferH264, picture_width_in_mbs_minus1), 612);
        assert_eq!(offset_of!(VAPictureParameterBufferH264, seq_fields), 620);
        assert_eq!(offset_of!(VAPictureParameterBufferH264, num_slice_groups_minus1), 624);
        assert_eq!(offset_of!(VAPictureParameterBufferH264, pic_init_qp_minus26), 628);
        assert_eq!(offset_of!(VAPictureParameterBufferH264, pic_fields), 632);
        assert_eq!(offset_of!(VAPictureParameterBufferH264, frame_num), 636);
        assert_eq!(size_of::<VAIQMatrixBufferH264>(), 240);
        assert_eq!(offset_of!(VASliceParameterBufferH264, slice_data_bit_offset), 12);
        assert_eq!(offset_of!(VASliceParameterBufferH264, slice_beta_offset_div2), 24);
        assert_eq!(offset_of!(VASliceParameterBufferH264, RefPicList0), 28);
        assert_eq!(offset_of!(VASliceParameterBufferH264, RefPicList1), 1180);
        assert_eq!(offset_of!(VASliceParameterBufferH264, luma_log2_weight_denom), 2332);
        assert_eq!(offset_of!(VASliceParameterBufferH264, luma_weight_l0), 2336);
        assert_eq!(offset_of!(VASliceParameterBufferH264, chroma_weight_l0_flag), 2464);
        assert_eq!(offset_of!(VASliceParameterBufferH264, chroma_weight_l0), 2466);
        assert_eq!(offset_of!(VASliceParameterBufferH264, luma_weight_l1_flag), 2722);
        assert_eq!(offset_of!(VASliceParameterBufferH264, chroma_offset_l1), 2982);
        assert_eq!(size_of::<VASliceParameterBufferH264>(), 3128);
    }

    #[test]
    fn without_a_gpu_it_says_why() {
        match super::open_display() {
            Ok(d) => eprintln!("VA-API: {} decodes H.264 profiles {:?}", d.vendor, d.h264_profiles()),
            Err(e) => {
                eprintln!("{e}");
                assert!(e.starts_with("VA-API"));
            }
        }
    }
}
