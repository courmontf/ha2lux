//! Icône dessinée par le programme (un soleil), indépendante du thème d'icônes du bureau.
//!
//! Certains thèmes n'ont pas d'image `display-brightness-symbolic` : l'espace de la zone de
//! notification reste alors vide. Une image ARGB32 fournie directement s'affiche toujours.

/// Couleur du soleil (ambre, lisible sur fond clair comme sur fond sombre).
const SUN: [u8; 3] = [0xF5, 0xB3, 0x01];
/// Couleur quand le démon est injoignable.
const GREY: [u8; 3] = [0x8A, 0x8A, 0x8A];

fn clamp01(v: f64) -> f64 {
    v.clamp(0.0, 1.0)
}

/// Image ARGB32 (octets A, R, V, B par pixel, ordre réseau) de `size` × `size` pixels.
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
            // Disque central, bord adouci sur un pixel.
            let disc = clamp01(radius * 0.40 - dist + 0.5);
            // Huit rayons : distance au rayon le plus proche (axes et diagonales).
            let angle = dy.atan2(dx);
            let step = std::f64::consts::FRAC_PI_4;
            let off = (angle / step - (angle / step).round()).abs() * step;
            let across = dist * off.sin();
            let ray_len =
                clamp01((radius * 0.95 - dist) + 0.5) * clamp01((dist - radius * 0.58) + 0.5);
            let ray = clamp01(n * 0.065 - across + 0.5) * ray_len;
            let alpha = clamp01(disc + ray);
            // La conversion est bornée à 0..=255 par `clamp01`.
            let a = (alpha * 255.0).round() as u8;
            data.extend_from_slice(&[a, color[0], color[1], color[2]]);
        }
    }
    data
}
