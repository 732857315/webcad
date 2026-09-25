//! AutoCAD Color Index (ACI) palette.

/// RGB for an ACI index. 0 (ByBlock) and 256 (ByLayer) are not colors; index 7 is returned as
/// white here, callers that know the background should special-case it (see `Color::resolve`).
pub fn rgb(index: u8) -> [u8; 3] {
    match index {
        0 => [0, 0, 0],
        1 => [255, 0, 0],
        2 => [255, 255, 0],
        3 => [0, 255, 0],
        4 => [0, 255, 255],
        5 => [0, 0, 255],
        6 => [255, 0, 255],
        7 => [255, 255, 255],
        8 => [128, 128, 128],
        9 => [192, 192, 192],
        250..=255 => {
            const GREYS: [u8; 6] = [51, 91, 132, 173, 214, 255];
            let g = GREYS[(index - 250) as usize];
            [g, g, g]
        }
        _ => hue_ring(index),
    }
}

/// Indices 10..=249: 24 hues × 5 brightness levels × 2 saturations (the standard ACI layout).
fn hue_ring(index: u8) -> [u8; 3] {
    let i = (index - 10) as u32;
    let hue = (i / 10) as f64 * 15.0; // degrees
    let row = i % 10;
    let value = [1.0, 1.0, 0.8, 0.8, 0.6, 0.6, 0.5, 0.5, 0.3, 0.3][row as usize];
    let sat = if row % 2 == 0 { 1.0 } else { 0.5 };
    hsv_to_rgb(hue, sat, value)
}

fn hsv_to_rgb(h: f64, s: f64, v: f64) -> [u8; 3] {
    let c = v * s;
    let hp = (h / 60.0) % 6.0;
    let x = c * (1.0 - ((hp % 2.0) - 1.0).abs());
    let (r, g, b) = match hp as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = v - c;
    let to = |f: f64| ((f + m) * 255.0).round().clamp(0.0, 255.0) as u8;
    [to(r), to(g), to(b)]
}

/// Nearest ACI index for an RGB color (for DXF export of true colors to old versions).
pub fn nearest(rgb_in: [u8; 3]) -> u8 {
    (1..=255u8)
        .min_by_key(|&i| {
            let c = rgb(i);
            let d = |a: u8, b: u8| (a as i32 - b as i32).pow(2);
            d(c[0], rgb_in[0]) + d(c[1], rgb_in[1]) + d(c[2], rgb_in[2])
        })
        .unwrap_or(7)
}

#[cfg(test)]
mod tests {
    #[test]
    fn primaries() {
        assert_eq!(super::rgb(1), [255, 0, 0]);
        assert_eq!(super::rgb(10), [255, 0, 0]);
        assert_eq!(super::nearest([250, 5, 5]), 1);
    }
}
