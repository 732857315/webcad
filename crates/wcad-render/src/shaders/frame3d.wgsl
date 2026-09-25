// Per-render 3D uniforms shared by the 3D shader modules (prepended after common.wgsl).
// View space is right-handed with the camera looking down -Z. Depth is reverse-Z (1 = near).

struct Frame3D {
    proj: mat4x4<f32>,
    // xy = viewport size px, z = pixel scale, w = 1 for perspective / 0 for orthographic
    viewport: vec4<f32>,
    // xyz = direction towards the light (view space), w = 1 for flat shading
    light: vec4<f32>,
    // xyz = world up in view space
    up: vec4<f32>,
    // hemispheric ambient (linear rgb)
    sky: vec4<f32>,
    ground: vec4<f32>,
    // gradient background (sRGB, straight alpha)
    bg_top: vec4<f32>,
    bg_bottom: vec4<f32>,
    // x = near plane distance, y = ortho depth range (for line bias), zw = unused
    misc: vec4<f32>,
};

@group(0) @binding(0) var<uniform> frame: Frame3D;
