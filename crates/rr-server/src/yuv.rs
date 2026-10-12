//! BGRA → YUV 4:2:0, the encoders' input, fast: integer BT.601 (limited range), 2×2 blocks for the
//! chroma, rows split across the CPU's cores. The generic per-pixel conversion took most of a
//! frame's time; this is several times faster. I420 (three planes) for openh264, NV12 (Y, then U and
//! V interleaved) for Media Foundation's encoders.

pub struct I420 {
    pub w: usize,
    pub h: usize,
    pub y: Vec<u8>,
    pub u: Vec<u8>,
    pub v: Vec<u8>,
}

impl I420 {
    pub fn new(w: usize, h: usize) -> Self {
        let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
        Self {
            w,
            h,
            y: vec![0; w * h],
            u: vec![0; cw * ch],
            v: vec![0; cw * ch],
        }
    }

    /// Convert `bgra` (`stride` bytes a row) into this frame. `w` and `h` are even.
    pub fn convert(&mut self, bgra: &[u8], stride: usize) {
        let (w, h) = (self.w, self.h);
        let cw = w / 2;
        let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1).clamp(1, 8);
        // Bands of an even number of rows, one per thread.
        let band = ((h / 2).div_ceil(threads)) * 2;
        std::thread::scope(|s| {
            let mut y_rest = self.y.as_mut_slice();
            let mut u_rest = self.u.as_mut_slice();
            let mut v_rest = self.v.as_mut_slice();
            let mut row0 = 0;
            while row0 < h {
                let rows = band.min(h - row0);
                let (yb, yr) = y_rest.split_at_mut(rows * w);
                let (ub, ur) = u_rest.split_at_mut(rows / 2 * cw);
                let (vb, vr) = v_rest.split_at_mut(rows / 2 * cw);
                (y_rest, u_rest, v_rest) = (yr, ur, vr);
                let src = &bgra[row0 * stride..];
                s.spawn(move || band_convert::<false>(src, stride, w, rows, yb, ub, vb));
                row0 += rows;
            }
        });
    }
}

/// Convert `bgra` (`stride` bytes a row, `w`×`h`, both even) into NV12 in `out` (`w * h * 3 / 2`
/// bytes: the Y plane, then the U and V samples interleaved), written straight into the encoder's
/// own buffer.
#[cfg_attr(not(any(windows, target_os = "linux")), allow(dead_code))] // Media Foundation's input, and VA-API's when the GPU can't convert
pub fn nv12(bgra: &[u8], stride: usize, w: usize, h: usize, out: &mut [u8]) {
    let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1).clamp(1, 8);
    let band = ((h / 2).div_ceil(threads)) * 2;
    let (y_all, uv_all) = out[..w * h * 3 / 2].split_at_mut(w * h);
    std::thread::scope(|s| {
        let (mut y_rest, mut uv_rest) = (y_all, uv_all);
        let mut row0 = 0;
        while row0 < h {
            let rows = band.min(h - row0);
            let (yb, yr) = y_rest.split_at_mut(rows * w);
            let (uvb, uvr) = uv_rest.split_at_mut(rows / 2 * w);
            (y_rest, uv_rest) = (yr, uvr);
            let src = &bgra[row0 * stride..];
            s.spawn(move || band_convert::<true>(src, stride, w, rows, yb, uvb, &mut []));
            row0 += rows;
        }
    });
}

#[inline(always)]
fn luma(b: i32, g: i32, r: i32) -> u8 {
    (((66 * r + 129 * g + 25 * b + 128) >> 8) + 16) as u8
}

/// A band of rows. `NV12`: `u` holds the U and V samples interleaved (and `v` is unused); else
/// they're two planes.
fn band_convert<const NV12: bool>(src: &[u8], stride: usize, w: usize, rows: usize, y: &mut [u8], u: &mut [u8], v: &mut [u8]) {
    let cw = w / 2;
    for pair in 0..rows / 2 {
        let r0 = &src[(pair * 2) * stride..(pair * 2) * stride + w * 4];
        let r1 = &src[(pair * 2 + 1) * stride..(pair * 2 + 1) * stride + w * 4];
        let (y0, y1) = y[pair * 2 * w..(pair * 2 + 2) * w].split_at_mut(w);
        let (uo, vo) = if NV12 {
            (&mut u[pair * w..(pair + 1) * w], &mut [][..])
        } else {
            (&mut u[pair * cw..(pair + 1) * cw], &mut v[pair * cw..(pair + 1) * cw])
        };
        for x in 0..cw {
            let i = x * 8;
            let (b00, g00, r00) = (r0[i] as i32, r0[i + 1] as i32, r0[i + 2] as i32);
            let (b01, g01, r01) = (r0[i + 4] as i32, r0[i + 5] as i32, r0[i + 6] as i32);
            let (b10, g10, r10) = (r1[i] as i32, r1[i + 1] as i32, r1[i + 2] as i32);
            let (b11, g11, r11) = (r1[i + 4] as i32, r1[i + 5] as i32, r1[i + 6] as i32);
            y0[x * 2] = luma(b00, g00, r00);
            y0[x * 2 + 1] = luma(b01, g01, r01);
            y1[x * 2] = luma(b10, g10, r10);
            y1[x * 2 + 1] = luma(b11, g11, r11);
            let (b, g, r) = (
                (b00 + b01 + b10 + b11 + 2) >> 2,
                (g00 + g01 + g10 + g11 + 2) >> 2,
                (r00 + r01 + r10 + r11 + 2) >> 2,
            );
            let cb = (((-38 * r - 74 * g + 112 * b + 128) >> 8) + 128) as u8;
            let cr = (((112 * r - 94 * g - 18 * b + 128) >> 8) + 128) as u8;
            if NV12 {
                uo[x * 2] = cb;
                uo[x * 2 + 1] = cr;
            } else {
                uo[x] = cb;
                vo[x] = cr;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_known_colors() {
        // 4×2: white, black, red, blue (each a 2×2 block when doubled vertically).
        let px: [[u8; 4]; 4] = [[255, 255, 255, 255], [255, 255, 255, 255], [0, 0, 255, 255], [0, 0, 255, 255]];
        let row: Vec<u8> = px.iter().flatten().copied().collect();
        let bgra = [row.clone(), row].concat();
        let mut f = I420::new(4, 2);
        f.convert(&bgra, 16);
        // White: Y 235, U/V 128. Red: Y 82, U 90, V 240 (BT.601 limited range).
        assert_eq!(&f.y[..2], &[235, 235]);
        assert_eq!((f.u[0], f.v[0]), (128, 128));
        assert_eq!(&f.y[2..4], &[82, 82]);
        assert!((f.u[1] as i32 - 90).abs() <= 1 && (f.v[1] as i32 - 240).abs() <= 1);
    }

    #[test]
    fn nv12_is_i420_with_the_chroma_interleaved() {
        let (w, h) = (10, 6);
        let bgra: Vec<u8> = (0..w * h * 4).map(|i| (i * 37 % 251) as u8).collect();
        let mut i420 = I420::new(w, h);
        i420.convert(&bgra, w * 4);
        let mut nv12 = vec![0u8; w * h * 3 / 2];
        super::nv12(&bgra, w * 4, w, h, &mut nv12);
        assert_eq!(&nv12[..w * h], &i420.y[..]);
        let uv: Vec<u8> = i420.u.iter().zip(&i420.v).flat_map(|(u, v)| [*u, *v]).collect();
        assert_eq!(&nv12[w * h..], &uv[..]);
    }

    #[test]
    fn handles_bands_that_dont_divide_evenly() {
        let (w, h) = (6, 10);
        let bgra = vec![128u8; w * h * 4];
        let mut f = I420::new(w, h);
        f.convert(&bgra, w * 4);
        assert!(f.y.iter().all(|&y| y == f.y[0]) && f.u.iter().all(|&u| u == 128));
    }
}
