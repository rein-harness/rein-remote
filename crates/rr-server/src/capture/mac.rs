//! macOS: a window (or a whole display) captured with ScreenCaptureKit, input sent with Core Graphics
//! events. Any app's window works the same way (a native app, the iOS Simulator, the Android
//! emulator, a game): nothing in the app needs to support it. ScreenCaptureKit sends a frame only
//! when the window changed, so a still window costs nothing.
//!
//! Permissions (System Settings → Privacy & Security): Screen Recording to see the window, and
//! Accessibility to control it, both for the app that runs rein-remote (your terminal, or Rein).

use super::{Frame, Source};
use block2::RcBlock;
use core_graphics::display::CGDisplay;
use core_graphics::event::{CGEvent, CGEventType, CGKeyCode, CGMouseButton, ScrollEventUnit};
use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};
use core_graphics::geometry::CGPoint;
use dispatch2::DispatchQueue;
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{define_class, msg_send, AllocAnyThread, DefinedClass};
use objc2_core_media::{CMSampleBuffer, CMTime, CMTimeFlags};
use objc2_core_video::{
    kCVPixelFormatType_32BGRA, CVPixelBufferGetBaseAddress, CVPixelBufferGetBytesPerRow, CVPixelBufferGetHeight, CVPixelBufferLockBaseAddress,
    CVPixelBufferLockFlags, CVPixelBufferUnlockBaseAddress,
};
use objc2_foundation::{NSError, NSObject, NSObjectProtocol};
use objc2_screen_capture_kit::{SCContentFilter, SCShareableContent, SCStream, SCStreamConfiguration, SCStreamOutput, SCStreamOutputType, SCWindow};
use rr_proto::{Button, Input};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn AXUIElementCreateApplication(pid: i32) -> *mut std::ffi::c_void;
    fn AXUIElementSetAttributeValue(el: *mut std::ffi::c_void, attr: core_foundation::string::CFStringRef, value: core_foundation::base::CFTypeRef) -> i32;
    fn AXUIElementCopyAttributeValue(
        el: *mut std::ffi::c_void,
        attr: core_foundation::string::CFStringRef,
        value: *mut core_foundation::base::CFTypeRef,
    ) -> i32;
    fn AXUIElementPerformAction(el: *mut std::ffi::c_void, action: core_foundation::string::CFStringRef) -> i32;
    /// The window id of an accessibility window element (private, but stable for years and what
    /// window managers use; when it's missing the first window is raised).
    fn _AXUIElementGetWindow(el: *mut std::ffi::c_void, id: *mut u32) -> i32;
    fn AXIsProcessTrustedWithOptions(options: core_foundation::dictionary::CFDictionaryRef) -> bool;
}

/// Whether the app is the one in front.
fn frontmost(pid: i32) -> bool {
    use core_foundation::base::{CFRelease, CFTypeRef, TCFType};
    use core_foundation::boolean::CFBoolean;
    use core_foundation::string::CFString;
    unsafe {
        let app = AXUIElementCreateApplication(pid);
        if app.is_null() {
            return false;
        }
        let mut v: CFTypeRef = std::ptr::null();
        let ok = AXUIElementCopyAttributeValue(app, CFString::new("AXFrontmost").as_concrete_TypeRef(), &mut v) == 0 && !v.is_null();
        let front = ok && CFBoolean::wrap_under_create_rule(v as _) == CFBoolean::true_value();
        CFRelease(app as CFTypeRef);
        front
    }
}

/// Bring the app to the front and its window above the others, as a click on it would locally: the
/// pointer's events then reach it (a click goes to the window on top where it lands). It happens
/// asynchronously, so this waits (up to half a second) for the app to be in front.
fn raise(pid: i32, window_id: u32) {
    raise_now(pid, window_id);
    let start = Instant::now();
    while !frontmost(pid) && start.elapsed() < Duration::from_millis(500) {
        std::thread::sleep(Duration::from_millis(15));
    }
    // The window server puts it on top a moment after the app becomes active.
    std::thread::sleep(Duration::from_millis(40));
}

fn raise_now(pid: i32, window_id: u32) {
    use core_foundation::array::{CFArray, CFArrayRef};
    use core_foundation::base::{CFRelease, CFTypeRef, TCFType};
    use core_foundation::boolean::CFBoolean;
    use core_foundation::string::CFString;
    unsafe {
        let app = AXUIElementCreateApplication(pid);
        if app.is_null() {
            return;
        }
        AXUIElementSetAttributeValue(app, CFString::new("AXFrontmost").as_concrete_TypeRef(), CFBoolean::true_value().as_CFTypeRef());
        let mut list: CFTypeRef = std::ptr::null();
        if AXUIElementCopyAttributeValue(app, CFString::new("AXWindows").as_concrete_TypeRef(), &mut list) == 0 && !list.is_null() {
            let windows: CFArray<*const std::ffi::c_void> = CFArray::wrap_under_create_rule(list as CFArrayRef);
            let mut target = None;
            for w in windows.iter() {
                let mut id = 0u32;
                if _AXUIElementGetWindow(*w as *mut _, &mut id) == 0 && id == window_id {
                    target = Some(*w);
                    break;
                }
            }
            if let Some(w) = target.or_else(|| windows.iter().next().map(|w| *w)) {
                AXUIElementPerformAction(w as *mut _, CFString::new("AXRaise").as_concrete_TypeRef());
            }
        }
        CFRelease(app as CFTypeRef);
    }
}

/// Whether this app may send input, asking macOS to prompt for it when it may not: the system
/// dialog adds the app (your terminal, or Rein) to Privacy & Security → Accessibility.
fn may_control() -> bool {
    use core_foundation::base::TCFType;
    use core_foundation::boolean::CFBoolean;
    use core_foundation::dictionary::CFDictionary;
    use core_foundation::string::CFString;
    let opts = CFDictionary::from_CFType_pairs(&[(CFString::new("AXTrustedCheckOptionPrompt"), CFBoolean::true_value())]);
    unsafe { AXIsProcessTrustedWithOptions(opts.as_concrete_TypeRef()) }
}

/// A window this machine shows: for `rein-remote windows` and for picking one by name.
pub struct WindowInfo {
    pub id: u32,
    pub app: String,
    pub pid: i32,
    pub title: String,
    pub width: u32,
    pub height: u32,
}

/// Objective-C objects handed between threads: only ever used by one at a time.
struct Sendable<T>(T);
unsafe impl<T> Send for Sendable<T> {}

fn error_text(err: *mut NSError) -> String {
    if err.is_null() {
        "unknown error".into()
    } else {
        unsafe { (*err).localizedDescription().to_string() }
    }
}

/// What's on screen now (ScreenCaptureKit answers on its own queue).
fn content() -> Result<Retained<SCShareableContent>, String> {
    let (tx, rx) = mpsc::channel();
    let block = RcBlock::new(move |c: *mut SCShareableContent, err: *mut NSError| {
        let r = unsafe { Retained::retain(c) }.map(Sendable).ok_or_else(|| error_text(err));
        let _ = tx.send(r);
    });
    unsafe { SCShareableContent::getShareableContentWithCompletionHandler(&block) };
    match rx.recv_timeout(Duration::from_secs(10)) {
        Ok(Ok(c)) => Ok(c.0),
        Ok(Err(e)) => Err(format!(
            "can't see the screen ({e}): allow Screen Recording for the app that runs rein-remote (System Settings → Privacy & Security → Screen Recording), then start it again"
        )),
        Err(_) => Err("the screen didn't answer (ScreenCaptureKit timed out)".into()),
    }
}

fn app_name(w: &SCWindow) -> String {
    unsafe { w.owningApplication() }
        .map(|a| unsafe { a.applicationName() }.to_string())
        .unwrap_or_default()
}

fn title(w: &SCWindow) -> String {
    unsafe { w.title() }.map(|t| t.to_string()).unwrap_or_default()
}

/// The windows on screen worth showing (titled or not, not tiny, not the system's own layers).
pub fn windows() -> Result<Vec<WindowInfo>, String> {
    let c = content()?;
    let mut out = Vec::new();
    for w in unsafe { c.windows() }.iter() {
        let f = unsafe { w.frame() };
        if !unsafe { w.isOnScreen() } || f.size.width < 64.0 || f.size.height < 64.0 || unsafe { w.windowLayer() } != 0 {
            continue;
        }
        let app = app_name(&w);
        if app.is_empty() || ["Window Server", "Dock", "Control Center", "Notification Center", "SystemUIServer", "Wallpaper"].contains(&app.as_str()) {
            continue;
        }
        out.push(WindowInfo {
            id: unsafe { w.windowID() },
            pid: unsafe { w.owningApplication() }.map_or(0, |a| unsafe { a.processID() }),
            app,
            title: title(&w),
            width: f.size.width as u32,
            height: f.size.height as u32,
        });
    }
    Ok(out)
}

/// The latest frame ScreenCaptureKit delivered, and how many it has.
#[derive(Default)]
struct Latest {
    bgra: Vec<u8>,
    stride: usize,
    rows: usize,
    count: u64,
}

define_class!(
    // Receives the window's frames on ScreenCaptureKit's queue.
    #[unsafe(super(NSObject))]
    #[name = "ReinRemoteFrameOutput"]
    #[ivars = Arc<Mutex<Latest>>]
    struct Output;

    unsafe impl NSObjectProtocol for Output {}

    unsafe impl SCStreamOutput for Output {
        #[unsafe(method(stream:didOutputSampleBuffer:ofType:))]
        unsafe fn did_output(&self, _stream: &SCStream, sample: &CMSampleBuffer, kind: SCStreamOutputType) {
            if kind != SCStreamOutputType::Screen {
                return;
            }
            // An idle frame (nothing changed) has no image.
            let Some(pb) = (unsafe { sample.image_buffer() }) else { return };
            unsafe { CVPixelBufferLockBaseAddress(&pb, CVPixelBufferLockFlags::ReadOnly) };
            let base = CVPixelBufferGetBaseAddress(&pb) as *const u8;
            let (stride, rows) = (CVPixelBufferGetBytesPerRow(&pb), CVPixelBufferGetHeight(&pb));
            if !base.is_null() {
                let bytes = unsafe { std::slice::from_raw_parts(base, stride * rows) };
                let mut l = self.ivars().lock().unwrap();
                l.bgra.clear();
                l.bgra.extend_from_slice(bytes);
                l.stride = stride;
                l.rows = rows;
                l.count += 1;
            }
            unsafe { CVPixelBufferUnlockBaseAddress(&pb, CVPixelBufferLockFlags::ReadOnly) };
        }
    }
);

impl Output {
    fn new(latest: Arc<Mutex<Latest>>) -> Retained<Self> {
        let this = Self::alloc().set_ivars(latest);
        unsafe { msg_send![super(this), init] }
    }
}

pub struct MacWindow {
    w: usize,
    h: usize,
    window_id: u32,
    /// Window pixels per screen point (2 on a Retina display).
    scale: f64,
    /// Where the window is on screen, in points: kept fresh, since it can move.
    origin: (f64, f64),
    origin_at: Instant,
    latest: Arc<Mutex<Latest>>,
    frame: Vec<u8>,
    stride: usize,
    _stream: Retained<SCStream>,
    _output: Retained<Output>,
    buttons: [bool; 3],
    /// The app that owns the window: events go to it, so your own pointer and keyboard stay yours
    /// (and a window behind others still gets them).
    pid: i32,
    /// Where the remote pointer is, on screen (events carry it; the real pointer doesn't move).
    pointer: CGPoint,
}

// The stream is only touched by whoever holds the source (behind a lock).
unsafe impl Send for MacWindow {}

impl MacWindow {
    /// `query`: a window id (from `rein-remote windows`), or part of an app's name or a window's title.
    pub fn open(query: &str) -> Result<Self, String> {
        let c = content()?;
        let q = query.to_lowercase();
        let list = unsafe { c.windows() };
        let window = list
            .iter()
            .filter(|w| unsafe { w.isOnScreen() } && unsafe { w.frame() }.size.width >= 64.0 && unsafe { w.windowLayer() } == 0)
            .find(|w| {
                query.parse::<u32>().is_ok_and(|id| id == unsafe { w.windowID() })
                    || app_name(w).to_lowercase().contains(&q)
                    || title(w).to_lowercase().contains(&q)
            })
            .ok_or_else(|| format!("no window matches \"{query}\" (rein-remote windows lists them)"))?;
        let frame = unsafe { window.frame() };
        let main = CGDisplay::main();
        let scale = (main.pixels_wide() as f64 / main.bounds().size.width).max(1.0);
        let (w, h) = (((frame.size.width * scale) as usize) & !1, ((frame.size.height * scale) as usize) & !1);
        let filter = unsafe { SCContentFilter::initWithDesktopIndependentWindow(SCContentFilter::alloc(), &window) };
        let config = unsafe { SCStreamConfiguration::new() };
        unsafe {
            config.setWidth(w);
            config.setHeight(h);
            config.setPixelFormat(kCVPixelFormatType_32BGRA);
            // The pointer goes apart from the video; the client draws its own.
            config.setShowsCursor(false);
            config.setMinimumFrameInterval(CMTime {
                value: 1,
                timescale: 120,
                flags: CMTimeFlags::Valid,
                epoch: 0,
            });
            config.setQueueDepth(3);
        }
        let latest = Arc::new(Mutex::new(Latest::default()));
        let output = Output::new(latest.clone());
        let stream = unsafe { SCStream::initWithFilter_configuration_delegate(SCStream::alloc(), &filter, &config, None) };
        let queue = DispatchQueue::new("rein-remote.frames", None);
        unsafe { stream.addStreamOutput_type_sampleHandlerQueue_error(ProtocolObject::from_ref(&*output), SCStreamOutputType::Screen, Some(&queue)) }
            .map_err(|e| e.localizedDescription().to_string())?;
        let (tx, rx) = mpsc::channel();
        let done = RcBlock::new(move |err: *mut NSError| {
            let _ = tx.send(if err.is_null() { Ok(()) } else { Err(error_text(err)) });
        });
        unsafe { stream.startCaptureWithCompletionHandler(Some(&done)) };
        rx.recv_timeout(Duration::from_secs(10))
            .map_err(|_| "the capture didn't start".to_string())?
            .map_err(|e| format!("couldn't start capturing the window: {e}"))?;
        let can_control = may_control();
        if !can_control {
            eprintln!("rein-remote: showing the window, but input won't reach it: allow Accessibility for the app that runs rein-remote (System Settings → Privacy & Security → Accessibility)");
        }
        let id = unsafe { window.windowID() };
        let t = title(&window);
        eprintln!(
            "rein-remote: window {id} ({}{}), {w}×{h}",
            app_name(&window),
            if t.is_empty() { String::new() } else { format!(": {t}") }
        );
        Ok(Self {
            w,
            h,
            window_id: id,
            scale,
            origin: (frame.origin.x, frame.origin.y),
            origin_at: Instant::now(),
            latest,
            frame: vec![0; w * h * 4],
            stride: w * 4,
            _stream: stream,
            _output: output,
            buttons: [false; 3],
            pid: unsafe { window.owningApplication() }.map_or(0, |a| unsafe { a.processID() }),
            pointer: CGPoint::new(frame.origin.x, frame.origin.y),
        })
    }

    /// The window's place on screen now (it may have moved), at most twice a second.
    fn refresh_origin(&mut self) {
        if self.origin_at.elapsed() < Duration::from_millis(500) {
            return;
        }
        self.origin_at = Instant::now();
        if let Ok(c) = content() {
            if let Some(w) = unsafe { c.windows() }.iter().find(|w| unsafe { w.windowID() } == self.window_id) {
                let f = unsafe { w.frame() };
                self.origin = (f.origin.x, f.origin.y);
            }
        }
    }

    /// A point in the window's pixels as a point on the screen.
    fn at(&mut self, x: u16, y: u16) -> CGPoint {
        self.refresh_origin();
        CGPoint::new(self.origin.0 + x as f64 / self.scale, self.origin.1 + y as f64 / self.scale)
    }

    /// Keys go straight to the window's app: they work while it's behind others, and your own
    /// keyboard keeps typing where you are.
    fn post_key(&self, ev: Result<CGEvent, ()>) {
        if let Ok(e) = ev {
            e.post_to_pid(self.pid);
        }
    }

    fn post(&self, ev: Result<CGEvent, ()>) {
        if let Ok(e) = ev {
            // The pointer works as on the machine itself, like Remote Desktop: it moves, and a press
            // brings the window to the front first (raise), since a click goes to the window on top
            // where it lands. AppKit ignores mouse events sent to a process that isn't in front.
            e.post(core_graphics::event::CGEventTapLocation::HID);
        }
    }
}

fn source() -> Option<CGEventSource> {
    CGEventSource::new(CGEventSourceStateID::HIDSystemState).ok()
}

/// macOS virtual key codes for the browser's key names (those without text).
fn keycode(key: &str) -> Option<CGKeyCode> {
    Some(match key {
        "Enter" => 0x24,
        "Tab" => 0x30,
        "Backspace" => 0x33,
        "Escape" => 0x35,
        "Delete" => 0x75,
        "ArrowLeft" => 0x7b,
        "ArrowRight" => 0x7c,
        "ArrowDown" => 0x7d,
        "ArrowUp" => 0x7e,
        "Home" => 0x73,
        "End" => 0x77,
        "PageUp" => 0x74,
        "PageDown" => 0x79,
        "Shift" => 0x38,
        "Control" => 0x3b,
        "Alt" => 0x3a,
        "Meta" => 0x37,
        "CapsLock" => 0x39,
        " " => 0x31,
        "F1" => 0x7a,
        "F2" => 0x78,
        "F3" => 0x63,
        "F4" => 0x76,
        "F5" => 0x60,
        "F6" => 0x61,
        "F7" => 0x62,
        "F8" => 0x64,
        "F9" => 0x65,
        "F10" => 0x6d,
        "F11" => 0x67,
        "F12" => 0x6f,
        _ => return None,
    })
}

/// Text typed as text (any language, emoji), not as key codes of one layout.
fn type_text(pid: i32, text: &str) {
    for chunk in text.chars().collect::<Vec<_>>().chunks(16) {
        let s: String = chunk.iter().collect();
        for down in [true, false] {
            if let Some(src) = source() {
                if let Ok(e) = CGEvent::new_keyboard_event(src, 0, down) {
                    e.set_string(&s);
                    e.post_to_pid(pid);
                }
            }
        }
    }
}

impl Source for MacWindow {
    fn size(&self) -> (usize, usize) {
        (self.w, self.h)
    }

    fn generation(&mut self) -> Option<u64> {
        Some(self.latest.lock().unwrap().count)
    }

    fn grab(&mut self) -> Result<Frame<'_>, String> {
        let l = self.latest.lock().unwrap();
        if l.count > 0 && l.rows >= self.h && l.stride >= self.w * 4 {
            self.frame.clear();
            self.frame.extend_from_slice(&l.bgra[..l.stride * self.h]);
            self.stride = l.stride;
        }
        drop(l);
        Ok(Frame {
            bgra: &self.frame,
            stride: self.stride,
        })
    }

    fn input(&mut self, ev: &Input) -> Result<(), String> {
        // Sent even when macOS says this app may not (it drops them then; the check can lag behind a
        // permission just given).
        match ev {
            Input::PointerMove { x, y } => {
                let p = self.at(*x, *y);
                self.pointer = p;
                let (kind, button) = match self.buttons {
                    [true, _, _] => (CGEventType::LeftMouseDragged, CGMouseButton::Left),
                    [_, true, _] => (CGEventType::RightMouseDragged, CGMouseButton::Right),
                    [_, _, true] => (CGEventType::OtherMouseDragged, CGMouseButton::Center),
                    _ => (CGEventType::MouseMoved, CGMouseButton::Left),
                };
                if let Some(src) = source() {
                    self.post(CGEvent::new_mouse_event(src, kind, p, button));
                }
            }
            Input::Button { button, down } => {
                let (i, b, d, u) = match button {
                    Button::Left => (0, CGMouseButton::Left, CGEventType::LeftMouseDown, CGEventType::LeftMouseUp),
                    Button::Right => (1, CGMouseButton::Right, CGEventType::RightMouseDown, CGEventType::RightMouseUp),
                    Button::Middle => (2, CGMouseButton::Center, CGEventType::OtherMouseDown, CGEventType::OtherMouseUp),
                    _ => return Ok(()),
                };
                self.buttons[i] = *down;
                if *down && !frontmost(self.pid) {
                    raise(self.pid, self.window_id);
                }
                let here = self.pointer;
                if let Some(src) = source() {
                    // A click count of 1: AppKit's controls ignore a press without one.
                    let ev = CGEvent::new_mouse_event(src, if *down { d } else { u }, here, b);
                    if let Ok(e) = &ev {
                        e.set_integer_value_field(core_graphics::event::EventField::MOUSE_EVENT_CLICK_STATE, 1);
                    }
                    self.post(ev);
                }
            }
            Input::Wheel { dx, dy } => {
                if let Some(src) = source() {
                    // Pixels, and content follows the fingers as on a trackpad.
                    self.post(CGEvent::new_scroll_event(src, ScrollEventUnit::PIXEL, 2, -(*dy as i32), -(*dx as i32), 0));
                }
            }
            Input::Key { key, down, .. } => {
                if let Some(code) = keycode(key) {
                    if let Some(src) = source() {
                        self.post_key(CGEvent::new_keyboard_event(src, code, *down));
                    }
                } else if *down && key.chars().count() == 1 {
                    type_text(self.pid, key);
                }
            }
            Input::Text { text } => type_text(self.pid, text),
            _ => {}
        }
        Ok(())
    }
}
