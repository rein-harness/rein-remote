//! An X11 display (a desktop's, or a virtual one: Xvfb, Xvnc): captured with GetImage, input sent
//! with XTest. Phase 1; MIT-SHM capture and XDamage (only what changed) come next for speed.

use super::{Frame, Pointer, Source};
use rr_proto::{Button, CursorShape, Input};
use x11rb::connection::Connection;
use x11rb::protocol::xfixes::ConnectionExt as _;
use x11rb::protocol::xproto::{ConnectionExt as _, ImageFormat, Window};
use x11rb::protocol::xtest::ConnectionExt as _;
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
}

const KEY_PRESS: u8 = 2;
const KEY_RELEASE: u8 = 3;
const BUTTON_PRESS: u8 = 4;
const BUTTON_RELEASE: u8 = 5;
const MOTION: u8 = 6;

impl X11 {
    /// `display`: ":20", or None for $DISPLAY.
    pub fn open(display: Option<&str>) -> Result<Self, String> {
        let (conn, screen) = x11rb::connect(display).map_err(|e| format!("can't open the X display {}: {e}", display.unwrap_or("$DISPLAY")))?;
        let root_screen = &conn.setup().roots[screen];
        let (root, w, h) = (root_screen.root, root_screen.width_in_pixels, root_screen.height_in_pixels);
        conn.xtest_get_version(2, 2)
            .map_err(|e| e.to_string())?
            .reply()
            .map_err(|_| "the display has no XTest (input won't work)".to_string())?;
        let keymap = keymap(&conn)?;
        let xfixes = conn.xfixes_query_version(4, 0).ok().and_then(|c| c.reply().ok()).is_some();
        Ok(Self {
            conn,
            root,
            w,
            h,
            frame: Vec::new(),
            keymap,
            shift_held: false,
            xfixes,
        })
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

/// The X keysym for a key name ("a", "Enter", "ArrowLeft"…), as the client sends them.
pub fn keysym(key: &str) -> Option<u32> {
    let named = match key {
        "Backspace" => 0xff08,
        "Tab" => 0xff09,
        "Enter" => 0xff0d,
        "Escape" => 0xff1b,
        "Delete" => 0xffff,
        "Home" => 0xff50,
        "ArrowLeft" => 0xff51,
        "ArrowUp" => 0xff52,
        "ArrowRight" => 0xff53,
        "ArrowDown" => 0xff54,
        "PageUp" => 0xff55,
        "PageDown" => 0xff56,
        "End" => 0xff57,
        "Insert" => 0xff63,
        "Shift" => 0xffe1,
        "Control" => 0xffe3,
        "Alt" => 0xffe9,
        "Meta" => 0xffeb,
        "CapsLock" => 0xffe5,
        " " => 0x20,
        _ => 0,
    };
    if named != 0 {
        return Some(named);
    }
    if let Some(f) = key.strip_prefix('F').and_then(|n| n.parse::<u32>().ok()).filter(|n| (1..=24).contains(n)) {
        return Some(0xffbd + f);
    }
    let mut chars = key.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) => Some(if (c as u32) < 0x100 { c as u32 } else { 0x0100_0000 + c as u32 }),
        _ => None,
    }
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
        Some(Pointer {
            x: c.x.max(0) as u16,
            y: c.y.max(0) as u16,
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

    fn grab(&mut self) -> Result<Frame<'_>, String> {
        let reply = self
            .conn
            .get_image(ImageFormat::Z_PIXMAP, self.root, 0, 0, self.w, self.h, !0)
            .map_err(|e| e.to_string())?
            .reply()
            .map_err(|e| e.to_string())?;
        // 24-bit depth in 32-bit pixels, little-endian: B, G, R, X. Make X opaque.
        self.frame = reply.data;
        for px in self.frame.chunks_exact_mut(4) {
            px[3] = 255;
        }
        Ok(Frame {
            bgra: &self.frame,
            stride: self.w as usize * 4,
        })
    }

    fn input(&mut self, ev: &Input) -> Result<(), String> {
        match ev {
            Input::PointerMove { x, y } => self.fake(MOTION, 0, *x as i16, *y as i16),
            Input::Button { button, down } => {
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
            Input::Key { key, down, .. } => keysym(key).map_or(Ok(()), |s| self.key(s, *down)),
            Input::Text { text } => {
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
