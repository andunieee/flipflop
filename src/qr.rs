//! QR code rendering and decoding.

use qrcode::{Color, QrCode};

/// Render `text` as an RGBA bitmap (black on white, with a quiet zone), `scale` pixels per module.
/// Returns `(width, height, rgba)`.
pub fn render(text: &str, scale: usize) -> Option<(u32, u32, Vec<u8>)> {
    let code = QrCode::new(text.as_bytes()).ok()?;
    let modules = code.width();
    let quiet = 4;
    let side = (modules + 2 * quiet) * scale;
    let colors = code.to_colors();
    let mut rgba = vec![255u8; side * side * 4];
    for y in 0..modules {
        for x in 0..modules {
            if colors[y * modules + x] != Color::Dark {
                continue;
            }
            for dy in 0..scale {
                for dx in 0..scale {
                    let px = (x + quiet) * scale + dx;
                    let py = (y + quiet) * scale + dy;
                    let i = (py * side + px) * 4;
                    rgba[i..i + 3].fill(0);
                }
            }
        }
    }
    Some((side as u32, side as u32, rgba))
}

/// Find and decode the first QR code in an 8-bit grayscale image.
pub fn decode_gray(width: usize, height: usize, gray: &[u8]) -> Option<String> {
    let mut img =
        rqrr::PreparedImage::prepare_from_greyscale(width, height, |x, y| gray[y * width + x]);
    img.detect_grids()
        .into_iter()
        .find_map(|grid| grid.decode().ok().map(|(_, text)| text))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let text = "endpointaaaabbbbccccddddeeeeffff";
        let (w, h, rgba) = render(text, 4).unwrap();
        let gray: Vec<u8> = rgba.chunks_exact(4).map(|p| p[0]).collect();
        assert_eq!(
            decode_gray(w as usize, h as usize, &gray).as_deref(),
            Some(text)
        );
    }
}
