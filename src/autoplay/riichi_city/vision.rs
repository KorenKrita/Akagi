//! Sight the Riichi City settlement "OK" button on screen, so the round
//! advance fires when the score breakdown is actually visible.
//!
//! The button is a solid turquoise fill (rgb 108,255,253) whose color is
//! unique on the settlement screen, so the detector is a color-blob
//! finder: immune to the countdown digits (no glyph pixels are compared)
//! and window-size-independent (thresholds scale with the frame width).
//! Calibrated against six labeled screenshots: the button blob is ~84×27
//! px at a 664-px frame width (fill 0.86, aspect 3.1, center ~85%/88%),
//! and the no-button frames contain no qualifying cluster at any rescale.

use image::RgbaImage;

/// Frame width the pixel thresholds were calibrated at; thresholds scale
/// with the square of the width ratio.
const CALIBRATION_WIDTH: f64 = 664.0;

fn is_button_cyan(r: u8, g: u8, b: u8) -> bool {
    let (r, g, b) = (i16::from(r), i16::from(g), i16::from(b));
    g > 190 && b > 190 && g - r > 60 && b - r > 40 && (g - b).abs() < 90
}

/// A sighting: bounding box in frame pixels and classified fill count.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ButtonSighting {
    pub x0: u32,
    pub y0: u32,
    pub x1: u32,
    pub y1: u32,
    pub pixels: u32,
}

/// Find the OK button in an RGBA frame, if visible. A blob qualifies when
/// it holds enough fill pixels (scaled to the frame width), is roughly
/// three times wider than tall, is mostly fill, and sits in the
/// bottom-right quadrant where the client renders it. Anti-aliased seams
/// are bridged by growing clusters through a ±6 px neighborhood.
pub fn ok_button_visible(img: &RgbaImage) -> Option<ButtonSighting> {
    let (w, h) = img.dimensions();
    if w == 0 || h == 0 {
        return None;
    }
    let scale = (f64::from(w) / CALIBRATION_WIDTH).powi(2);
    // ~1950 px at the calibration width; 800 tolerates smaller windows
    // while staying above scattered cyan UI accents (<200 px).
    let min_pixels = (800.0 * scale) as u32;

    let stride = w as usize;
    let mut mask = vec![false; stride * h as usize];
    for (x, y, p) in img.enumerate_pixels() {
        let [r, g, b, _a] = p.0;
        if is_button_cyan(r, g, b) {
            mask[y as usize * stride + x as usize] = true;
        }
    }

    let mut visited = vec![false; mask.len()];
    let mut best: Option<ButtonSighting> = None;
    for y in 0..h {
        for x in 0..w {
            let seed = y as usize * stride + x as usize;
            if !mask[seed] || visited[seed] {
                continue;
            }
            let (mut x0, mut x1, mut y0, mut y1) = (x, x, y, y);
            let mut pixels = 0u32;
            let mut stack = vec![(x, y)];
            visited[seed] = true;
            while let Some((cx, cy)) = stack.pop() {
                pixels += 1;
                x0 = x0.min(cx);
                x1 = x1.max(cx);
                y0 = y0.min(cy);
                y1 = y1.max(cy);
                let nx_lo = cx.saturating_sub(6);
                let nx_hi = (cx + 6).min(w - 1);
                let ny_lo = cy.saturating_sub(6);
                let ny_hi = (cy + 6).min(h - 1);
                for ny in ny_lo..=ny_hi {
                    for nx in nx_lo..=nx_hi {
                        let i = ny as usize * stride + nx as usize;
                        if mask[i] && !visited[i] {
                            visited[i] = true;
                            stack.push((nx, ny));
                        }
                    }
                }
            }
            if pixels < min_pixels {
                continue;
            }
            let (bw, bh) = (x1 - x0 + 1, y1 - y0 + 1);
            let aspect = f64::from(bw) / f64::from(bh);
            if !(2.0..=4.5).contains(&aspect) {
                continue;
            }
            if f64::from(pixels) / f64::from(bw * bh) < 0.55 {
                continue;
            }
            let center_x = f64::from(x0 + x1) / (2.0 * f64::from(w));
            let center_y = f64::from(y0 + y1) / (2.0 * f64::from(h));
            if center_x <= 0.6 || center_y <= 0.6 {
                continue;
            }
            if best.as_ref().is_none_or(|b| pixels > b.pixels) {
                best = Some(ButtonSighting {
                    x0,
                    y0,
                    x1,
                    y1,
                    pixels,
                });
            }
        }
    }
    best
}

/// Capture the Riichi City client window and look for the OK button.
/// Blocking (screen capture); call from `spawn_blocking`. `None` means "no
/// sighting": window not found, minimized, capture failed, or button absent
/// — callers must not advance on `None`.
pub fn capture_ok_button() -> Option<ButtonSighting> {
    let window = find_riichi_city_window()?;
    let frame = window.capture_image().ok()?;
    ok_button_visible(&frame)
}

/// The frontmost non-minimized window belonging to the Riichi City client.
/// Matched by executable name (`Mahjong-JP.exe`) with the window title as
/// fallback, since the Unity player's title varies by locale.
fn find_riichi_city_window() -> Option<xcap::Window> {
    let windows = xcap::Window::all().ok()?;
    // `Window::all` is z-ordered, so the first hit is the frontmost client.
    windows.into_iter().find(|window| {
        let Ok(title) = window.title() else {
            return false;
        };
        let app = window.app_name().unwrap_or_default().to_lowercase();
        let title_lower = title.to_lowercase();
        let matches_client = app.contains("mahjong-jp")
            || title_lower.contains("riichi city")
            || title.contains("麻雀一番街");
        matches_client
            && !window.is_minimized().unwrap_or(true)
            && window.width().unwrap_or(0) >= 400
            && window.height().unwrap_or(0) >= 300
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::imageops::FilterType;

    fn fixture(name: &str) -> RgbaImage {
        let path = format!(
            "{}/src/autoplay/riichi_city/assets/fixtures/{name}.png",
            env!("CARGO_MANIFEST_DIR")
        );
        image::open(path)
            .unwrap_or_else(|e| panic!("fixture {name}: {e}"))
            .to_rgba8()
    }

    /// The four positive screenshots span countdown 59s→56s; matching all
    /// of them is the digit-invariance proof.
    #[test]
    fn detects_the_button_at_every_countdown_value() {
        for name in ["ok59", "ok58", "ok57", "ok56"] {
            let sight = ok_button_visible(&fixture(name))
                .unwrap_or_else(|| panic!("{name} should contain the OK button"));
            assert_eq!(
                (sight.x0, sight.y0, sight.x1, sight.y1),
                (562, 329, 645, 355),
                "{name}: unexpected button box"
            );
        }
    }

    #[test]
    fn frames_without_the_button_yield_no_sighting() {
        assert!(ok_button_visible(&fixture("nook1")).is_none());
        assert!(ok_button_visible(&fixture("nook2")).is_none());
    }

    /// Rescaling simulates a differently sized client window; the fixtures
    /// are half-scale shots, so 2× is roughly the live client.
    #[test]
    fn detection_survives_client_rescale() {
        for (name, factor) in [("ok56", 2.0), ("ok59", 1.5)] {
            let img = fixture(name);
            let (w, h) = img.dimensions();
            let scaled = image::imageops::resize(
                &img,
                (w as f64 * factor) as u32,
                (h as f64 * factor) as u32,
                FilterType::Lanczos3,
            );
            assert!(
                ok_button_visible(&scaled).is_some(),
                "{name} at {factor}x should still be detected"
            );
        }
        for (name, factor) in [("nook1", 0.5), ("nook2", 2.0)] {
            let img = fixture(name);
            let (w, h) = img.dimensions();
            let scaled = image::imageops::resize(
                &img,
                ((w as f64 * factor) as u32).max(1),
                ((h as f64 * factor) as u32).max(1),
                FilterType::Lanczos3,
            );
            assert!(
                ok_button_visible(&scaled).is_none(),
                "{name} at {factor}x must stay undetected"
            );
        }
    }

    #[test]
    fn cyan_classifier_admits_the_measured_fill_and_rejects_neighbors() {
        assert!(is_button_cyan(108, 255, 253));
        // Capture pipelines shift channels a few units; still the button.
        assert!(is_button_cyan(116, 250, 247));
        // White panel, dark text, and warm/cool accents must not classify.
        assert!(!is_button_cyan(255, 255, 255));
        assert!(!is_button_cyan(37, 37, 37));
        assert!(!is_button_cyan(253, 108, 255));
        assert!(!is_button_cyan(108, 253, 160));
    }
}
