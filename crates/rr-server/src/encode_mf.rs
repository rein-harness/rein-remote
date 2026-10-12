//! Windows: H.264 through Media Foundation. The vendors' hardware encoders are Media Foundation
//! transforms (NVIDIA's NVENC, AMD's AMF, Intel's Quick Sync), found by asking Media Foundation for
//! H.264 encoders, hardware first; Microsoft's own software one comes after them. Each is set up for
//! real time (low-latency mode, constant bitrate, no B-frames, a keyframe only on request) and gives
//! the same Annex B stream as the other encoders (start codes, SPS and PPS before each keyframe).
//!
//! Frames go in as NV12. From memory, BGRA is converted on the CPU (yuv.rs) straight into the
//! encoder's buffer. Zero copy, where the encoder can read GPU memory (a hardware encoder that is
//! D3D11-aware): the capture's texture (capture/win.rs) is converted to NV12 on the same device by
//! the GPU's video processor, and that texture goes to the encoder as it is. The CPU never touches
//! the pixels.
//!
//! Hardware encoders are asynchronous: they say when they want a frame and when one is done
//! (events), and work while the server reads the next frame. Microsoft's is synchronous: a frame in,
//! the frame out. Both are handled, and frames come back with their tags either way.
//!
//! mfplat.dll is loaded when first needed, not linked: Windows without Media Foundation (the N
//! editions, Server without its media feature) still runs rein-remote, encoding in software.

use crate::encode::{Encoded, VideoEncoder};
use rr_proto::Codec;
use std::collections::VecDeque;
use std::ffi::c_void;
use std::mem::ManuallyDrop;
use std::sync::OnceLock;
use std::time::{Duration, Instant};
use windows::core::{Interface, GUID, HRESULT, PWSTR};
use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_NV12, DXGI_RATIONAL, DXGI_SAMPLE_DESC};
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::{CoInitializeEx, CoTaskMemFree, COINIT_MULTITHREADED};
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
use windows::Win32::System::Variant::VARIANT;

type EnumEx = unsafe extern "system" fn(GUID, u32, *const MFT_REGISTER_TYPE_INFO, *const MFT_REGISTER_TYPE_INFO, *mut *mut *mut c_void, *mut u32) -> HRESULT;
type Create = unsafe extern "system" fn(*mut *mut c_void) -> HRESULT;
type CreateBuffer = unsafe extern "system" fn(u32, *mut *mut c_void) -> HRESULT;
type CreateSurfaceBuffer = unsafe extern "system" fn(*const GUID, *mut c_void, u32, windows::core::BOOL, *mut *mut c_void) -> HRESULT;
type CreateManager = unsafe extern "system" fn(*mut u32, *mut *mut c_void) -> HRESULT;
type Startup = unsafe extern "system" fn(u32, u32) -> HRESULT;
type Proc = unsafe extern "system" fn() -> isize;

/// Media Foundation's functions, from mfplat.dll (loaded, not linked: see the top).
struct Mf {
    enum_ex: EnumEx,
    create_media_type: Create,
    create_sample: Create,
    create_memory_buffer: CreateBuffer,
    create_dxgi_surface_buffer: CreateSurfaceBuffer,
    create_dxgi_device_manager: CreateManager,
}

/// Media Foundation, started once for the process; why not, when this Windows has none.
fn mf() -> Result<&'static Mf, String> {
    static MF: OnceLock<Result<Mf, String>> = OnceLock::new();
    MF.get_or_init(|| unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let lib = LoadLibraryW(windows::core::w!("mfplat.dll")).map_err(|_| "no Media Foundation on this Windows (mfplat.dll)".to_string())?;
        macro_rules! get {
            ($name:literal as $ty:ty) => {
                std::mem::transmute::<Proc, $ty>(GetProcAddress(lib, windows::core::s!($name)).ok_or(concat!("Media Foundation: no ", $name))?)
            };
        }
        let startup = get!("MFStartup" as Startup);
        startup(MF_VERSION, MFSTARTUP_FULL)
            .ok()
            .map_err(|e| format!("Media Foundation didn't start ({e})"))?;
        Ok(Mf {
            enum_ex: get!("MFTEnumEx" as EnumEx),
            create_media_type: get!("MFCreateMediaType" as Create),
            create_sample: get!("MFCreateSample" as Create),
            create_memory_buffer: get!("MFCreateMemoryBuffer" as CreateBuffer),
            create_dxgi_surface_buffer: get!("MFCreateDXGISurfaceBuffer" as CreateSurfaceBuffer),
            create_dxgi_device_manager: get!("MFCreateDXGIDeviceManager" as CreateManager),
        })
    })
    .as_ref()
    .map_err(Clone::clone)
}

/// A COM object a Media Foundation function made (through `out`).
unsafe fn made<T: Interface>(hr: HRESULT, out: *mut c_void) -> windows::core::Result<T> {
    hr.ok()?;
    Ok(T::from_raw(out))
}

fn media_type(mf: &Mf) -> windows::core::Result<IMFMediaType> {
    let mut p = std::ptr::null_mut();
    unsafe { made((mf.create_media_type)(&mut p), p) }
}

fn new_sample(mf: &Mf) -> windows::core::Result<IMFSample> {
    let mut p = std::ptr::null_mut();
    unsafe { made((mf.create_sample)(&mut p), p) }
}

/// An encoder Media Foundation knows: how to make it, what it's called, whether it's hardware.
struct Candidate {
    activate: IMFActivate,
    name: String,
    vendor: String,
    hardware: bool,
}

/// The H.264 encoders on this machine: hardware first (`hardware_only`: only those), best first
/// within each (Media Foundation's own order).
fn candidates(hardware_only: bool) -> Result<Vec<Candidate>, String> {
    let mf = mf()?;
    let output = MFT_REGISTER_TYPE_INFO {
        guidMajorType: MFMediaType_Video,
        guidSubtype: MFVideoFormat_H264,
    };
    let mut out = Vec::new();
    let groups: &[(MFT_ENUM_FLAG, bool)] = if hardware_only {
        &[(MFT_ENUM_FLAG_HARDWARE, true)]
    } else {
        &[(MFT_ENUM_FLAG_HARDWARE, true), (MFT_ENUM_FLAG_SYNCMFT, false)]
    };
    for &(flags, hardware) in groups {
        let (mut list, mut n) = (std::ptr::null_mut::<*mut c_void>(), 0u32);
        let flags = flags | MFT_ENUM_FLAG_SORTANDFILTER;
        let hr = unsafe { (mf.enum_ex)(MFT_CATEGORY_VIDEO_ENCODER, flags.0 as u32, std::ptr::null(), &output, &mut list, &mut n) };
        if hr.is_err() || list.is_null() {
            continue;
        }
        for i in 0..n as usize {
            let p = unsafe { *list.add(i) };
            if p.is_null() {
                continue;
            }
            let activate = unsafe { IMFActivate::from_raw(p) };
            let name = string(&activate, &MFT_FRIENDLY_NAME_Attribute).unwrap_or_else(|| "an H.264 encoder".into());
            let vendor = string(&activate, &MFT_ENUM_HARDWARE_VENDOR_ID_Attribute).unwrap_or_default();
            out.push(Candidate {
                activate,
                name,
                vendor,
                hardware,
            });
        }
        unsafe { CoTaskMemFree(Some(list as *const c_void)) };
    }
    Ok(out)
}

fn string(attrs: &IMFActivate, key: &GUID) -> Option<String> {
    let (mut p, mut len) = (PWSTR::null(), 0u32);
    unsafe {
        attrs.GetAllocatedString(key, &mut p, &mut len).ok()?;
        let s = p.to_string().ok();
        CoTaskMemFree(Some(p.0 as *const c_void));
        s
    }
}

/// What to call an encoder in the log: the vendor's own name for its encoder where it's one of the
/// three (from the PCI vendor id, or the transform's name).
fn describe(name: &str, vendor: &str, hardware: bool) -> String {
    let (n, v) = (name.to_ascii_lowercase(), vendor.to_ascii_uppercase());
    if !hardware {
        return format!("H.264 in software ({name} via Media Foundation)");
    }
    let which = if v.contains("10DE") || n.contains("nvidia") {
        "NVIDIA NVENC".to_string()
    } else if v.contains("1002") || v.contains("1022") || n.contains("amd") {
        "AMD AMF".to_string()
    } else if v.contains("8086") || n.contains("intel") || n.contains("quick sync") {
        "Intel Quick Sync".to_string()
    } else {
        name.to_string()
    };
    format!("H.264 in hardware ({which} via Media Foundation)")
}

/// The GPU side of zero copy: BGRA textures from the capture into NV12 ones the encoder reads, by
/// the GPU's video processor, on the capture's device.
struct Gpu {
    context: ID3D11VideoContext,
    video: ID3D11VideoDevice,
    enumerator: ID3D11VideoProcessorEnumerator,
    processor: ID3D11VideoProcessor,
    /// NV12 textures taken in turn: one the encoder still reads isn't written again until several
    /// frames later (more than it ever holds at once).
    ring: Vec<(ID3D11Texture2D, ID3D11VideoProcessorOutputView)>,
    next: usize,
    /// The input view of the capture's texture (the same texture each frame, until its size changes).
    input: Option<(*mut c_void, ID3D11VideoProcessorInputView)>,
    /// Shared with the encoder (it reads the textures through it).
    manager: IMFDXGIDeviceManager,
    width: u32,
    height: u32,
}

const RING: usize = 8;

impl Gpu {
    fn new(mf: &Mf, device: &ID3D11Device, width: u32, height: u32, fps: u32) -> windows::core::Result<Self> {
        unsafe {
            let video: ID3D11VideoDevice = device.cast()?;
            let context: ID3D11VideoContext = device.GetImmediateContext()?.cast()?;
            let rate = DXGI_RATIONAL {
                Numerator: fps.max(1),
                Denominator: 1,
            };
            let enumerator = video.CreateVideoProcessorEnumerator(&D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
                InputFrameFormat: D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
                InputFrameRate: rate,
                InputWidth: width,
                InputHeight: height,
                OutputFrameRate: rate,
                OutputWidth: width,
                OutputHeight: height,
                Usage: D3D11_VIDEO_USAGE_OPTIMAL_SPEED,
            })?;
            let support = enumerator.CheckVideoProcessorFormat(DXGI_FORMAT_NV12)?;
            if support & D3D11_VIDEO_PROCESSOR_FORMAT_SUPPORT_OUTPUT.0 as u32 == 0 {
                return Err(windows::core::Error::new(
                    windows::Win32::Foundation::E_NOTIMPL,
                    "the video processor can't make NV12",
                ));
            }
            let processor = video.CreateVideoProcessor(&enumerator, 0)?;
            // The capture's BGRA (full range) to BT.601 limited range, the same as the software
            // path's conversion, with nothing the driver might add (sharpening, denoising).
            context.VideoProcessorSetStreamColorSpace(&processor, 0, &D3D11_VIDEO_PROCESSOR_COLOR_SPACE { _bitfield: 0 });
            context.VideoProcessorSetOutputColorSpace(&processor, &D3D11_VIDEO_PROCESSOR_COLOR_SPACE { _bitfield: 1 << 4 });
            context.VideoProcessorSetStreamFrameFormat(&processor, 0, D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE);
            context.VideoProcessorSetStreamAutoProcessingMode(&processor, 0, false);
            let full = RECT {
                left: 0,
                top: 0,
                right: width as i32,
                bottom: height as i32,
            };
            context.VideoProcessorSetOutputTargetRect(&processor, true, Some(&full));
            // Black where a window that shrank no longer reaches.
            let black = D3D11_VIDEO_COLOR {
                Anonymous: D3D11_VIDEO_COLOR_0 {
                    YCbCr: D3D11_VIDEO_COLOR_YCbCrA {
                        Y: 16.0 / 255.0,
                        Cb: 0.5,
                        Cr: 0.5,
                        A: 1.0,
                    },
                },
            };
            context.VideoProcessorSetOutputBackgroundColor(&processor, true, &black);
            let mut ring = Vec::new();
            for _ in 0..RING {
                let mut desc = D3D11_TEXTURE2D_DESC {
                    Width: width,
                    Height: height,
                    MipLevels: 1,
                    ArraySize: 1,
                    Format: DXGI_FORMAT_NV12,
                    SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                    Usage: D3D11_USAGE_DEFAULT,
                    BindFlags: (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_VIDEO_ENCODER.0) as u32,
                    CPUAccessFlags: 0,
                    MiscFlags: 0,
                };
                let mut tex = None;
                if device.CreateTexture2D(&desc, None, Some(&mut tex)).is_err() {
                    // Older drivers don't know the encoder bind flag.
                    desc.BindFlags = D3D11_BIND_RENDER_TARGET.0 as u32;
                    device.CreateTexture2D(&desc, None, Some(&mut tex))?;
                }
                let tex = tex.ok_or_else(|| windows::core::Error::from(windows::Win32::Foundation::E_POINTER))?;
                let mut view = None;
                video.CreateVideoProcessorOutputView(
                    &tex,
                    &enumerator,
                    &D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
                        ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D,
                        Anonymous: D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0 {
                            Texture2D: D3D11_TEX2D_VPOV { MipSlice: 0 },
                        },
                    },
                    Some(&mut view),
                )?;
                ring.push((tex, view.ok_or_else(|| windows::core::Error::from(windows::Win32::Foundation::E_POINTER))?));
            }
            let (mut token, mut p) = (0u32, std::ptr::null_mut());
            let manager: IMFDXGIDeviceManager = made((mf.create_dxgi_device_manager)(&mut token, &mut p), p)?;
            manager.ResetDevice(device, token)?;
            Ok(Self {
                context,
                video,
                enumerator,
                processor,
                ring,
                next: 0,
                input: None,
                manager,
                width,
                height,
            })
        }
    }

    /// `bgra` (the capture's texture) into the next NV12 texture of the ring, on the GPU: its
    /// top-left part when it's bigger than the stream, black around it when it's smaller.
    fn convert(&mut self, bgra: &ID3D11Texture2D) -> windows::core::Result<ID3D11Texture2D> {
        unsafe {
            let raw = bgra.as_raw();
            if self.input.as_ref().is_none_or(|(p, _)| *p != raw) {
                let mut view = None;
                self.video.CreateVideoProcessorInputView(
                    bgra,
                    &self.enumerator,
                    &D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
                        FourCC: 0,
                        ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
                        Anonymous: D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0 {
                            Texture2D: D3D11_TEX2D_VPIV { MipSlice: 0, ArraySlice: 0 },
                        },
                    },
                    Some(&mut view),
                )?;
                // (The view holds the texture, so no other texture can come at the same address.)
                self.input = view.map(|v| (raw, v));
            }
            let input = self.input.as_ref().map(|(_, v)| v.clone());
            let mut desc = D3D11_TEXTURE2D_DESC::default();
            bgra.GetDesc(&mut desc);
            let part = RECT {
                left: 0,
                top: 0,
                right: desc.Width.min(self.width) as i32,
                bottom: desc.Height.min(self.height) as i32,
            };
            self.context.VideoProcessorSetStreamSourceRect(&self.processor, 0, true, Some(&part));
            self.context.VideoProcessorSetStreamDestRect(&self.processor, 0, true, Some(&part));
            let (tex, view) = &self.ring[self.next];
            self.next = (self.next + 1) % self.ring.len();
            let stream = D3D11_VIDEO_PROCESSOR_STREAM {
                Enable: true.into(),
                pInputSurface: ManuallyDrop::new(input),
                ..Default::default()
            };
            let mut streams = [stream];
            let r = self.context.VideoProcessorBlt(&self.processor, view, 0, &streams);
            ManuallyDrop::drop(&mut streams[0].pInputSurface);
            r?;
            Ok(tex.clone())
        }
    }
}

/// H.264 through a Media Foundation transform (hardware or Microsoft's software one).
pub struct MediaFoundation {
    mft: IMFTransform,
    activate: IMFActivate,
    /// A hardware (asynchronous) encoder's events: when it wants a frame, when one is done.
    events: Option<IMFMediaEventGenerator>,
    codec_api: Option<ICodecAPI>,
    name: String,
    hardware: bool,
    width: usize,
    height: usize,
    /// Frame duration, in Media Foundation's 100 ns units.
    duration: i64,
    frame_no: i64,
    keyframe_next: bool,
    input_id: u32,
    output_id: u32,
    /// The encoder makes its own output samples; else one of ours, reused.
    provides_samples: bool,
    out_sample: Option<IMFSample>,
    out_size: u32,
    /// Frames the encoder asked for and hasn't been given yet (asynchronous encoders).
    need_input: u32,
    /// Frames in the encoder: their sample time and tag, in order.
    pending: VecDeque<(i64, u64)>,
    done: Vec<Encoded>,
    /// The SPS and PPS as Annex B, from the output type, for an encoder that sends them only once.
    parameter_sets: Vec<u8>,
    gpu: Option<Gpu>,
}

// The transform is free-threaded (Media Foundation's multithreaded apartment) and only used by
// whoever holds the encoder.
unsafe impl Send for MediaFoundation {}

/// A message about an encoder that couldn't start, once (encoders are opened again for each stream).
fn note(msg: String) {
    static SEEN: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());
    let mut seen = SEEN.lock().unwrap();
    if !seen.contains(&msg) {
        eprintln!("rein-remote: {msg}");
        seen.push(msg);
    }
}

impl MediaFoundation {
    /// The first of this machine's H.264 encoders that starts with these settings: hardware first,
    /// then (unless `hardware_only`) Microsoft's software one.
    pub fn new(codec: Codec, width: usize, height: usize, fps: f32, bitrate_kbps: u32, hardware_only: bool) -> Result<Self, String> {
        if codec != Codec::H264 {
            return Err(format!("Media Foundation: no {codec:?} encoder here (H.264 only)"));
        }
        let (width, height) = (width & !1, height & !1);
        let list = candidates(hardware_only)?;
        if list.is_empty() {
            return Err(if hardware_only {
                "Media Foundation: no hardware H.264 encoder".into()
            } else {
                "Media Foundation: no H.264 encoder".into()
            });
        }
        let mut why = Vec::new();
        for c in list {
            match Self::start(&c, width, height, fps, bitrate_kbps) {
                Ok(e) => return Ok(e),
                Err(e) => {
                    let _ = unsafe { c.activate.ShutdownObject() };
                    if c.hardware {
                        note(format!("{} couldn't start: {e}", c.name));
                    }
                    why.push(format!("{}: {e}", c.name));
                }
            }
        }
        Err(format!("Media Foundation: {}", why.join("; ")))
    }

    fn start(c: &Candidate, width: usize, height: usize, fps: f32, kbps: u32) -> windows::core::Result<Self> {
        let mf = mf().map_err(|e| windows::core::Error::new(windows::Win32::Foundation::E_FAIL, e))?;
        let fps_n = fps.round().max(1.0) as u32;
        unsafe {
            let mft: IMFTransform = c.activate.ActivateObject()?;
            let attrs = mft.GetAttributes().ok();
            // An asynchronous (hardware) encoder must be unlocked before it's used at all.
            let is_async = attrs.as_ref().is_some_and(|a| a.GetUINT32(&MF_TRANSFORM_ASYNC).unwrap_or(0) != 0);
            let events = if is_async {
                if let Some(a) = &attrs {
                    a.SetUINT32(&MF_TRANSFORM_ASYNC_UNLOCK, 1)?;
                }
                Some(mft.cast::<IMFMediaEventGenerator>()?)
            } else {
                None
            };
            if let Some(a) = &attrs {
                let _ = a.SetUINT32(&MF_LOW_LATENCY, 1);
            }
            // Zero copy: a hardware encoder that reads GPU memory gets the capture's device.
            let d3d_aware = attrs.as_ref().is_some_and(|a| a.GetUINT32(&MF_SA_D3D11_AWARE).unwrap_or(0) != 0);
            let mut gpu = None;
            if c.hardware && d3d_aware {
                if let Some(device) = crate::capture::win::shared_device() {
                    match Gpu::new(mf, &device, width as u32, height as u32, fps_n) {
                        Ok(g) => match mft.ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER, g.manager.as_raw() as usize) {
                            Ok(()) => gpu = Some(g),
                            Err(e) => note(format!("{} won't read GPU memory ({e}): frames go through memory", c.name)),
                        },
                        Err(e) => note(format!("no GPU color conversion for {} ({e}): frames go through memory", c.name)),
                    }
                }
            }
            let codec_api = mft.cast::<ICodecAPI>().ok();
            let bps = kbps.saturating_mul(1000);
            // Real time, before the media types (some encoders read them only then).
            if let Some(api) = &codec_api {
                let set = |key: &GUID, v: VARIANT| api.SetValue(key, &v);
                let _ = set(&CODECAPI_AVLowLatencyMode, VARIANT::from(true));
                let _ = set(&CODECAPI_AVEncCommonRateControlMode, VARIANT::from(eAVEncCommonRateControlMode_CBR.0 as u32));
                let _ = set(&CODECAPI_AVEncCommonMeanBitRate, VARIANT::from(bps));
                let _ = set(&CODECAPI_AVEncMPVDefaultBPictureCount, VARIANT::from(0u32));
                // A keyframe only when someone needs one: the longest interval the encoder takes.
                for gop in [u32::MAX, i32::MAX as u32, 0xffff] {
                    if set(&CODECAPI_AVEncMPVGOPSize, VARIANT::from(gop)).is_ok() {
                        break;
                    }
                }
            }
            let size = ((width as u64) << 32) | height as u64;
            let rate = ((fps_n as u64) << 32) | 1;
            let out = media_type(mf)?;
            out.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            out.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_H264)?;
            out.SetUINT32(&MF_MT_AVG_BITRATE, bps)?;
            out.SetUINT64(&MF_MT_FRAME_SIZE, size)?;
            out.SetUINT64(&MF_MT_FRAME_RATE, rate)?;
            out.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, (1 << 32) | 1)?;
            out.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
            out.SetUINT32(&MF_MT_MPEG2_PROFILE, eAVEncH264VProfile_Main.0 as u32)?;
            mft.SetOutputType(0, &out, 0)?;
            // NV12 in: the encoder's own description of it where it offers one.
            let mut input = None;
            for i in 0.. {
                let Ok(t) = mft.GetInputAvailableType(0, i) else { break };
                if t.GetGUID(&MF_MT_SUBTYPE).is_ok_and(|s| s == MFVideoFormat_NV12) {
                    input = Some(t);
                    break;
                }
            }
            let input = match input {
                Some(t) => t,
                None => {
                    let t = media_type(mf)?;
                    t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
                    t.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_NV12)?;
                    t
                }
            };
            input.SetUINT64(&MF_MT_FRAME_SIZE, size)?;
            input.SetUINT64(&MF_MT_FRAME_RATE, rate)?;
            input.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, (1 << 32) | 1)?;
            input.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
            input.SetUINT32(&MF_MT_DEFAULT_STRIDE, width as u32)?;
            let _ = input.SetUINT32(&MF_MT_YUV_MATRIX, MFVideoTransferMatrix_BT601.0 as u32);
            let _ = input.SetUINT32(&MF_MT_VIDEO_NOMINAL_RANGE, MFNominalRange_16_235.0 as u32);
            mft.SetInputType(0, &input, 0)?;
            // Stream ids: most encoders number them from 0 and don't say.
            let (mut input_id, mut output_id) = (0u32, 0u32);
            let _ = mft.GetStreamIDs(std::slice::from_mut(&mut input_id), std::slice::from_mut(&mut output_id));
            let info = mft.GetOutputStreamInfo(output_id)?;
            let provides_samples = info.dwFlags & (MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 | MFT_OUTPUT_STREAM_CAN_PROVIDE_SAMPLES.0) as u32 != 0;
            mft.ProcessMessage(MFT_MESSAGE_COMMAND_FLUSH, 0)?;
            mft.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)?;
            mft.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)?;
            let mut enc = Self {
                mft,
                activate: c.activate.clone(),
                events,
                codec_api,
                name: describe(&c.name, &c.vendor, c.hardware),
                hardware: c.hardware,
                width,
                height,
                duration: 10_000_000 / fps_n as i64,
                frame_no: 0,
                keyframe_next: true,
                input_id,
                output_id,
                provides_samples,
                out_sample: None,
                out_size: info.cbSize.max((width * height * 3 / 2) as u32),
                need_input: 0,
                pending: VecDeque::new(),
                done: Vec::new(),
                parameter_sets: Vec::new(),
                gpu,
            };
            enc.read_parameter_sets();
            Ok(enc)
        }
    }

    /// The SPS and PPS from the output type (MF_MT_MPEG_SEQUENCE_HEADER), as Annex B, when it has them.
    fn read_parameter_sets(&mut self) {
        unsafe {
            let Ok(t) = self.mft.GetOutputCurrentType(self.output_id) else { return };
            let Ok(n) = t.GetBlobSize(&MF_MT_MPEG_SEQUENCE_HEADER) else { return };
            let mut blob = vec![0u8; n as usize];
            if n > 0 && t.GetBlob(&MF_MT_MPEG_SEQUENCE_HEADER, &mut blob, None).is_ok() {
                self.parameter_sets = annex_b_header(&blob);
            }
        }
    }

    pub fn size(&self) -> (usize, usize) {
        (self.width, self.height)
    }

    /// A new target bitrate, taken at once (no keyframe).
    pub fn set_bitrate(&mut self, kbps: u32) -> Result<(), String> {
        let api = self.codec_api.as_ref().ok_or("Media Foundation: this encoder's bitrate can't change")?;
        unsafe { api.SetValue(&CODECAPI_AVEncCommonMeanBitRate, &VARIANT::from(kbps.saturating_mul(1000))) }
            .map_err(|e| format!("Media Foundation: can't set the bitrate ({e})"))
    }

    pub fn encode(&mut self, bgra: &[u8], stride: usize, tag: u64) -> Result<Vec<Encoded>, String> {
        let (w, h) = (self.width, self.height);
        if bgra.len() < stride * (h - 1) + w * 4 {
            return Err("Media Foundation: the frame is smaller than the stream".into());
        }
        let mf = mf()?;
        let e = |err: windows::core::Error| format!("Media Foundation: {err}");
        let len = (w * h * 3 / 2) as u32;
        let sample = unsafe {
            let mut p = std::ptr::null_mut();
            let buffer: IMFMediaBuffer = made((mf.create_memory_buffer)(len, &mut p), p).map_err(e)?;
            let mut data = std::ptr::null_mut();
            buffer.Lock(&mut data, None, None).map_err(e)?;
            crate::yuv::nv12(bgra, stride, w, h, std::slice::from_raw_parts_mut(data, len as usize));
            buffer.Unlock().map_err(e)?;
            buffer.SetCurrentLength(len).map_err(e)?;
            let sample = new_sample(mf).map_err(e)?;
            sample.AddBuffer(&buffer).map_err(e)?;
            sample
        };
        self.submit(sample, tag)
    }

    /// The capture's texture, converted to NV12 on the GPU and given to the encoder there.
    pub fn encode_surface(&mut self, surface: &crate::capture::Surface, tag: u64) -> Result<Vec<Encoded>, String> {
        let mf = mf()?;
        let e = |err: windows::core::Error| format!("Media Foundation: {err}");
        let gpu = self.gpu.as_mut().ok_or("Media Foundation: this encoder reads frames from memory")?;
        let nv12 = gpu.convert(surface.as_texture()).map_err(|err| format!("GPU color conversion: {err}"))?;
        let sample = unsafe {
            let mut p = std::ptr::null_mut();
            let buffer: IMFMediaBuffer = made(
                (mf.create_dxgi_surface_buffer)(&ID3D11Texture2D::IID, nv12.as_raw(), 0, false.into(), &mut p),
                p,
            )
            .map_err(e)?;
            if let Ok(b2) = buffer.cast::<IMF2DBuffer>() {
                if let Ok(n) = b2.GetContiguousLength() {
                    let _ = buffer.SetCurrentLength(n);
                }
            }
            let sample = new_sample(mf).map_err(e)?;
            sample.AddBuffer(&buffer).map_err(e)?;
            sample
        };
        self.submit(sample, tag)
    }

    /// Hands a frame to the encoder (when it wants one: an asynchronous encoder says so), and gives
    /// back what's done by then.
    fn submit(&mut self, sample: IMFSample, tag: u64) -> Result<Vec<Encoded>, String> {
        let e = |err: windows::core::Error| format!("Media Foundation: {err}");
        if self.events.is_some() {
            let deadline = Instant::now() + Duration::from_secs(2);
            self.pump()?;
            while self.need_input == 0 {
                if Instant::now() > deadline {
                    return Err(format!("{}: the encoder stopped answering", self.name));
                }
                std::thread::sleep(Duration::from_micros(200));
                self.pump()?;
            }
        }
        let time = self.frame_no * self.duration;
        self.frame_no += 1;
        unsafe {
            sample.SetSampleTime(time).map_err(e)?;
            sample.SetSampleDuration(self.duration).map_err(e)?;
            if std::mem::take(&mut self.keyframe_next) {
                if let Some(api) = &self.codec_api {
                    let _ = api.SetValue(&CODECAPI_AVEncVideoForceKeyFrame, &VARIANT::from(1u32));
                }
            }
            let mut r = self.mft.ProcessInput(self.input_id, &sample, 0);
            if r.as_ref().is_err_and(|err| err.code() == MF_E_NOTACCEPTING) && self.events.is_none() {
                // A synchronous encoder with a frame still to give: take it, then this one goes in.
                while self.output()? {}
                r = self.mft.ProcessInput(self.input_id, &sample, 0);
            }
            r.map_err(e)?;
        }
        self.pending.push_back((time, tag));
        if self.events.is_some() {
            self.need_input -= 1;
            self.pump()?;
        } else {
            while self.output()? {}
        }
        Ok(std::mem::take(&mut self.done))
    }

    /// An asynchronous encoder's events so far, without waiting: frames it wants, frames done.
    fn pump(&mut self) -> Result<(), String> {
        let Some(events) = self.events.clone() else { return Ok(()) };
        loop {
            let ev = match unsafe { events.GetEvent(MF_EVENT_FLAG_NO_WAIT) } {
                Ok(ev) => ev,
                Err(err) if err.code() == MF_E_NO_EVENTS_AVAILABLE => return Ok(()),
                Err(err) => return Err(format!("{}: {err}", self.name)),
            };
            let kind = unsafe { ev.GetType() }.map_err(|err| format!("{}: {err}", self.name))?;
            match kind {
                k if k == METransformNeedInput.0 as u32 => self.need_input += 1,
                k if k == METransformHaveOutput.0 as u32 => {
                    self.output()?;
                }
                k if k == MEError.0 as u32 => {
                    let status = unsafe { ev.GetStatus() }.unwrap_or(windows::Win32::Foundation::E_FAIL);
                    return Err(format!("{}: the encoder failed ({})", self.name, windows::core::Error::from(status)));
                }
                _ => {}
            }
        }
    }

    /// One frame out of the encoder, if it has one: false when it needs more input first.
    fn output(&mut self) -> Result<bool, String> {
        let e = |err: windows::core::Error| format!("Media Foundation: {err}");
        let given = if self.provides_samples {
            None
        } else {
            Some(self.output_sample().map_err(e)?)
        };
        let mut buf = [MFT_OUTPUT_DATA_BUFFER {
            dwStreamID: self.output_id,
            pSample: ManuallyDrop::new(given),
            dwStatus: 0,
            pEvents: ManuallyDrop::new(None),
        }];
        let mut status = 0u32;
        let r = unsafe { self.mft.ProcessOutput(0, &mut buf, &mut status) };
        let [buf] = buf;
        let sample = ManuallyDrop::into_inner(buf.pSample);
        drop(ManuallyDrop::into_inner(buf.pEvents));
        match r {
            Ok(()) => {}
            Err(err) if err.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => return Ok(false),
            Err(err) if err.code() == MF_E_TRANSFORM_STREAM_CHANGE => {
                // The encoder settled its output format: take it as it offers it.
                unsafe {
                    let t = self.mft.GetOutputAvailableType(self.output_id, 0).map_err(e)?;
                    self.mft.SetOutputType(self.output_id, &t, 0).map_err(e)?;
                }
                self.read_parameter_sets();
                // The frame that was ready is still there, in the new format: an asynchronous encoder
                // won't say so again, so it's taken now.
                return self.output();
            }
            Err(err) => return Err(e(err)),
        }
        let Some(sample) = sample else { return Ok(true) };
        let mut data = unsafe {
            let buffer = sample.ConvertToContiguousBuffer().map_err(e)?;
            let (mut p, mut n) = (std::ptr::null_mut(), 0u32);
            buffer.Lock(&mut p, None, Some(&mut n)).map_err(e)?;
            let v = std::slice::from_raw_parts(p, n as usize).to_vec();
            let _ = buffer.Unlock();
            v
        };
        let clean_point = unsafe { sample.GetUINT32(&MFSampleExtension_CleanPoint) }.unwrap_or(0) != 0;
        let keyframe = clean_point || nal_types(&data).any(|t| t == 5);
        if keyframe && !nal_types(&data).any(|t| t == 7) {
            // The parameter sets went out once, in the output type: the client needs them before
            // every keyframe (one joining later starts there).
            if self.parameter_sets.is_empty() {
                self.read_parameter_sets();
            }
            data.splice(0..0, self.parameter_sets.iter().copied());
        }
        // Its tag: frames the encoder dropped (earlier times) are no longer in flight.
        let time = unsafe { sample.GetSampleTime() }.ok();
        let mut tag = 0;
        while let Some(&(t, g)) = self.pending.front() {
            if time.is_some_and(|time| t < time) {
                self.pending.pop_front();
                continue;
            }
            if time.is_none() || time == Some(t) {
                self.pending.pop_front();
                tag = g;
            }
            break;
        }
        if !data.is_empty() {
            self.done.push(Encoded { data, keyframe, tag });
        }
        Ok(true)
    }

    /// Our output sample, for an encoder that writes into the caller's (Microsoft's): reused, emptied.
    fn output_sample(&mut self) -> windows::core::Result<IMFSample> {
        if self.out_sample.is_none() {
            let mf = mf().map_err(|e| windows::core::Error::new(windows::Win32::Foundation::E_FAIL, e))?;
            let mut p = std::ptr::null_mut();
            let buffer: IMFMediaBuffer = unsafe { made((mf.create_memory_buffer)(self.out_size, &mut p), p)? };
            let sample = new_sample(mf)?;
            unsafe { sample.AddBuffer(&buffer)? };
            self.out_sample = Some(sample);
        }
        let sample = self.out_sample.clone().unwrap();
        unsafe { sample.GetBufferByIndex(0)?.SetCurrentLength(0)? };
        Ok(sample)
    }

    pub fn in_flight(&self) -> usize {
        self.pending.len()
    }

    /// The frames done since the last call, without waiting.
    pub fn poll(&mut self) -> Result<Vec<Encoded>, String> {
        if self.events.is_some() {
            self.pump()?;
        } else if !self.pending.is_empty() {
            while self.output()? {}
        }
        Ok(std::mem::take(&mut self.done))
    }
}

/// The NAL unit types in an Annex B stream (after each 3- or 4-byte start code).
fn nal_types(data: &[u8]) -> impl Iterator<Item = u8> + '_ {
    data.windows(4).filter(|w| w[..3] == [0, 0, 1]).map(|w| w[3] & 0x1f)
}

/// A sequence header as Annex B: as it is when it already is (start codes), else read as an
/// `avcC` record (lengths before each SPS and PPS).
fn annex_b_header(blob: &[u8]) -> Vec<u8> {
    if blob.starts_with(&[0, 0, 1]) || blob.starts_with(&[0, 0, 0, 1]) {
        return blob.to_vec();
    }
    let mut out = Vec::new();
    if blob.len() < 7 || blob[0] != 1 {
        return out;
    }
    let mut at = 5;
    for count_mask in [0x1f, 0xff] {
        let Some(&count) = blob.get(at) else { break };
        at += 1;
        for _ in 0..(count & count_mask) {
            let Some(len) = blob.get(at..at + 2).map(|b| u16::from_be_bytes([b[0], b[1]]) as usize) else {
                return out;
            };
            at += 2;
            let Some(nal) = blob.get(at..at + len) else { return out };
            out.extend_from_slice(&[0, 0, 0, 1]);
            out.extend_from_slice(nal);
            at += len;
        }
    }
    out
}

impl VideoEncoder for MediaFoundation {
    fn name(&self) -> String {
        self.name.clone()
    }
    fn hardware(&self) -> bool {
        self.hardware
    }
    fn size(&self) -> (usize, usize) {
        MediaFoundation::size(self)
    }
    fn keyframe(&mut self) {
        self.keyframe_next = true;
    }
    fn wants_keyframe(&self) -> bool {
        self.keyframe_next
    }
    fn set_bitrate(&mut self, kbps: u32) -> Result<(), String> {
        MediaFoundation::set_bitrate(self, kbps)
    }
    fn encode(&mut self, bgra: &[u8], stride: usize, tag: u64) -> Result<Vec<Encoded>, String> {
        MediaFoundation::encode(self, bgra, stride, tag)
    }
    fn takes_surfaces(&self) -> bool {
        self.gpu.is_some()
    }
    fn encode_surface(&mut self, surface: &crate::capture::Surface, tag: u64) -> Result<Vec<Encoded>, String> {
        MediaFoundation::encode_surface(self, surface, tag)
    }
    fn in_flight(&self) -> usize {
        MediaFoundation::in_flight(self)
    }
    fn poll(&mut self) -> Result<Vec<Encoded>, String> {
        MediaFoundation::poll(self)
    }
}

impl Drop for MediaFoundation {
    fn drop(&mut self) {
        unsafe {
            let _ = self.mft.ProcessMessage(MFT_MESSAGE_NOTIFY_END_OF_STREAM, 0);
            let _ = self.mft.ProcessMessage(MFT_MESSAGE_NOTIFY_END_STREAMING, 0);
            // The encoder lets go of the GPU before the device manager goes.
            if self.gpu.is_some() {
                let _ = self.mft.ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER, 0);
            }
            if let Ok(s) = self.mft.cast::<IMFShutdown>() {
                let _ = s.Shutdown();
            }
            let _ = self.activate.ShutdownObject();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The first H.264 encoder Media Foundation has here (a GPU's on a machine with one; Microsoft's
    /// software one on CI), or why there's none.
    fn open(w: usize, h: usize, kbps: u32) -> Option<MediaFoundation> {
        match MediaFoundation::new(Codec::H264, w, h, 30.0, kbps, false) {
            Ok(e) => {
                println!("encoder: {}", e.name);
                Some(e)
            }
            Err(e) => {
                println!("skipped: no Media Foundation H.264 encoder on this machine ({e})");
                None
            }
        }
    }

    /// One frame in, waited for: the frame out.
    fn one(enc: &mut MediaFoundation, frame: &[u8], stride: usize, tag: u64) -> Option<Encoded> {
        let mut out = enc.encode(frame, stride, tag).unwrap();
        let start = Instant::now();
        while enc.in_flight() > 0 && start.elapsed() < Duration::from_secs(2) {
            std::thread::sleep(Duration::from_millis(1));
            out.extend(enc.poll().unwrap());
        }
        out.into_iter().find(|e| e.tag == tag)
    }

    fn noise(frame: &mut [u8], seed: u32) {
        let mut x = seed.wrapping_mul(2654435761) | 1;
        for b in frame.iter_mut() {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            *b = x as u8;
        }
    }

    #[test]
    fn encodes_a_keyframe_then_deltas_with_sps_and_forced_keyframes() {
        let (w, h) = (320, 240);
        let Some(mut enc) = open(w, h, 2000) else { return };
        let mut frame = vec![40u8; w * h * 4];
        let first = one(&mut enc, &frame, w * 4, 1).expect("a first frame");
        assert!(first.keyframe);
        assert!(first.data.starts_with(&[0, 0, 0, 1]) || first.data.starts_with(&[0, 0, 1]));
        assert!(nal_types(&first.data).any(|t| t == 7), "SPS before the keyframe");
        assert!(nal_types(&first.data).any(|t| t == 8), "PPS before the keyframe");
        for n in 2..6 {
            for px in frame[..w * 8 * n as usize * 4].chunks_mut(4) {
                px.copy_from_slice(&[255, 255, 255, 255]);
            }
            let delta = one(&mut enc, &frame, w * 4, n).expect("a delta frame");
            assert!(!delta.keyframe, "frame {n} is a delta");
            assert!(delta.data.len() < first.data.len() * 2);
        }
        enc.keyframe();
        let forced = one(&mut enc, &frame, w * 4, 10).expect("a forced frame");
        assert!(forced.keyframe, "a keyframe on request");
        assert!(nal_types(&forced.data).any(|t| t == 7), "SPS before every keyframe");
        println!("keyframe {} bytes, forced keyframe {} bytes", first.data.len(), forced.data.len());
    }

    #[test]
    fn the_bitrate_changes_live() {
        let (w, h) = (640, 360);
        let Some(mut enc) = open(w, h, 20_000) else { return };
        let mut frame = vec![0u8; w * h * 4];
        // Noise every frame: the encoder spends all it's allowed.
        let mut size = |enc: &mut MediaFoundation, from: u64| -> usize {
            (from..from + 15)
                .map(|n| {
                    noise(&mut frame, n as u32);
                    one(enc, &frame, w * 4, n).map_or(0, |e| e.data.len())
                })
                .skip(5)
                .sum()
        };
        let high = size(&mut enc, 1);
        enc.set_bitrate(500).unwrap();
        let low = size(&mut enc, 100);
        println!("10 frames at 20 Mbit/s: {high} bytes; at 0.5 Mbit/s: {low} bytes");
        assert!(low * 2 < high, "the lower bitrate took ({high} → {low})");
    }

    #[test]
    fn frames_come_back_in_order_with_their_tags() {
        let (w, h) = (640, 360);
        let Some(mut enc) = open(w, h, 4000) else { return };
        let mut frame = vec![0u8; w * h * 4];
        let mut out = Vec::new();
        for n in 1..=20u64 {
            let at = (n as usize * 997) % frame.len();
            frame[at] ^= 0xff;
            out.extend(enc.encode(&frame, w * 4, n).unwrap());
        }
        let start = Instant::now();
        while enc.in_flight() > 0 && start.elapsed() < Duration::from_secs(2) {
            std::thread::sleep(Duration::from_millis(1));
            out.extend(enc.poll().unwrap());
        }
        let tags: Vec<u64> = out.iter().map(|e| e.tag).collect();
        assert!(tags.windows(2).all(|t| t[0] < t[1]), "{tags:?}");
        assert_eq!(tags.first(), Some(&1));
        assert!(out[0].keyframe);
    }

    #[test]
    fn reads_an_avcc_header_as_annex_b() {
        // avcC: version 1, profile, compat, level, 0xff, one SPS (2 bytes), one PPS (1 byte).
        let avcc = [1, 77, 0, 31, 0xff, 0xe1, 0, 2, 0x67, 0xaa, 1, 0, 1, 0x68];
        assert_eq!(annex_b_header(&avcc), [0, 0, 0, 1, 0x67, 0xaa, 0, 0, 0, 1, 0x68]);
        let annex_b = [0, 0, 0, 1, 0x67, 1, 0, 0, 1, 0x68, 2];
        assert_eq!(annex_b_header(&annex_b), annex_b);
        assert_eq!(nal_types(&annex_b).collect::<Vec<_>>(), [7, 8]);
    }

    #[test]
    fn names_the_vendors_encoders() {
        assert_eq!(
            describe("NVIDIA H.264 Encoder MFT", "VEN_10DE", true),
            "H.264 in hardware (NVIDIA NVENC via Media Foundation)"
        );
        assert_eq!(describe("AMDh264Encoder", "VEN_1002", true), "H.264 in hardware (AMD AMF via Media Foundation)");
        assert_eq!(
            describe("Intel® Quick Sync Video H.264 Encoder MFT", "VEN_8086", true),
            "H.264 in hardware (Intel Quick Sync via Media Foundation)"
        );
        assert_eq!(
            describe("H264 Encoder MFT", "", false),
            "H.264 in software (H264 Encoder MFT via Media Foundation)"
        );
    }
}
