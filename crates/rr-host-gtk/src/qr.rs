//! QR codes as pixels (the `qrcode` crate's modules, scaled, with the quiet zone readers need).

use qrcode::{Color, QrCode};

/// An RGBA picture, `size` × `size`, 4 bytes a pixel, rows of `size * 4`.
pub struct QrImage {
    pub size: usize,
    pub rgba: Vec<u8>,
}

/// `text` as a QR code, each module `scale` pixels, a 4-module white border around it.
pub fn render(text: &str, scale: usize) -> Option<QrImage> {
    let code = QrCode::new(text.as_bytes()).ok()?;
    let modules = code.width();
    let colors = code.to_colors();
    let quiet = 4;
    let size = (modules + 2 * quiet) * scale;
    let mut rgba = vec![255u8; size * size * 4];
    for my in 0..modules {
        for mx in 0..modules {
            if colors[my * modules + mx] != Color::Dark {
                continue;
            }
            for y in 0..scale {
                let row = ((my + quiet) * scale + y) * size;
                for x in 0..scale {
                    let p = (row + (mx + quiet) * scale + x) * 4;
                    rgba[p..p + 3].copy_from_slice(&[0, 0, 0]);
                }
            }
        }
    }
    Some(QrImage { size, rgba })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_link_as_a_qr_code() {
        let img = render("http://192.168.1.20:7979/#pair=123456&id=abcd-efgh-ijkl-mnop", 4).unwrap();
        assert_eq!(img.rgba.len(), img.size * img.size * 4);
        // The border is white, the top-left finder pattern's corner black.
        assert_eq!(&img.rgba[0..4], &[255, 255, 255, 255]);
        let corner = (16 * img.size + 16) * 4;
        assert_eq!(&img.rgba[corner..corner + 4], &[0, 0, 0, 255]);
    }
}
