//! The window: the picker of machines on this network (when started without one), then the stream,
//! scaled to fit with its aspect kept, the remote pointer drawn here (at once where the local mouse
//! is, else where the server says), and the mouse, keyboard and gamepads sent back. The title says
//! where it's connected, over what, decoded with what, at how many frames a second and how late.
//!
//! Ctrl+Alt+Enter toggles full screen (the only keys kept here; everything else goes to the server).

use crate::decode::Picture;
use crate::present::{self, CursorImage, Rect};
use crate::stream::{self, Shared, Update};
use crate::{keys, measure, pad, text, Args, Report};
use rr_client::discover::Found;
use rr_client::Control;
use rr_proto::{Button, Input, INPUT_GAMEPAD};
use std::collections::HashMap;
use std::num::NonZeroU32;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalPosition};
use winit::event::{ElementState, Ime, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::keyboard::{Key, ModifiersState, NamedKey, PhysicalKey};
use winit::window::{Fullscreen, Window, WindowId};

/// What the other threads tell the window.
pub enum UserEvent {
    Stream(Update),
    /// Machines found on the network (the picker).
    Found(Vec<Found>),
    /// Connected: the session's handle, and where its stream's thread puts what it decodes.
    Connected(Control, Arc<Shared>),
    /// Couldn't connect, and why.
    Failed(String),
}

enum Mode {
    Picker,
    Connecting(String),
    Streaming,
}

/// Overlay text: a line's height, in font pixels.
const LINE_H: usize = 14;

/// Mouse wheel lines (a notch) as pixels, as browsers send them.
const LINE_PX: f64 = 50.0;

pub fn run(args: Args) -> Result<(), String> {
    let event_loop = EventLoop::<UserEvent>::with_user_event().build().map_err(|e| e.to_string())?;
    event_loop.set_control_flow(ControlFlow::Wait);
    let proxy = event_loop.create_proxy();
    // Gamepads to whichever session is open, when its server takes them.
    let pads: Arc<Mutex<Option<Control>>> = Default::default();
    let pads_on = Arc::new(AtomicBool::new(false));
    {
        let (pads, on) = (pads.clone(), pads_on.clone());
        pad::watch(move |i| {
            if on.load(Ordering::Relaxed) {
                if let Some(c) = pads.lock().unwrap().as_ref() {
                    c.input(i);
                }
            }
        });
    }
    let mut app = App {
        proxy,
        window: None,
        surface: None,
        mode: Mode::Picker,
        found: Vec::new(),
        picker_note: None,
        pairing: None,
        searching: Arc::new(AtomicBool::new(false)),
        target: None,
        control: None,
        shared: None,
        stop: Arc::new(AtomicBool::new(false)),
        shown: None,
        measured: None,
        gpu_scale: false,
        rect: Rect::default(),
        cursor: None,
        remote_at: (0, 0),
        remote_visible: true,
        local_at: None,
        last_move: Instant::now(),
        wheel: (0.0, 0.0),
        modifiers: ModifiersState::empty(),
        keys_down: HashMap::new(),
        buttons_down: Vec::new(),
        rate: measure::Rate::default(),
        glass_recent: Vec::new(),
        report: None,
        first_frame: None,
        title_at: Instant::now() - Duration::from_secs(1),
        sized: false,
        pads,
        pads_on,
        args,
        exit_error: None,
    };
    if let Some(secs) = app.args.seconds {
        // A measured run ends, frames or not: its frames' time, and a while to connect.
        let proxy = app.proxy.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_secs(secs + 30));
            let _ = proxy.send_event(UserEvent::Failed(format!("no picture for {secs} s within {} s", secs + 30)));
        });
    }
    if let Some(t) = app.args.target.clone() {
        app.connect(t);
    } else {
        app.search();
    }
    event_loop.run_app(&mut app).map_err(|e| e.to_string())?;
    app.finish();
    match app.exit_error.take() {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

struct App {
    args: Args,
    proxy: EventLoopProxy<UserEvent>,
    window: Option<Rc<Window>>,
    surface: Option<softbuffer::Surface<Rc<Window>, Rc<Window>>>,
    mode: Mode,
    // The picker: what's found, a line to say (an error), and the pairing code being typed for one.
    found: Vec<Found>,
    picker_note: Option<String>,
    pairing: Option<(String, String)>,
    searching: Arc<AtomicBool>,
    /// What this session connected to (its link), for pairing and reconnecting.
    target: Option<String>,
    control: Option<Control>,
    shared: Option<Arc<Shared>>,
    stop: Arc<AtomicBool>,
    /// The picture on screen.
    shown: Option<Picture>,
    /// The last picture read for the barcode (a redraw for the pointer isn't a new frame).
    measured: Option<u32>,
    /// The compositor scales the picture (macOS), so the buffer is the picture's size.
    gpu_scale: bool,
    rect: Rect,
    cursor: Option<CursorImage>,
    remote_at: (u16, u16),
    remote_visible: bool,
    /// The local mouse over the window (its pixels) and when it last moved.
    local_at: Option<(PhysicalPosition<f64>, Instant)>,
    last_move: Instant,
    /// Wheel movement not yet sent (less than a pixel).
    wheel: (f64, f64),
    modifiers: ModifiersState,
    /// Keys held, by position, with the name they were pressed as (released by the same name, even
    /// if Shift was let go first), and buttons held: all let go when the window loses focus.
    keys_down: HashMap<PhysicalKey, (u16, String)>,
    buttons_down: Vec<Button>,
    rate: measure::Rate,
    glass_recent: Vec<f64>,
    report: Option<Report>,
    first_frame: Option<Instant>,
    title_at: Instant,
    /// The window was sized to the stream once.
    sized: bool,
    pads: Arc<Mutex<Option<Control>>>,
    pads_on: Arc<AtomicBool>,
    exit_error: Option<String>,
}

impl App {
    /// Connects on a thread of its own (it can take seconds: pairing, signing in, QUIC's handshake),
    /// then runs the stream there.
    fn connect(&mut self, target: String) {
        self.searching.store(false, Ordering::Relaxed);
        self.mode = Mode::Connecting(target.clone());
        self.target = Some(target.clone());
        self.stop = Arc::new(AtomicBool::new(false));
        let mut opts = self.client_options();
        if let Some((_, code)) = self.pairing.take() {
            opts.pair = Some(code);
        }
        let (proxy, stop) = (self.proxy.clone(), self.stop.clone());
        let sopts = stream::Options {
            prefer: self.args.prefer,
            measure: self.args.measure,
        };
        std::thread::spawn(move || match rr_client::connect(&target, opts) {
            Ok(session) => {
                let shared = Arc::new(Shared::default());
                let _ = proxy.send_event(UserEvent::Connected(session.control(), shared.clone()));
                let p = proxy.clone();
                stream::run(
                    session,
                    shared,
                    sopts,
                    || stop.load(Ordering::Relaxed),
                    move |u| {
                        let _ = p.send_event(UserEvent::Stream(u));
                    },
                );
            }
            Err(e) => {
                let _ = proxy.send_event(UserEvent::Failed(e));
            }
        });
        self.redraw();
    }

    fn client_options(&self) -> rr_client::Options {
        let o = &self.args.client;
        rr_client::Options {
            name: o.name.clone(),
            pair: o.pair.clone(),
            webtransport: o.webtransport,
            datagrams: o.datagrams,
            direct: o.direct,
            codecs: o.codecs.clone(),
            refresh_mhz: o.refresh_mhz,
            auto_feedback: o.auto_feedback,
            keyframe_on_loss: o.keyframe_on_loss,
            input: o.input,
        }
    }

    /// Looks for machines on the network every few seconds while the picker shows.
    fn search(&mut self) {
        self.mode = Mode::Picker;
        if self.searching.swap(true, Ordering::Relaxed) {
            return;
        }
        let (proxy, on) = (self.proxy.clone(), self.searching.clone());
        std::thread::spawn(move || {
            while on.load(Ordering::Relaxed) {
                match rr_client::discover::discover(Duration::from_secs(2)) {
                    Ok(f) => {
                        let _ = proxy.send_event(UserEvent::Found(f));
                    }
                    Err(e) => {
                        let _ = proxy.send_event(UserEvent::Failed(e));
                        return;
                    }
                }
                std::thread::sleep(Duration::from_secs(2));
            }
        });
    }

    fn close_session(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(c) = self.control.take() {
            c.close();
        }
        *self.pads.lock().unwrap() = None;
        self.pads_on.store(false, Ordering::Relaxed);
        self.shared = None;
        self.shown = None;
        self.cursor = None;
    }

    /// The report, when measuring (printed once, at the end).
    fn finish(&mut self) {
        let status = self.status();
        self.close_session();
        if let Some(r) = self.report.take() {
            r.print(&status, self.args.json);
            if r.frames == 0 && self.exit_error.is_none() {
                self.exit_error = Some("no frames were shown".into());
            }
        }
    }

    fn status(&self) -> stream::Status {
        self.shared.as_ref().map(|s| s.status.lock().unwrap().clone()).unwrap_or_default()
    }

    fn send(&self, i: Input) {
        if let Some(c) = &self.control {
            c.input(i);
        }
    }

    fn redraw(&self) {
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }

    /// The newest decoded picture, if there's one not yet shown.
    fn take_frame(&mut self) -> bool {
        let Some(shared) = &self.shared else { return false };
        let Some(pic) = shared.latest.lock().unwrap().take() else { return false };
        if let Some(old) = self.shown.replace(pic) {
            shared.spare.lock().unwrap().push(old);
        }
        true
    }

    fn draw(&mut self) {
        let lines = self.overlay();
        let (Some(window), Some(surface)) = (&self.window, &mut self.surface) else {
            return;
        };
        let size = window.inner_size();
        let (ww, wh) = (size.width as usize, size.height as usize);
        // The buffer: the picture's size when the compositor scales it (macOS), else the window's.
        let (dw, dh) = match &self.shown {
            Some(pic) if self.gpu_scale => (pic.w, pic.h),
            _ => (ww, wh),
        };
        let (Some(w), Some(h)) = (NonZeroU32::new(dw as u32), NonZeroU32::new(dh as u32)) else {
            return;
        };
        if surface.resize(w, h).is_err() {
            return;
        }
        let Ok(mut buf) = surface.buffer_mut() else { return };
        let mut barcode = None;
        if let Some(pic) = &self.shown {
            // Where the picture is in the window (for the mouse), and where it's drawn in the buffer.
            self.rect = present::fit(pic.w, pic.h, ww, wh);
            let at = if self.gpu_scale { present::fit(pic.w, pic.h, dw, dh) } else { self.rect };
            present::draw(pic, &mut buf, dw, dh, at);
            if self.measured != Some(pic.frame) {
                self.measured = Some(pic.frame);
                barcode = pic.barcode;
            }
            if let (Some(c), true, false) = (&self.cursor, self.remote_visible, self.rect.w == 0) {
                // At once where the local mouse is (no round trip), else where the server says.
                let (x, y) = match self.local_at {
                    Some((p, t)) if t.elapsed() < Duration::from_millis(250) => (
                        at.x as f64 + (p.x - self.rect.x as f64) * at.w as f64 / self.rect.w as f64,
                        at.y as f64 + (p.y - self.rect.y as f64) * at.h as f64 / self.rect.h as f64,
                    ),
                    _ => (
                        at.x as f64 + self.remote_at.0 as f64 * at.w as f64 / pic.w.max(1) as f64,
                        at.y as f64 + self.remote_at.1 as f64 * at.h as f64 / pic.h.max(1) as f64,
                    ),
                };
                present::cursor(&mut buf, dw, dh, c, x, y, at.w as f64 / pic.w.max(1) as f64);
            }
        } else {
            buf.fill(0x0010_1418);
        }
        // Text a size that reads at any buffer size: 8 pixels a line of 360.
        let ts = (dh / 360).max(1);
        let mut y = 12 * ts;
        for (line, color) in &lines {
            text::draw(&mut buf, (dw, dh), (12 * ts, y), ts, *color, line);
            y += LINE_H * ts;
        }
        let _ = buf.present();
        // Glass to glass: the barcode's capture time to now, the picture just handed to the screen.
        if let Some(g) = barcode.and_then(measure::glass_ms) {
            self.glass_recent.push(g as f64);
            if self.glass_recent.len() > 120 {
                self.glass_recent.drain(..60);
            }
            if let Some(r) = &mut self.report {
                r.glass.push(g as f64);
            }
        }
    }

    /// The lines over the picture: the picker, or the connection's state when there's no picture.
    fn overlay(&self) -> Vec<(String, u32)> {
        const WHITE: u32 = 0x00ff_ffff;
        const DIM: u32 = 0x00a0_a8b0;
        const RED: u32 = 0x00ff_8080;
        let mut out = Vec::new();
        match &self.mode {
            Mode::Picker => {
                out.push(("Rein Remote".to_string(), WHITE));
                if let Some((name, code)) = &self.pairing {
                    out.push((format!("Pair with {name}: type the code it shows, then Enter"), WHITE));
                    out.push((format!("  {code}_"), WHITE));
                    out.push(("Esc: back".into(), DIM));
                } else if self.found.is_empty() {
                    out.push(("Looking for machines on this network...".into(), DIM));
                } else {
                    out.push(("Machines on this network:".into(), DIM));
                    for (i, f) in self.found.iter().take(9).enumerate() {
                        let at = f.addrs.first().map_or(String::new(), |a| format!("{a}:{}", f.port));
                        out.push((format!("{}  {}  {at}", i + 1, f.name), WHITE));
                    }
                    out.push(("Press its number (or click it) to connect. Esc: quit".into(), DIM));
                }
                if let Some(n) = &self.picker_note {
                    out.push((n.clone(), RED));
                }
            }
            Mode::Connecting(t) => out.push((format!("Connecting to {}...", rr_client::redact(t)), WHITE)),
            Mode::Streaming => {
                let s = self.status();
                if s.closed {
                    out.push((s.state.clone(), RED));
                    out.push((if self.args.target.is_some() { "Esc: quit" } else { "Esc: back to the list" }.into(), DIM));
                } else if self.shown.is_none() {
                    out.push((format!("{}...", s.state), WHITE));
                }
            }
        }
        out
    }

    fn update_title(&mut self, force: bool) {
        if !force && self.title_at.elapsed() < Duration::from_millis(250) {
            return;
        }
        self.title_at = Instant::now();
        let Some(w) = &self.window else { return };
        let title = match &self.mode {
            Mode::Picker => "Rein Remote".to_string(),
            Mode::Connecting(t) => format!("Rein Remote: connecting to {}", rr_client::redact(t)),
            Mode::Streaming => {
                let s = self.status();
                let late = if !self.glass_recent.is_empty() {
                    format!("{:.0} ms glass to glass", measure::percentile(&self.glass_recent, 0.5))
                } else {
                    s.latency_ms.map_or("…".into(), |l| format!("{l:.0} ms"))
                };
                if s.closed {
                    format!("Rein Remote: {} ({})", s.server, s.state)
                } else {
                    format!("{} · {} · {} · {} fps · {late}", s.server, s.transport, s.decoder, self.rate.fps())
                }
            }
        };
        w.set_title(&title);
    }

    fn on_stream(&mut self, el: &ActiveEventLoop, u: Update) {
        match u {
            Update::Frame => {
                if self.take_frame() {
                    self.rate.tick();
                    let now = Instant::now();
                    self.first_frame.get_or_insert(now);
                    if let Some(pic) = &self.shown {
                        if !self.sized {
                            self.sized = true;
                            self.size_to(pic.w, pic.h);
                        }
                    }
                    let latency = self.status().latency_ms;
                    if let Some(r) = &mut self.report {
                        r.frame(None, latency);
                    }
                    self.draw();
                    self.update_title(false);
                    if let (Some(secs), Some(t)) = (self.args.seconds, self.first_frame) {
                        if t.elapsed() >= Duration::from_secs(secs) {
                            el.exit();
                        }
                    }
                }
            }
            Update::Cursor(c) => {
                if let Some(s) = c.shape {
                    let has = s.w > 0 && s.h > 0;
                    self.cursor = has.then_some(CursorImage {
                        w: s.w as usize,
                        h: s.h as usize,
                        hx: s.hx as usize,
                        hy: s.hy as usize,
                        rgba: s.rgba,
                    });
                    // The remote pointer is drawn: this one's hidden over the window.
                    if let Some(w) = &self.window {
                        w.set_cursor_visible(!has);
                    }
                }
                self.remote_at = (c.x, c.y);
                self.remote_visible = c.visible;
                self.redraw();
            }
            Update::Status => {
                let s = self.status();
                self.pads_on.store(s.server_input & INPUT_GAMEPAD != 0, Ordering::Relaxed);
                if s.closed {
                    if let Some(w) = &self.window {
                        w.set_cursor_visible(true);
                    }
                    if self.args.seconds.is_some() || self.args.measure {
                        self.exit_error = Some(s.state.clone());
                        el.exit();
                    }
                }
                self.update_title(true);
                self.redraw();
            }
        }
    }

    /// The window to the stream's size (in points: as large as the picture on a normal screen), within
    /// the screen it's on.
    fn size_to(&self, w: usize, h: usize) {
        let Some(win) = &self.window else { return };
        if win.fullscreen().is_some() {
            return;
        }
        let (mut lw, mut lh) = (w as f64, h as f64);
        if let Some(m) = win.current_monitor() {
            let ms = m.size().to_logical::<f64>(m.scale_factor());
            let k = (ms.width * 0.9 / lw).min(ms.height * 0.85 / lh).min(1.0);
            lw *= k;
            lh *= k;
        }
        let _ = win.request_inner_size(LogicalSize::new(lw, lh));
    }

    fn picker_key(&mut self, el: &ActiveEventLoop, key: &Key) {
        if let Some((name, code)) = &mut self.pairing {
            let mut go = None;
            match key {
                Key::Named(NamedKey::Escape) => self.pairing = None,
                Key::Named(NamedKey::Backspace) => {
                    code.pop();
                }
                Key::Named(NamedKey::Enter) => go = Some(name.clone()),
                Key::Character(c) if c.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') => code.push_str(c),
                _ => {}
            }
            // Connect with the code (`connect` takes it from `pairing`).
            if let Some(link) = go.and_then(|n| self.found.iter().find(|f| f.name == n)).and_then(|f| f.link().ok()) {
                self.connect(link);
            }
            self.redraw();
            return;
        }
        match key {
            Key::Named(NamedKey::Escape) => el.exit(),
            Key::Character(c) => {
                if let Some(n) = c.parse::<usize>().ok().filter(|n| (1..=self.found.len().min(9)).contains(n)) {
                    self.pick(n - 1);
                }
            }
            _ => {}
        }
    }

    fn pick(&mut self, i: usize) {
        let Some(f) = self.found.get(i).cloned() else { return };
        match f.link() {
            Ok(link) => {
                self.picker_note = None;
                // Paired before: straight in. Else the code first.
                if rr_client::pairing::paired(&f.id).is_some() || self.args.client.pair.is_some() {
                    self.connect(link);
                } else {
                    self.pairing = Some((f.name.clone(), String::new()));
                    self.redraw();
                }
            }
            Err(e) => {
                self.picker_note = Some(format!("{}: {e}", f.name));
                self.redraw();
            }
        }
    }

    fn release_all(&mut self) {
        for (_, (hid, key)) in std::mem::take(&mut self.keys_down) {
            self.send(Input::Key { hid, down: false, key });
        }
        for button in std::mem::take(&mut self.buttons_down) {
            self.send(Input::Button { button, down: false });
        }
    }

    /// The pointer's place in the stream's pixels, when it's over the picture.
    fn stream_at(&self, p: PhysicalPosition<f64>) -> Option<(u16, u16)> {
        let pic = self.shown.as_ref()?;
        self.rect.to_stream(p.x, p.y, pic.w, pic.h)
    }
}

impl ApplicationHandler<UserEvent> for App {
    fn resumed(&mut self, el: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let attrs = Window::default_attributes()
            .with_title("Rein Remote")
            .with_inner_size(LogicalSize::new(1280.0, 720.0))
            .with_fullscreen(self.args.fullscreen.then_some(Fullscreen::Borderless(None)));
        let window = match el.create_window(attrs) {
            Ok(w) => Rc::new(w),
            Err(e) => {
                self.exit_error = Some(format!("can't open a window: {e}"));
                el.exit();
                return;
            }
        };
        // Text from an input method (Chinese, Japanese…) comes as text, not keys.
        window.set_ime_allowed(true);
        let surface = softbuffer::Context::new(window.clone()).and_then(|c| softbuffer::Surface::new(&c, window.clone()));
        match surface {
            Ok(s) => self.surface = Some(s),
            Err(e) => {
                self.exit_error = Some(format!("can't draw in the window: {e}"));
                el.exit();
                return;
            }
        }
        #[cfg(target_os = "macos")]
        {
            self.gpu_scale = crate::mac_layer::scale_on_gpu(&window);
        }
        self.window = Some(window);
        self.update_title(true);
        self.redraw();
    }

    fn user_event(&mut self, el: &ActiveEventLoop, ev: UserEvent) {
        match ev {
            UserEvent::Stream(u) => self.on_stream(el, u),
            UserEvent::Found(f) => {
                if matches!(self.mode, Mode::Picker) {
                    self.found = f;
                    self.redraw();
                }
            }
            UserEvent::Connected(control, shared) => {
                *self.pads.lock().unwrap() = Some(control.clone());
                self.control = Some(control);
                self.shared = Some(shared);
                self.mode = Mode::Streaming;
                if self.args.measure || self.args.seconds.is_some() {
                    self.report = Some(Report::new());
                }
                self.update_title(true);
                self.redraw();
            }
            UserEvent::Failed(e) => {
                eprintln!("rein-remote-viewer: {e}");
                if self.args.target.is_some() {
                    self.exit_error = Some(e);
                    el.exit();
                    return;
                }
                // Back to the list, saying why; a machine not paired with asks for its code.
                let name = self
                    .target
                    .as_ref()
                    .and_then(|t| self.found.iter().find(|f| f.link().ok().as_ref() == Some(t)))
                    .map(|f| f.name.clone());
                if let (true, Some(n)) = (e.contains("not paired"), name) {
                    self.pairing = Some((n, String::new()));
                    self.picker_note = None;
                } else {
                    self.picker_note = Some(e);
                }
                self.search();
                self.update_title(true);
                self.redraw();
            }
        }
    }

    fn window_event(&mut self, el: &ActiveEventLoop, _id: WindowId, ev: WindowEvent) {
        match ev {
            WindowEvent::CloseRequested => el.exit(),
            WindowEvent::RedrawRequested => self.draw(),
            WindowEvent::Resized(_) | WindowEvent::ScaleFactorChanged { .. } => self.redraw(),
            WindowEvent::Focused(false) => self.release_all(),
            WindowEvent::ModifiersChanged(m) => self.modifiers = m.state(),
            WindowEvent::CursorMoved { position, .. } => {
                self.local_at = Some((position, Instant::now()));
                if !matches!(self.mode, Mode::Streaming) {
                    return;
                }
                if let Some((x, y)) = self.stream_at(position) {
                    // One a few milliseconds at most (a fast mouse reports at 1000 Hz).
                    if self.last_move.elapsed() >= Duration::from_millis(4) {
                        self.last_move = Instant::now();
                        self.send(Input::PointerMove { x, y });
                    }
                }
                if self.cursor.is_some() {
                    self.redraw();
                }
            }
            WindowEvent::CursorLeft { .. } => {
                self.local_at = None;
                self.redraw();
            }
            WindowEvent::MouseInput { state, button, .. } => {
                if matches!(self.mode, Mode::Picker) {
                    // A click on a machine's line picks it.
                    if state == ElementState::Pressed && button == MouseButton::Left && self.pairing.is_none() {
                        if let (Some((p, _)), Some(w)) = (self.local_at, &self.window) {
                            // As `draw` lays the lines out (the picker has no picture: the buffer is the window's).
                            let ts = (w.inner_size().height as usize / 360).max(1);
                            let line = ((p.y as usize).saturating_sub(12 * ts - 2 * ts)) / (LINE_H * ts);
                            if line >= 2 {
                                self.pick(line - 2);
                            }
                        }
                    }
                    return;
                }
                let button = match button {
                    MouseButton::Left => Button::Left,
                    MouseButton::Right => Button::Right,
                    MouseButton::Middle => Button::Middle,
                    MouseButton::Back => Button::Back,
                    MouseButton::Forward => Button::Forward,
                    MouseButton::Other(_) => return,
                };
                let down = state == ElementState::Pressed;
                if let Some((x, y)) = self.local_at.and_then(|(p, _)| self.stream_at(p)) {
                    self.send(Input::PointerMove { x, y });
                } else if down {
                    // A press outside the picture (on a bar) isn't the remote's.
                    return;
                }
                if down {
                    self.buttons_down.push(button);
                } else if let Some(i) = self.buttons_down.iter().position(|b| *b == button) {
                    self.buttons_down.remove(i);
                } else {
                    return;
                }
                self.send(Input::Button { button, down });
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let sf = self.window.as_ref().map_or(1.0, |w| w.scale_factor());
                // Positive: down / right, as the web's deltaY; winit's is the other way.
                let (dx, dy) = match delta {
                    MouseScrollDelta::LineDelta(x, y) => (-x as f64 * LINE_PX, -y as f64 * LINE_PX),
                    MouseScrollDelta::PixelDelta(p) => (-p.x / sf, -p.y / sf),
                };
                self.wheel.0 += dx;
                self.wheel.1 += dy;
                let (sx, sy) = (self.wheel.0.trunc(), self.wheel.1.trunc());
                if sx != 0.0 || sy != 0.0 {
                    self.wheel.0 -= sx;
                    self.wheel.1 -= sy;
                    let clamp = |v: f64| v.clamp(i16::MIN as f64, i16::MAX as f64) as i16;
                    self.send(Input::Wheel { dx: clamp(sx), dy: clamp(sy) });
                }
            }
            WindowEvent::KeyboardInput { event, is_synthetic, .. } => {
                if is_synthetic {
                    return;
                }
                let down = event.state == ElementState::Pressed;
                if matches!(self.mode, Mode::Picker) {
                    if down {
                        self.picker_key(el, &event.logical_key);
                    }
                    return;
                }
                let status = self.status();
                if status.closed {
                    if down && event.logical_key == Key::Named(NamedKey::Escape) {
                        if self.args.target.is_some() {
                            el.exit();
                        } else {
                            self.close_session();
                            self.search();
                            self.update_title(true);
                            self.redraw();
                        }
                    }
                    return;
                }
                // Ctrl+Alt+Enter: full screen, here.
                if event.logical_key == Key::Named(NamedKey::Enter) && self.modifiers.control_key() && self.modifiers.alt_key() {
                    if down {
                        if let Some(w) = &self.window {
                            w.set_fullscreen(if w.fullscreen().is_some() { None } else { Some(Fullscreen::Borderless(None)) });
                        }
                    }
                    return;
                }
                let pk = event.physical_key;
                if down {
                    let Some(name) = keys::name(&event.logical_key) else { return };
                    let hid = keys::hid(pk);
                    self.keys_down.insert(pk, (hid, name.clone()));
                    self.send(Input::Key { hid, down: true, key: name });
                } else if let Some((hid, key)) = self.keys_down.remove(&pk) {
                    self.send(Input::Key { hid, down: false, key });
                }
            }
            WindowEvent::Ime(Ime::Commit(text)) if matches!(self.mode, Mode::Streaming) && !text.is_empty() => self.send(Input::Text { text }),
            _ => {}
        }
    }
}
