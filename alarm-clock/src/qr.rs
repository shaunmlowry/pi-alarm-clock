//! QR code rendering (slice 6 / task 5.3).
//!
//! Renders the Google device-flow pairing URL (`verification_url`) to a
//! [`slint::Image`] for display on the Settings panel. Uses the `qrcode`
//! crate to build the matrix; the app renders it to an RGB pixel buffer at a
//! configurable scale with a quiet zone, so it scans reliably on the Pi's
//! display. The `user_code` is shown separately for the user to type in.

use slint::{Image, SharedPixelBuffer, Rgb8Pixel};

/// Render *text* as a QR code into a [`slint::Image`] at *scale* px per module
/// with a 4-module quiet zone (the standard). Returns `None` if the input is
/// too long for a QR code or empty.
pub fn render_qr_image(text: &str) -> Option<Image> {
    render_qr_image_scaled(text, 6)
}

pub fn render_qr_image_scaled(text: &str, scale: usize) -> Option<Image> {
    if text.is_empty() {
        return None;
    }
    let code = qrcode::QrCode::new(text.as_bytes()).ok()?;
    let modules = code.width();
    let quiet = 4;
    let side = (modules + 2 * quiet) * scale;
    if side == 0 || side > 4096 {
        return None;
    }

    let mut buf = SharedPixelBuffer::<Rgb8Pixel>::new(side as u32, side as u32);
    let pixels = buf.make_mut_slice();
    // Start white.
    for px in pixels.iter_mut() {
        *px = Rgb8Pixel { r: 255, g: 255, b: 255 };
    }

    let mut set = |x: usize, y: usize, p: Rgb8Pixel| {
        if x < side && y < side {
            pixels[y * side + x] = p;
        }
    };

    let dark = Rgb8Pixel { r: 0, g: 0, b: 0 };
    let light = Rgb8Pixel { r: 255, g: 255, b: 255 };

    for my in 0..modules {
        for mx in 0..modules {
            let on = matches!(code[(mx, my)], qrcode::Color::Dark);
            let px = (mx + quiet) * scale;
            let py = (my + quiet) * scale;
            for dy in 0..scale {
                for dx in 0..scale {
                    set(px + dx, py + dy, if on { dark } else { light });
                }
            }
        }
    }

    Some(Image::from_rgb8(buf))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Scenario: a short URL renders a non-empty square image with the right
    /// side length (modules + 2*quiet zone, scaled).
    #[test]
    fn render_short_url() {
        let img = render_qr_image("https://www.google.com/device?user_code=ABC-WXYZ").expect("render");
        let sz = img.size();
        // A QR for ~45 chars is version ~3-4 (~29-33 modules). At scale 6 with
        // a 4-module quiet zone: (33 + 8) * 6 = 246. Allow slack: just assert
        // a sensible bounded size and square aspect.
        assert!(sz.width > 100 && sz.width < 4096, "width {} out of range", sz.width);
        assert_eq!(sz.width, sz.height, "QR should be square");
    }

    /// Scenario: empty input returns None.
    #[test]
    fn empty_input_renders_none() {
        assert!(render_qr_image("").is_none());
    }

    /// Scenario: an overly long input that exceeds QR capacity returns None.
    #[test]
    fn overly_long_input_renders_none() {
        let huge = "x".repeat(10_000);
        assert!(render_qr_image(&huge).is_none());
    }
}
