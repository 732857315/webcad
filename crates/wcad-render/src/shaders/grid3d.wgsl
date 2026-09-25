// Anti-aliased grid on a plane, computed per fragment from screen-space derivatives.

struct GridDraw {
    // grid-local (u, v, 0) -> view space
    model_view: mat4x4<f32>,
    minor: vec4<f32>,
    major: vec4<f32>,
    axis_x: vec4<f32>,
    axis_y: vec4<f32>,
    // x = spacing, y = major spacing (0 = none), z = radius, w = line width px
    params: vec4<f32>,
    // xy = offset added to local uv for the periodic grid (anchor modulo major spacing),
    // zw = offset added to local uv to get coordinates relative to the plane origin
    offsets: vec4<f32>,
};

@group(0) @binding(1) var<uniform> grid: GridDraw;

struct GridOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_grid(@builtin(vertex_index) vi: u32) -> GridOut {
    let c = quad_corner(vi);
    let r = grid.params.z;
    let uv = vec2<f32>((c.x * 2.0 - 1.0) * r, c.y * r);
    var vp = (grid.model_view * vec4<f32>(uv, 0.0, 1.0)).xyz;
    if (frame.viewport.w > 0.5) {
        vp = vp * (1.0 - 5e-5);
    }
    var o: GridOut;
    o.clip = frame.proj * vec4<f32>(vp, 1.0);
    o.uv = uv;
    return o;
}

// Coverage of lines at multiples of `spacing` along coordinate `c` (screen derivative `fw`).
fn grid_lines(c: f32, fw: f32, spacing: f32, hw: f32) -> f32 {
    let px = abs(fract(c / spacing + 0.5) - 0.5) * spacing / max(fw, 1e-20);
    return clamp(hw + 0.5 - px, 0.0, 1.0);
}

// Straight-alpha "src over dst".
fn over(dst: vec4<f32>, src: vec4<f32>) -> vec4<f32> {
    let a = src.a + dst.a * (1.0 - src.a);
    let rgb = (src.rgb * src.a + dst.rgb * dst.a * (1.0 - src.a)) / max(a, 1e-6);
    return vec4<f32>(rgb, a);
}

@fragment
fn fs_grid(i: GridOut) -> @location(0) vec4<f32> {
    let g = i.uv + grid.offsets.xy;
    let fw = fwidth(g);
    let fmax = max(max(fw.x, fw.y), 1e-20);
    let hw = max(grid.params.w * frame.viewport.z, 1.0) * 0.5;
    let spacing = grid.params.x;
    let major = grid.params.y;
    var col = vec4<f32>(0.0);
    // Lines fade out when their cells get smaller than a few pixels (avoids moire).
    let minor_fade = smoothstep(4.0, 10.0, spacing / fmax);
    let minor_cov = max(grid_lines(g.x, fw.x, spacing, hw), grid_lines(g.y, fw.y, spacing, hw));
    col = over(col, vec4<f32>(grid.minor.rgb, grid.minor.a * minor_cov * minor_fade));
    if (major > 0.0) {
        let major_fade = smoothstep(4.0, 10.0, major / fmax);
        let major_cov = max(grid_lines(g.x, fw.x, major, hw), grid_lines(g.y, fw.y, major, hw));
        col = over(col, vec4<f32>(grid.major.rgb, grid.major.a * major_cov * major_fade));
    }
    // Axis lines through the plane origin: the X axis is v == 0, the Y axis is u == 0.
    let p = i.uv + grid.offsets.zw;
    let ax = clamp(hw * 1.5 + 0.5 - abs(p.y) / max(fw.y, 1e-20), 0.0, 1.0);
    let ay = clamp(hw * 1.5 + 0.5 - abs(p.x) / max(fw.x, 1e-20), 0.0, 1.0);
    col = over(col, vec4<f32>(grid.axis_y.rgb, grid.axis_y.a * ay));
    col = over(col, vec4<f32>(grid.axis_x.rgb, grid.axis_x.a * ax));
    // Fade towards the edge of the drawn square.
    let r = length(i.uv) / max(grid.params.z, 1e-20);
    let a = col.a * (1.0 - smoothstep(0.55, 1.0, r));
    if (a <= 0.0) {
        discard;
    }
    return vec4<f32>(col.rgb * a, a);
}
