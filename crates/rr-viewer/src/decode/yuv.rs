//! YUV 4:2:0 → BGRA, for decoders that give planes (openh264's I420; NV12 from Media Foundation and
//! VA-API when they're read back): integer BT.601, limited range, the inverse of what the server's
//! encoders are given (rr-server's yuv.rs). Rows are split across a few cores: a 1080p frame is
//! 2 million pixels.

/// One plane: its bytes and how many a row.
#[derive(Clone, Copy)]
pub struct Plane<'a> {
    pub data: &'a [u8],
    pub stride: usize,
}

/// The chroma of 4:2:0: two planes (I420), or one with U and V interleaved (NV12).
#[derive(Clone, Copy)]
#[cfg_attr(not(windows), allow(dead_code))] // NV12 is Media Foundation's and VA-API's
pub enum Chroma<'a> {
    I420(Plane<'a>, Plane<'a>),
    Nv12(Plane<'a>),
}

/// Converts `w`×`h` of `y` and `chroma` into `out` (BGRA, `w * 4` a row).
pub fn to_bgra(y: Plane, chroma: Chroma, w: usize, h: usize, out: &mut [u8]) {
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get()).clamp(1, 4);
    // Bands of an even number of rows (a chroma row covers two), one per thread.
    let band = (h.div_ceil(2).div_ceil(threads) * 2).max(2);
    std::thread::scope(|s| {
        for (i, rows) in out[..w * h * 4].chunks_mut(band * w * 4).enumerate() {
            s.spawn(move || convert_rows(y, chroma, w, i * band, rows));
        }
    });
}

fn convert_rows(y: Plane, chroma: Chroma, w: usize, row0: usize, out: &mut [u8]) {
    for (r, line) in out.chunks_exact_mut(w * 4).enumerate() {
        let row = row0 + r;
        let yl = &y.data[row * y.stride..];
        let cr = row / 2;
        for x in 0..w {
            let (u, v) = match chroma {
                Chroma::I420(u, v) => (u.data[cr * u.stride + x / 2], v.data[cr * v.stride + x / 2]),
                Chroma::Nv12(uv) => {
                    let i = cr * uv.stride + (x & !1);
                    (uv.data[i], uv.data[i + 1])
                }
            };
            let px = pixel(yl[x], u, v);
            line[x * 4..x * 4 + 4].copy_from_slice(&px);
        }
    }
}

/// One pixel, BT.601 limited range: 16..235 luma, 16..240 chroma (scaled by 256).
#[inline]
fn pixel(y: u8, u: u8, v: u8) -> [u8; 4] {
    let c = (y as i32 - 16) * 298;
    let d = u as i32 - 128;
    let e = v as i32 - 128;
    let clamp = |x: i32| ((x + 128) >> 8).clamp(0, 255) as u8;
    [clamp(c + 516 * d), clamp(c - 100 * d - 208 * e), clamp(c + 409 * e), 255]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn white_black_and_red_come_back() {
        // As rr-server's yuv.rs makes them: white (235, 128, 128), black (16, 128, 128), red
        // (81, 90, 240).
        assert_eq!(pixel(235, 128, 128), [255, 255, 255, 255]);
        assert_eq!(pixel(16, 128, 128), [0, 0, 0, 255]);
        let [b, g, r, _] = pixel(81, 90, 240);
        assert!(r > 250 && g < 5 && b < 5, "{r} {g} {b}");
    }

    #[test]
    fn i420_and_nv12_give_the_same_picture() {
        let (w, h) = (6, 4);
        let y: Vec<u8> = (0..w * h).map(|i| (16 + i * 9) as u8).collect();
        let u = [100u8, 120, 140, 160, 180, 200];
        let v = [60u8, 80, 90, 110, 130, 150];
        let uv: Vec<u8> = u.iter().zip(&v).flat_map(|(&a, &b)| [a, b]).collect();
        let (mut a, mut b) = (vec![0; w * h * 4], vec![0; w * h * 4]);
        let yp = Plane { data: &y, stride: w };
        to_bgra(yp, Chroma::I420(Plane { data: &u, stride: 3 }, Plane { data: &v, stride: 3 }), w, h, &mut a);
        to_bgra(yp, Chroma::Nv12(Plane { data: &uv, stride: 6 }), w, h, &mut b);
        assert_eq!(a, b);
        assert_eq!(&a[..4], &pixel(16, 100, 60));
        assert_eq!(&a[(w + 3) * 4..(w + 4) * 4], &pixel(y[w + 3], 120, 80));
    }
}
