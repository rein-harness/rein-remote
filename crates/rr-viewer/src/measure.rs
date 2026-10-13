//! Measuring what the viewer shows: glass to glass from the test pattern's barcode (as the web
//! client's `measure` does), frames a second, and this process's CPU.
//!
//! `rein-remote serve --test-pattern` draws its capture time along the bottom of each frame: blocks of
//! 24×24, white, black (to tell it's there), then the time in milliseconds (32 bits, high first). A
//! viewer on the same clock (the same machine) reads it back from the decoded picture and subtracts
//! it from the time the picture went to the screen.

use crate::decode::Picture;
use std::collections::VecDeque;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// The capture time the test pattern drew into `pic` (ms, mod 2^32), when it has one. The blocks are
/// 24 px, less on a picture under 816 wide or one the server scaled down: their size is taken from
/// the white sync block, as the web client does.
pub fn barcode(pic: &Picture) -> Option<u32> {
    if pic.h < 8 || pic.w < 34 * 4 {
        return None;
    }
    // Two rows from the bottom (inside a block of any size); green, as the web client reads.
    let row = pic.row(pic.h - 2);
    let green = |x: usize| (row[x] >> 8 & 255) > 128;
    let bs = (0..30.min(row.len())).take_while(|&x| green(x)).count();
    if !(4..30).contains(&bs) || row.len() < 34 * bs {
        return None;
    }
    let bit = |k: usize| green(k * bs + bs / 2);
    if !bit(0) || bit(1) {
        return None;
    }
    Some((2..34).fold(0u32, |ms, k| ms << 1 | bit(k) as u32))
}

/// Now on the wall clock, ms mod 2^32 (as the barcode is written).
pub fn now_ms() -> u32 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64) as u32
}

/// From the barcode to now, ms: None when it's not plausible (no pattern, another clock).
pub fn glass_ms(barcode: u32) -> Option<u32> {
    let d = now_ms().wrapping_sub(barcode);
    (d < 10_000).then_some(d)
}

/// The share `p` of the way through `v`, sorted (0.5: the median).
pub fn percentile(v: &[f64], p: f64) -> f64 {
    if v.is_empty() {
        return 0.0;
    }
    let mut v = v.to_vec();
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[((v.len() - 1) as f64 * p).round() as usize]
}

/// Frames shown in the last second.
#[derive(Default)]
pub struct Rate(VecDeque<Instant>);

impl Rate {
    pub fn tick(&mut self) {
        let now = Instant::now();
        self.0.push_back(now);
        while self.0.front().is_some_and(|t| now - *t > Duration::from_secs(1)) {
            self.0.pop_front();
        }
    }

    pub fn fps(&self) -> usize {
        let now = Instant::now();
        self.0.iter().filter(|t| now - **t <= Duration::from_secs(1)).count()
    }
}

/// This process's CPU time so far (user and system), to report its use as a share of one core.
pub fn cpu_time() -> Duration {
    #[cfg(unix)]
    unsafe {
        let mut u: libc::rusage = std::mem::zeroed();
        libc::getrusage(libc::RUSAGE_SELF, &mut u);
        let tv = |t: libc::timeval| Duration::new(t.tv_sec as u64, t.tv_usec as u32 * 1000);
        tv(u.ru_utime) + tv(u.ru_stime)
    }
    #[cfg(windows)]
    {
        crate::platform::cpu_time()
    }
    #[cfg(not(any(unix, windows)))]
    Duration::ZERO
}

#[cfg(test)]
mod tests {
    use super::*;

    const BLOCK: usize = 24;

    #[test]
    fn reads_the_barcode_the_test_pattern_draws() {
        // Drawn as rr-server's TestPattern draws it.
        let (w, h) = (1280, 720);
        let mut pic = Picture::default();
        pic.reshape(w, h);
        let ms: u32 = 0xdead_beef;
        for k in 0..34 {
            let on = match k {
                0 => true,
                1 => false,
                _ => ms >> (33 - k) & 1 == 1,
            };
            for y in h - BLOCK..h {
                for x in k * BLOCK..(k + 1) * BLOCK {
                    let v = if on { 235 } else { 20 };
                    pic.bytes_mut()[(y * w + x) * 4..(y * w + x) * 4 + 4].copy_from_slice(&[v, v, v, 255]);
                }
            }
        }
        assert_eq!(barcode(&pic), Some(ms));
        // Smaller blocks (a 640-wide picture: 18 px) read the same.
        let (w, h, bs) = (640, 360, 18);
        let mut small = Picture::default();
        small.reshape(w, h);
        for k in 0..34 {
            let on = k == 0 || (k > 1 && ms >> (33 - k) & 1 == 1);
            for y in h - bs..h {
                for x in k * bs..(k + 1) * bs {
                    let v = if on { 235 } else { 20 };
                    small.bytes_mut()[(y * w + x) * 4..(y * w + x) * 4 + 4].copy_from_slice(&[v, v, v, 255]);
                }
            }
        }
        assert_eq!(barcode(&small), Some(ms));
        // No sync blocks: no time.
        pic.px.fill(0);
        assert_eq!(barcode(&pic), None);
        assert_eq!(percentile(&[3.0, 1.0, 2.0], 0.5), 2.0);
    }
}
