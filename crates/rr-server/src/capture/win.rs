//! Windows: any app's window captured with Windows Graphics Capture (the compositor hands over each
//! frame as it changes, the yellow border and the cursor left out) and controlled with SendInput.
//! Nothing in the app needs to support it. A press brings the window to the front first, like
//! Remote Desktop, since a click goes to the window on top where it lands.

use super::{Frame, Source};
use rr_proto::{Button, Input};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_EXTENDED_FRAME_BOUNDS};
use windows::Win32::UI::HiDpi::{SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2};
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::{
    BringWindowToTop, GetForegroundWindow, GetSystemMetrics, SetForegroundWindow, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
};
use windows_capture::capture::{CaptureControl, Context, GraphicsCaptureApiHandler};
use windows_capture::frame::Frame as WgcFrame;
use windows_capture::graphics_capture_api::InternalCaptureControl;
use windows_capture::settings::{
    ColorFormat, CursorCaptureSettings, DirtyRegionSettings, DrawBorderSettings, MinimumUpdateIntervalSettings, SecondaryWindowSettings, Settings,
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

#[derive(Default)]
struct Latest {
    bgra: Vec<u8>,
    stride: usize,
    w: usize,
    h: usize,
    count: u64,
}

struct Handler(Arc<Mutex<Latest>>);

impl GraphicsCaptureApiHandler for Handler {
    type Flags = Arc<Mutex<Latest>>;
    type Error = Box<dyn std::error::Error + Send + Sync>;

    fn new(ctx: Context<Self::Flags>) -> Result<Self, Self::Error> {
        Ok(Self(ctx.flags))
    }

    fn on_frame_arrived(&mut self, frame: &mut WgcFrame, _control: InternalCaptureControl) -> Result<(), Self::Error> {
        let mut buf = frame.buffer()?;
        let (w, h, pitch) = (buf.width() as usize, buf.height() as usize, buf.row_pitch() as usize);
        let raw = buf.as_raw_buffer();
        let mut l = self.0.lock().unwrap();
        l.bgra.clear();
        l.bgra.extend_from_slice(&raw[..(pitch * h).min(raw.len())]);
        (l.stride, l.w, l.h) = (pitch, w, h);
        l.count += 1;
        Ok(())
    }
}

pub struct WinWindow {
    hwnd: HWND,
    w: usize,
    h: usize,
    latest: Arc<Mutex<Latest>>,
    frame: Vec<u8>,
    _control: CaptureControl<Handler, Box<dyn std::error::Error + Send + Sync>>,
    pointer: (i32, i32),
}

// The window handle and the capture are only used by whoever holds the source (behind a lock).
unsafe impl Send for WinWindow {}

impl WinWindow {
    /// `query`: a window id (from `rein-remote windows`), or part of its title or its program's name.
    pub fn open(query: &str) -> Result<Self, String> {
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
        let latest = Arc::new(Mutex::new(Latest::default()));
        let settings = Settings::new(
            window,
            // The pointer goes apart from the video; the client draws its own.
            CursorCaptureSettings::WithoutCursor,
            DrawBorderSettings::WithoutBorder,
            SecondaryWindowSettings::Default,
            MinimumUpdateIntervalSettings::Custom(Duration::from_millis(4)),
            DirtyRegionSettings::Default,
            ColorFormat::Bgra8,
            latest.clone(),
        );
        let control = Handler::start_free_threaded(settings).map_err(|e| format!("couldn't start capturing the window: {e}"))?;
        eprintln!("rein-remote: window {} ({title}), {w}×{h}", hwnd.0 as u64);
        Ok(Self {
            hwnd,
            w,
            h,
            latest,
            frame: vec![0; w * h * 4],
            _control: control,
            pointer: (rect.left, rect.top),
        })
    }

    fn front(&self) {
        unsafe {
            if GetForegroundWindow() != self.hwnd {
                let _ = SetForegroundWindow(self.hwnd);
                let _ = BringWindowToTop(self.hwnd);
                std::thread::sleep(Duration::from_millis(40));
            }
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

impl Source for WinWindow {
    fn size(&self) -> (usize, usize) {
        (self.w, self.h)
    }

    fn generation(&mut self) -> Option<u64> {
        Some(self.latest.lock().unwrap().count)
    }

    fn grab(&mut self) -> Result<Frame<'_>, String> {
        let l = self.latest.lock().unwrap();
        if l.count > 0 {
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

    fn input(&mut self, ev: &Input) -> Result<(), String> {
        let flags = MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK;
        match ev {
            Input::PointerMove { x, y } => {
                let r = bounds(self.hwnd).ok_or("the window is gone")?;
                self.pointer = (r.left + *x as i32, r.top + *y as i32);
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
