//! Text over the picture (the machine picker, the connection's state), in an 8×8 bitmap font drawn
//! straight into the window's pixels: no font files, no text engine, for a few lines.

use font8x8::UnicodeFonts;

/// Draws `s` with its top left at `x`,`y`, each font pixel `scale`×`scale`, on a dark box (so it reads
/// over any picture). Characters the font doesn't have are drawn as `?`.
pub fn draw(dst: &mut [u32], (dw, dh): (usize, usize), (x, y): (usize, usize), scale: usize, color: u32, s: &str) {
    let scale = scale.max(1);
    let cell = 8 * scale;
    let n = s.chars().count();
    // The box first: a pixel's worth of the font around the text.
    let (bx0, by0) = (x.saturating_sub(scale * 2), y.saturating_sub(scale * 2));
    let (bx1, by1) = ((x + n * cell + scale * 2).min(dw), (y + cell + scale * 2).min(dh));
    for row in by0..by1 {
        for p in &mut dst[row * dw + bx0..row * dw + bx1.max(bx0)] {
            // Darkened to a quarter: the picture still shows a little.
            *p = (*p >> 2) & 0x003f_3f3f;
        }
    }
    for (i, c) in s.chars().enumerate() {
        let glyph = font8x8::BASIC_FONTS.get(c).or_else(|| font8x8::BASIC_FONTS.get('?')).unwrap_or([0; 8]);
        for (gy, bits) in glyph.iter().enumerate() {
            for gx in 0..8 {
                if bits >> gx & 1 == 0 {
                    continue;
                }
                for sy in 0..scale {
                    let py = y + gy * scale + sy;
                    if py >= dh {
                        break;
                    }
                    let px0 = x + i * cell + gx * scale;
                    for px in px0..(px0 + scale).min(dw) {
                        dst[py * dw + px] = color;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn letters_light_pixels_inside_their_cell() {
        let (w, h) = (40, 20);
        let mut dst = vec![0u32; w * h];
        super::draw(&mut dst, (w, h), (4, 4), 1, 0xffffff, "Hi");
        let lit: Vec<usize> = (0..w * h).filter(|&i| dst[i] == 0xffffff).collect();
        assert!(!lit.is_empty());
        assert!(lit.iter().all(|&i| (4..20).contains(&(i % w)) && (4..12).contains(&(i / w))));
    }
}
