//! An X11 display (a desktop's, or a virtual one: Xvfb, Xvnc). Captured into shared memory (MIT-SHM:
//! the X server writes the screen straight into our memory, no copy over the socket), or with
//! GetImage where that isn't there (a display on another machine). XDamage says when the screen
//! changed, so a still screen costs nothing. Input goes in with XTest; the pointer comes out apart
//! with XFixes. One window can be streamed instead of the screen (XComposite: it's drawn off
//! screen, so a window behind others shows as it is); a press raises it first, like Remote Desktop.

use super::{keysym, Frame, Pointer, Source};
use rr_proto::{Button, CursorShape, Input};
use x11rb::connection::Connection;
use x11rb::protocol::composite::{ConnectionExt as _, Redirect};
use x11rb::protocol::damage::{self, ConnectionExt as _};
use x11rb::protocol::shm::ConnectionExt as _;
use x11rb::protocol::xfixes::ConnectionExt as _;
use x11rb::protocol::xproto::{
    AtomEnum, ClientMessageEvent, ConfigureWindowAux, ConnectionExt as _, EventMask, ImageFormat, InputFocus, MapState, StackMode, Window,
};
use x11rb::protocol::xtest::ConnectionExt as _;
use x11rb::protocol::Event;
use x11rb::rust_connection::RustConnection;
use x11rb::CURRENT_TIME;

pub struct X11 {
    conn: RustConnection,
    root: Window,
    w: u16,
    h: u16,
    frame: Vec<u8>,
    /// keysym → (keycode, needs Shift), from the display's keyboard mapping.
    keymap: std::collections::HashMap<u32, (u8, bool)>,
    /// The client is holding Shift (so a shifted character needs nothing more).
    shift_held: bool,
    /// The display has XFixes (the pointer's image and place, apart from the screen).
    xfixes: bool,
    shm: Option<Shm>,
    /// Damage on the root window, and a count of the changes it reported.
    damage: Option<damage::Damage>,
    generation: u64,
    /// What's captured: the root (the whole screen), or a window's off-screen pixmap.
    drawable: u32,
    /// The window streamed instead of the screen, and where it is on screen (input lands there).
    window: Option<Window>,
    offset: (i16, i16),
}

/// A top-level window: for `rein-remote windows` and picking one by name.
pub struct WindowInfo {
    pub id: u32,
    pub name: String,
    pub class: String,
    pub width: u16,
    pub height: u16,
}

fn atom(conn: &RustConnection, name: &str) -> Option<u32> {
    Some(conn.intern_atom(false, name.as_bytes()).ok()?.reply().ok()?.atom)
}

fn text_prop(conn: &RustConnection, w: Window, prop: u32, kind: u32) -> Option<String> {
    let r = conn.get_property(false, w, prop, kind, 0, 1024).ok()?.reply().ok()?;
    (!r.value.is_empty()).then(|| String::from_utf8_lossy(&r.value).trim_end_matches('\0').to_string())
}

/// The windows people would call windows: the window manager's list, or (no window manager, as in a
/// bare Xvfb) the root's mapped children that have a name.
fn top_windows(conn: &RustConnection, root: Window) -> Vec<WindowInfo> {
    let utf8 = atom(conn, "UTF8_STRING").unwrap_or(AtomEnum::STRING.into());
    let net_name = atom(conn, "_NET_WM_NAME");
    let listed: Vec<Window> = atom(conn, "_NET_CLIENT_LIST")
        .and_then(|a| conn.get_property(false, root, a, AtomEnum::WINDOW, 0, 4096).ok()?.reply().ok())
        .and_then(|r| r.value32().map(|v| v.collect()))
        .filter(|v: &Vec<Window>| !v.is_empty())
        .or_else(|| conn.query_tree(root).ok()?.reply().ok().map(|t| t.children))
        .unwrap_or_default();
    listed
        .into_iter()
        .filter_map(|w| {
            let attrs = conn.get_window_attributes(w).ok()?.reply().ok()?;
            if attrs.map_state != MapState::VIEWABLE || attrs.override_redirect {
                return None;
            }
            let g = conn.get_geometry(w).ok()?.reply().ok()?;
            let name = net_name
                .and_then(|a| text_prop(conn, w, a, utf8))
                .or_else(|| text_prop(conn, w, AtomEnum::WM_NAME.into(), AtomEnum::STRING.into()))
                .unwrap_or_default();
            let class = text_prop(conn, w, AtomEnum::WM_CLASS.into(), AtomEnum::STRING.into())
                .map(|c| c.split('\0').next_back().unwrap_or("").to_string())
                .unwrap_or_default();
            (g.width >= 32 && g.height >= 32 && !(name.is_empty() && class.is_empty())).then_some(WindowInfo {
                id: w,
                name,
                class,
                width: g.width,
                height: g.height,
            })
        })
        .collect()
}

/// The display's windows (`rein-remote windows`).
pub fn windows(display: Option<&str>) -> Result<Vec<WindowInfo>, String> {
    let (conn, screen) = connect(display)?;
    let root = conn.setup().roots[screen].root;
    Ok(top_windows(&conn, root))
}

fn connect(display: Option<&str>) -> Result<(RustConnection, usize), String> {
    x11rb::connect(display)
        .or_else(|e| abstract_socket(display).ok_or(e))
        .map_err(|e| format!("can't open the X display {}: {e}", display.unwrap_or("$DISPLAY")))
}

/// A shared memory segment the X server writes frames into.
struct Shm {
    seg: u32,
    ptr: *mut u8,
    len: usize,
}

// The mapping is only touched by whoever holds the source (it's behind a lock).
unsafe impl Send for Shm {}

impl Drop for Shm {
    fn drop(&mut self) {
        unsafe { libc::munmap(self.ptr.cast(), self.len) };
    }
}

/// A local display that only has its abstract socket (Linux: "@/tmp/.X11-unix/X20"; the socket file
/// can be missing, as when two X servers start at once). Xlib tries it; x11rb doesn't.
fn abstract_socket(display: Option<&str>) -> Option<(RustConnection, usize)> {
    use std::os::linux::net::SocketAddrExt;
    use std::os::unix::net::{SocketAddr, UnixStream};
    let name = display.map(str::to_string).or_else(|| std::env::var("DISPLAY").ok())?;
    let (num, screen) = name
        .strip_prefix(':')?
        .split_once('.')
        .map_or((name[1..].to_string(), 0), |(n, s)| (n.to_string(), s.parse().unwrap_or(0)));
    num.parse::<u32>().ok()?;
    let addr = SocketAddr::from_abstract_name(format!("/tmp/.X11-unix/X{num}")).ok()?;
    let stream = UnixStream::connect_addr(&addr).ok()?;
    let (stream, _) = x11rb::rust_connection::DefaultStream::from_unix_stream(stream).ok()?;
    Some((RustConnection::connect_to_stream(stream, screen).ok()?, screen))
}

fn shm(conn: &RustConnection, w: u16, h: u16) -> Option<Shm> {
    use std::os::fd::AsRawFd;
    conn.shm_query_version().ok()?.reply().ok()?;
    let len = w as usize * h as usize * 4;
    let seg = conn.generate_id().ok()?;
    // MIT-SHM 1.2: the server makes the segment and passes it as a file descriptor.
    let reply = conn.shm_create_segment(seg, len as u32, false).ok()?.reply().ok()?;
    let ptr = unsafe { libc::mmap(std::ptr::null_mut(), len, libc::PROT_READ, libc::MAP_SHARED, reply.shm_fd.as_raw_fd(), 0) };
    if ptr == libc::MAP_FAILED {
        let _ = conn.shm_detach(seg);
        return None;
    }
    Some(Shm { seg, ptr: ptr.cast(), len })
}

const KEY_PRESS: u8 = 2;
const KEY_RELEASE: u8 = 3;
const BUTTON_PRESS: u8 = 4;
const BUTTON_RELEASE: u8 = 5;
const MOTION: u8 = 6;

impl X11 {
    /// `display`: ":20", or None for $DISPLAY.
    pub fn open(display: Option<&str>) -> Result<Self, String> {
        Self::start(display, None)
    }

    /// One window: its id (from `rein-remote windows`, decimal or 0x hex), or part of its title or class.
    pub fn open_window(display: Option<&str>, query: &str) -> Result<Self, String> {
        Self::start(display, Some(query))
    }

    fn start(display: Option<&str>, query: Option<&str>) -> Result<Self, String> {
        let (conn, screen) = connect(display)?;
        let root_screen = &conn.setup().roots[screen];
        let root = root_screen.root;
        let (mut drawable, mut w, mut h, mut window, mut offset) = (root, root_screen.width_in_pixels, root_screen.height_in_pixels, None, (0, 0));
        if let Some(q) = query {
            let id = q.strip_prefix("0x").map_or_else(|| q.parse().ok(), |hex| u32::from_str_radix(hex, 16).ok());
            let ql = q.to_lowercase();
            let found = top_windows(&conn, root)
                .into_iter()
                .find(|x| Some(x.id) == id || x.name.to_lowercase().contains(&ql) || x.class.to_lowercase().contains(&ql))
                .ok_or_else(|| format!("no window matches \"{q}\" (rein-remote windows lists them)"))?;
            conn.composite_query_version(0, 4)
                .map_err(|e| e.to_string())?
                .reply()
                .map_err(|_| "the display has no XComposite, so one window can't be captured on its own".to_string())?;
            // Drawn off screen too, so it shows as it is when something covers it.
            conn.composite_redirect_window(found.id, Redirect::AUTOMATIC).map_err(|e| e.to_string())?;
            let pixmap = conn.generate_id().map_err(|e| e.to_string())?;
            conn.composite_name_window_pixmap(found.id, pixmap)
                .map_err(|e| e.to_string())?
                .check()
                .map_err(|e| format!("can't capture that window: {e}"))?;
            let at = conn
                .translate_coordinates(found.id, root, 0, 0)
                .map_err(|e| e.to_string())?
                .reply()
                .map_err(|e| e.to_string())?;
            (drawable, w, h, window, offset) = (pixmap, found.width, found.height, Some(found.id), (at.dst_x, at.dst_y));
            eprintln!(
                "rein-remote: window 0x{:x} ({}{}), {w}×{h}",
                found.id,
                found.class,
                if found.name.is_empty() { String::new() } else { format!(": {}", found.name) }
            );
        }
        let watch = window.unwrap_or(root);
        conn.xtest_get_version(2, 2)
            .map_err(|e| e.to_string())?
            .reply()
            .map_err(|_| "the display has no XTest (input won't work)".to_string())?;
        let keymap = keymap(&conn)?;
        let xfixes = conn.xfixes_query_version(4, 0).ok().and_then(|c| c.reply().ok()).is_some();
        let shm = shm(&conn, w, h);
        let damage = (|| {
            conn.damage_query_version(1, 1).ok()?.reply().ok()?;
            let id = conn.generate_id().ok()?;
            conn.damage_create(id, watch, damage::ReportLevel::NON_EMPTY).ok()?.check().ok()?;
            Some(id)
        })();
        eprintln!(
            "rein-remote: X display {}: capture {}, {}",
            display.unwrap_or("$DISPLAY"),
            if shm.is_some() { "in shared memory" } else { "with GetImage (no MIT-SHM)" },
            if damage.is_some() {
                "changes from XDamage"
            } else {
                "every frame (no XDamage)"
            }
        );
        Ok(Self {
            conn,
            root,
            w,
            h,
            frame: Vec::new(),
            keymap,
            shift_held: false,
            xfixes,
            shm,
            damage,
            generation: 0,
            drawable,
            window,
            offset,
        })
    }

    /// Typing into a window: it gets the keyboard if something else has it.
    fn focus(&mut self) {
        let Some(win) = self.window else { return };
        let focused = self.conn.get_input_focus().ok().and_then(|c| c.reply().ok()).map(|f| f.focus);
        if focused != Some(win) {
            let _ = self.conn.set_input_focus(InputFocus::PARENT, win, CURRENT_TIME);
            let _ = self.conn.flush();
        }
    }

    /// A press on a window: where it is now (it may have moved), then bring it to the front and give
    /// it the keyboard, through the window manager when there is one.
    fn raise(&mut self) {
        let Some(win) = self.window else { return };
        if let Ok(Ok(at)) = self.conn.translate_coordinates(win, self.root, 0, 0).map(|c| c.reply()) {
            self.offset = (at.dst_x, at.dst_y);
        }
        if let Some(active) = atom(&self.conn, "_NET_ACTIVE_WINDOW") {
            let ev = ClientMessageEvent::new(32, win, active, [2, CURRENT_TIME, 0, 0, 0]);
            let _ = self
                .conn
                .send_event(false, self.root, EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY, ev);
        }
        let _ = self.conn.configure_window(win, &ConfigureWindowAux::new().stack_mode(StackMode::ABOVE));
        let _ = self.conn.set_input_focus(InputFocus::PARENT, win, CURRENT_TIME);
        let _ = self.conn.flush();
    }

    fn fake(&self, kind: u8, detail: u8, x: i16, y: i16) -> Result<(), String> {
        self.conn
            .xtest_fake_input(kind, detail, CURRENT_TIME, self.root, x, y, 0)
            .map_err(|e| e.to_string())?;
        self.conn.flush().map_err(|e| e.to_string())
    }

    /// A key going down or up. A character on the shifted level ("A", "!") pressed without Shift
    /// held (a phone keyboard, a client that sends the character only) gets Shift around it, so it
    /// types what the user meant.
    fn key(&mut self, sym: u32, down: bool) -> Result<(), String> {
        if sym == 0xffe1 || sym == 0xffe2 {
            self.shift_held = down;
        }
        let Some(&(code, shifted)) = self.keymap.get(&sym) else { return Ok(()) }; // not on this layout
        let shift = self.keymap.get(&0xffe1).map(|k| k.0).filter(|_| down && shifted && !self.shift_held);
        if let Some(s) = shift {
            self.fake(KEY_PRESS, s, 0, 0)?;
        }
        self.fake(if down { KEY_PRESS } else { KEY_RELEASE }, code, 0, 0)?;
        if let Some(s) = shift {
            self.fake(KEY_RELEASE, s, 0, 0)?;
        }
        Ok(())
    }

    /// Type one character: its key, with Shift held when the layout puts it on the shifted level.
    fn type_char(&self, sym: u32) -> Result<(), String> {
        let Some(&(code, shift)) = self.keymap.get(&sym) else { return Ok(()) };
        let shift_code = self.keymap.get(&0xffe1).map(|k| k.0);
        if let (true, Some(s)) = (shift, shift_code) {
            self.fake(KEY_PRESS, s, 0, 0)?;
        }
        self.fake(KEY_PRESS, code, 0, 0)?;
        self.fake(KEY_RELEASE, code, 0, 0)?;
        if let (true, Some(s)) = (shift, shift_code) {
            self.fake(KEY_RELEASE, s, 0, 0)?;
        }
        Ok(())
    }
}

fn keymap(conn: &RustConnection) -> Result<std::collections::HashMap<u32, (u8, bool)>, String> {
    let setup = conn.setup();
    let (min, max) = (setup.min_keycode, setup.max_keycode);
    let reply = conn
        .get_keyboard_mapping(min, max - min + 1)
        .map_err(|e| e.to_string())?
        .reply()
        .map_err(|e| e.to_string())?;
    let per = reply.keysyms_per_keycode as usize;
    let mut map = std::collections::HashMap::new();
    for (i, syms) in reply.keysyms.chunks(per.max(1)).enumerate() {
        for (level, &sym) in syms.iter().take(2).enumerate() {
            if sym != 0 {
                map.entry(sym).or_insert((min + i as u8, level == 1));
            }
        }
    }
    Ok(map)
}

impl Source for X11 {
    fn cursor(&mut self) -> Option<Pointer> {
        if !self.xfixes {
            return None;
        }
        let c = self.conn.xfixes_get_cursor_image().ok()?.reply().ok()?;
        // Premultiplied ARGB → straight RGBA.
        let mut rgba = Vec::with_capacity(c.cursor_image.len() * 4);
        for p in &c.cursor_image {
            let a = (p >> 24) & 0xff;
            let un = |v: u32| (v * 255 + a / 2).checked_div(a).map_or(0, |v| v.min(255) as u8);
            rgba.extend_from_slice(&[un((p >> 16) & 0xff), un((p >> 8) & 0xff), un(p & 0xff), a as u8]);
        }
        let (x, y) = (c.x - self.offset.0, c.y - self.offset.1);
        Some(Pointer {
            visible: x >= 0 && y >= 0 && (x as u16) < self.w && (y as u16) < self.h,
            x: x.max(0) as u16,
            y: y.max(0) as u16,
            serial: c.cursor_serial,
            shape: CursorShape {
                w: c.width,
                h: c.height,
                hx: c.xhot,
                hy: c.yhot,
                rgba,
            },
        })
    }

    fn size(&self) -> (usize, usize) {
        (self.w as usize, self.h as usize)
    }

    fn generation(&mut self) -> Option<u64> {
        let damage = self.damage?;
        let mut changed = false;
        while let Ok(Some(ev)) = self.conn.poll_for_event() {
            if let Event::DamageNotify(_) = ev {
                changed = true;
            }
        }
        if changed {
            // Clear what was reported so the next change reports again (NonEmpty level).
            let _ = self.conn.damage_subtract(damage, x11rb::NONE, x11rb::NONE);
            let _ = self.conn.flush();
            self.generation += 1;
        }
        Some(self.generation)
    }

    fn grab(&mut self) -> Result<Frame<'_>, String> {
        if let Some(shm) = &self.shm {
            let ok = self
                .conn
                .shm_get_image(self.drawable, 0, 0, self.w, self.h, !0, ImageFormat::Z_PIXMAP.into(), shm.seg, 0)
                .ok()
                .and_then(|c| c.reply().ok())
                .is_some();
            if ok {
                // B, G, R, X: the encoder ignores the X byte, so the frame is used as the server wrote it.
                let bgra = unsafe { std::slice::from_raw_parts(shm.ptr, shm.len) };
                return Ok(Frame {
                    bgra,
                    stride: self.w as usize * 4,
                });
            }
            eprintln!("rein-remote: shared-memory capture failed; using GetImage");
            self.shm = None;
        }
        let reply = self
            .conn
            .get_image(ImageFormat::Z_PIXMAP, self.drawable, 0, 0, self.w, self.h, !0)
            .map_err(|e| e.to_string())?
            .reply()
            .map_err(|e| e.to_string())?;
        // 24-bit depth in 32-bit pixels, little-endian: B, G, R, X (the encoder ignores X).
        self.frame = reply.data;
        Ok(Frame {
            bgra: &self.frame,
            stride: self.w as usize * 4,
        })
    }

    fn input(&mut self, ev: &Input) -> Result<(), String> {
        match ev {
            Input::PointerMove { x, y } => self.fake(MOTION, 0, *x as i16 + self.offset.0, *y as i16 + self.offset.1),
            Input::Button { button, down } => {
                if *down && self.window.is_some() {
                    self.raise();
                }
                let b = match button {
                    Button::Left => 1,
                    Button::Middle => 2,
                    Button::Right => 3,
                    Button::Back => 8,
                    Button::Forward => 9,
                };
                self.fake(if *down { BUTTON_PRESS } else { BUTTON_RELEASE }, b, 0, 0)
            }
            Input::Wheel { dx, dy } => {
                // X wheels are buttons: 4 up, 5 down, 6 left, 7 right; one click per notch.
                let mut clicks = vec![];
                if *dy != 0 {
                    clicks.push((if *dy < 0 { 4 } else { 5 }, (dy.unsigned_abs() / 40).max(1)));
                }
                if *dx != 0 {
                    clicks.push((if *dx < 0 { 6 } else { 7 }, (dx.unsigned_abs() / 40).max(1)));
                }
                for (b, n) in clicks {
                    for _ in 0..n.min(10) {
                        self.fake(BUTTON_PRESS, b, 0, 0)?;
                        self.fake(BUTTON_RELEASE, b, 0, 0)?;
                    }
                }
                Ok(())
            }
            Input::Key { key, down, .. } => {
                self.focus();
                keysym(key).map_or(Ok(()), |s| self.key(s, *down))
            }
            Input::Text { text } => {
                self.focus();
                for c in text.chars() {
                    if let Some(s) = keysym(&c.to_string()) {
                        self.type_char(s)?;
                    }
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }
}
