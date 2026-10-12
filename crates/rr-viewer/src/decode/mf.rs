//! Windows: H.264 and HEVC through Media Foundation's decoders, on the GPU (DXVA, D3D11): the
//! decoder is given a D3D11 device of the GPU's, and decodes there. Each frame goes in as it comes
//! (Annex B) and comes out at once (low-latency mode: nothing held back for reordering), as NV12,
//! read back and converted to BGRA on the CPU (yuv.rs).
//!
//! It counts as hardware only when the GPU's driver decodes the codec (a DXVA decoder profile for it):
//! with no GPU (a VM, a CI runner) the viewer decodes with openh264 instead. `RR_DECODER=
//! mediafoundation` uses Media Foundation without the GPU even so (its own software decoding): the
//! same path, checkable where there's no GPU.
//!
//! mfplat.dll is loaded when first needed, not linked: Windows without Media Foundation (the N
//! editions) still runs the viewer, decoding with openh264.

use super::yuv::{to_bgra, Chroma, Plane};
use super::{Decoder, Picture};
use rr_proto::Codec;
use std::ffi::c_void;
use std::mem::ManuallyDrop;
use std::sync::OnceLock;
use windows::core::{Interface, GUID, HRESULT};
use windows::Win32::Foundation::HMODULE;
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_FEATURE_LEVEL_11_0};
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::{CoInitializeEx, CoTaskMemFree, COINIT_MULTITHREADED};
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};

type EnumEx = unsafe extern "system" fn(GUID, u32, *const MFT_REGISTER_TYPE_INFO, *const MFT_REGISTER_TYPE_INFO, *mut *mut *mut c_void, *mut u32) -> HRESULT;
type Create = unsafe extern "system" fn(*mut *mut c_void) -> HRESULT;
type CreateBuffer = unsafe extern "system" fn(u32, *mut *mut c_void) -> HRESULT;
type CreateManager = unsafe extern "system" fn(*mut u32, *mut *mut c_void) -> HRESULT;
type Startup = unsafe extern "system" fn(u32, u32) -> HRESULT;
type Proc = unsafe extern "system" fn() -> isize;

/// Media Foundation's functions, from mfplat.dll (loaded, not linked: see the top).
struct Mf {
    enum_ex: EnumEx,
    create_media_type: Create,
    create_sample: Create,
    create_memory_buffer: CreateBuffer,
    create_dxgi_device_manager: CreateManager,
}

fn mf() -> Result<&'static Mf, String> {
    static MF: OnceLock<Result<Mf, String>> = OnceLock::new();
    MF.get_or_init(|| unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let lib: HMODULE = LoadLibraryW(windows::core::w!("mfplat.dll")).map_err(|_| "no Media Foundation on this Windows (mfplat.dll)".to_string())?;
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

fn subtype(codec: Codec) -> Option<GUID> {
    match codec {
        Codec::H264 => Some(MFVideoFormat_H264),
        Codec::H265 => Some(MFVideoFormat_HEVC),
        _ => None,
    }
}

/// Media Foundation without the GPU (`RR_DECODER=mediafoundation`): see the top.
fn forced() -> bool {
    std::env::var("RR_DECODER").is_ok_and(|v| v.eq_ignore_ascii_case("mediafoundation"))
}

/// A D3D11 device on the GPU that decodes video (none on a machine with no GPU: the basic render
/// driver has no video decoder).
fn gpu_device() -> windows::core::Result<ID3D11Device> {
    let mut device = None;
    unsafe {
        D3D11CreateDevice(
            None,
            D3D_DRIVER_TYPE_HARDWARE,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_VIDEO_SUPPORT | D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            Some(&[D3D_FEATURE_LEVEL_11_0]),
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            None,
        )?;
    }
    device.ok_or_else(|| windows::core::Error::from(windows::Win32::Foundation::E_POINTER))
}

/// Whether the GPU's driver decodes `codec` (it lists a DXVA decoder profile for it).
pub fn supported(codec: Codec) -> bool {
    static CACHE: OnceLock<(bool, bool)> = OnceLock::new();
    let (h264, hevc) = *CACHE.get_or_init(|| {
        let Ok(device) = gpu_device() else { return (false, false) };
        let Ok(video) = device.cast::<ID3D11VideoDevice>() else {
            return (false, false);
        };
        let mut profiles = Vec::new();
        unsafe {
            for i in 0..video.GetVideoDecoderProfileCount() {
                if let Ok(p) = video.GetVideoDecoderProfile(i) {
                    profiles.push(p);
                }
            }
        }
        (
            profiles.contains(&D3D11_DECODER_PROFILE_H264_VLD_NOFGT),
            profiles.contains(&D3D11_DECODER_PROFILE_HEVC_VLD_MAIN),
        )
    });
    match codec {
        // Forced (a test where there's no GPU): taken as the hardware path it stands in for.
        Codec::H264 => h264 || forced(),
        Codec::H265 => hevc,
        _ => false,
    }
}

pub struct MediaFoundation {
    mft: IMFTransform,
    codec: Codec,
    /// The GPU's device manager, when decoding there (kept alive with the decoder).
    _manager: Option<IMFDXGIDeviceManager>,
    hardware: bool,
    provides_samples: bool,
    output_size: u32,
    /// The picture's size (the stream's), and the decoder's planes' (rows rounded up to 16).
    w: usize,
    h: usize,
    plane_h: usize,
    stride: usize,
}

// The decoder is only used by whoever holds it (on one thread, which has COM).
unsafe impl Send for MediaFoundation {}

impl MediaFoundation {
    pub fn new(codec: Codec, w: usize, h: usize) -> Result<Self, String> {
        let e = |err: windows::core::Error| format!("Media Foundation: {err}");
        if !supported(codec) && !forced() {
            return Err(format!("the GPU here doesn't decode {codec:?} (no DXVA profile for it)"));
        }
        let mf = mf()?;
        let sub = subtype(codec).ok_or("Media Foundation: no such codec")?;
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            // The decoder: Microsoft's (DXVA when given a device), the first Media Foundation lists.
            let input = MFT_REGISTER_TYPE_INFO {
                guidMajorType: MFMediaType_Video,
                guidSubtype: sub,
            };
            let (mut list, mut n) = (std::ptr::null_mut::<*mut c_void>(), 0u32);
            let flags = (MFT_ENUM_FLAG_SYNCMFT | MFT_ENUM_FLAG_SORTANDFILTER).0 as u32;
            (mf.enum_ex)(MFT_CATEGORY_VIDEO_DECODER, flags, &input, std::ptr::null(), &mut list, &mut n)
                .ok()
                .map_err(e)?;
            if list.is_null() || n == 0 {
                return Err(format!("Media Foundation: no {codec:?} decoder"));
            }
            let activates: Vec<IMFActivate> = (0..n as usize)
                .filter_map(|i| {
                    let p = *list.add(i);
                    (!p.is_null()).then(|| IMFActivate::from_raw(p))
                })
                .collect();
            CoTaskMemFree(Some(list as *const c_void));
            let mft: IMFTransform = activates[0].ActivateObject().map_err(e)?;
            let attrs = mft.GetAttributes().ok();
            if let Some(a) = &attrs {
                // Each frame out as soon as it's in (no frames held for reordering: there are none).
                let _ = a.SetUINT32(&MF_LOW_LATENCY, 1);
            }
            // The GPU, when the decoder takes a D3D11 device and the driver decodes this codec.
            let d3d_aware = attrs.as_ref().is_some_and(|a| a.GetUINT32(&MF_SA_D3D11_AWARE).unwrap_or(0) != 0);
            let mut manager = None;
            if d3d_aware && !forced() {
                let device = gpu_device().map_err(e)?;
                // The decoder and this thread both use the device.
                if let Ok(mt) = device.cast::<ID3D11Multithread>() {
                    let _ = mt.SetMultithreadProtected(true);
                }
                let (mut token, mut p) = (0u32, std::ptr::null_mut());
                let m: IMFDXGIDeviceManager = made((mf.create_dxgi_device_manager)(&mut token, &mut p), p).map_err(e)?;
                m.ResetDevice(&device, token).map_err(e)?;
                mft.ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER, m.as_raw() as usize)
                    .map_err(|err| format!("Media Foundation's decoder won't use the GPU ({err})"))?;
                manager = Some(m);
            } else if !forced() {
                return Err("Media Foundation's decoder can't use the GPU (not D3D11-aware)".into());
            }
            let mut p = std::ptr::null_mut();
            let t: IMFMediaType = made((mf.create_media_type)(&mut p), p).map_err(e)?;
            t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video).map_err(e)?;
            t.SetGUID(&MF_MT_SUBTYPE, &sub).map_err(e)?;
            t.SetUINT64(&MF_MT_FRAME_SIZE, ((w as u64) << 32) | h as u64).map_err(e)?;
            t.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32).map_err(e)?;
            mft.SetInputType(0, &t, 0).map_err(e)?;
            let mut dec = Self {
                mft,
                codec,
                _manager: manager.clone(),
                hardware: manager.is_some(),
                provides_samples: false,
                output_size: 0,
                w,
                h,
                plane_h: h.next_multiple_of(16),
                stride: w,
            };
            dec.set_output().map_err(e)?;
            dec.mft.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0).map_err(e)?;
            dec.mft.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0).map_err(e)?;
            Ok(dec)
        }
    }

    /// NV12 out (the decoder's own description of it), and the size of its planes.
    fn set_output(&mut self) -> windows::core::Result<()> {
        unsafe {
            for i in 0.. {
                let t = self.mft.GetOutputAvailableType(0, i)?;
                if t.GetGUID(&MF_MT_SUBTYPE).is_ok_and(|s| s == MFVideoFormat_NV12) {
                    self.mft.SetOutputType(0, &t, 0)?;
                    if let Ok(size) = t.GetUINT64(&MF_MT_FRAME_SIZE) {
                        self.stride = ((size >> 32) as usize).max(self.w);
                        self.plane_h = ((size & 0xffff_ffff) as usize).max(self.h);
                    }
                    if let Ok(s) = t.GetUINT32(&MF_MT_DEFAULT_STRIDE) {
                        self.stride = (s as i32).unsigned_abs() as usize;
                    }
                    let info = self.mft.GetOutputStreamInfo(0)?;
                    self.provides_samples = info.dwFlags & (MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 | MFT_OUTPUT_STREAM_CAN_PROVIDE_SAMPLES.0) as u32 != 0;
                    self.output_size = info.cbSize;
                    return Ok(());
                }
            }
        }
        Err(windows::core::Error::from(MF_E_INVALIDMEDIATYPE))
    }

    fn input(&self, data: &[u8]) -> windows::core::Result<()> {
        let mf = mf().map_err(|e| windows::core::Error::new(windows::Win32::Foundation::E_FAIL, e))?;
        unsafe {
            let mut p = std::ptr::null_mut();
            let buffer: IMFMediaBuffer = made((mf.create_memory_buffer)(data.len() as u32, &mut p), p)?;
            let mut at = std::ptr::null_mut();
            buffer.Lock(&mut at, None, None)?;
            std::ptr::copy_nonoverlapping(data.as_ptr(), at, data.len());
            buffer.Unlock()?;
            buffer.SetCurrentLength(data.len() as u32)?;
            let mut p = std::ptr::null_mut();
            let sample: IMFSample = made((mf.create_sample)(&mut p), p)?;
            sample.AddBuffer(&buffer)?;
            self.mft.ProcessInput(0, &sample, 0)
        }
    }

    /// One picture out, if the decoder has one.
    fn output(&mut self, out: &mut Picture) -> Result<bool, String> {
        let e = |err: windows::core::Error| format!("Media Foundation: {err}");
        let mf = mf()?;
        for _ in 0..3 {
            let given = if self.provides_samples {
                None
            } else {
                unsafe {
                    let mut p = std::ptr::null_mut();
                    let buffer: IMFMediaBuffer = made(
                        (mf.create_memory_buffer)(self.output_size.max((self.stride * self.plane_h * 3 / 2) as u32), &mut p),
                        p,
                    )
                    .map_err(e)?;
                    let mut p = std::ptr::null_mut();
                    let sample: IMFSample = made((mf.create_sample)(&mut p), p).map_err(e)?;
                    sample.AddBuffer(&buffer).map_err(e)?;
                    Some(sample)
                }
            };
            let mut buf = [MFT_OUTPUT_DATA_BUFFER {
                dwStreamID: 0,
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
                    // The decoder read the stream's size: its output type again, then the picture.
                    self.set_output().map_err(e)?;
                    continue;
                }
                Err(err) => return Err(e(err)),
            }
            let Some(sample) = sample else { return Ok(false) };
            return self.read(&sample, out).map_err(e);
        }
        Ok(false)
    }

    /// The NV12 picture in `sample` into `out`, as BGRA.
    fn read(&self, sample: &IMFSample, out: &mut Picture) -> windows::core::Result<bool> {
        unsafe {
            let buffer = sample.GetBufferByIndex(0)?;
            out.reshape(self.w, self.h);
            // A 2D buffer (GPU memory, read back as it's locked) says its own pitch.
            if let Ok(b2) = buffer.cast::<IMF2DBuffer>() {
                let (mut scan0, mut pitch) = (std::ptr::null_mut(), 0i32);
                b2.Lock2D(&mut scan0, &mut pitch)?;
                let pitch = pitch.unsigned_abs() as usize;
                // The luma's rows (the chroma follows them): from the buffer's size, NV12 being 1.5
                // rows of bytes a row of picture.
                let rows = match b2.GetContiguousLength() {
                    Ok(len) if pitch > 0 => (len as usize / pitch) * 2 / 3,
                    _ => self.plane_h,
                };
                self.convert(scan0, pitch, rows.max(self.h), out);
                b2.Unlock2D()?;
            } else {
                let (mut p, mut len) = (std::ptr::null_mut(), 0u32);
                buffer.Lock(&mut p, None, Some(&mut len))?;
                self.convert(p, self.stride, self.plane_h, out);
                buffer.Unlock()?;
            }
        }
        Ok(true)
    }

    /// NV12 at `base` (`pitch` bytes a row, the chroma after `rows` rows of luma) → `out`.
    unsafe fn convert(&self, base: *const u8, pitch: usize, rows: usize, out: &mut Picture) {
        let y = std::slice::from_raw_parts(base, pitch * self.h);
        let uv = std::slice::from_raw_parts(base.add(pitch * rows), pitch * self.h.div_ceil(2));
        to_bgra(
            Plane { data: y, stride: pitch },
            Chroma::Nv12(Plane { data: uv, stride: pitch }),
            self.w,
            self.h,
            out.bytes_mut(),
        );
    }
}

impl Decoder for MediaFoundation {
    fn name(&self) -> String {
        let c = if self.codec == Codec::H265 { "HEVC" } else { "H.264" };
        if self.hardware {
            format!("Media Foundation {c} (hardware, DXVA)")
        } else {
            format!("Media Foundation {c} (software)")
        }
    }

    fn hardware(&self) -> bool {
        self.hardware
    }

    fn decode(&mut self, data: &[u8], _keyframe: bool, out: &mut Picture) -> Result<bool, String> {
        let e = |err: windows::core::Error| format!("Media Foundation: {err}");
        match self.input(data) {
            Ok(()) => {}
            // It has a picture waiting: that first, then this frame.
            Err(err) if err.code() == MF_E_NOTACCEPTING => {
                let got = self.output(out)?;
                self.input(data).map_err(e)?;
                if got {
                    return Ok(true);
                }
            }
            Err(err) => return Err(e(err)),
        }
        self.output(out)
    }
}
