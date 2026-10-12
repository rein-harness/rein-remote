//! macOS: a window, an app (all its windows) or a whole display captured with ScreenCaptureKit, input
//! sent with Core Graphics events. Any app's window works the same way (a native app, the iOS Simulator, the Android
//! emulator, a game): nothing in the app needs to support it. ScreenCaptureKit sends a frame only
//! when the window changed, so a still window costs nothing.
//!
//! Permissions (System Settings → Privacy & Security): Screen Recording to see the window, and
//! Accessibility to control it, both for the app that runs rein-remote (your terminal, or Rein).

use super::{Frame, Pointer, Source};
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
use objc2_foundation::NSArray;
use objc2_foundation::{NSError, NSObject, NSObjectProtocol};
use objc2_screen_capture_kit::{SCContentFilter, SCDisplay, SCShareableContent, SCStream, SCStreamConfiguration, SCStreamOutput, SCStreamOutputType, SCWindow};
use rr_proto::{Button, CursorShape, Input};
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

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGColorSpaceCreateDeviceRGB() -> *mut std::ffi::c_void;
    fn CGColorSpaceRelease(cs: *mut std::ffi::c_void);
    fn CGBitmapContextCreate(
        data: *mut std::ffi::c_void,
        w: usize,
        h: usize,
        bits: usize,
        row: usize,
        cs: *mut std::ffi::c_void,
        info: u32,
    ) -> *mut std::ffi::c_void;
    fn CGContextDrawImage(ctx: *mut std::ffi::c_void, rect: core_graphics::geometry::CGRect, image: *const std::ffi::c_void);
    fn CGContextRelease(ctx: *mut std::ffi::c_void);
}

/// The system's pointer as it looks now (an arrow, an I-beam, a resize arrow…), drawn at `scale`
/// pixels a point: RGBA (straight alpha) and the hotspot.
fn system_cursor(scale: f64) -> Option<CursorShape> {
    // Deprecated, but the only way to have the pointer apart from the video (Apple's alternative draws
    // it into the frames, which costs video every time it moves and can't be drawn locally).
    #[allow(deprecated)]
    let cursor = objc2_app_kit::NSCursor::currentSystemCursor()?;
    let image = cursor.image();
    let (size, hot) = (image.size(), cursor.hotSpot());
    let cg = unsafe { image.CGImageForProposedRect_context_hints(std::ptr::null_mut(), None, None) }?;
    let (w, h) = (
        ((size.width * scale).round() as usize).clamp(1, 256),
        ((size.height * scale).round() as usize).clamp(1, 256),
    );
    let mut rgba = vec![0u8; w * h * 4];
    unsafe {
        let cs = CGColorSpaceCreateDeviceRGB();
        // Premultiplied RGBA, the only kind of 8-bit RGBA context there is.
        let ctx = CGBitmapContextCreate(
            rgba.as_mut_ptr().cast(),
            w,
            h,
            8,
            w * 4,
            cs,
            1 /* PremultipliedLast */ | (4 << 12), /* 32Big */
        );
        CGColorSpaceRelease(cs);
        if ctx.is_null() {
            return None;
        }
        let rect = core_graphics::geometry::CGRect::new(&CGPoint::new(0.0, 0.0), &core_graphics::geometry::CGSize::new(w as f64, h as f64));
        CGContextDrawImage(ctx, rect, objc2::rc::Retained::as_ptr(&cg).cast());
        CGContextRelease(ctx);
    }
    for px in rgba.chunks_exact_mut(4) {
        let a = px[3] as u32;
        if a > 0 && a < 255 {
            for c in &mut px[..3] {
                *c = ((*c as u32 * 255 + a / 2) / a).min(255) as u8;
            }
        }
    }
    Some(CursorShape {
        w: w as u16,
        h: h as u16,
        hx: (hot.x * scale).round() as u16,
        hy: (hot.y * scale).round() as u16,
        rgba,
    })
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
            // No window (an app or a display shown): the app in front is enough.
            if let Some(w) = target.or_else(|| windows.iter().next().map(|w| *w)).filter(|_| window_id != 0) {
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
pub(crate) struct Sendable<T>(pub T);
unsafe impl<T> Send for Sendable<T> {}

pub(crate) fn error_text(err: *mut NSError) -> String {
    if err.is_null() {
        "unknown error".into()
    } else {
        unsafe { (*err).localizedDescription().to_string() }
    }
}

/// What's on screen now (ScreenCaptureKit answers on its own queue).
pub(crate) fn content() -> Result<Retained<SCShareableContent>, String> {
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

/// A display by its number (1 is the main one, then in ScreenCaptureKit's order) or its id; none: the main one.
pub(crate) fn pick_display(c: &SCShareableContent, which: Option<&str>) -> Result<Retained<SCDisplay>, String> {
    let list = unsafe { c.displays() };
    let main = CGDisplay::main().id;
    let mut ordered: Vec<Retained<SCDisplay>> = list.iter().collect();
    ordered.sort_by_key(|d| unsafe { d.displayID() } != main);
    match which.map(str::trim).filter(|w| !w.is_empty()) {
        None => ordered.into_iter().next().ok_or_else(|| "no display".to_string()),
        Some(w) => {
            let n: u32 = w
                .trim_start_matches(':')
                .parse()
                .map_err(|_| format!("--display {w}: a number (1 is the main display) or a display id"))?;
            ordered
                .iter()
                .find(|d| unsafe { d.displayID() } == n)
                .or_else(|| ordered.get((n as usize).wrapping_sub(1)))
                .cloned()
                .ok_or_else(|| format!("no display {w} (rein-remote displays lists them)"))
        }
    }
}

/// Pixels per point on a display (2 on a Retina display): from its mode's real pixels (a scaled
/// mode's "pixels wide" is in points).
fn display_scale(id: u32, points_wide: f64) -> f64 {
    let d = CGDisplay::new(id);
    let px = d.display_mode().map_or(d.pixels_wide() as f64, |m| m.pixel_width() as f64);
    (px / points_wide.max(1.0)).max(1.0)
}

/// A display, for `rein-remote displays`.
pub struct DisplayInfo {
    /// What `--display` takes (1 is the main display).
    pub n: u32,
    pub id: u32,
    pub width: usize,
    pub height: usize,
    pub main: bool,
}

/// The displays, the main one first.
pub fn displays() -> Result<Vec<DisplayInfo>, String> {
    let c = content()?;
    let main = CGDisplay::main().id;
    let mut list: Vec<Retained<SCDisplay>> = unsafe { c.displays() }.iter().collect();
    list.sort_by_key(|d| unsafe { d.displayID() } != main);
    Ok(list
        .iter()
        .enumerate()
        .map(|(i, d)| {
            let id = unsafe { d.displayID() };
            let f = unsafe { d.frame() };
            let k = display_scale(id, f.size.width);
            DisplayInfo {
                n: i as u32 + 1,
                id,
                width: (f.size.width * k) as usize,
                height: (f.size.height * k) as usize,
                main: id == main,
            }
        })
        .collect())
}

/// The latest frame ScreenCaptureKit delivered, and how many it has.
#[derive(Default)]
struct Latest {
    /// The latest image as ScreenCaptureKit gave it (in GPU memory): read only when needed.
    image: Option<Sendable<objc2_core_foundation::CFRetained<objc2_core_video::CVBuffer>>>,
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
            // An idle frame (nothing changed) has no image. The image is kept, not copied: the
            // hardware encoder reads it where it is.
            let Some(pb) = (unsafe { sample.image_buffer() }) else { return };
            let mut l = self.ivars().lock().unwrap();
            l.image = Some(Sendable(pb));
            l.count += 1;
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
    /// What's captured, kept for the sound (audio::mac captures it with the same filter).
    filter: Retained<SCContentFilter>,
    buttons: [bool; 3],
    /// The app that owns the window: events go to it, so your own pointer and keyboard stay yours
    /// (and a window behind others still gets them).
    pid: i32,
    /// Where the remote pointer is, on screen (events carry it; the real pointer doesn't move).
    pointer: CGPoint,
    /// The pointer's look as last read, its serial (a hash of the pixels), and when it was read.
    shape: Option<(CursorShape, u32)>,
    shape_at: Instant,
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
        let scale = display_scale(main.id, main.bounds().size.width);
        let filter = unsafe { SCContentFilter::initWithDesktopIndependentWindow(SCContentFilter::alloc(), &window) };
        let id = unsafe { window.windowID() };
        let t = title(&window);
        let what = format!(
            "window {id} ({}{})",
            app_name(&window),
            if t.is_empty() { String::new() } else { format!(": {t}") }
        );
        let pid = unsafe { window.owningApplication() }.map_or(0, |a| unsafe { a.processID() });
        Self::start(&filter, frame, scale, id, pid, what)
    }

    /// A whole display: `which` is its number in `rein-remote displays` order (1 is the main one) or
    /// its display id; none: the main display. Input goes where it lands, as on the machine itself.
    pub fn open_display(which: Option<&str>) -> Result<Self, String> {
        let c = content()?;
        let display = pick_display(&c, which)?;
        let none: Retained<NSArray<SCWindow>> = NSArray::new();
        let filter = unsafe { SCContentFilter::initWithDisplay_excludingWindows(SCContentFilter::alloc(), &display, &none) };
        let id = unsafe { display.displayID() };
        let frame = unsafe { display.frame() };
        let what = format!("display {id}");
        Self::start(&filter, frame, display_scale(id, frame.size.width), 0, 0, what)
    }

    /// One app, all its windows (on the display its first window is on), without the rest of the
    /// screen: `query` is part of its name or its bundle id. A press brings it to the front.
    pub fn open_app(query: &str) -> Result<Self, String> {
        let c = content()?;
        let q = query.to_lowercase();
        let app = unsafe { c.applications() }
            .iter()
            .find(|a| unsafe { a.applicationName() }.to_string().to_lowercase().contains(&q) || unsafe { a.bundleIdentifier() }.to_string().to_lowercase() == q)
            .ok_or_else(|| format!("no app matches \"{query}\""))?;
        let pid = unsafe { app.processID() };
        // The display its first window is on, else the main one.
        let at = unsafe { c.windows() }
            .iter()
            .find(|w| unsafe { w.isOnScreen() } && unsafe { w.owningApplication() }.is_some_and(|o| unsafe { o.processID() } == pid))
            .map(|w| unsafe { w.frame() }.origin);
        let display = unsafe { c.displays() }
            .iter()
            .find(|d| {
                let f = unsafe { d.frame() };
                at.is_some_and(|p| p.x >= f.origin.x && p.y >= f.origin.y && p.x < f.origin.x + f.size.width && p.y < f.origin.y + f.size.height)
            })
            .map_or_else(|| pick_display(&c, None), Ok)?;
        let apps = NSArray::from_retained_slice(std::slice::from_ref(&app));
        let none: Retained<NSArray<SCWindow>> = NSArray::new();
        let filter = unsafe { SCContentFilter::initWithDisplay_includingApplications_exceptingWindows(SCContentFilter::alloc(), &display, &apps, &none) };
        let frame = unsafe { display.frame() };
        let what = format!("app {} (pid {pid}) on display {}", unsafe { app.applicationName() }, unsafe {
            display.displayID()
        });
        Self::start(&filter, frame, display_scale(unsafe { display.displayID() }, frame.size.width), 0, pid, what)
    }

    /// Captures what `filter` shows, `frame` on screen (in points) at `scale` pixels a point.
    fn start(filter: &SCContentFilter, frame: objc2_core_foundation::CGRect, scale: f64, window_id: u32, pid: i32, what: String) -> Result<Self, String> {
        let (w, h) = (((frame.size.width * scale) as usize) & !1, ((frame.size.height * scale) as usize) & !1);
        let config = unsafe { SCStreamConfiguration::new() };
        unsafe {
            config.setWidth(w);
            config.setHeight(h);
            config.setPixelFormat(kCVPixelFormatType_32BGRA);
            // The pointer goes apart from the video; the client draws its own.
            config.setShowsCursor(false);
            // As often as the screen changes, up to 240 times a second (a high-refresh display).
            config.setMinimumFrameInterval(CMTime {
                value: 1,
                timescale: 240,
                flags: CMTimeFlags::Valid,
                epoch: 0,
            });
            config.setQueueDepth(3);
        }
        let latest = Arc::new(Mutex::new(Latest::default()));
        let output = Output::new(latest.clone());
        let stream = unsafe { SCStream::initWithFilter_configuration_delegate(SCStream::alloc(), filter, &config, None) };
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
            .map_err(|e| format!("couldn't start capturing the {what}: {e}"))?;
        let can_control = may_control();
        if !can_control {
            eprintln!("rein-remote: showing the {what}, but input won't reach it: allow Accessibility for the app that runs rein-remote (System Settings → Privacy & Security → Accessibility)");
        }
        eprintln!("rein-remote: {what}, {w}×{h}");
        Ok(Self {
            w,
            h,
            window_id,
            scale,
            origin: (frame.origin.x, frame.origin.y),
            origin_at: Instant::now(),
            latest,
            frame: vec![0; w * h * 4],
            stride: w * 4,
            _stream: stream,
            _output: output,
            filter: objc2::Message::retain(filter),
            buttons: [false; 3],
            pid,
            pointer: CGPoint::new(frame.origin.x, frame.origin.y),
            shape: None,
            shape_at: Instant::now() - Duration::from_secs(1),
        })
    }

    /// The sound that goes with this: what the app plays (a window or an app shown), or the whole
    /// machine's (a display).
    pub fn sound(&self) -> crate::audio::mac::Filter {
        crate::audio::mac::Filter::new(self.filter.clone(), self.pid != 0)
    }

    /// The window's place on screen now (it may have moved), at most twice a second.
    fn refresh_origin(&mut self) {
        // A display or an app's display doesn't move.
        if self.window_id == 0 || self.origin_at.elapsed() < Duration::from_millis(500) {
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
            match self.pid {
                // A display: wherever the keyboard focus is, as typing on the machine.
                0 => e.post(core_graphics::event::CGEventTapLocation::HID),
                pid => e.post_to_pid(pid),
            }
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
                    match pid {
                        0 => e.post(core_graphics::event::CGEventTapLocation::HID),
                        pid => e.post_to_pid(pid),
                    }
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

    fn cursor(&mut self) -> Option<Pointer> {
        // The look at most ten times a second (drawing it costs a little); where it is, every frame.
        if self.shape_at.elapsed() > Duration::from_millis(100) {
            self.shape_at = Instant::now();
            if let Some(s) = system_cursor(self.scale) {
                use std::hash::{Hash, Hasher};
                let mut h = std::collections::hash_map::DefaultHasher::new();
                (s.w, s.h, s.hx, s.hy, &s.rgba).hash(&mut h);
                self.shape = Some((s, h.finish() as u32));
            }
        }
        let (shape, serial) = self.shape.clone()?;
        let at = CGEvent::new(source()?).ok()?.location();
        self.refresh_origin();
        let (x, y) = ((at.x - self.origin.0) * self.scale, (at.y - self.origin.1) * self.scale);
        Some(Pointer {
            visible: x >= 0.0 && y >= 0.0 && (x as usize) < self.w && (y as usize) < self.h,
            x: x.max(0.0) as u16,
            y: y.max(0.0) as u16,
            serial,
            shape,
        })
    }

    fn surface(&mut self) -> Option<super::Surface> {
        let l = self.latest.lock().unwrap();
        let pb = l.image.as_ref()?;
        // Retained for the encoder; released when it drops the Surface.
        let ptr = objc2_core_foundation::CFRetained::as_ptr(&pb.0).as_ptr() as *mut std::ffi::c_void;
        extern "C" {
            fn CFRetain(p: *const std::ffi::c_void) -> *const std::ffi::c_void;
        }
        unsafe { CFRetain(ptr) };
        Some(super::Surface(ptr))
    }

    fn grab(&mut self) -> Result<Frame<'_>, String> {
        // The software encoder's way: the image read into memory.
        let image = self.latest.lock().unwrap().image.as_ref().map(|i| Sendable(i.0.clone()));
        if let Some(Sendable(pb)) = image {
            unsafe { CVPixelBufferLockBaseAddress(&pb, CVPixelBufferLockFlags::ReadOnly) };
            let base = CVPixelBufferGetBaseAddress(&pb) as *const u8;
            let (stride, rows) = (CVPixelBufferGetBytesPerRow(&pb), CVPixelBufferGetHeight(&pb));
            if !base.is_null() && rows >= self.h && stride >= self.w * 4 {
                self.frame.clear();
                self.frame.extend_from_slice(unsafe { std::slice::from_raw_parts(base, stride * self.h) });
                self.stride = stride;
            }
            unsafe { CVPixelBufferUnlockBaseAddress(&pb, CVPixelBufferLockFlags::ReadOnly) };
        }
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
                if *down && self.pid != 0 && !frontmost(self.pid) {
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
