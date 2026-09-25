// Helpers shared by every wcad-render shader module (prepended to each module).
// The render target is a gamma-space Rgba8Unorm texture, so colors are written sRGB-encoded.

fn srgb_to_linear(c: vec3<f32>) -> vec3<f32> {
    let lo = c / 12.92;
    let hi = pow((c + vec3<f32>(0.055)) / 1.055, vec3<f32>(2.4));
    return select(hi, lo, c <= vec3<f32>(0.04045));
}

fn linear_to_srgb(c: vec3<f32>) -> vec3<f32> {
    let x = clamp(c, vec3<f32>(0.0), vec3<f32>(1.0));
    let lo = x * 12.92;
    let hi = 1.055 * pow(x, vec3<f32>(1.0 / 2.4)) - vec3<f32>(0.055);
    return select(hi, lo, x < vec3<f32>(0.0031308));
}

// Coverage of a capsule (segment 0..len along x, half width hw) at local pixel position q.
fn capsule_coverage(q: vec2<f32>, len: f32, hw: f32) -> f32 {
    let dx = max(max(-q.x, q.x - len), 0.0);
    let d = length(vec2<f32>(dx, q.y));
    return clamp(hw + 0.5 - d, 0.0, 1.0);
}

// Corner of a segment quad for vertex_index 0..5 (two triangles): x = end (0 = a, 1 = b), y = side (-1 / +1).
fn quad_corner(vi: u32) -> vec2<f32> {
    let i = vi % 6u;
    let end = select(0.0, 1.0, i == 1u || i == 2u || i == 4u);
    let side = select(-1.0, 1.0, i == 2u || i == 4u || i == 5u);
    return vec2<f32>(end, side);
}
