//! The picture into the window's pixels: scaled to fit with its aspect kept (black bars around it),
//! bilinear, on a few cores; then the remote pointer on top. Pixels are 0RGB in a `u32`, which is
//! BGRA in memory on every platform this runs on, so a decoded picture reads as it is.

use crate::decode::Picture;

/// Where the picture goes in the window, in the window's pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rect {
    pub x: usize,
    pub y: usize,
    pub w: usize,
    pub h: usize,
}

impl Rect {
    /// The window position (`wx`,`wy`, its pixels) in the stream's pixels (`sw`×`sh`), when it's on
    /// the picture.
    pub fn to_stream(self, wx: f64, wy: f64, sw: usize, sh: usize) -> Option<(u16, u16)> {
        if self.w == 0 || self.h == 0 {
            return None;
        }
        let (fx, fy) = ((wx - self.x as f64) / self.w as f64, (wy - self.y as f64) / self.h as f64);
        if !(0.0..1.0).contains(&fx) || !(0.0..1.0).contains(&fy) {
            return None;
        }
        Some(((fx * sw as f64) as u16, (fy * sh as f64) as u16))
    }
}

/// The largest rectangle with the picture's aspect that fits `dw`×`dh`, centered.
pub fn fit(sw: usize, sh: usize, dw: usize, dh: usize) -> Rect {
    if sw == 0 || sh == 0 || dw == 0 || dh == 0 {
        return Rect::default();
    }
    // Whichever side is tighter sets the scale.
    let (w, h) = if dw * sh <= dh * sw {
        (dw, (dw * sh / sw).max(1))
    } else {
        ((dh * sw / sh).max(1), dh)
    };
    Rect {
        x: (dw - w) / 2,
        y: (dh - h) / 2,
        w,
        h,
    }
}

/// Draws `pic` into `dst` (`dw`×`dh`) at `r`, the rest black.
pub fn draw(pic: &Picture, dst: &mut [u32], dw: usize, dh: usize, r: Rect) {
    let src = &pic.px;
    let (sw, sh) = (pic.w, pic.h);
    if sw == 0 || sh == 0 || r.w == 0 || r.h == 0 || src.len() < sw * sh {
        dst.fill(0);
        return;
    }
    // Per destination column: the source column to its left and how far toward the next (of 256).
    let xs: Vec<(u32, u32)> = (0..r.w)
        .map(|x| {
            let f = (((x as f64 + 0.5) * sw as f64 / r.w as f64 - 0.5).max(0.0) * 256.0) as u32;
            ((f >> 8).min(sw as u32 - 1), f & 255)
        })
        .collect();
    let same = sw == r.w && sh == r.h;
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get()).clamp(1, 4);
    let band = dh.div_ceil(threads).max(1);
    std::thread::scope(|s| {
        for (i, rows) in dst[..dw * dh].chunks_mut(band * dw).enumerate() {
            let xs = &xs;
            s.spawn(move || {
                for (j, line) in rows.chunks_exact_mut(dw).enumerate() {
                    let y = i * band + j;
                    if y < r.y || y >= r.y + r.h {
                        line.fill(0);
                        continue;
                    }
                    line[..r.x].fill(0);
                    line[r.x + r.w..].fill(0);
                    let out = &mut line[r.x..r.x + r.w];
                    let dy = y - r.y;
                    if same {
                        for (o, p) in out.iter_mut().zip(&src[dy * sw..(dy + 1) * sw]) {
                            *o = p & 0x00ff_ffff;
                        }
                        continue;
                    }
                    let f = (((dy as f64 + 0.5) * sh as f64 / r.h as f64 - 0.5).max(0.0) * 256.0) as u32;
                    let (y0, wy) = (((f >> 8) as usize).min(sh - 1), f & 255);
                    let y1 = (y0 + 1).min(sh - 1);
                    let (a, b) = (&src[y0 * sw..(y0 + 1) * sw], &src[y1 * sw..(y1 + 1) * sw]);
                    for (o, &(x0, wx)) in out.iter_mut().zip(xs) {
                        let x0 = x0 as usize;
                        let x1 = (x0 + 1).min(sw - 1);
                        let top = lerp(a[x0], a[x1], wx);
                        let bottom = lerp(b[x0], b[x1], wx);
                        *o = lerp(top, bottom, wy);
                    }
                }
            });
        }
    });
}

/// Between two 0RGB pixels, `w` of 256 of the way: red and blue together, green apart (no lane
/// overflows: each holds at most 255 × 256).
#[inline]
fn lerp(p: u32, q: u32, w: u32) -> u32 {
    let iw = 256 - w;
    let rb = ((p & 0x00ff_00ff) * iw + (q & 0x00ff_00ff) * w) >> 8 & 0x00ff_00ff;
    let g = ((p & 0x0000_ff00) * iw + (q & 0x0000_ff00) * w) >> 8 & 0x0000_ff00;
    rb | g
}

/// The remote pointer's image: RGBA, its hotspot.
#[derive(Clone, Default)]
pub struct CursorImage {
    pub w: usize,
    pub h: usize,
    pub hx: usize,
    pub hy: usize,
    pub rgba: Vec<u8>,
}

/// Draws the pointer with its hotspot at `x`,`y` (window pixels), `k` window pixels to its pixels.
pub fn cursor(dst: &mut [u32], dw: usize, dh: usize, c: &CursorImage, x: f64, y: f64, k: f64) {
    if c.w == 0 || c.h == 0 || c.rgba.len() < c.w * c.h * 4 {
        return;
    }
    let (ow, oh) = ((c.w as f64 * k).round() as usize, (c.h as f64 * k).round() as usize);
    let (x0, y0) = (x - c.hx as f64 * k, y - c.hy as f64 * k);
    for oy in 0..oh {
        let py = y0 as i64 + oy as i64;
        if py < 0 || py >= dh as i64 {
            continue;
        }
        let sy = ((oy as f64 / k) as usize).min(c.h - 1);
        for ox in 0..ow {
            let px = x0 as i64 + ox as i64;
            if px < 0 || px >= dw as i64 {
                continue;
            }
            let sx = ((ox as f64 / k) as usize).min(c.w - 1);
            let s = &c.rgba[(sy * c.w + sx) * 4..(sy * c.w + sx) * 4 + 4];
            let a = s[3] as u32;
            if a == 0 {
                continue;
            }
            let d = &mut dst[py as usize * dw + px as usize];
            let blend = |src: u8, shift: u32| ((src as u32 * a + ((*d >> shift) & 255) * (255 - a)) / 255) << shift;
            *d = blend(s[0], 16) | blend(s[1], 8) | blend(s[2], 0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fits_with_the_aspect_kept_and_maps_back() {
        // 16:9 into a square: bars above and below.
        assert_eq!(fit(1280, 720, 1000, 1000), Rect { x: 0, y: 219, w: 1000, h: 562 });
        // Into a tall-enough wide window: bars at the sides.
        assert_eq!(fit(1280, 720, 2000, 720), Rect { x: 360, y: 0, w: 1280, h: 720 });
        let r = fit(1280, 720, 2560, 1440);
        assert_eq!(r.to_stream(1280.0, 720.0, 1280, 720), Some((640, 360)));
        assert_eq!(r.to_stream(-1.0, 5.0, 1280, 720), None);
        assert_eq!(Rect { x: 10, ..r }.to_stream(5.0, 5.0, 1280, 720), None);
    }

    #[test]
    fn draws_scaled_with_bars_and_a_pointer() {
        let mut pic = Picture::default();
        pic.reshape(4, 2);
        // Left half red, right half blue (BGRA).
        for (i, px) in pic.px.iter_mut().enumerate() {
            *px = if i % 4 < 2 { 0xffff_0000 } else { 0xff00_00ff };
        }
        let (dw, dh) = (8, 8);
        let mut dst = vec![0xdead_beef; dw * dh];
        let r = fit(4, 2, dw, dh);
        assert_eq!(r, Rect { x: 0, y: 2, w: 8, h: 4 });
        draw(&pic, &mut dst, dw, dh, r);
        assert_eq!(dst[0], 0, "a bar is black");
        assert_eq!(dst[3 * dw], 0x00ff_0000, "red at the left");
        assert_eq!(dst[3 * dw + 7], 0x0000_00ff, "blue at the right");
        // Same size: copied as is.
        let mut same = vec![0; 8];
        draw(&pic, &mut same, 4, 2, fit(4, 2, 4, 2));
        assert_eq!(same[0], 0x00ff_0000);
        let c = CursorImage {
            w: 1,
            h: 1,
            hx: 0,
            hy: 0,
            rgba: vec![0, 255, 0, 255],
        };
        cursor(&mut dst, dw, dh, &c, 1.0, 1.0, 2.0);
        assert_eq!((dst[dw + 1], dst[2 * dw + 2], dst[3 * dw + 1]), (0x0000_ff00, 0x0000_ff00, 0x00ff_0000));
    }
}
