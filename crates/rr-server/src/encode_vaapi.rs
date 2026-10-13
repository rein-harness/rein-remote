//! Linux: H.264 in hardware with VA-API (Intel's media engine through iHD or i965, AMD's through
//! Mesa). libva is loaded when the first stream starts (dlopen), so the binary runs where it isn't
//! installed: the server encodes in software there.
//!
//! A frame from memory goes into a BGRX surface, and the GPU converts it to NV12 (VA's video
//! processing); a frame from KMS (`serve --kms`) is the screen's own buffer, imported as it is
//! (DMA-BUF) and converted the same way, so the CPU never touches its pixels. Each frame is encoded at
//! once: an IDR when one is asked for, else a P-frame that refers to the frame before (no B-frames,
//! nothing reordered), CBR at a bitrate that changes live. The output is the same Annex B stream the
//! other encoders make, the SPS and PPS before each IDR (ours, see h264.rs, where the driver takes
//! them).

use crate::encode::{Encoded, VideoEncoder};
use crate::h264::{self, Params, Profile};
use rr_proto::Codec;
use std::ffi::{c_char, c_void, CStr};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::sync::OnceLock;

/// What libva's headers declare (va.h, va_enc_h264.h, va_vpp.h, va_drmcommon.h of libva 2.x), only
/// what this file uses. Each struct is laid out as C lays it out, its reserved tail included (that's
/// what keeps libva 2.x's ABI stable); the tests check every size and offset against the headers.
#[allow(non_camel_case_types, non_snake_case, dead_code)]
mod ffi {
    use std::ffi::c_void;

    pub type VADisplay = *mut c_void;
    pub type VAStatus = i32;

    pub const VA_STATUS_SUCCESS: VAStatus = 0;
    pub const VA_INVALID_ID: u32 = 0xffff_ffff;

    // VAProfile, VAEntrypoint
    pub const VA_PROFILE_NONE: i32 = -1;
    pub const VA_PROFILE_H264_MAIN: i32 = 6;
    pub const VA_PROFILE_H264_CONSTRAINED_BASELINE: i32 = 13;
    pub const VA_ENTRYPOINT_ENC_SLICE: i32 = 6;
    pub const VA_ENTRYPOINT_ENC_SLICE_LP: i32 = 8;
    pub const VA_ENTRYPOINT_VIDEO_PROC: i32 = 10;

    // VAConfigAttribType
    pub const VA_CONFIG_ATTRIB_RT_FORMAT: i32 = 0;
    pub const VA_CONFIG_ATTRIB_RATE_CONTROL: i32 = 5;
    pub const VA_CONFIG_ATTRIB_ENC_PACKED_HEADERS: i32 = 10;
    pub const VA_CONFIG_ATTRIB_ENC_MAX_REF_FRAMES: i32 = 13;
    pub const VA_CONFIG_ATTRIB_ENC_INTRA_REFRESH: i32 = 23;
    pub const VA_ENC_INTRA_REFRESH_ROLLING_ROW: u32 = 0x2;
    pub const VA_ATTRIB_NOT_SUPPORTED: u32 = 0x8000_0000;

    pub const VA_RT_FORMAT_YUV420: u32 = 0x1;
    pub const VA_RT_FORMAT_RGB32: u32 = 0x2_0000;
    pub const VA_RT_FORMAT_RGB32_10: u32 = 0x20_0000;
    pub const VA_RC_CBR: u32 = 0x2;
    pub const VA_RC_VBR: u32 = 0x4;
    pub const VA_ENC_PACKED_HEADER_SEQUENCE: u32 = 0x1;
    pub const VA_ENC_PACKED_HEADER_SLICE: u32 = 0x4;
    pub const VA_PROGRESSIVE: i32 = 0x1;

    // VABufferType
    pub const VA_ENC_CODED_BUFFER: i32 = 21;
    pub const VA_ENC_SEQUENCE_PARAMETER_BUFFER: i32 = 22;
    pub const VA_ENC_PICTURE_PARAMETER_BUFFER: i32 = 23;
    pub const VA_ENC_SLICE_PARAMETER_BUFFER: i32 = 24;
    pub const VA_ENC_PACKED_HEADER_PARAMETER_BUFFER: i32 = 25;
    pub const VA_ENC_PACKED_HEADER_DATA_BUFFER: i32 = 26;
    pub const VA_ENC_MISC_PARAMETER_BUFFER: i32 = 27;
    pub const VA_PROC_PIPELINE_PARAMETER_BUFFER: i32 = 41;

    // VAEncMiscParameterType, VAEncPackedHeaderType
    pub const VA_ENC_MISC_FRAME_RATE: u32 = 0;
    pub const VA_ENC_MISC_RATE_CONTROL: u32 = 1;
    pub const VA_ENC_MISC_HRD: u32 = 5;
    pub const VA_ENC_MISC_RIR: u32 = 7;
    pub const VA_ENC_PACKED_HEADER_TYPE_SEQUENCE: u32 = 1;
    pub const VA_ENC_PACKED_HEADER_TYPE_SLICE: u32 = 3;

    pub const VA_PICTURE_H264_INVALID: u32 = 0x1;
    pub const VA_PICTURE_H264_SHORT_TERM_REFERENCE: u32 = 0x8;
    pub const VA_CODED_BUF_STATUS_SLICE_OVERFLOW_MASK: u32 = 0x200;

    // Surfaces: attributes, and memory from DRM (VADRMPRIMESurfaceDescriptor).
    pub const VA_SURFACE_ATTRIB_PIXEL_FORMAT: i32 = 1;
    pub const VA_SURFACE_ATTRIB_MEMORY_TYPE: i32 = 6;
    pub const VA_SURFACE_ATTRIB_EXTERNAL_BUFFER_DESCRIPTOR: i32 = 7;
    pub const VA_SURFACE_ATTRIB_SETTABLE: u32 = 0x2;
    pub const VA_GENERIC_VALUE_TYPE_INTEGER: i32 = 1;
    pub const VA_GENERIC_VALUE_TYPE_POINTER: i32 = 3;
    pub const VA_SURFACE_ATTRIB_MEM_TYPE_DRM_PRIME_2: u32 = 0x4000_0000;

    // Video processing: the colour standards, range, the fast path.
    pub const VA_PROC_COLOR_STANDARD_BT601: u32 = 1;
    pub const VA_PROC_COLOR_STANDARD_SRGB: u32 = 8;
    pub const VA_SOURCE_RANGE_REDUCED: u8 = 1;
    pub const VA_SOURCE_RANGE_FULL: u8 = 2;
    pub const VA_FILTER_SCALING_FAST: u32 = 0x100;

    pub const fn fourcc(s: &[u8; 4]) -> u32 {
        u32::from_le_bytes(*s)
    }
    pub const VA_FOURCC_NV12: u32 = fourcc(b"NV12");
    pub const VA_FOURCC_BGRX: u32 = fourcc(b"BGRX");
    pub const VA_FOURCC_BGRA: u32 = fourcc(b"BGRA");
    pub const VA_FOURCC_RGBX: u32 = fourcc(b"RGBX");
    pub const VA_FOURCC_RGBA: u32 = fourcc(b"RGBA");
    pub const VA_FOURCC_X2R10G10B10: u32 = fourcc(b"XR30");

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    pub struct VAConfigAttrib {
        pub type_: i32,
        pub value: u32,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub union VAGenericValueUnion {
        pub i: i32,
        pub f: f32,
        pub p: *mut c_void,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct VAGenericValue {
        pub type_: i32,
        pub value: VAGenericValueUnion,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct VASurfaceAttrib {
        pub type_: i32,
        pub flags: u32,
        pub value: VAGenericValue,
    }

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
    #[derive(Clone, Copy, Default)]
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
    pub struct VAEncSequenceParameterBufferH264 {
        pub seq_parameter_set_id: u8,
        pub level_idc: u8,
        pub intra_period: u32,
        pub intra_idr_period: u32,
        pub ip_period: u32,
        pub bits_per_second: u32,
        pub max_num_ref_frames: u32,
        pub picture_width_in_mbs: u16,
        pub picture_height_in_mbs: u16,
        /// Bit fields, from bit 0: chroma_format_idc (2), frame_mbs_only_flag, mb_adaptive_frame_field_flag,
        /// seq_scaling_matrix_present_flag, direct_8x8_inference_flag, log2_max_frame_num_minus4 (4),
        /// pic_order_cnt_type (2), log2_max_pic_order_cnt_lsb_minus4 (4), delta_pic_order_always_zero_flag.
        pub seq_fields: u32,
        pub bit_depth_luma_minus8: u8,
        pub bit_depth_chroma_minus8: u8,
        pub num_ref_frames_in_pic_order_cnt_cycle: u8,
        pub offset_for_non_ref_pic: i32,
        pub offset_for_top_to_bottom_field: i32,
        pub offset_for_ref_frame: [i32; 256],
        pub frame_cropping_flag: u8,
        pub frame_crop_left_offset: u32,
        pub frame_crop_right_offset: u32,
        pub frame_crop_top_offset: u32,
        pub frame_crop_bottom_offset: u32,
        pub vui_parameters_present_flag: u8,
        /// Bit fields, from bit 0: aspect_ratio_info_present_flag, timing_info_present_flag,
        /// bitstream_restriction_flag, log2_max_mv_length_horizontal (5), log2_max_mv_length_vertical (5),
        /// fixed_frame_rate_flag, low_delay_hrd_flag, motion_vectors_over_pic_boundaries_flag.
        pub vui_fields: u32,
        pub aspect_ratio_idc: u8,
        pub sar_width: u32,
        pub sar_height: u32,
        pub num_units_in_tick: u32,
        pub time_scale: u32,
        pub va_reserved: [u32; 4],
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct VAEncPictureParameterBufferH264 {
        pub CurrPic: VAPictureH264,
        pub ReferenceFrames: [VAPictureH264; 16],
        pub coded_buf: u32,
        pub pic_parameter_set_id: u8,
        pub seq_parameter_set_id: u8,
        pub last_picture: u8,
        pub frame_num: u16,
        pub pic_init_qp: u8,
        pub num_ref_idx_l0_active_minus1: u8,
        pub num_ref_idx_l1_active_minus1: u8,
        pub chroma_qp_index_offset: i8,
        pub second_chroma_qp_index_offset: i8,
        /// Bit fields, from bit 0: idr_pic_flag, reference_pic_flag (2), entropy_coding_mode_flag,
        /// weighted_pred_flag, weighted_bipred_idc (2), constrained_intra_pred_flag, transform_8x8_mode_flag,
        /// deblocking_filter_control_present_flag, redundant_pic_cnt_present_flag, pic_order_present_flag,
        /// pic_scaling_matrix_present_flag.
        pub pic_fields: u32,
        pub va_reserved: [u32; 4],
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct VAEncSliceParameterBufferH264 {
        pub macroblock_address: u32,
        pub num_macroblocks: u32,
        pub macroblock_info: u32,
        pub slice_type: u8,
        pub pic_parameter_set_id: u8,
        pub idr_pic_id: u16,
        pub pic_order_cnt_lsb: u16,
        pub delta_pic_order_cnt_bottom: i32,
        pub delta_pic_order_cnt: [i32; 2],
        pub direct_spatial_mv_pred_flag: u8,
        pub num_ref_idx_active_override_flag: u8,
        pub num_ref_idx_l0_active_minus1: u8,
        pub num_ref_idx_l1_active_minus1: u8,
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
        pub cabac_init_idc: u8,
        pub slice_qp_delta: i8,
        pub disable_deblocking_filter_idc: u8,
        pub slice_alpha_c0_offset_div2: i8,
        pub slice_beta_offset_div2: i8,
        pub va_reserved: [u32; 4],
    }

    /// VAEncMiscParameterBuffer: the type, then the parameters themselves in the same buffer.
    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct VAEncMiscParameter<T> {
        pub type_: u32,
        pub data: T,
    }

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    pub struct VAEncMiscParameterRateControl {
        pub bits_per_second: u32,
        pub target_percentage: u32,
        pub window_size: u32,
        pub initial_qp: u32,
        pub min_qp: u32,
        pub basic_unit_size: u32,
        /// Bit fields, from bit 0: reset, disable_frame_skip, disable_bit_stuffing, mb_rate_control (4),
        /// temporal_id (8), cfs_I_frames, enable_parallel_brc, enable_dynamic_scaling, frame_tolerance_mode (2).
        pub rc_flags: u32,
        pub ICQ_quality_factor: u32,
        pub max_qp: u32,
        pub quality_factor: u32,
        pub target_frame_size: u32,
        pub va_reserved: [u32; 4],
    }

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    pub struct VAEncMiscParameterFrameRate {
        pub framerate: u32,
        pub framerate_flags: u32,
        pub va_reserved: [u32; 4],
    }

    /// Rolling intra refresh: `intra_insert_size` rows (or columns) of macroblocks from
    /// `intra_insertion_location` are coded intra in this frame.
    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    pub struct VAEncMiscParameterRIR {
        /// Bit 0: columns, bit 1: rows.
        pub rir_flags: u32,
        pub intra_insertion_location: u16,
        pub intra_insert_size: u16,
        pub qp_delta_for_inserted_intra: u8,
        pub va_reserved: [u32; 4],
    }

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    pub struct VAEncMiscParameterHRD {
        pub initial_buffer_fullness: u32,
        pub buffer_size: u32,
        pub va_reserved: [u32; 4],
    }

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    pub struct VAEncPackedHeaderParameterBuffer {
        pub type_: u32,
        pub bit_length: u32,
        pub has_emulation_bytes: u8,
        pub va_reserved: [u32; 4],
    }

    #[repr(C)]
    pub struct VACodedBufferSegment {
        pub size: u32,
        pub bit_offset: u32,
        pub status: u32,
        pub reserved: u32,
        pub buf: *mut c_void,
        pub next: *mut c_void,
        pub va_reserved: [u32; 4],
    }

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    pub struct VARectangle {
        pub x: i16,
        pub y: i16,
        pub width: u16,
        pub height: u16,
    }

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    pub struct VAProcColorProperties {
        pub chroma_sample_location: u8,
        pub color_range: u8,
        pub colour_primaries: u8,
        pub transfer_characteristics: u8,
        pub matrix_coefficients: u8,
        pub reserved: [u8; 3],
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct VAProcPipelineParameterBuffer {
        pub surface: u32,
        pub surface_region: *const VARectangle,
        pub surface_color_standard: u32,
        pub output_region: *const VARectangle,
        pub output_background_color: u32,
        pub output_color_standard: u32,
        pub pipeline_flags: u32,
        pub filter_flags: u32,
        pub filters: *mut u32,
        pub num_filters: u32,
        pub forward_references: *mut u32,
        pub num_forward_references: u32,
        pub backward_references: *mut u32,
        pub num_backward_references: u32,
        pub rotation_state: u32,
        pub blend_state: *const c_void,
        pub mirror_state: u32,
        pub additional_outputs: *mut u32,
        pub num_additional_outputs: u32,
        pub input_surface_flag: u32,
        pub output_surface_flag: u32,
        pub input_color_properties: VAProcColorProperties,
        pub output_color_properties: VAProcColorProperties,
        pub processing_mode: u32,
        pub output_hdr_metadata: *mut c_void,
        /// VA_PADDING_LARGE - 16 on 64-bit, where the pointers above take two words each.
        pub va_reserved: [u32; 16],
    }

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    pub struct VADRMPRIMEObject {
        pub fd: i32,
        pub size: u32,
        pub drm_format_modifier: u64,
    }

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    pub struct VADRMPRIMELayer {
        pub drm_format: u32,
        pub num_planes: u32,
        pub object_index: [u32; 4],
        pub offset: [u32; 4],
        pub pitch: [u32; 4],
    }

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    pub struct VADRMPRIMESurfaceDescriptor {
        pub fourcc: u32,
        pub width: u32,
        pub height: u32,
        pub num_objects: u32,
        pub objects: [VADRMPRIMEObject; 4],
        pub num_layers: u32,
        pub layers: [VADRMPRIMELayer; 4],
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
    vaGetConfigAttributes: fn(VADisplay, i32, i32, *mut VAConfigAttrib, i32) -> VAStatus;
    vaCreateConfig: fn(VADisplay, i32, i32, *mut VAConfigAttrib, i32, *mut u32) -> VAStatus;
    vaDestroyConfig: fn(VADisplay, u32) -> VAStatus;
    vaCreateSurfaces: fn(VADisplay, u32, u32, u32, *mut u32, u32, *mut VASurfaceAttrib, u32) -> VAStatus;
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
    vaMaxNumImageFormats: fn(VADisplay) -> i32;
    vaQueryImageFormats: fn(VADisplay, *mut VAImageFormat, *mut i32) -> VAStatus;
    vaCreateImage: fn(VADisplay, *mut VAImageFormat, i32, i32, *mut VAImage) -> VAStatus;
    vaDeriveImage: fn(VADisplay, u32, *mut VAImage) -> VAStatus;
    vaDestroyImage: fn(VADisplay, u32) -> VAStatus;
    vaPutImage: fn(VADisplay, u32, u32, i32, i32, u32, u32, i32, i32, u32, u32) -> VAStatus;
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

    fn profiles(&self) -> Vec<i32> {
        unsafe {
            let mut list = vec![0; (self.va.vaMaxNumProfiles)(self.dpy).max(1) as usize];
            let mut n = 0;
            if (self.va.vaQueryConfigProfiles)(self.dpy, list.as_mut_ptr(), &mut n) != VA_STATUS_SUCCESS {
                return Vec::new();
            }
            list.truncate(n.max(0) as usize);
            list
        }
    }

    fn entrypoints(&self, profile: i32) -> Vec<i32> {
        unsafe {
            let mut list = vec![0; (self.va.vaMaxNumEntrypoints)(self.dpy).max(1) as usize];
            let mut n = 0;
            if (self.va.vaQueryConfigEntrypoints)(self.dpy, profile, list.as_mut_ptr(), &mut n) != VA_STATUS_SUCCESS {
                return Vec::new();
            }
            list.truncate(n.max(0) as usize);
            list
        }
    }

    /// One config attribute's value for a profile and entrypoint (None: not supported).
    fn attribute(&self, profile: i32, entrypoint: i32, kind: i32) -> Option<u32> {
        let mut a = VAConfigAttrib { type_: kind, value: 0 };
        let s = unsafe { (self.va.vaGetConfigAttributes)(self.dpy, profile, entrypoint, &mut a, 1) };
        (s == VA_STATUS_SUCCESS && a.value != VA_ATTRIB_NOT_SUPPORTED).then_some(a.value)
    }

    fn surfaces(&self, rt_format: u32, fourcc: u32, w: u32, h: u32, n: usize) -> Result<Vec<u32>, String> {
        let mut attrib = VASurfaceAttrib {
            type_: VA_SURFACE_ATTRIB_PIXEL_FORMAT,
            flags: VA_SURFACE_ATTRIB_SETTABLE,
            value: VAGenericValue {
                type_: VA_GENERIC_VALUE_TYPE_INTEGER,
                value: VAGenericValueUnion { i: fourcc as i32 },
            },
        };
        let mut ids = vec![VA_INVALID_ID; n];
        let s = unsafe { (self.va.vaCreateSurfaces)(self.dpy, rt_format, w, h, ids.as_mut_ptr(), n as u32, &mut attrib, 1) };
        self.check("creating surfaces", s)?;
        Ok(ids)
    }

    /// A parameter buffer holding `value`.
    fn buffer<T>(&self, context: u32, kind: i32, value: &T) -> Result<u32, String> {
        self.bytes_buffer(context, kind, unsafe {
            std::slice::from_raw_parts((value as *const T).cast(), std::mem::size_of::<T>())
        })
    }

    fn bytes_buffer(&self, context: u32, kind: i32, data: &[u8]) -> Result<u32, String> {
        let mut id = VA_INVALID_ID;
        let s = unsafe { (self.va.vaCreateBuffer)(self.dpy, context, kind, data.len() as u32, 1, data.as_ptr() as *mut c_void, &mut id) };
        self.check("creating a buffer", s)?;
        Ok(id)
    }

    /// Runs one picture: the buffers into `target` on `context` (destroyed after, as libva 2 leaves
    /// them to the caller).
    fn picture(&self, context: u32, target: u32, mut buffers: Vec<u32>) -> Result<(), String> {
        let va = self.va;
        let r = unsafe {
            self.check("vaBeginPicture", (va.vaBeginPicture)(self.dpy, context, target)).and_then(|_| {
                let rendered = self.check(
                    "vaRenderPicture",
                    (va.vaRenderPicture)(self.dpy, context, buffers.as_mut_ptr(), buffers.len() as i32),
                );
                // Ended even when rendering failed: the context is ready for the next picture.
                let ended = self.check("vaEndPicture", (va.vaEndPicture)(self.dpy, context));
                rendered.and(ended)
            })
        };
        for b in buffers {
            unsafe { (va.vaDestroyBuffer)(self.dpy, b) };
        }
        r
    }

    fn image_format(&self, fourcc: u32) -> Option<VAImageFormat> {
        unsafe {
            let mut list = vec![VAImageFormat::default(); (self.va.vaMaxNumImageFormats)(self.dpy).max(1) as usize];
            let mut n = 0;
            if (self.va.vaQueryImageFormats)(self.dpy, list.as_mut_ptr(), &mut n) != VA_STATUS_SUCCESS {
                return None;
            }
            list.truncate(n.max(0) as usize);
            list.into_iter().find(|f| f.fourcc == fourcc)
        }
    }
}

impl Drop for Display {
    fn drop(&mut self) {
        unsafe { (self.va.vaTerminate)(self.dpy) };
    }
}

fn err_str(va: &Va, s: VAStatus) -> String {
    let p = unsafe { (va.vaErrorStr)(s) };
    if p.is_null() {
        format!("status {s}")
    } else {
        unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
    }
}

/// Writing a surface's pixels from the CPU: through the surface's own memory (vaDeriveImage) where
/// the driver allows it, else through an image copied in (vaPutImage).
struct Upload {
    surface: u32,
    fourcc: u32,
    /// The image vaPutImage copies from, once deriving has failed.
    image: Option<VAImage>,
}

/// The GPU's colour conversion (VA video processing): a BGRX surface (from memory, or the screen's
/// own buffer) into the encoder's NV12.
struct Vpp {
    config: u32,
    context: u32,
}

pub struct Vaapi {
    d: Display,
    config: u32,
    context: u32,
    vpp: Option<Vpp>,
    /// The encoder's input (NV12), what VPP writes into or the CPU fills.
    input: u32,
    /// Frames from memory: the BGRX surface for VPP, or the NV12 input itself when there's no VPP.
    upload: Upload,
    /// The CPU's conversion, when there's no VPP.
    nv12: Vec<u8>,
    /// Reconstructed frames: the one being written and the one it refers to, taking turns.
    recon: [u32; 2],
    coded: u32,
    coded_size: usize,
    params: Params,
    /// The packed headers the driver takes from us (VA_ENC_PACKED_HEADER_*).
    packed: u32,
    rate_control: u32,
    entrypoint: i32,
    fps: u32,
    kbps: u32,
    /// The bitrate changed: the next frame carries the new one.
    kbps_changed: bool,
    keyframe_next: bool,
    /// Frames since the IDR (frame_num, and the picture order count, count from it).
    since_idr: u32,
    /// Rolling intra refresh, when the driver does it: rows of macroblocks coded intra each P-frame,
    /// a band moving down the picture, so a frame lost on the way is healed within half a second
    /// whether or not a keyframe comes.
    intra_refresh: bool,
    idr_pic_id: u16,
    /// Which recon surface this frame writes (the other holds the frame it refers to).
    cur: usize,
    /// Logged once: frames came straight from the GPU's memory.
    zero_copy_seen: bool,
    /// The GPU has converted a frame (until then, a failure falls back to the CPU's conversion).
    vpp_works: bool,
}

// The display and its objects are only used by whoever holds the encoder.
unsafe impl Send for Vaapi {}

/// Tried in order: low power first (Intel's fixed-function encoder: lower latency, less power), Main
/// before Constrained Baseline (CABAC: the same picture in fewer bits).
const CHOICES: [(i32, i32, Profile); 4] = [
    (VA_ENTRYPOINT_ENC_SLICE_LP, VA_PROFILE_H264_MAIN, Profile::Main),
    (VA_ENTRYPOINT_ENC_SLICE_LP, VA_PROFILE_H264_CONSTRAINED_BASELINE, Profile::ConstrainedBaseline),
    (VA_ENTRYPOINT_ENC_SLICE, VA_PROFILE_H264_MAIN, Profile::Main),
    (VA_ENTRYPOINT_ENC_SLICE, VA_PROFILE_H264_CONSTRAINED_BASELINE, Profile::ConstrainedBaseline),
];

impl Vaapi {
    pub fn new(codec: Codec, width: usize, height: usize, fps: f32, bitrate_kbps: u32) -> Result<Self, String> {
        if codec != Codec::H264 {
            return Err(format!("VA-API: no {codec:?} encoder here (H.264 only)"));
        }
        let (width, height) = ((width & !1) as u32, (height & !1) as u32);
        if width < 16 || height < 16 {
            return Err("VA-API: the picture is too small".into());
        }
        va()?;
        let nodes = devices();
        if nodes.is_empty() {
            return Err("VA-API: no GPU render node (/dev/dri/renderD*)".into());
        }
        let mut why = Vec::new();
        for path in nodes {
            match Display::open(&path).and_then(|d| Self::on(d, width, height, fps, bitrate_kbps)) {
                Ok(e) => return Ok(e),
                Err(e) => why.push(e),
            }
        }
        Err(format!("VA-API: {}", why.join("; ")))
    }

    /// The encoder on one GPU, when it encodes H.264.
    fn on(d: Display, width: u32, height: u32, fps: f32, kbps: u32) -> Result<Self, String> {
        let profiles = d.profiles();
        let mut tried = Vec::new();
        for (entrypoint, va_profile, profile) in CHOICES {
            if !profiles.contains(&va_profile) || !d.entrypoints(va_profile).contains(&entrypoint) {
                continue;
            }
            let attr = |kind| d.attribute(va_profile, entrypoint, kind);
            if attr(VA_CONFIG_ATTRIB_RT_FORMAT).is_some_and(|f| f & VA_RT_FORMAT_YUV420 == 0) {
                continue;
            }
            // One reference in list 0 is all this needs (0: the driver doesn't say).
            if attr(VA_CONFIG_ATTRIB_ENC_MAX_REF_FRAMES).is_some_and(|n| n & 0xffff == 0) {
                continue;
            }
            let rc = attr(VA_CONFIG_ATTRIB_RATE_CONTROL).unwrap_or(0);
            let rate_control = if rc & VA_RC_CBR != 0 {
                VA_RC_CBR
            } else if rc & VA_RC_VBR != 0 {
                VA_RC_VBR
            } else {
                tried.push(format!("{} has no bitrate control", what(entrypoint, profile)));
                continue;
            };
            // Our SPS and PPS where it takes them (so the stream says what we mean), and our slice headers
            // where it takes those (ffmpeg does the same; drivers that want them get them).
            let packed = attr(VA_CONFIG_ATTRIB_ENC_PACKED_HEADERS).unwrap_or(0) & (VA_ENC_PACKED_HEADER_SEQUENCE | VA_ENC_PACKED_HEADER_SLICE);
            let rir = attr(VA_CONFIG_ATTRIB_ENC_INTRA_REFRESH).is_some_and(|v| v & VA_ENC_INTRA_REFRESH_ROLLING_ROW != 0);
            let params = Params {
                profile,
                level_idc: h264::level(width, height, fps, kbps),
                width,
                height,
            };
            let mut attribs = vec![
                VAConfigAttrib {
                    type_: VA_CONFIG_ATTRIB_RT_FORMAT,
                    value: VA_RT_FORMAT_YUV420,
                },
                VAConfigAttrib {
                    type_: VA_CONFIG_ATTRIB_RATE_CONTROL,
                    value: rate_control,
                },
            ];
            if packed != 0 {
                attribs.push(VAConfigAttrib {
                    type_: VA_CONFIG_ATTRIB_ENC_PACKED_HEADERS,
                    value: packed,
                });
            }
            if rir {
                attribs.push(VAConfigAttrib {
                    type_: VA_CONFIG_ATTRIB_ENC_INTRA_REFRESH,
                    value: VA_ENC_INTRA_REFRESH_ROLLING_ROW,
                });
            }
            let mut config = VA_INVALID_ID;
            let s = unsafe { (d.va.vaCreateConfig)(d.dpy, va_profile, entrypoint, attribs.as_mut_ptr(), attribs.len() as i32, &mut config) };
            if s != VA_STATUS_SUCCESS {
                tried.push(format!("{}: {}", what(entrypoint, profile), err_str(d.va, s)));
                continue;
            }
            return Self::with_config(d, config, entrypoint, params, packed, rate_control, fps, kbps).map(|mut e| {
                e.intra_refresh = rir;
                e
            });
        }
        Err(format!(
            "{} has no H.264 encoder{}",
            d.vendor,
            if tried.is_empty() {
                String::new()
            } else {
                format!(" ({})", tried.join("; "))
            }
        ))
    }

    #[allow(clippy::too_many_arguments)]
    fn with_config(d: Display, config: u32, entrypoint: i32, params: Params, packed: u32, rate_control: u32, fps: f32, kbps: u32) -> Result<Self, String> {
        let (cw, ch) = (params.width_mbs() * 16, params.height_mbs() * 16);
        // Everything is made into this first, so a failure part way frees what was made (Drop).
        let mut e = Self {
            d,
            config,
            context: VA_INVALID_ID,
            vpp: None,
            input: VA_INVALID_ID,
            upload: Upload {
                surface: VA_INVALID_ID,
                fourcc: VA_FOURCC_NV12,
                image: None,
            },
            nv12: Vec::new(),
            recon: [VA_INVALID_ID; 2],
            coded: VA_INVALID_ID,
            // A frame never needs more than its raw size; a keyframe at a high bitrate comes close.
            coded_size: (cw * ch * 3 / 2).max(256 * 1024) as usize,
            params,
            packed,
            rate_control,
            entrypoint,
            fps: fps.round().max(1.0) as u32,
            kbps,
            kbps_changed: false,
            keyframe_next: true,
            since_idr: 0,
            intra_refresh: false,
            idr_pic_id: 0,
            cur: 0,
            zero_copy_seen: false,
            vpp_works: false,
        };
        // Surfaces in whole macroblocks (the SPS crops to the picture).
        e.input = e.d.surfaces(VA_RT_FORMAT_YUV420, VA_FOURCC_NV12, cw, ch, 1)?[0];
        let recon = e.d.surfaces(VA_RT_FORMAT_YUV420, VA_FOURCC_NV12, cw, ch, 2)?;
        e.recon = [recon[0], recon[1]];
        let s = unsafe { (e.d.va.vaCreateContext)(e.d.dpy, config, cw as i32, ch as i32, VA_PROGRESSIVE, e.recon.as_mut_ptr(), 2, &mut e.context) };
        e.d.check("creating the encoder", s)?;
        let mut coded = VA_INVALID_ID;
        let s = unsafe {
            (e.d.va.vaCreateBuffer)(
                e.d.dpy,
                e.context,
                VA_ENC_CODED_BUFFER,
                e.coded_size as u32,
                1,
                std::ptr::null_mut(),
                &mut coded,
            )
        };
        e.d.check("creating the output buffer", s)?;
        e.coded = coded;
        // The GPU converts the colour where it can; else the CPU does, into the NV12 input.
        match e.open_vpp() {
            Ok((vpp, upload)) => {
                e.vpp = Some(vpp);
                e.upload.surface = upload;
                e.upload.fourcc = VA_FOURCC_BGRX;
            }
            Err(why) => {
                eprintln!("rein-remote: VA-API: no colour conversion on the GPU ({why}): converting on the CPU");
                e.upload.surface = e.input;
            }
        }
        Ok(e)
    }

    fn open_vpp(&mut self) -> Result<(Vpp, u32), String> {
        let d = &self.d;
        if !d.entrypoints(VA_PROFILE_NONE).contains(&VA_ENTRYPOINT_VIDEO_PROC) {
            return Err("no video processing".into());
        }
        let mut config = VA_INVALID_ID;
        let s = unsafe { (d.va.vaCreateConfig)(d.dpy, VA_PROFILE_NONE, VA_ENTRYPOINT_VIDEO_PROC, std::ptr::null_mut(), 0, &mut config) };
        d.check("creating the converter", s)?;
        let (cw, ch) = (self.params.width_mbs() * 16, self.params.height_mbs() * 16);
        let mut context = VA_INVALID_ID;
        let s = unsafe { (d.va.vaCreateContext)(d.dpy, config, cw as i32, ch as i32, VA_PROGRESSIVE, &mut self.input, 1, &mut context) };
        if let Err(e) = d.check("creating the converter", s) {
            unsafe { (d.va.vaDestroyConfig)(d.dpy, config) };
            return Err(e);
        }
        let vpp = Vpp { config, context };
        match d.surfaces(VA_RT_FORMAT_RGB32, VA_FOURCC_BGRX, self.params.width, self.params.height, 1) {
            Ok(s) => Ok((vpp, s[0])),
            Err(e) => {
                unsafe {
                    (d.va.vaDestroyContext)(d.dpy, vpp.context);
                    (d.va.vaDestroyConfig)(d.dpy, vpp.config);
                }
                Err(e)
            }
        }
    }

    pub fn size(&self) -> (usize, usize) {
        (self.params.width as usize, self.params.height as usize)
    }

    pub fn set_bitrate(&mut self, kbps: u32) -> Result<(), String> {
        if kbps != self.kbps {
            self.kbps = kbps;
            self.kbps_changed = true;
        }
        Ok(())
    }

    /// A frame from memory: into the upload surface (its own memory where the driver lets us map it),
    /// converted on the GPU when there's VPP, then encoded.
    pub fn encode(&mut self, bgra: &[u8], stride: usize, tag: u64) -> Result<Vec<Encoded>, String> {
        let (w, h) = (self.params.width as usize, self.params.height as usize);
        if stride < w * 4 || bgra.len() < stride * (h - 1) + w * 4 {
            return Err("VA-API: the frame is smaller than the stream".into());
        }
        // No colour conversion on the GPU: the CPU converts (then copies the rows at the surface's pitch).
        let mut nv12 = std::mem::take(&mut self.nv12);
        if self.vpp.is_none() {
            nv12.resize(w * h * 3 / 2, 0);
            crate::yuv::nv12(bgra, stride, w, h, &mut nv12);
        }
        let r = self.write_upload(|fourcc, dst, pitches, offsets| unsafe {
            if fourcc == VA_FOURCC_NV12 {
                let (y, uv) = nv12.split_at(w * h);
                for row in 0..h {
                    std::ptr::copy_nonoverlapping(y.as_ptr().add(row * w), dst.add(offsets[0] as usize + row * pitches[0] as usize), w);
                }
                for row in 0..h / 2 {
                    std::ptr::copy_nonoverlapping(uv.as_ptr().add(row * w), dst.add(offsets[1] as usize + row * pitches[1] as usize), w);
                }
            } else {
                for row in 0..h {
                    std::ptr::copy_nonoverlapping(bgra.as_ptr().add(row * stride), dst.add(offsets[0] as usize + row * pitches[0] as usize), w * 4);
                }
            }
        });
        self.nv12 = nv12;
        r?;
        if self.vpp.is_some() {
            let full = VARectangle {
                x: 0,
                y: 0,
                width: w as u16,
                height: h as u16,
            };
            match self.convert(self.upload.surface, full) {
                Ok(()) => self.vpp_works = true,
                // A driver that lists video processing but can't do this conversion: the CPU does it
                // from now on, this frame included.
                Err(e) if !self.vpp_works => {
                    eprintln!("rein-remote: VA-API: the GPU's colour conversion failed ({e}): converting on the CPU");
                    self.drop_vpp();
                    return self.encode(bgra, stride, tag);
                }
                Err(e) => return Err(e),
            }
        }
        self.encode_input(tag)
    }

    /// Fills the upload surface from the CPU: `fill(fourcc, base, pitches, offsets)`.
    fn write_upload(&mut self, fill: impl FnOnce(u32, *mut u8, [u32; 3], [u32; 3])) -> Result<(), String> {
        let (d, up) = (&self.d, &mut self.upload);
        let (w, h) = (self.params.width, self.params.height);
        unsafe {
            if up.image.is_none() {
                let mut img = VAImage::default();
                if (d.va.vaDeriveImage)(d.dpy, up.surface, &mut img) == VA_STATUS_SUCCESS {
                    let mut p = std::ptr::null_mut();
                    let mapped = d.check("mapping the frame", (d.va.vaMapBuffer)(d.dpy, img.buf, &mut p));
                    if mapped.is_ok() {
                        fill(img.format.fourcc, p.cast(), img.pitches, img.offsets);
                        (d.va.vaUnmapBuffer)(d.dpy, img.buf);
                    }
                    (d.va.vaDestroyImage)(d.dpy, img.image_id);
                    return mapped;
                }
                // The surface's memory isn't for the CPU (tiled, or in VRAM): an image to copy in.
                let fmt = d
                    .image_format(up.fourcc)
                    .or_else(|| (up.fourcc == VA_FOURCC_BGRX).then(|| d.image_format(VA_FOURCC_BGRA)).flatten());
                let mut fmt = fmt.ok_or("VA-API: no image format for the upload")?;
                let mut img = VAImage::default();
                d.check("creating an image", (d.va.vaCreateImage)(d.dpy, &mut fmt, w as i32, h as i32, &mut img))?;
                up.image = Some(img);
            }
            let img = up.image.unwrap();
            let mut p = std::ptr::null_mut();
            d.check("mapping the image", (d.va.vaMapBuffer)(d.dpy, img.buf, &mut p))?;
            fill(img.format.fourcc, p.cast(), img.pitches, img.offsets);
            (d.va.vaUnmapBuffer)(d.dpy, img.buf);
            d.check(
                "copying the image in",
                (d.va.vaPutImage)(d.dpy, up.surface, img.image_id, 0, 0, w, h, 0, 0, w, h),
            )
        }
    }

    /// No more GPU conversion: frames from memory go straight into the NV12 input (converted by the CPU).
    fn drop_vpp(&mut self) {
        let (va, dpy) = (self.d.va, self.d.dpy);
        unsafe {
            if let Some(img) = self.upload.image.take() {
                (va.vaDestroyImage)(dpy, img.image_id);
            }
            if let Some(vpp) = self.vpp.take() {
                (va.vaDestroyContext)(dpy, vpp.context);
                (va.vaDestroyConfig)(dpy, vpp.config);
            }
            if self.upload.surface != self.input {
                (va.vaDestroySurfaces)(dpy, &mut self.upload.surface, 1);
            }
        }
        self.upload = Upload {
            surface: self.input,
            fourcc: VA_FOURCC_NV12,
            image: None,
        };
    }

    /// The GPU converts `region` of an RGB surface into the encoder's NV12 input (BT.601, limited
    /// range: what the SPS says, and what yuv.rs does).
    fn convert(&mut self, surface: u32, region: VARectangle) -> Result<(), String> {
        let vpp = self.vpp.as_ref().ok_or("VA-API: no colour conversion on the GPU")?;
        let out = VARectangle {
            x: 0,
            y: 0,
            width: self.params.width as u16,
            height: self.params.height as u16,
        };
        let mut p: VAProcPipelineParameterBuffer = unsafe { std::mem::zeroed() };
        p.surface = surface;
        p.surface_region = &region;
        p.surface_color_standard = VA_PROC_COLOR_STANDARD_SRGB;
        p.output_region = &out;
        p.output_color_standard = VA_PROC_COLOR_STANDARD_BT601;
        p.filter_flags = VA_FILTER_SCALING_FAST;
        p.input_color_properties.color_range = VA_SOURCE_RANGE_FULL;
        p.output_color_properties.color_range = VA_SOURCE_RANGE_REDUCED;
        // The black around a picture smaller than the surface (alpha 0xff, ARGB).
        p.output_background_color = 0xff00_0000;
        let buf = self.d.buffer(vpp.context, VA_PROC_PIPELINE_PARAMETER_BUFFER, &p)?;
        self.d.picture(vpp.context, self.input, vec![buf])
    }

    /// A frame from KMS: the screen's buffer imported as it is (no copy), converted on the GPU, encoded.
    pub fn encode_surface(&mut self, surface: &crate::capture::Surface, tag: u64) -> Result<Vec<Encoded>, String> {
        let buf = &surface.0;
        let imported = self.import(buf)?;
        let region = VARectangle {
            x: buf.crop.0 as i16,
            y: buf.crop.1 as i16,
            width: buf.crop.2.min(self.params.width) as u16,
            height: buf.crop.3.min(self.params.height) as u16,
        };
        let r = self.convert(imported, region).and_then(|_| self.encode_input(tag));
        // Done with it (the encode waited for the GPU): the screen's buffer is the screen's again.
        let mut s = imported;
        unsafe { (self.d.va.vaDestroySurfaces)(self.d.dpy, &mut s, 1) };
        if r.is_ok() && !std::mem::replace(&mut self.zero_copy_seen, true) {
            eprintln!(
                "rein-remote: VA-API: frames straight from the screen's buffer ({}, modifier {:#x}), no copy",
                fourcc_str(buf.format),
                buf.modifier
            );
        }
        r
    }

    /// A DMA-BUF as a VA surface (VA_SURFACE_ATTRIB_MEM_TYPE_DRM_PRIME_2), its layout as KMS gave it.
    fn import(&self, buf: &crate::capture::kms::DmaBuf) -> Result<u32, String> {
        let (fourcc, rt_format) = va_format(buf.format).ok_or_else(|| format!("VA-API: can't read a {} screen", fourcc_str(buf.format)))?;
        if buf.objects.is_empty() || buf.objects.len() > 4 || buf.planes.is_empty() || buf.planes.len() > 4 {
            return Err("VA-API: a screen buffer with an odd layout".into());
        }
        let mut desc = VADRMPRIMESurfaceDescriptor {
            fourcc,
            width: buf.width,
            height: buf.height,
            num_objects: buf.objects.len() as u32,
            num_layers: 1,
            ..Default::default()
        };
        for (o, (fd, size)) in desc.objects.iter_mut().zip(&buf.objects) {
            *o = VADRMPRIMEObject {
                fd: fd.as_raw_fd(),
                size: *size,
                drm_format_modifier: buf.modifier,
            };
        }
        let layer = &mut desc.layers[0];
        layer.drm_format = buf.format;
        layer.num_planes = buf.planes.len() as u32;
        for (i, p) in buf.planes.iter().enumerate() {
            layer.object_index[i] = p.object as u32;
            layer.offset[i] = p.offset;
            layer.pitch[i] = p.pitch;
        }
        let mut attribs = [
            VASurfaceAttrib {
                type_: VA_SURFACE_ATTRIB_MEMORY_TYPE,
                flags: VA_SURFACE_ATTRIB_SETTABLE,
                value: VAGenericValue {
                    type_: VA_GENERIC_VALUE_TYPE_INTEGER,
                    value: VAGenericValueUnion {
                        i: VA_SURFACE_ATTRIB_MEM_TYPE_DRM_PRIME_2 as i32,
                    },
                },
            },
            VASurfaceAttrib {
                type_: VA_SURFACE_ATTRIB_EXTERNAL_BUFFER_DESCRIPTOR,
                flags: VA_SURFACE_ATTRIB_SETTABLE,
                value: VAGenericValue {
                    type_: VA_GENERIC_VALUE_TYPE_POINTER,
                    value: VAGenericValueUnion {
                        p: &mut desc as *mut VADRMPRIMESurfaceDescriptor as *mut c_void,
                    },
                },
            },
        ];
        let mut id = VA_INVALID_ID;
        let s = unsafe { (self.d.va.vaCreateSurfaces)(self.d.dpy, rt_format, buf.width, buf.height, &mut id, 1, attribs.as_mut_ptr(), 2) };
        self.d.check(
            &format!("importing the screen's buffer ({}, modifier {:#x})", fourcc_str(buf.format), buf.modifier),
            s,
        )?;
        Ok(id)
    }

    /// Encodes what's in the NV12 input: an IDR when one is due, else a P-frame on the frame before.
    fn encode_input(&mut self, tag: u64) -> Result<Vec<Encoded>, String> {
        let idr = std::mem::take(&mut self.keyframe_next);
        if idr {
            self.since_idr = 0;
            self.idr_pic_id = self.idr_pic_id.wrapping_add(1);
        }
        let r = self.render(idr).and_then(|_| self.read_output(idr, tag));
        if r.is_err() {
            // Whatever happened to this frame, the next starts the stream again.
            self.keyframe_next = true;
        } else {
            self.since_idr = self.since_idr.wrapping_add(1);
            self.cur ^= 1;
        }
        r.map(|e| e.into_iter().collect())
    }

    fn render(&mut self, idr: bool) -> Result<(), String> {
        let d = &self.d;
        let p = &self.params;
        let ctx = self.context;
        let rate_changed = idr || self.kbps_changed;
        let mut bufs = Vec::new();
        let r = (|| {
            let bps = self.kbps.saturating_mul(1000);
            if idr {
                bufs.push(d.buffer(ctx, VA_ENC_SEQUENCE_PARAMETER_BUFFER, &self.sequence())?);
                bufs.push(d.buffer(
                    ctx,
                    VA_ENC_MISC_PARAMETER_BUFFER,
                    &VAEncMiscParameter {
                        type_: VA_ENC_MISC_FRAME_RATE,
                        data: VAEncMiscParameterFrameRate {
                            framerate: self.fps,
                            ..Default::default()
                        },
                    },
                )?);
            }
            if rate_changed {
                // CBR to the bitrate; no frames skipped and no padding to fill it (a still screen
                // should cost nothing), and one second of buffer like ffmpeg's default.
                let rc = VAEncMiscParameterRateControl {
                    bits_per_second: bps,
                    target_percentage: 100,
                    window_size: 1000,
                    rc_flags: RC_NO_SKIP_NO_STUFFING,
                    ..Default::default()
                };
                bufs.push(d.buffer(
                    ctx,
                    VA_ENC_MISC_PARAMETER_BUFFER,
                    &VAEncMiscParameter {
                        type_: VA_ENC_MISC_RATE_CONTROL,
                        data: rc,
                    },
                )?);
                let hrd = VAEncMiscParameterHRD {
                    initial_buffer_fullness: bps / 4 * 3,
                    buffer_size: bps,
                    ..Default::default()
                };
                bufs.push(d.buffer(
                    ctx,
                    VA_ENC_MISC_PARAMETER_BUFFER,
                    &VAEncMiscParameter {
                        type_: VA_ENC_MISC_HRD,
                        data: hrd,
                    },
                )?);
            }
            if idr && self.packed & VA_ENC_PACKED_HEADER_SEQUENCE != 0 {
                let headers = [p.sps(), p.pps()].concat();
                self.packed_header(&mut bufs, VA_ENC_PACKED_HEADER_TYPE_SEQUENCE, &headers, headers.len() * 8)?;
            }
            if self.intra_refresh && !idr {
                let (location, size) = rir_band(p.height_mbs() as u32, self.fps, self.since_idr);
                bufs.push(d.buffer(
                    ctx,
                    VA_ENC_MISC_PARAMETER_BUFFER,
                    &VAEncMiscParameter {
                        type_: VA_ENC_MISC_RIR,
                        data: VAEncMiscParameterRIR {
                            rir_flags: VA_ENC_INTRA_REFRESH_ROLLING_ROW,
                            intra_insertion_location: location,
                            intra_insert_size: size,
                            ..Default::default()
                        },
                    },
                )?);
            }
            bufs.push(d.buffer(ctx, VA_ENC_PICTURE_PARAMETER_BUFFER, &self.picture_params(idr))?);
            if self.packed & VA_ENC_PACKED_HEADER_SLICE != 0 {
                let (header, bits) = p.slice_header(idr, self.since_idr, self.idr_pic_id as u32);
                self.packed_header(&mut bufs, VA_ENC_PACKED_HEADER_TYPE_SLICE, &header, bits)?;
            }
            bufs.push(d.buffer(ctx, VA_ENC_SLICE_PARAMETER_BUFFER, &self.slice_params(idr))?);
            Ok(())
        })();
        if let Err(e) = r {
            for b in bufs {
                unsafe { (d.va.vaDestroyBuffer)(d.dpy, b) };
            }
            return Err(e);
        }
        d.picture(ctx, self.input, bufs)?;
        self.kbps_changed = false;
        Ok(())
    }

    fn packed_header(&self, bufs: &mut Vec<u32>, kind: u32, data: &[u8], bits: usize) -> Result<(), String> {
        let param = VAEncPackedHeaderParameterBuffer {
            type_: kind,
            bit_length: bits as u32,
            has_emulation_bytes: 1,
            ..Default::default()
        };
        bufs.push(self.d.buffer(self.context, VA_ENC_PACKED_HEADER_PARAMETER_BUFFER, &param)?);
        bufs.push(self.d.bytes_buffer(self.context, VA_ENC_PACKED_HEADER_DATA_BUFFER, data)?);
        Ok(())
    }

    /// The sequence as the SPS says it (the driver writes its own SPS from this when it doesn't take ours).
    fn sequence(&self) -> VAEncSequenceParameterBufferH264 {
        let p = &self.params;
        let mut s: VAEncSequenceParameterBufferH264 = unsafe { std::mem::zeroed() };
        s.level_idc = p.level_idc;
        // Hints for the rate control (frame types are ours: an IDR only when asked for).
        s.intra_period = 2048;
        s.intra_idr_period = 2048;
        s.ip_period = 1;
        s.bits_per_second = self.kbps.saturating_mul(1000);
        s.max_num_ref_frames = 1;
        s.picture_width_in_mbs = p.width_mbs() as u16;
        s.picture_height_in_mbs = p.height_mbs() as u16;
        s.seq_fields = seq_fields(h264::LOG2_MAX_FRAME_NUM - 4, 2);
        let (right, bottom) = p.crop();
        s.frame_cropping_flag = (right != 0 || bottom != 0) as u8;
        s.frame_crop_right_offset = right;
        s.frame_crop_bottom_offset = bottom;
        s.vui_parameters_present_flag = 1;
        s.vui_fields = vui_fields(15, 15);
        s.num_units_in_tick = 1;
        s.time_scale = self.fps * 2;
        s
    }

    fn picture_params(&self, idr: bool) -> VAEncPictureParameterBufferH264 {
        let p = &self.params;
        let mut pic: VAEncPictureParameterBufferH264 = unsafe { std::mem::zeroed() };
        pic.CurrPic = self.picture(self.cur, self.since_idr);
        pic.ReferenceFrames = [VAPictureH264::INVALID; 16];
        if !idr {
            pic.ReferenceFrames[0] = self.reference();
        }
        pic.coded_buf = self.coded;
        pic.frame_num = self.frame_num();
        pic.pic_init_qp = h264::PIC_INIT_QP as u8;
        pic.pic_fields = pic_fields(idr, p.cabac());
        pic
    }

    fn slice_params(&self, idr: bool) -> VAEncSliceParameterBufferH264 {
        let p = &self.params;
        let mut s: VAEncSliceParameterBufferH264 = unsafe { std::mem::zeroed() };
        s.num_macroblocks = p.width_mbs() * p.height_mbs();
        s.macroblock_info = VA_INVALID_ID;
        s.slice_type = if idr { h264::SLICE_I } else { h264::SLICE_P };
        s.idr_pic_id = self.idr_pic_id;
        s.RefPicList0 = [VAPictureH264::INVALID; 32];
        s.RefPicList1 = [VAPictureH264::INVALID; 32];
        if !idr {
            s.RefPicList0[0] = self.reference();
        }
        s
    }

    fn frame_num(&self) -> u16 {
        (self.since_idr % (1 << h264::LOG2_MAX_FRAME_NUM)) as u16
    }

    /// A frame `n` frames after the IDR, reconstructed into recon surface `i`.
    fn picture(&self, i: usize, n: u32) -> VAPictureH264 {
        VAPictureH264 {
            picture_id: self.recon[i],
            frame_idx: n % (1 << h264::LOG2_MAX_FRAME_NUM),
            flags: 0,
            // pic_order_cnt_type 2: twice the frame's place in the stream.
            TopFieldOrderCnt: (n as i32).wrapping_mul(2),
            BottomFieldOrderCnt: (n as i32).wrapping_mul(2),
            va_reserved: [0; 4],
        }
    }

    /// The frame before, which this one refers to.
    fn reference(&self) -> VAPictureH264 {
        VAPictureH264 {
            flags: VA_PICTURE_H264_SHORT_TERM_REFERENCE,
            ..self.picture(self.cur ^ 1, self.since_idr.wrapping_sub(1))
        }
    }

    /// Waits for the frame, and gives it back in Annex B: our SPS and PPS first on an IDR if the driver
    /// didn't write them.
    fn read_output(&mut self, idr: bool, tag: u64) -> Result<Option<Encoded>, String> {
        let d = &self.d;
        d.check("encoding", unsafe { (d.va.vaSyncSurface)(d.dpy, self.input) })?;
        let mut data = Vec::new();
        let mut overflow = false;
        unsafe {
            let mut seg: *mut c_void = std::ptr::null_mut();
            d.check("reading the frame", (d.va.vaMapBuffer)(d.dpy, self.coded, &mut seg))?;
            while !seg.is_null() {
                let s = &*(seg as *const VACodedBufferSegment);
                overflow |= s.status & VA_CODED_BUF_STATUS_SLICE_OVERFLOW_MASK != 0;
                if !s.buf.is_null() {
                    data.extend_from_slice(std::slice::from_raw_parts(s.buf as *const u8, s.size as usize));
                }
                seg = s.next;
            }
            (d.va.vaUnmapBuffer)(d.dpy, self.coded);
        }
        if overflow {
            return Err("VA-API: the frame didn't fit its buffer".into());
        }
        if data.is_empty() {
            return Ok(None);
        }
        if idr && !nal_types(&data).any(|t| t == h264::NAL_SPS) {
            data = [self.params.sps(), self.params.pps(), data].concat();
        }
        Ok(Some(Encoded { data, keyframe: idr, tag }))
    }
}

/// NAL unit types in an Annex B stream, in order.
fn nal_types(data: &[u8]) -> impl Iterator<Item = u8> + '_ {
    data.windows(4).filter(|w| w[..3] == [0, 0, 1]).map(|w| w[3] & 31)
}

fn what(entrypoint: i32, profile: Profile) -> String {
    format!("{profile:?}{}", if entrypoint == VA_ENTRYPOINT_ENC_SLICE_LP { " (low power)" } else { "" })
}

/// The SPS fields VA-API takes as bits: 4:2:0, frames only, direct 8×8 inference.
fn seq_fields(log2_max_frame_num_minus4: u32, pic_order_cnt_type: u32) -> u32 {
    1 | 1 << 2 | 1 << 5 | (log2_max_frame_num_minus4 & 15) << 6 | (pic_order_cnt_type & 3) << 10
}

/// The VUI fields: bitstream restriction (no reordering), motion vectors over the edges.
fn vui_fields(log2_max_mv_length_horizontal: u32, log2_max_mv_length_vertical: u32) -> u32 {
    1 << 2 | (log2_max_mv_length_horizontal & 31) << 3 | (log2_max_mv_length_vertical & 31) << 8 | 1 << 15
}

/// The PPS and picture fields: every frame a reference, deblocking control present (the slice header
/// carries it), CABAC for Main.
/// Rate control's flags: disable_frame_skip and disable_bit_stuffing.
const RC_NO_SKIP_NO_STUFFING: u32 = 1 << 1 | 1 << 2;

fn pic_fields(idr: bool, cabac: bool) -> u32 {
    idr as u32 | 1 << 1 | (cabac as u32) << 3 | 1 << 9
}

/// A DRM format as VA-API names it, and its surface format: the screens KMS scans out.
fn va_format(drm: u32) -> Option<(u32, u32)> {
    Some(match &drm.to_le_bytes() {
        b"XR24" => (VA_FOURCC_BGRX, VA_RT_FORMAT_RGB32),
        b"AR24" => (VA_FOURCC_BGRA, VA_RT_FORMAT_RGB32),
        b"XB24" => (VA_FOURCC_RGBX, VA_RT_FORMAT_RGB32),
        b"AB24" => (VA_FOURCC_RGBA, VA_RT_FORMAT_RGB32),
        b"XR30" => (VA_FOURCC_X2R10G10B10, VA_RT_FORMAT_RGB32_10),
        _ => return None,
    })
}

pub fn fourcc_str(f: u32) -> String {
    f.to_le_bytes().iter().map(|&c| if c.is_ascii_graphic() { c as char } else { '?' }).collect()
}

impl VideoEncoder for Vaapi {
    fn name(&self) -> String {
        format!(
            "H.264 in hardware (VA-API: {}, {:?}{}, {}{})",
            self.d.vendor,
            self.params.profile,
            if self.entrypoint == VA_ENTRYPOINT_ENC_SLICE_LP { ", low power" } else { "" },
            if self.rate_control == VA_RC_CBR { "CBR" } else { "VBR" },
            if self.intra_refresh { ", intra refresh" } else { "" }
        )
    }
    fn hardware(&self) -> bool {
        true
    }
    fn size(&self) -> (usize, usize) {
        Vaapi::size(self)
    }
    fn keyframe(&mut self) {
        self.keyframe_next = true;
    }
    fn wants_keyframe(&self) -> bool {
        self.keyframe_next
    }
    fn set_bitrate(&mut self, kbps: u32) -> Result<(), String> {
        Vaapi::set_bitrate(self, kbps)
    }
    fn encode(&mut self, bgra: &[u8], stride: usize, tag: u64) -> Result<Vec<Encoded>, String> {
        Vaapi::encode(self, bgra, stride, tag)
    }
    fn takes_surfaces(&self) -> bool {
        self.vpp.is_some()
    }
    fn encode_surface(&mut self, surface: &crate::capture::Surface, tag: u64) -> Result<Vec<Encoded>, String> {
        Vaapi::encode_surface(self, surface, tag)
    }
}

impl Drop for Vaapi {
    fn drop(&mut self) {
        let (va, dpy) = (self.d.va, self.d.dpy);
        unsafe {
            if let Some(img) = self.upload.image {
                (va.vaDestroyImage)(dpy, img.image_id);
            }
            if let Some(vpp) = &self.vpp {
                (va.vaDestroyContext)(dpy, vpp.context);
                (va.vaDestroyConfig)(dpy, vpp.config);
            }
            if self.coded != VA_INVALID_ID {
                (va.vaDestroyBuffer)(dpy, self.coded);
            }
            if self.context != VA_INVALID_ID {
                (va.vaDestroyContext)(dpy, self.context);
            }
            let mut surfaces: Vec<u32> = [self.input, self.recon[0], self.recon[1]]
                .into_iter()
                .chain((self.upload.surface != self.input).then_some(self.upload.surface))
                .filter(|&s| s != VA_INVALID_ID)
                .collect();
            (va.vaDestroySurfaces)(dpy, surfaces.as_mut_ptr(), surfaces.len() as i32);
            (va.vaDestroyConfig)(dpy, self.config);
        }
        // Then the display itself (vaTerminate) and the device, as `d` drops.
    }
}

/// Rolling intra refresh's band for the P-frame `since_idr` frames after the IDR: where it starts
/// and how many macroblock rows, so the whole picture (`rows` high) is refreshed every half second.
fn rir_band(rows: u32, fps: u32, since_idr: u32) -> (u16, u16) {
    let period = (fps / 2).clamp(1, rows.max(1));
    let size = rows.div_ceil(period).max(1);
    let location = (since_idr.saturating_sub(1) % period) * size;
    (location.min(rows.saturating_sub(1)) as u16, size as u16)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::{offset_of, size_of};

    #[test]
    fn the_intra_refresh_band_covers_the_picture_every_half_second() {
        // 720p: 45 rows of macroblocks; at 60 fps, a wave of 30 frames, 2 rows a frame.
        let rows = 45;
        let mut covered = vec![false; rows as usize];
        for f in 1..=30 {
            let (at, size) = rir_band(rows, 60, f);
            assert_eq!(size, 2);
            for r in at..(at + size).min(rows as u16) {
                covered[r as usize] = true;
            }
        }
        assert!(covered.iter().all(|c| *c), "{covered:?}");
        // And again from the top.
        assert_eq!(rir_band(rows, 60, 31).0, 0);
        // A short picture at a high rate: a row a frame, never past the bottom.
        assert_eq!(rir_band(9, 240, 50), (((50 - 1) % 9) as u16, 1));
    }

    /// Sizes and offsets from libva 2.20's headers (gcc, LP64: the same on x86-64 and arm64), so a
    /// field out of place fails here, not in a driver.
    #[cfg(target_pointer_width = "64")]
    #[test]
    fn structs_are_laid_out_as_libva_lays_them_out() {
        assert_eq!(size_of::<VAPictureH264>(), 36);
        assert_eq!(size_of::<VAEncSequenceParameterBufferH264>(), 1132);
        assert_eq!(size_of::<VAEncPictureParameterBufferH264>(), 648);
        assert_eq!(size_of::<VAEncSliceParameterBufferH264>(), 3140);
        assert_eq!(size_of::<VAEncMiscParameterRateControl>(), 60);
        assert_eq!(size_of::<VAEncMiscParameterFrameRate>(), 24);
        assert_eq!(size_of::<VAEncMiscParameterHRD>(), 24);
        // libva 2.17's va.h (VA_VERSION 1.17): sizeof 28; location at 4, size at 6, qp delta at 8.
        assert_eq!(size_of::<VAEncMiscParameterRIR>(), 28);
        assert_eq!(offset_of!(VAEncMiscParameterRIR, intra_insertion_location), 4);
        assert_eq!(offset_of!(VAEncMiscParameterRIR, intra_insert_size), 6);
        assert_eq!(offset_of!(VAEncMiscParameterRIR, qp_delta_for_inserted_intra), 8);
        assert_eq!(offset_of!(VAEncMiscParameterRIR, va_reserved), 12);
        assert_eq!(size_of::<VAEncMiscParameter<VAEncMiscParameterRateControl>>(), 4 + 60);
        assert_eq!(offset_of!(VAEncMiscParameter<VAEncMiscParameterHRD>, data), 4);
        assert_eq!(size_of::<VAEncPackedHeaderParameterBuffer>(), 28);
        assert_eq!(size_of::<VACodedBufferSegment>(), 48);
        assert_eq!(size_of::<VAConfigAttrib>(), 8);
        assert_eq!(size_of::<VAGenericValue>(), 16);
        assert_eq!(size_of::<VASurfaceAttrib>(), 24);
        assert_eq!(size_of::<VAImageFormat>(), 48);
        assert_eq!(size_of::<VAImage>(), 120);
        assert_eq!(size_of::<VARectangle>(), 8);
        assert_eq!(size_of::<VAProcPipelineParameterBuffer>(), 224);
        assert_eq!(size_of::<VAProcColorProperties>(), 8);
        assert_eq!(size_of::<VADRMPRIMESurfaceDescriptor>(), 312);
        for &(got, want) in LAYOUT {
            assert_eq!(got, want);
        }
    }

    /// (offset here, offset in C) for every field the encoder sets.
    #[cfg(target_pointer_width = "64")]
    const LAYOUT: &[(usize, usize)] = &[
        (offset_of!(VAEncSequenceParameterBufferH264, intra_period), 4),
        (offset_of!(VAEncSequenceParameterBufferH264, bits_per_second), 16),
        (offset_of!(VAEncSequenceParameterBufferH264, max_num_ref_frames), 20),
        (offset_of!(VAEncSequenceParameterBufferH264, picture_width_in_mbs), 24),
        (offset_of!(VAEncSequenceParameterBufferH264, seq_fields), 28),
        (offset_of!(VAEncSequenceParameterBufferH264, bit_depth_luma_minus8), 32),
        (offset_of!(VAEncSequenceParameterBufferH264, offset_for_non_ref_pic), 36),
        (offset_of!(VAEncSequenceParameterBufferH264, offset_for_ref_frame), 44),
        (offset_of!(VAEncSequenceParameterBufferH264, frame_cropping_flag), 1068),
        (offset_of!(VAEncSequenceParameterBufferH264, frame_crop_left_offset), 1072),
        (offset_of!(VAEncSequenceParameterBufferH264, frame_crop_bottom_offset), 1084),
        (offset_of!(VAEncSequenceParameterBufferH264, vui_parameters_present_flag), 1088),
        (offset_of!(VAEncSequenceParameterBufferH264, vui_fields), 1092),
        (offset_of!(VAEncSequenceParameterBufferH264, aspect_ratio_idc), 1096),
        (offset_of!(VAEncSequenceParameterBufferH264, num_units_in_tick), 1108),
        (offset_of!(VAEncSequenceParameterBufferH264, time_scale), 1112),
        (offset_of!(VAEncSequenceParameterBufferH264, va_reserved), 1116),
        (offset_of!(VAEncPictureParameterBufferH264, ReferenceFrames), 36),
        (offset_of!(VAEncPictureParameterBufferH264, coded_buf), 612),
        (offset_of!(VAEncPictureParameterBufferH264, pic_parameter_set_id), 616),
        (offset_of!(VAEncPictureParameterBufferH264, last_picture), 618),
        (offset_of!(VAEncPictureParameterBufferH264, frame_num), 620),
        (offset_of!(VAEncPictureParameterBufferH264, pic_init_qp), 622),
        (offset_of!(VAEncPictureParameterBufferH264, num_ref_idx_l0_active_minus1), 623),
        (offset_of!(VAEncPictureParameterBufferH264, chroma_qp_index_offset), 625),
        (offset_of!(VAEncPictureParameterBufferH264, pic_fields), 628),
        (offset_of!(VAEncPictureParameterBufferH264, va_reserved), 632),
        (offset_of!(VAEncSliceParameterBufferH264, macroblock_info), 8),
        (offset_of!(VAEncSliceParameterBufferH264, slice_type), 12),
        (offset_of!(VAEncSliceParameterBufferH264, idr_pic_id), 14),
        (offset_of!(VAEncSliceParameterBufferH264, pic_order_cnt_lsb), 16),
        (offset_of!(VAEncSliceParameterBufferH264, delta_pic_order_cnt_bottom), 20),
        (offset_of!(VAEncSliceParameterBufferH264, direct_spatial_mv_pred_flag), 32),
        (offset_of!(VAEncSliceParameterBufferH264, num_ref_idx_l0_active_minus1), 34),
        (offset_of!(VAEncSliceParameterBufferH264, RefPicList0), 36),
        (offset_of!(VAEncSliceParameterBufferH264, RefPicList1), 1188),
        (offset_of!(VAEncSliceParameterBufferH264, luma_log2_weight_denom), 2340),
        (offset_of!(VAEncSliceParameterBufferH264, luma_weight_l0), 2344),
        (offset_of!(VAEncSliceParameterBufferH264, chroma_weight_l0_flag), 2472),
        (offset_of!(VAEncSliceParameterBufferH264, chroma_weight_l0), 2474),
        (offset_of!(VAEncSliceParameterBufferH264, luma_weight_l1_flag), 2730),
        (offset_of!(VAEncSliceParameterBufferH264, chroma_weight_l1_flag), 2860),
        (offset_of!(VAEncSliceParameterBufferH264, chroma_offset_l1), 2990),
        (offset_of!(VAEncSliceParameterBufferH264, cabac_init_idc), 3118),
        (offset_of!(VAEncSliceParameterBufferH264, slice_qp_delta), 3119),
        (offset_of!(VAEncSliceParameterBufferH264, disable_deblocking_filter_idc), 3120),
        (offset_of!(VAEncSliceParameterBufferH264, slice_beta_offset_div2), 3122),
        (offset_of!(VAEncSliceParameterBufferH264, va_reserved), 3124),
        (offset_of!(VAEncMiscParameterRateControl, rc_flags), 24),
        (offset_of!(VAEncMiscParameterRateControl, max_qp), 32),
        (offset_of!(VAEncMiscParameterRateControl, target_frame_size), 40),
        (offset_of!(VAEncPackedHeaderParameterBuffer, has_emulation_bytes), 8),
        (offset_of!(VAEncPackedHeaderParameterBuffer, va_reserved), 12),
        (offset_of!(VACodedBufferSegment, buf), 16),
        (offset_of!(VACodedBufferSegment, next), 24),
        (offset_of!(VASurfaceAttrib, value), 8),
        (offset_of!(VAGenericValue, value), 8),
        (offset_of!(VAImage, buf), 52),
        (offset_of!(VAImage, width), 56),
        (offset_of!(VAImage, num_planes), 64),
        (offset_of!(VAImage, pitches), 68),
        (offset_of!(VAImage, offsets), 80),
        (offset_of!(VAImage, component_order), 100),
        (offset_of!(VAProcPipelineParameterBuffer, surface_region), 8),
        (offset_of!(VAProcPipelineParameterBuffer, surface_color_standard), 16),
        (offset_of!(VAProcPipelineParameterBuffer, output_region), 24),
        (offset_of!(VAProcPipelineParameterBuffer, output_color_standard), 36),
        (offset_of!(VAProcPipelineParameterBuffer, filter_flags), 44),
        (offset_of!(VAProcPipelineParameterBuffer, filters), 48),
        (offset_of!(VAProcPipelineParameterBuffer, blend_state), 96),
        (offset_of!(VAProcPipelineParameterBuffer, input_color_properties), 132),
        (offset_of!(VAProcPipelineParameterBuffer, output_color_properties), 140),
        (offset_of!(VAProcPipelineParameterBuffer, output_hdr_metadata), 152),
        (offset_of!(VADRMPRIMESurfaceDescriptor, objects), 16),
        (offset_of!(VADRMPRIMESurfaceDescriptor, num_layers), 80),
        (offset_of!(VADRMPRIMESurfaceDescriptor, layers), 84),
    ];

    /// The bit fields as gcc packs them from the headers' declarations (each value read off the union
    /// in C, with these fields set).
    #[test]
    fn bit_fields_are_where_libva_packs_them() {
        // chroma_format_idc=1, frame_mbs_only_flag=1, direct_8x8_inference_flag=1,
        // log2_max_frame_num_minus4=12, pic_order_cnt_type=2.
        assert_eq!(seq_fields(12, 2), 0x0b25);
        // bitstream_restriction_flag=1, log2_max_mv_length_horizontal=15, vertical=15,
        // motion_vectors_over_pic_boundaries_flag=1.
        assert_eq!(vui_fields(15, 15), 0x8f7c);
        // idr_pic_flag, reference_pic_flag=1, entropy_coding_mode_flag, deblocking_filter_control_present_flag.
        assert_eq!(pic_fields(true, true), 0x020b);
        assert_eq!(pic_fields(false, false), 0x0202);
        // disable_frame_skip, disable_bit_stuffing.
        assert_eq!(RC_NO_SKIP_NO_STUFFING, 0x6);
    }

    #[test]
    fn formats_from_kms() {
        assert_eq!(va_format(u32::from_le_bytes(*b"XR24")), Some((VA_FOURCC_BGRX, VA_RT_FORMAT_RGB32)));
        assert_eq!(va_format(u32::from_le_bytes(*b"NV12")), None);
        // libva's own values (va.h).
        assert_eq!(VA_FOURCC_NV12, 0x3231_564e);
        assert_eq!(VA_FOURCC_BGRX, 0x5852_4742);
        assert_eq!(VA_FOURCC_X2R10G10B10, 0x3033_5258);
        assert_eq!(fourcc_str(u32::from_le_bytes(*b"XR24")), "XR24");
    }

    #[test]
    fn finds_parameter_sets_in_the_output() {
        let p = Params {
            profile: Profile::Main,
            level_idc: 40,
            width: 64,
            height: 64,
        };
        let stream = [p.sps(), p.pps(), p.slice_header(true, 0, 1).0].concat();
        assert_eq!(nal_types(&stream).collect::<Vec<_>>(), [7, 8, 5]);
    }

    /// Without libva or a GPU (CI), opening fails with a reason, never a crash; with one, it encodes.
    #[test]
    fn opens_or_says_why() {
        match Vaapi::new(Codec::H264, 320, 240, 30.0, 2000) {
            Err(e) => assert!(e.starts_with("VA-API"), "{e}"),
            Ok(mut enc) => {
                println!("encoder: {}", enc.name());
                let frame = vec![80u8; 320 * 240 * 4];
                let first = enc.encode(&frame, 320 * 4, 1).unwrap();
                assert!(first[0].keyframe && nal_types(&first[0].data).any(|t| t == 7), "SPS before the keyframe");
                let next = enc.encode(&frame, 320 * 4, 2).unwrap();
                assert!(next.iter().all(|e| !e.keyframe));
                enc.set_bitrate(500).unwrap();
                enc.keyframe();
                let key = enc.encode(&frame, 320 * 4, 3).unwrap();
                assert!(key[0].keyframe);
                println!(
                    "keyframe {} bytes, then {} bytes, forced keyframe {} bytes",
                    first[0].data.len(),
                    next.iter().map(|e| e.data.len()).sum::<usize>(),
                    key[0].data.len()
                );
            }
        }
        assert!(Vaapi::new(Codec::H265, 320, 240, 30.0, 2000).is_err());
    }
}
