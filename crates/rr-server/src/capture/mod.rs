//! Screen sources. Each gives frames of BGRA pixels and takes input events. Phase 1: an X11 display
//! (Linux: a real one, or a virtual display), and a test pattern (any platform: CI, and checking a
//! client without a display). macOS (ScreenCaptureKit) and Windows (Windows Graphics Capture) come
//! in phase 3 behind the same trait.

#[cfg(target_os = "macos")]
pub mod mac;
pub mod pipe;
pub mod vnc;
#[cfg(windows)]
pub mod win;
#[cfg(target_os = "linux")]
pub mod x11;

use rr_proto::{CursorShape, Input};

pub struct Frame<'a> {
    pub bgra: &'a [u8],
    pub stride: usize,
}

pub trait Source: Send {
    fn size(&self) -> (usize, usize);
    /// The screen now.
    fn grab(&mut self) -> Result<Frame<'_>, String>;
    /// An input event from the client, in the source's pixels.
    fn input(&mut self, _ev: &Input) -> Result<(), String> {
        Ok(())
    }
    /// The pointer, when the source can see it apart from the screen (so a moving pointer costs
    /// no video): where it is, and its image with a serial that changes when the image does.
    fn cursor(&mut self) -> Option<Pointer> {
        None
    }
    /// A count that goes up when the screen changes, when the source can tell (so a still screen is
    /// neither read nor encoded). None: it can't, read every frame.
    fn generation(&mut self) -> Option<u64> {
        None
    }
    /// The latest frame as the system's own image (macOS: a CVPixelBuffer in GPU memory), for an
    /// encoder that takes it as it is: no copy at all. None: use `grab`.
    fn surface(&mut self) -> Option<Surface> {
        None
    }
}

/// A frame the hardware encoder reads in place: a retained CVPixelBuffer (macOS).
#[cfg_attr(not(target_os = "macos"), allow(dead_code))] // only macOS sources make one so far
pub struct Surface(pub *mut std::ffi::c_void);

// Retained for as long as it's held; released when dropped.
unsafe impl Send for Surface {}

#[cfg(target_os = "macos")]
impl Drop for Surface {
    fn drop(&mut self) {
        extern "C" {
            fn CFRelease(p: *const std::ffi::c_void);
        }
        unsafe { CFRelease(self.0) }
    }
}

pub struct Pointer {
    pub x: u16,
    pub y: u16,
    pub serial: u32,
    pub shape: CursorShape,
    /// Over what's streamed (a window's stream hides it when it's elsewhere on the screen).
    pub visible: bool,
}

/// A moving pattern with a frame counter: something to stream without a display. Only what moves
/// is drawn each frame (the block, the barcode, the pointer's cross), so it costs next to nothing
/// even at 240 frames a second.
pub struct TestPattern {
    w: usize,
    h: usize,
    n: u64,
    buf: Vec<u8>,
    /// Where the last pointer event landed (drawn as a cross), so input can be seen working.
    pointer: Option<(usize, usize)>,
    /// What was drawn over the background last frame, to put back: the block's left edge, the cross.
    drawn: Option<(usize, Option<(usize, usize)>)>,
    /// Full motion (`--motion full`): a detailed scene scrolling under it all, every pixel changing
    /// every frame, as a game's would; the texture it scrolls (twice the width, so rows copy whole).
    scene: Option<Vec<u8>>,
}

impl TestPattern {
    pub fn new(w: usize, h: usize) -> Self {
        Self {
            w,
            h,
            n: 0,
            buf: vec![0; w * h * 4],
            pointer: None,
            drawn: None,
            scene: None,
        }
    }

    /// Every pixel moving every frame: a busy textured scene scrolling diagonally (hard to encode, as
    /// a game is), with the block, the barcode and the pointer's cross on top.
    pub fn full_motion(w: usize, h: usize) -> Self {
        let mut t = Self::new(w, h);
        let tw = w * 2;
        let mut scene = vec![0u8; tw * h * 4];
        let mut x: u32 = 0x9e37_79b9;
        for (i, px) in scene.chunks_exact_mut(4).enumerate() {
            let (cx, cy) = ((i % tw) as u32, (i / tw) as u32);
            // Smooth bands plus fine noise: gradients and detail, like a rendered scene.
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            let n = (x & 63) as u8;
            px.copy_from_slice(&[
                (((cx * 3 + cy) % 256) as u8).wrapping_add(n),
                (((cx + cy * 2) / 3 % 256) as u8).wrapping_add(n / 2),
                ((((cx ^ cy) >> 2) % 256) as u8).wrapping_add(n / 3),
                255,
            ]);
        }
        t.scene = Some(scene);
        t
    }

    fn background(&mut self, x0: usize, x1: usize, y0: usize, y1: usize) {
        let (w, h) = (self.w, self.h);
        for y in y0..y1.min(h) {
            for x in x0..x1.min(w) {
                let i = (y * w + x) * 4;
                self.buf[i..i + 4].copy_from_slice(&[(x * 255 / w.max(1)) as u8, (y * 255 / h.max(1)) as u8, 64, 255]);
            }
        }
    }

    fn cross(&mut self, (px, py): (usize, usize), color: Option<[u8; 4]>) {
        let (w, h) = (self.w, self.h);
        for d in 0..20usize {
            for (x, y) in [(px + d, py), (px.saturating_sub(d), py), (px, py + d), (px, py.saturating_sub(d))] {
                if x < w && y < h {
                    match color {
                        Some(c) => {
                            let i = (y * w + x) * 4;
                            self.buf[i..i + 4].copy_from_slice(&c);
                        }
                        None => self.background(x, x + 1, y, y + 1),
                    }
                }
            }
        }
    }
}

impl Source for TestPattern {
    fn size(&self) -> (usize, usize) {
        (self.w, self.h)
    }
    fn grab(&mut self) -> Result<Frame<'_>, String> {
        self.n += 1;
        let (w, h, n) = (self.w, self.h, self.n as usize);
        // Full motion: the scene, scrolled on, under everything (all of it changes).
        if let Some(scene) = &self.scene {
            let (tw, dx, dy) = (w * 2, (n * 6) % w.max(1), (n * 2) % h.max(1));
            for y in 0..h {
                let sy = (y + dy) % h;
                self.buf[y * w * 4..(y + 1) * w * 4].copy_from_slice(&scene[(sy * tw + dx) * 4..(sy * tw + dx + w) * 4]);
            }
            self.drawn = Some((0, None));
        }
        // The background once; after that, only what was drawn over it goes back.
        match self.drawn.take().filter(|_| self.scene.is_none()) {
            None if self.scene.is_some() => {}
            None => self.background(0, w, 0, h),
            Some((bx, cross)) => {
                self.background(bx, bx + w / 12, h / 3, h / 3 + h / 6);
                if let Some(c) = cross {
                    self.cross(c, None);
                }
            }
        }
        // A white block moving left to right, one step a frame.
        let bx = (n * 8) % w.max(1);
        for y in h / 3..(h / 3 + h / 6).min(h) {
            for x in bx..(bx + w / 12).min(w) {
                let i = (y * w + x) * 4;
                self.buf[i..i + 4].copy_from_slice(&[255, 255, 255, 255]);
            }
        }
        // The capture time as a barcode along the bottom, for measuring glass to glass: a client
        // that shares this clock reads it from the decoded frame (rein-remote.js, `measure`).
        // Blocks of 24×24: white, black (sync), then the time in milliseconds, 32 bits, high first.
        let ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() as u64) as u32;
        const B: usize = 24;
        if w >= 34 * B && h >= B {
            for k in 0..34 {
                let on = match k {
                    0 => true,
                    1 => false,
                    _ => ms >> (33 - k) & 1 == 1,
                };
                let v = if on { 255 } else { 0 };
                for y in h - B..h {
                    let i = (y * w + k * B) * 4;
                    for px in self.buf[i..i + B * 4].chunks_exact_mut(4) {
                        px.copy_from_slice(&[v, v, v, 255]);
                    }
                }
            }
        }
        if let Some(p) = self.pointer {
            self.cross(p, Some([0, 0, 255, 255]));
        }
        self.drawn = Some((bx, self.pointer));
        Ok(Frame {
            bgra: &self.buf,
            stride: w * 4,
        })
    }
    fn input(&mut self, ev: &Input) -> Result<(), String> {
        if let Input::PointerMove { x, y } = ev {
            self.pointer = Some((*x as usize, *y as usize));
        }
        Ok(())
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_test_pattern_drawn_in_parts_is_the_same_as_drawn_whole() {
        let (w, h) = (320, 240);
        let mut p = TestPattern::new(w, h);
        for n in 0..50 {
            if n == 10 {
                p.input(&Input::PointerMove { x: 100, y: 50 }).unwrap();
            }
            if n == 30 {
                p.input(&Input::PointerMove { x: 5, y: 200 }).unwrap();
            }
            let got = p.grab().unwrap().bgra.to_vec();
            // The same frame drawn from scratch.
            let mut fresh = TestPattern::new(w, h);
            fresh.n = p.n - 1;
            fresh.pointer = p.pointer;
            assert!(fresh.grab().unwrap().bgra == got.as_slice(), "frame {n}");
        }
    }
}
