//! Icon drawn by the program (a sun), independent of the desktop icon theme.
//!
//! Some themes have no `display-brightness-symbolic` image: the notification area
//! slot then stays empty. An ARGB32 image supplied directly always displays.

/// Sun color (amber, readable on both light and dark backgrounds).
const SUN: [u8; 3] = [0xF5, 0xB3, 0x01];
/// Color when the daemon is unreachable.
const GREY: [u8; 3] = [0x8A, 0x8A, 0x8A];

fn clamp01(v: f64) -> f64 {
    v.clamp(0.0, 1.0)
}

/// ARGB32 image (A, R, G, B bytes per pixel, network order) of `size` × `size` pixels.
pub fn sun_argb(size: u32, reachable: bool) -> Vec<u8> {
    let color = if reachable { SUN } else { GREY };
    let n = f64::from(size);
    let center = (n - 1.0) / 2.0;
    let radius = n / 2.0;
    let mut data = Vec::with_capacity((size * size * 4) as usize);
    for y in 0..size {
        for x in 0..size {
            let (dx, dy) = (f64::from(x) - center, f64::from(y) - center);
            let dist = dx.hypot(dy);
            // Central disc, edge softened over one pixel.
            let disc = clamp01(radius * 0.40 - dist + 0.5);
            // Eight rays: distance to the nearest ray (axes and diagonals).
            let angle = dy.atan2(dx);
            let step = std::f64::consts::FRAC_PI_4;
            let off = (angle / step - (angle / step).round()).abs() * step;
            let across = dist * off.sin();
            let ray_len =
                clamp01((radius * 0.95 - dist) + 0.5) * clamp01((dist - radius * 0.58) + 0.5);
            let ray = clamp01(n * 0.065 - across + 0.5) * ray_len;
            let alpha = clamp01(disc + ray);
            // The conversion is bounded to 0..=255 by `clamp01`.
            let a = (alpha * 255.0).round() as u8;
            data.extend_from_slice(&[a, color[0], color[1], color[2]]);
        }
    }
    data
}
