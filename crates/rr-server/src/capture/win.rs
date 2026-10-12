//! Windows: any app's window, or the whole primary display, captured with Windows Graphics Capture
//! (the compositor hands over each frame as it changes, the yellow border and the cursor left out)
//! and controlled with SendInput. Nothing in the app needs to support it. A press brings the window
//! to the front first, like Remote Desktop, since a click goes to the window on top where it lands.
//!
//! Frames stay on the GPU when the encoder can take them there (Media Foundation with a hardware
//! encoder: see encode_mf.rs): each is copied, GPU to GPU, into a texture of the capture's own (the
//! system reuses its own the moment the frame is handed back) on a D3D11 device the encoder shares.
//! Otherwise each is read back into memory, as BGRA rows.

use super::{Frame, Source, Surface};
use rr_proto::{Button, Input};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use windows::core::Interface;
use windows::Foundation::TypedEventHandler;
use windows::Graphics::Capture::{Direct3D11CaptureFramePool, GraphicsCaptureItem, GraphicsCaptureSession};
use windows::Graphics::DirectX::Direct3D11::IDirect3DDevice;
use windows::Graphics::DirectX::DirectXPixelFormat;
use windows::Win32::Foundation::{HMODULE, HWND, POINT, RECT};
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_EXTENDED_FRAME_BOUNDS};
use windows::Win32::Graphics::Dxgi::IDXGIDevice;
use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MonitorFromPoint, MONITORINFO, MONITOR_DEFAULTTOPRIMARY};
use windows::Win32::System::WinRT::Direct3D11::{CreateDirect3D11DeviceFromDXGIDevice, IDirect3DDxgiInterfaceAccess};
use windows::Win32::System::WinRT::Graphics::Capture::IGraphicsCaptureItemInterop;
use windows::Win32::System::WinRT::{RoInitialize, RO_INIT_MULTITHREADED};
use windows::Win32::UI::HiDpi::{SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2};
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::{
    BringWindowToTop, GetForegroundWindow, GetSystemMetrics, SetForegroundWindow, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
};
use windows_capture::window::Window;

/// A window this machine shows: for `rein-remote windows` and for picking one by name.
pub struct WindowInfo {
    pub id: u64,
    pub app: String,
    pub title: String,
    pub width: i32,
    pub height: i32,
}

fn dpi_aware() {
    // Physical pixels everywhere, so where a window is matches what's captured of it.
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
}

pub fn windows() -> Result<Vec<WindowInfo>, String> {
    dpi_aware();
    let list = Window::enumerate().map_err(|e| format!("can't list windows: {e}"))?;
    Ok(list
        .into_iter()
        .filter_map(|w| {
            let title = w.title().ok()?;
            let (width, height) = (w.width().ok()?, w.height().ok()?);
            (width >= 64 && height >= 64 && !title.is_empty()).then(|| WindowInfo {
                id: w.as_raw_hwnd() as u64,
                app: w.process_name().unwrap_or_default(),
                title,
                width,
                height,
            })
        })
        .collect())
}

/// The capture's D3D11 device, for an encoder that takes its frames where they are (the most recent
/// capture's; one source a process).
static DEVICE: Mutex<Option<Device>> = Mutex::new(None);

struct Device(ID3D11Device);
// D3D11 devices are free-threaded (and this one is multithread-protected: see `device`).
unsafe impl Send for Device {}

/// The device the capture's frames are on, when a capture has started.
pub fn shared_device() -> Option<ID3D11Device> {
    DEVICE.lock().unwrap().as_ref().map(|d| d.0.clone())
}

/// A D3D11 device on the default adapter (the one the compositor uses), with video support where
/// the driver has it (the encoder's color conversion and Media Foundation need it), and safe to use
/// from the capture's thread and the encoder's at once.
fn device() -> Result<(ID3D11Device, ID3D11DeviceContext), String> {
    let make = |flags: D3D11_CREATE_DEVICE_FLAG| {
        let (mut device, mut context) = (None, None);
        unsafe {
            D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_HARDWARE,
                HMODULE::default(),
                flags,
                None,
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut context),
            )
        }
        .ok()
        .and_then(|_| Some((device?, context?)))
    };
    let (device, context) = make(D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_VIDEO_SUPPORT)
        .or_else(|| make(D3D11_CREATE_DEVICE_BGRA_SUPPORT))
        .ok_or("no Direct3D 11 device for capturing")?;
    if let Ok(mt) = device.cast::<ID3D11Multithread>() {
        unsafe {
            let _ = mt.SetMultithreadProtected(true);
        }
    }
    Ok((device, context))
}

/// A WinRT device for the frame pool's thread (Direct3D devices are free-threaded).
struct Agile<T>(T);
unsafe impl<T> Send for Agile<T> {}
impl<T> Agile<T> {
    fn get(&self) -> &T {
        &self.0
    }
}

/// What the capture's callback leaves for the source: the latest frame, in memory or on the GPU.
struct Latest {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    /// The latest frame read back into memory (BGRA, `stride` bytes a row), when `count == cpu_count`.
    bgra: Vec<u8>,
    stride: usize,
    w: usize,
    h: usize,
    cpu_count: u64,
    /// Frames as they come stay on the GPU (the encoder takes them there): copied into `texture`,
    /// which holds the latest when `count == gpu_count`.
    gpu: bool,
    texture: Option<ID3D11Texture2D>,
    gpu_count: u64,
    /// A texture the CPU can read, to read a frame back.
    staging: Option<ID3D11Texture2D>,
    count: u64,
}

// COM objects used from the capture's thread and the source's, one at a time (behind the lock); the
// device is multithread-protected.
unsafe impl Send for Latest {}

impl Latest {
    /// A copy of `tex` (BGRA) in a texture of the same size, kept: `slot` is made or remade to fit.
    fn copy_into(&self, slot: &mut Option<ID3D11Texture2D>, tex: &ID3D11Texture2D, staging: bool) -> Result<(), String> {
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        unsafe { tex.GetDesc(&mut desc) };
        let fits = slot.as_ref().is_some_and(|t| {
            let mut d = D3D11_TEXTURE2D_DESC::default();
            unsafe { t.GetDesc(&mut d) };
            (d.Width, d.Height, d.Format) == (desc.Width, desc.Height, desc.Format)
        });
        if !fits {
            let mut want = D3D11_TEXTURE2D_DESC {
                MipLevels: 1,
                ArraySize: 1,
                MiscFlags: 0,
                ..desc
            };
            if staging {
                (want.Usage, want.BindFlags, want.CPUAccessFlags) = (D3D11_USAGE_STAGING, 0, D3D11_CPU_ACCESS_READ.0 as u32);
            } else {
                // Read by the encoder's video processor (an input view needs a texture it can render
                // from).
                (want.Usage, want.BindFlags, want.CPUAccessFlags) =
                    (D3D11_USAGE_DEFAULT, (D3D11_BIND_SHADER_RESOURCE.0 | D3D11_BIND_RENDER_TARGET.0) as u32, 0);
            }
            let mut t = None;
            unsafe { self.device.CreateTexture2D(&want, None, Some(&mut t)) }.map_err(|e| format!("can't make a texture: {e}"))?;
            *slot = t;
        }
        let dst = slot.as_ref().ok_or("no texture")?;
        unsafe { self.context.CopyResource(dst, tex) };
        Ok(())
    }

    /// The frame in `tex` into memory, its top-left `w`×`h`.
    fn read_back(&mut self, tex: &ID3D11Texture2D, w: usize, h: usize) -> Result<(), String> {
        let mut staging = self.staging.take();
        let r = self.copy_into(&mut staging, tex, true);
        self.staging = staging;
        r?;
        let staging = self.staging.as_ref().ok_or("no staging texture")?;
        let mut map = D3D11_MAPPED_SUBRESOURCE::default();
        unsafe { self.context.Map(staging, 0, D3D11_MAP_READ, 0, Some(&mut map)) }.map_err(|e| format!("can't read the frame: {e}"))?;
        let pitch = map.RowPitch as usize;
        let rows = unsafe { std::slice::from_raw_parts(map.pData as *const u8, pitch * h) };
        self.bgra.clear();
        self.bgra.extend_from_slice(rows);
        unsafe { self.context.Unmap(staging, 0) };
        (self.stride, self.w, self.h) = (pitch, w, h);
        Ok(())
    }
}

/// A running capture (a window's or a display's), handing each new frame to `Latest`.
struct Capture {
    latest: Arc<Mutex<Latest>>,
    pool: Direct3D11CaptureFramePool,
    session: GraphicsCaptureSession,
    token: i64,
}

impl Capture {
    fn start(item: GraphicsCaptureItem) -> Result<Self, String> {
        unsafe {
            let _ = RoInitialize(RO_INIT_MULTITHREADED);
        }
        let e = |what: &str| {
            let what = what.to_string();
            move |err: windows::core::Error| format!("couldn't start capturing: {what} ({err})")
        };
        let (device, context) = device()?;
        let winrt_device: IDirect3DDevice = unsafe { CreateDirect3D11DeviceFromDXGIDevice(&device.cast::<IDXGIDevice>().map_err(e("DXGI"))?) }
            .and_then(|d| d.cast())
            .map_err(e("a WinRT device"))?;
        let size = item.Size().map_err(e("its size"))?;
        // Free-threaded: frames arrive on a system thread, no message loop needed. Two buffers: one
        // being read while the next is drawn.
        let format = DirectXPixelFormat::B8G8R8A8UIntNormalized;
        let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(&winrt_device, format, 2, size).map_err(e("a frame pool"))?;
        let latest = Arc::new(Mutex::new(Latest {
            device: device.clone(),
            context,
            bgra: Vec::new(),
            stride: 0,
            w: 0,
            h: 0,
            cpu_count: 0,
            gpu: false,
            texture: None,
            gpu_count: 0,
            staging: None,
            count: 0,
        }));
        let shared = latest.clone();
        let last_size = Mutex::new(size);
        let pool_device = Agile(winrt_device.clone());
        let token = pool
            .FrameArrived(&TypedEventHandler::<Direct3D11CaptureFramePool, windows::core::IInspectable>::new(
                move |pool, _| {
                    let Some(pool) = pool.as_ref() else { return Ok(()) };
                    let frame = pool.TryGetNextFrame()?;
                    let content = frame.ContentSize()?;
                    // The window changed size: buffers of the new size from the next frame on.
                    let mut last = last_size.lock().unwrap();
                    if (content.Width, content.Height) != (last.Width, last.Height) {
                        *last = content;
                        pool.Recreate(pool_device.get(), format, 2, content)?;
                        return Ok(());
                    }
                    let tex: ID3D11Texture2D = unsafe { frame.Surface()?.cast::<IDirect3DDxgiInterfaceAccess>()?.GetInterface()? };
                    let (w, h) = (content.Width.max(0) as usize, content.Height.max(0) as usize);
                    let mut l = shared.lock().unwrap();
                    l.count += 1;
                    if l.gpu {
                        let mut t = l.texture.take();
                        let r = l.copy_into(&mut t, &tex, false);
                        l.texture = t;
                        (l.w, l.h) = (w, h);
                        if r.is_ok() {
                            l.gpu_count = l.count;
                        } else if let Err(err) = r {
                            // Back to frames in memory: the source says no surface from now on.
                            eprintln!("rein-remote: {err}; reading frames back into memory");
                            l.gpu = false;
                        }
                    }
                    if !l.gpu && l.read_back(&tex, w, h).is_ok() {
                        l.cpu_count = l.count;
                    }
                    Ok(())
                },
            ))
            .map_err(e("frame events"))?;
        let session = pool.CreateCaptureSession(&item).map_err(e("a session"))?;
        // The pointer goes apart from the video (the client draws its own), no yellow border, and
        // frames as often as they change (the compositor's default is its refresh rate at most).
        // Older Windows lacks some of these: capture anyway.
        let _ = session.SetIsCursorCaptureEnabled(false);
        let _ = session.SetIsBorderRequired(false);
        let _ = session.SetMinUpdateInterval(Duration::from_millis(4).into());
        session.StartCapture().map_err(e("the session"))?;
        *DEVICE.lock().unwrap() = Some(Device(device));
        Ok(Self { latest, pool, session, token })
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        let _ = self.pool.RemoveFrameArrived(self.token);
        let _ = self.session.Close();
        let _ = self.pool.Close();
    }
}

/// What's streamed: a window (found again for each pointer event, as it can move), or a display
/// (where it is on the desktop).
enum Target {
    Window(HWND),
    Display(RECT),
}

pub struct WinSource {
    target: Target,
    w: usize,
    h: usize,
    capture: Capture,
    frame: Vec<u8>,
    pointer: (i32, i32),
}

// The window handle and the capture are only used by whoever holds the source (behind a lock).
unsafe impl Send for WinSource {}

impl WinSource {
    /// `query`: a window id (from `rein-remote windows`), or part of its title or its program's name.
    pub fn open_window(query: &str) -> Result<Self, String> {
        dpi_aware();
        let q = query.to_lowercase();
        let id = query
            .strip_prefix("0x")
            .map_or_else(|| query.parse::<u64>().ok(), |hex| u64::from_str_radix(hex, 16).ok());
        let window = Window::enumerate()
            .map_err(|e| format!("can't list windows: {e}"))?
            .into_iter()
            .filter(|w| w.width().is_ok_and(|x| x >= 64))
            .find(|w| {
                Some(w.as_raw_hwnd() as u64) == id
                    || w.title().is_ok_and(|t| t.to_lowercase().contains(&q))
                    || w.process_name().is_ok_and(|p| p.to_lowercase().contains(&q))
            })
            .ok_or_else(|| format!("no window matches \"{query}\" (rein-remote windows lists them)"))?;
        let hwnd = HWND(window.as_raw_hwnd());
        let rect = bounds(hwnd).ok_or("can't tell where the window is")?;
        let (w, h) = (((rect.right - rect.left) as usize) & !1, ((rect.bottom - rect.top) as usize) & !1);
        let title = window.title().unwrap_or_default();
        let interop = windows::core::factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>().map_err(|e| format!("no Graphics Capture here: {e}"))?;
        let item: GraphicsCaptureItem = unsafe { interop.CreateForWindow(hwnd) }.map_err(|e| format!("couldn't capture the window: {e}"))?;
        let capture = Capture::start(item)?;
        eprintln!("rein-remote: window {} ({title}), {w}×{h}", hwnd.0 as u64);
        Ok(Self {
            target: Target::Window(hwnd),
            w,
            h,
            capture,
            frame: vec![0; w * h * 4],
            pointer: (rect.left, rect.top),
        })
    }

    /// The primary display, whole; input lands on it.
    pub fn open_display() -> Result<Self, String> {
        dpi_aware();
        let (monitor, rect) = unsafe {
            let m = MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY);
            let mut info = MONITORINFO {
                cbSize: std::mem::size_of::<MONITORINFO>() as u32,
                ..Default::default()
            };
            if !GetMonitorInfoW(m, &mut info).as_bool() {
                return Err("can't find the primary display".into());
            }
            (m, info.rcMonitor)
        };
        let (w, h) = (((rect.right - rect.left) as usize) & !1, ((rect.bottom - rect.top) as usize) & !1);
        let interop = windows::core::factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>().map_err(|e| format!("no Graphics Capture here: {e}"))?;
        let item: GraphicsCaptureItem = unsafe { interop.CreateForMonitor(monitor) }.map_err(|e| format!("couldn't capture the display: {e}"))?;
        let capture = Capture::start(item)?;
        eprintln!("rein-remote: the primary display, {w}×{h}");
        Ok(Self {
            target: Target::Display(rect),
            w,
            h,
            capture,
            frame: vec![0; w * h * 4],
            pointer: (rect.left, rect.top),
        })
    }

    fn front(&self) {
        let Target::Window(hwnd) = self.target else { return };
        unsafe {
            if GetForegroundWindow() != hwnd {
                let _ = SetForegroundWindow(hwnd);
                let _ = BringWindowToTop(hwnd);
                std::thread::sleep(Duration::from_millis(40));
            }
        }
    }

    /// Where what's streamed is on the desktop now: its top-left corner.
    fn origin(&self) -> Result<(i32, i32), String> {
        match self.target {
            Target::Window(hwnd) => bounds(hwnd).map(|r| (r.left, r.top)).ok_or_else(|| "the window is gone".into()),
            Target::Display(r) => Ok((r.left, r.top)),
        }
    }
}

/// Where the window is on screen, as it's drawn (without the invisible resize borders).
fn bounds(hwnd: HWND) -> Option<RECT> {
    let mut r = RECT::default();
    unsafe {
        DwmGetWindowAttribute(
            hwnd,
            DWMWA_EXTENDED_FRAME_BOUNDS,
            &mut r as *mut RECT as *mut _,
            std::mem::size_of::<RECT>() as u32,
        )
    }
    .ok()?;
    Some(r)
}

fn send(inputs: &[INPUT]) {
    unsafe {
        SendInput(inputs, std::mem::size_of::<INPUT>() as i32);
    }
}

fn mouse(flags: MOUSE_EVENT_FLAGS, x: i32, y: i32, data: i32) -> INPUT {
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx: x,
                dy: y,
                mouseData: data as u32,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

fn key(vk: u16, scan: u16, flags: KEYBD_EVENT_FLAGS) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VIRTUAL_KEY(vk),
                wScan: scan,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

/// Text as Unicode key events (any language, emoji), not as one layout's keys.
fn type_text(text: &str) {
    let mut events = Vec::new();
    for unit in text.encode_utf16() {
        events.push(key(0, unit, KEYEVENTF_UNICODE));
        events.push(key(0, unit, KEYEVENTF_UNICODE | KEYEVENTF_KEYUP));
    }
    send(&events);
}

/// Virtual-key codes for the browser's key names (those without text).
fn vk(name: &str) -> Option<u16> {
    Some(match name {
        "Enter" => 0x0d,
        "Tab" => 0x09,
        "Backspace" => 0x08,
        "Escape" => 0x1b,
        "Delete" => 0x2e,
        "Insert" => 0x2d,
        "Home" => 0x24,
        "End" => 0x23,
        "PageUp" => 0x21,
        "PageDown" => 0x22,
        "ArrowLeft" => 0x25,
        "ArrowUp" => 0x26,
        "ArrowRight" => 0x27,
        "ArrowDown" => 0x28,
        "Shift" => 0x10,
        "Control" => 0x11,
        "Alt" => 0x12,
        "Meta" => 0x5b,
        "CapsLock" => 0x14,
        " " => 0x20,
        f if f.starts_with('F') && f[1..].parse::<u16>().is_ok_and(|n| (1..=24).contains(&n)) => 0x6f + f[1..].parse::<u16>().unwrap(),
        _ => return None,
    })
}

impl Source for WinSource {
    fn size(&self) -> (usize, usize) {
        (self.w, self.h)
    }

    fn generation(&mut self) -> Option<u64> {
        Some(self.capture.latest.lock().unwrap().count)
    }

    fn grab(&mut self) -> Result<Frame<'_>, String> {
        let mut l = self.capture.latest.lock().unwrap();
        // The latest frame, if it only went to the GPU, read back now.
        if l.count > 0 && l.cpu_count != l.count && l.gpu_count == l.count {
            if let Some(tex) = l.texture.clone() {
                let (w, h) = (l.w, l.h);
                l.read_back(&tex, w, h)?;
                l.cpu_count = l.count;
            }
        }
        if l.cpu_count > 0 {
            // The window may have changed size since: its top-left part, the rest black.
            let (rows, cols) = (l.h.min(self.h), l.w.min(self.w) * 4);
            for y in 0..rows {
                self.frame[y * self.w * 4..y * self.w * 4 + cols].copy_from_slice(&l.bgra[y * l.stride..y * l.stride + cols]);
            }
        }
        drop(l);
        Ok(Frame {
            bgra: &self.frame,
            stride: self.w * 4,
        })
    }

    /// The latest frame where it is, on the GPU (a BGRA texture on `shared_device()`): asking once
    /// keeps the next frames there. None until one has come that way (read this one with `grab`).
    fn surface(&mut self) -> Option<Surface> {
        let mut l = self.capture.latest.lock().unwrap();
        l.gpu = true;
        if l.gpu_count != l.count {
            return None;
        }
        l.texture.clone().map(Surface::texture)
    }

    fn input(&mut self, ev: &Input) -> Result<(), String> {
        let flags = MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK;
        match ev {
            Input::PointerMove { x, y } => {
                let (left, top) = self.origin()?;
                self.pointer = (left + *x as i32, top + *y as i32);
                // Absolute positions are 0..65535 across the whole virtual desktop.
                let (vx, vy, vw, vh) = unsafe {
                    (
                        GetSystemMetrics(SM_XVIRTUALSCREEN),
                        GetSystemMetrics(SM_YVIRTUALSCREEN),
                        GetSystemMetrics(SM_CXVIRTUALSCREEN).max(1),
                        GetSystemMetrics(SM_CYVIRTUALSCREEN).max(1),
                    )
                };
                let nx = ((self.pointer.0 - vx) * 65535) / (vw - 1).max(1);
                let ny = ((self.pointer.1 - vy) * 65535) / (vh - 1).max(1);
                send(&[mouse(flags | MOUSEEVENTF_MOVE, nx, ny, 0)]);
            }
            Input::Button { button, down } => {
                if *down {
                    self.front();
                }
                let f = match (button, down) {
                    (Button::Left, true) => MOUSEEVENTF_LEFTDOWN,
                    (Button::Left, false) => MOUSEEVENTF_LEFTUP,
                    (Button::Right, true) => MOUSEEVENTF_RIGHTDOWN,
                    (Button::Right, false) => MOUSEEVENTF_RIGHTUP,
                    (Button::Middle, true) => MOUSEEVENTF_MIDDLEDOWN,
                    (Button::Middle, false) => MOUSEEVENTF_MIDDLEUP,
                    (Button::Back | Button::Forward, true) => MOUSEEVENTF_XDOWN,
                    (Button::Back | Button::Forward, false) => MOUSEEVENTF_XUP,
                };
                let data = match button {
                    Button::Back => 1,
                    Button::Forward => 2,
                    _ => 0,
                };
                send(&[mouse(f, 0, 0, data)]);
            }
            Input::Wheel { dx, dy } => {
                // Pixels as wheel units (120 a notch); content follows the fingers.
                if *dy != 0 {
                    send(&[mouse(MOUSEEVENTF_WHEEL, 0, 0, -(*dy as i32) * 3)]);
                }
                if *dx != 0 {
                    send(&[mouse(MOUSEEVENTF_HWHEEL, 0, 0, *dx as i32 * 3)]);
                }
            }
            Input::Key { key: name, down, .. } => {
                self.front();
                if let Some(code) = vk(name) {
                    send(&[key(code, 0, if *down { KEYBD_EVENT_FLAGS(0) } else { KEYEVENTF_KEYUP })]);
                } else if *down && name.chars().count() == 1 {
                    type_text(name);
                }
            }
            Input::Text { text } => {
                self.front();
                type_text(text);
            }
            _ => {}
        }
        Ok(())
    }
}
