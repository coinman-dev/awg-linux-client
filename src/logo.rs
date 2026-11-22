//! App logo: orange rounded square with a white shield and an "A".
//! One geometry (in a 128×128 design space, same as `desktop::ICON_SVG`) rendered by
//! a tiny anti-aliased software rasterizer — used for the header logo and window icon.

use eframe::egui::{ColorImage, IconData, pos2, Pos2};

const TOP: [u8; 3] = [0xfb, 0xb2, 0x6a];
const BOTTOM: [u8; 3] = [0xe2, 0x58, 0x2b];

fn cubic(p0: Pos2, p1: Pos2, p2: Pos2, p3: Pos2, out: &mut Vec<Pos2>) {
    for i in 1..=16 {
        let t = i as f32 / 16.0;
        let u = 1.0 - t;
        let w = [u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t];
        out.push(pos2(
            w[0] * p0.x + w[1] * p1.x + w[2] * p2.x + w[3] * p3.x,
            w[0] * p0.y + w[1] * p1.y + w[2] * p2.y + w[3] * p3.y,
        ));
    }
}

/// Stroked polylines: (points, stroke width).
fn strokes() -> Vec<(Vec<Pos2>, f32)> {
    // Shield: M64 24 L98 36 V62 C98 84 83 99 64 106 C45 99 30 84 30 62 V36 Z
    let mut shield = vec![pos2(64.0, 24.0), pos2(98.0, 36.0), pos2(98.0, 62.0)];
    cubic(pos2(98.0, 62.0), pos2(98.0, 84.0), pos2(83.0, 99.0), pos2(64.0, 106.0), &mut shield);
    cubic(pos2(64.0, 106.0), pos2(45.0, 99.0), pos2(30.0, 84.0), pos2(30.0, 62.0), &mut shield);
    shield.extend([pos2(30.0, 36.0), pos2(64.0, 24.0)]);
    vec![
        (shield, 8.0),
        (vec![pos2(50.0, 80.0), pos2(64.0, 46.0), pos2(78.0, 80.0)], 7.0),
        (vec![pos2(55.0, 69.0), pos2(73.0, 69.0)], 7.0),
    ]
}

fn seg_dist(p: Pos2, a: Pos2, b: Pos2) -> f32 {
    let ab = b - a;
    let t = ((p - a).dot(ab) / ab.length_sq()).clamp(0.0, 1.0);
    (p - (a + ab * t)).length()
}

/// RGBA (unmultiplied) pixels of a `size`×`size` logo.
pub fn rasterize(size: usize) -> Vec<u8> {
    let s = 128.0 / size as f32; // design units per pixel
    let strokes = strokes();
    let mut rgba = vec![0u8; size * size * 4];
    for y in 0..size {
        for x in 0..size {
            let p = pos2((x as f32 + 0.5) * s, (y as f32 + 0.5) * s);
            // rounded square 6..122, radius 26
            let r = 26.0;
            let qx = (p.x - 64.0).abs() - (58.0 - r);
            let qy = (p.y - 64.0).abs() - (58.0 - r);
            let d = (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt() + qx.max(qy).min(0.0) - r;
            let alpha = (0.5 - d / s).clamp(0.0, 1.0);
            if alpha == 0.0 {
                continue;
            }
            let mut white: f32 = 0.0;
            for (pts, w) in &strokes {
                let dist = pts.windows(2).map(|ab| seg_dist(p, ab[0], ab[1])).fold(f32::MAX, f32::min);
                white = white.max((0.5 - (dist - w / 2.0) / s).clamp(0.0, 1.0));
            }
            let t = ((p.x + p.y) / 256.0).clamp(0.0, 1.0);
            let i = (y * size + x) * 4;
            for c in 0..3 {
                let g = TOP[c] as f32 + (BOTTOM[c] as f32 - TOP[c] as f32) * t;
                rgba[i + c] = (g + (255.0 - g) * white).round() as u8;
            }
            rgba[i + 3] = (alpha * 255.0).round() as u8;
        }
    }
    rgba
}

pub fn color_image(size: usize) -> ColorImage {
    ColorImage::from_rgba_unmultiplied([size, size], &rasterize(size))
}

pub fn window_icon() -> IconData {
    IconData { rgba: rasterize(128), width: 128, height: 128 }
}

#[cfg(test)]
mod tests {
    #[test]
    fn renders() {
        let px = super::rasterize(64);
        assert_eq!(px[3], 0, "corner must be transparent");
        let c = (32 * 64 + 32) * 4;
        assert_eq!(px[c + 3], 255, "center must be opaque");
        if let Some(p) = std::env::var_os("AWG_LOGO_PNG") {
            let img = super::color_image(256);
            crate::desktop::write_png(std::path::Path::new(&p), &img).unwrap();
        }
    }
}
