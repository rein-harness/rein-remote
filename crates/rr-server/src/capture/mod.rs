//! Screen sources. Each gives frames of BGRA pixels and takes input events. Phase 1: an X11 display
//! (Linux: a real one, or a virtual display), and a test pattern (any platform: CI, and checking a
//! client without a display). macOS (ScreenCaptureKit) and Windows (Windows Graphics Capture) come
//! in phase 3 behind the same trait.

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
}

pub struct Pointer {
    pub x: u16,
    pub y: u16,
    pub serial: u32,
    pub shape: CursorShape,
}

/// A moving pattern with a frame counter: something to stream without a display.
pub struct TestPattern {
    w: usize,
    h: usize,
    n: u64,
    buf: Vec<u8>,
    /// Where the last pointer event landed (drawn as a cross), so input can be seen working.
    pointer: Option<(usize, usize)>,
}

impl TestPattern {
    pub fn new(w: usize, h: usize) -> Self {
        Self {
            w,
            h,
            n: 0,
            buf: vec![0; w * h * 4],
            pointer: None,
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
        for y in 0..h {
            for x in 0..w {
                let i = (y * w + x) * 4;
                self.buf[i] = (x * 255 / w.max(1)) as u8;
                self.buf[i + 1] = (y * 255 / h.max(1)) as u8;
                self.buf[i + 2] = 64;
                self.buf[i + 3] = 255;
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
        if let Some((px, py)) = self.pointer {
            for d in 0..20usize {
                for (x, y) in [(px + d, py), (px.saturating_sub(d), py), (px, py + d), (px, py.saturating_sub(d))] {
                    if x < w && y < h {
                        let i = (y * w + x) * 4;
                        self.buf[i..i + 4].copy_from_slice(&[0, 0, 255, 255]);
                    }
                }
            }
        }
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
