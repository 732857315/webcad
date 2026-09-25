// 3D scene: gradient background, lit meshes and screen-space-width lines.
// Outputs are premultiplied alpha (blend One / OneMinusSrcAlpha), sRGB-encoded.

struct Draw3D {
    model_view: mat4x4<f32>,
    normal_mat: mat4x4<f32>,
    color: vec4<f32>,
    // lines: x = width px, y = depth pull, z = 1 to use the override color
    params: vec4<f32>,
};

@group(0) @binding(1) var<uniform> draw: Draw3D;

// ---------------------------------------------------------------------------------------------
// Background gradient (full-screen triangle)

struct BgOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) t: f32,
};

@vertex
fn vs_background(@builtin(vertex_index) vi: u32) -> BgOut {
    let x = f32((vi << 1u) & 2u) * 2.0 - 1.0;
    let y = f32(vi & 2u) * 2.0 - 1.0;
    var o: BgOut;
    o.clip = vec4<f32>(x, y, 0.0, 1.0);
    o.t = y * 0.5 + 0.5;
    return o;
}

@fragment
fn fs_background(i: BgOut) -> @location(0) vec4<f32> {
    let c = mix(frame.bg_bottom, frame.bg_top, clamp(i.t, 0.0, 1.0));
    return vec4<f32>(c.rgb * c.a, c.a);
}

// ---------------------------------------------------------------------------------------------
// Meshes

struct MeshIn {
    @location(0) pos: vec3<f32>,
    @location(1) normal: vec3<f32>,
};

struct MeshOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) view_pos: vec3<f32>,
    @location(1) normal: vec3<f32>,
};

@vertex
fn vs_mesh(v: MeshIn) -> MeshOut {
    let vp = draw.model_view * vec4<f32>(v.pos, 1.0);
    var o: MeshOut;
    o.clip = frame.proj * vp;
    o.view_pos = vp.xyz;
    o.normal = (draw.normal_mat * vec4<f32>(v.normal, 0.0)).xyz;
    return o;
}

@fragment
fn fs_mesh(i: MeshOut) -> @location(0) vec4<f32> {
    let face_n = cross(dpdx(i.view_pos), dpdy(i.view_pos));
    var n = select(i.normal, face_n, frame.light.w > 0.5);
    let nl = length(n);
    n = select(vec3<f32>(0.0, 0.0, 1.0), n / max(nl, 1e-30), nl > 1e-30);
    let persp = frame.viewport.w > 0.5;
    let vl = length(i.view_pos);
    let v = select(vec3<f32>(0.0, 0.0, 1.0), -i.view_pos / max(vl, 1e-30), persp && vl > 1e-30);
    // Two-sided lighting: always shade the side facing the viewer.
    n = select(n, -n, dot(n, v) < 0.0);
    let l = normalize(frame.light.xyz);
    let base = srgb_to_linear(draw.color.rgb);
    let hemi = dot(n, frame.up.xyz) * 0.5 + 0.5;
    let ambient = mix(frame.ground.rgb, frame.sky.rgb, hemi);
    let diffuse = max(dot(n, l), 0.0);
    let h = normalize(l + v);
    let spec = pow(max(dot(n, h), 0.0), 48.0) * 0.18;
    let lit = base * (ambient + vec3<f32>(0.72 * diffuse)) + vec3<f32>(spec);
    let a = draw.color.a;
    return vec4<f32>(linear_to_srgb(lit) * a, a);
}

// ---------------------------------------------------------------------------------------------
// Lines (one instance per segment, 6 vertices per instance)

struct Line3In {
    @builtin(vertex_index) vi: u32,
    @location(0) a: vec3<f32>,
    @location(1) b: vec3<f32>,
    @location(2) ca: vec4<f32>,
    @location(3) cb: vec4<f32>,
};

struct Line3Out {
    @builtin(position) clip: vec4<f32>,
    @location(0) color: vec4<f32>,
    // (along, across) * w and w: screen-linear interpolation without `noperspective` (WebGL2)
    @location(1) local_w: vec3<f32>,
    // x = segment length px, y = half width px, z = opacity scale for sub-pixel widths
    @location(2) @interpolate(flat) params: vec4<f32>,
};

fn pull(p: vec3<f32>) -> vec3<f32> {
    let bias = draw.params.y;
    if (frame.viewport.w > 0.5) {
        return p * (1.0 - bias);
    }
    return vec3<f32>(p.xy, p.z + bias * frame.misc.y);
}

@vertex
fn vs_line3(v: Line3In) -> Line3Out {
    var va = (draw.model_view * vec4<f32>(v.a, 1.0)).xyz;
    var vb = (draw.model_view * vec4<f32>(v.b, 1.0)).xyz;
    var o: Line3Out;
    if (frame.viewport.w > 0.5) {
        // Clip against the near plane in view space so w stays positive.
        let zn = -frame.misc.x;
        if (va.z > zn && vb.z > zn) {
            o.clip = vec4<f32>(0.0, 0.0, -2.0, 1.0);
            o.color = vec4<f32>(0.0);
            o.local_w = vec3<f32>(0.0, 0.0, 1.0);
            o.params = vec4<f32>(0.0);
            return o;
        }
        if (va.z > zn) {
            va = mix(va, vb, (zn - va.z) / (vb.z - va.z));
        } else if (vb.z > zn) {
            vb = mix(vb, va, (zn - vb.z) / (va.z - vb.z));
        }
    }
    let ka = frame.proj * vec4<f32>(pull(va), 1.0);
    let kb = frame.proj * vec4<f32>(pull(vb), 1.0);
    let half_vp = frame.viewport.xy * 0.5;
    let sa = ka.xy / ka.w * half_vp;
    let sb = kb.xy / kb.w * half_vp;
    let d = sb - sa;
    let len = length(d);
    let dir = select(vec2<f32>(1.0, 0.0), d / max(len, 1e-20), len > 1e-6);
    let nrm = vec2<f32>(-dir.y, dir.x);
    let w = max(draw.params.x * frame.viewport.z, 0.0);
    let wl = select(w, 1.0, w <= 0.0);
    let hw = max(wl, 1.0) * 0.5;
    let ext = hw + 1.0;
    let corner = quad_corner(v.vi);
    let is_b = corner.x > 0.5;
    let k = select(ka, kb, is_b);
    let off = dir * select(-ext, ext, is_b) + nrm * (corner.y * ext);
    o.clip = vec4<f32>(k.xy + off / half_vp * k.w, k.z, k.w);
    let along = select(-ext, len + ext, is_b);
    o.local_w = vec3<f32>(along * k.w, corner.y * ext * k.w, k.w);
    let c = select(v.ca, v.cb, is_b);
    o.color = select(c, draw.color, draw.params.z > 0.5);
    o.params = vec4<f32>(len, hw, min(wl, 1.0), 0.0);
    return o;
}

@fragment
fn fs_line3(i: Line3Out) -> @location(0) vec4<f32> {
    let q = i.local_w.xy / i.local_w.z;
    let cov = capsule_coverage(q, i.params.x, i.params.y) * i.params.z;
    if (cov <= 0.0) {
        discard;
    }
    let a = i.color.a * cov;
    return vec4<f32>(i.color.rgb * a, a);
}
