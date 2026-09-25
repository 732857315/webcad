// 2D batches: filled triangles, screen-space-width capsule lines and point markers.
// Pixel space inside the shader: origin at the viewport center, Y up.
// Outputs are premultiplied alpha (blend One / OneMinusSrcAlpha), sRGB-encoded.

struct Frame2D {
    // xy = viewport size in pixels, z = pixel scale for widths/sizes, w = unused
    viewport: vec4<f32>,
};

struct Draw2D {
    // Column-major 2x2 matrix: local -> pixels (xy = column 0, zw = column 1)
    m: vec4<f32>,
    // xy = translation in pixels, zw = unused
    t: vec4<f32>,
    // Override color (used when flags.x > 0.5)
    color: vec4<f32>,
    // x = use override color, y = opacity multiplier
    flags: vec4<f32>,
};

@group(0) @binding(0) var<uniform> frame: Frame2D;
@group(0) @binding(1) var<uniform> draw: Draw2D;

fn to_px(p: vec2<f32>) -> vec2<f32> {
    return vec2<f32>(draw.m.x * p.x + draw.m.z * p.y, draw.m.y * p.x + draw.m.w * p.y) + draw.t.xy;
}

fn px_to_clip(s: vec2<f32>) -> vec4<f32> {
    return vec4<f32>(s / (frame.viewport.xy * 0.5), 0.0, 1.0);
}

fn style_color(c: vec4<f32>) -> vec4<f32> {
    let base = select(c, draw.color, draw.flags.x > 0.5);
    return vec4<f32>(base.rgb, base.a * draw.flags.y);
}

// ---------------------------------------------------------------------------------------------
// Fills

struct FillIn {
    @location(0) pos: vec2<f32>,
    @location(1) color: vec4<f32>,
};

struct FillOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) color: vec4<f32>,
};

@vertex
fn vs_fill(v: FillIn) -> FillOut {
    var o: FillOut;
    o.clip = px_to_clip(to_px(v.pos));
    o.color = style_color(v.color);
    return o;
}

@fragment
fn fs_fill(i: FillOut) -> @location(0) vec4<f32> {
    return vec4<f32>(i.color.rgb * i.color.a, i.color.a);
}

// ---------------------------------------------------------------------------------------------
// Lines (one instance per segment, 6 vertices per instance)

struct LineIn {
    @builtin(vertex_index) vi: u32,
    @location(0) a: vec2<f32>,
    @location(1) b: vec2<f32>,
    @location(2) ca: vec4<f32>,
    @location(3) cb: vec4<f32>,
    @location(4) width: f32,
};

struct LineOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) color: vec4<f32>,
    // (along, across) in pixels relative to the segment start
    @location(1) local: vec2<f32>,
    // x = segment length (px), y = half width (px), z = opacity scale for sub-pixel widths
    @location(2) @interpolate(flat) params: vec4<f32>,
};

@vertex
fn vs_line(v: LineIn) -> LineOut {
    let pa = to_px(v.a);
    let pb = to_px(v.b);
    let d = pb - pa;
    let len = length(d);
    let dir = select(vec2<f32>(1.0, 0.0), d / max(len, 1e-20), len > 1e-6);
    let nrm = vec2<f32>(-dir.y, dir.x);
    var w = v.width * frame.viewport.z;
    w = select(w, 1.0, w <= 0.0);
    let hw = max(w, 1.0) * 0.5;
    let ext = hw + 1.0;
    let corner = quad_corner(v.vi);
    let along = select(-ext, len + ext, corner.x > 0.5);
    let across = corner.y * ext;
    var o: LineOut;
    o.clip = px_to_clip(pa + dir * along + nrm * across);
    o.color = style_color(select(v.ca, v.cb, corner.x > 0.5));
    o.local = vec2<f32>(along, across);
    o.params = vec4<f32>(len, hw, min(w, 1.0), 0.0);
    return o;
}

@fragment
fn fs_line(i: LineOut) -> @location(0) vec4<f32> {
    let cov = capsule_coverage(i.local, i.params.x, i.params.y) * i.params.z;
    if (cov <= 0.0) {
        discard;
    }
    let a = i.color.a * cov;
    return vec4<f32>(i.color.rgb * a, a);
}

// ---------------------------------------------------------------------------------------------
// Point markers (one instance per point)

struct PointIn {
    @builtin(vertex_index) vi: u32,
    @location(0) pos: vec2<f32>,
    @location(1) color: vec4<f32>,
    @location(2) size: f32,
    @location(3) shape: u32,
};

struct PointOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) color: vec4<f32>,
    // pixel offset from the marker center
    @location(1) q: vec2<f32>,
    // x = half size (px), y = stroke width (px), z = shape
    @location(2) @interpolate(flat) params: vec4<f32>,
};

@vertex
fn vs_point(v: PointIn) -> PointOut {
    let c = to_px(v.pos);
    let h = max(v.size * frame.viewport.z, 1.0) * 0.5;
    let stroke = max(1.25 * frame.viewport.z, 1.0);
    let ext = h + stroke + 1.0;
    let corner = quad_corner(v.vi);
    let q = vec2<f32>((corner.x * 2.0 - 1.0) * ext, corner.y * ext);
    var o: PointOut;
    o.clip = px_to_clip(c + q);
    o.color = style_color(v.color);
    o.q = q;
    o.params = vec4<f32>(h, stroke, f32(v.shape), 0.0);
    return o;
}

fn box_sd(q: vec2<f32>, half: vec2<f32>) -> f32 {
    let d = abs(q) - half;
    return length(max(d, vec2<f32>(0.0))) + min(max(d.x, d.y), 0.0);
}

@fragment
fn fs_point(i: PointOut) -> @location(0) vec4<f32> {
    let h = i.params.x;
    let t = i.params.y;
    let shape = u32(i.params.z + 0.5);
    let q = i.q;
    var d: f32;
    if (shape == 1u) {
        d = abs(box_sd(q, vec2<f32>(h - t * 0.5))) - t * 0.5;
    } else if (shape == 2u) {
        d = min(box_sd(q, vec2<f32>(h, t * 0.5)), box_sd(q, vec2<f32>(t * 0.5, h)));
    } else if (shape == 3u) {
        let r = vec2<f32>(q.x + q.y, q.x - q.y) * 0.70710678;
        let l = h * 1.41421356;
        d = min(box_sd(r, vec2<f32>(l, t * 0.5)), box_sd(r, vec2<f32>(t * 0.5, l)));
    } else if (shape == 4u) {
        d = length(q) - h;
    } else if (shape == 5u) {
        d = abs(length(q) - (h - t * 0.5)) - t * 0.5;
    } else {
        d = box_sd(q, vec2<f32>(h));
    }
    let cov = clamp(0.5 - d, 0.0, 1.0);
    if (cov <= 0.0) {
        discard;
    }
    let a = i.color.a * cov;
    return vec4<f32>(i.color.rgb * a, a);
}
