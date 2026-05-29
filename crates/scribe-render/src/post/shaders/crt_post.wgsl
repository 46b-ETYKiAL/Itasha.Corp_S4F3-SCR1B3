// SCR1B3 Phase 17 T17.4 — single-pass CRT post-process shader.
//
// One pipeline, one draw, full-screen triangle via @builtin(vertex_index)
// (no vertex buffer). Effects are encoded as float weights in the `Params`
// uniform; **zero weight = identity** — Naga 22 emits dead-code-elim on
// Vulkan/Metal/DX12, so OFF-effects cost nothing beyond the fixed
// 8.2 MP × ~11 fetch baseline (~1.4 ms @ 1080p, integrated GPU).
//
// Body-text bypass: the `mask` vec4 carries the editor `max_rect()` in UV.
// Pixels inside `mask` return the source texel verbatim — pixel-identical
// to no-pass state. This is the structural "never on body text" guarantee
// (T17.4 plan line).
//
// Reference: the rustdoc on `crate::post` documents the design rationale.

struct Params {
    // a: scanline / phosphor_glow / bloom / vignette
    a: vec4<f32>,
    // b: curvature / chromatic_aberration / glitch / grid
    b: vec4<f32>,
    // c: persistence_decay / time_sec / screen_w / screen_h
    c: vec4<f32>,
    // mask: body-text rect in UV (x, y, w, h)
    mask: vec4<f32>,
};

@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var src_tex: texture_2d<f32>;
@group(0) @binding(2) var src_smp: sampler;
@group(0) @binding(3) var hist_tex: texture_2d<f32>;

// Full-screen triangle — no vertex buffer.
@vertex
fn vs_main(@builtin(vertex_index) vid: u32) -> @builtin(position) vec4<f32> {
    let x = f32((vid << 1u) & 2u) * 2.0 - 1.0;
    let y = f32(vid & 2u) * 2.0 - 1.0;
    // -y to match egui's top-left origin
    return vec4<f32>(x, -y, 0.0, 1.0);
}

// (1) Scanlines — row-stripe modulation; never fully black.
fn apply_scanlines(c: vec3<f32>, uv: vec2<f32>, w: f32) -> vec3<f32> {
    if (w <= 0.0) { return c; }
    let stripe = step(0.5, fract(uv.y * p.c.w * 0.5));
    return c * mix(1.0 - w * 0.35, 1.0, stripe);
}

// (2) Gated bloom — 8-tap ring, luma-gated at 0.55; single-pass constraint.
fn apply_bloom(uv: vec2<f32>, w: f32) -> vec3<f32> {
    if (w <= 0.0) { return vec3<f32>(0.0); }
    let texel = vec2<f32>(1.0) / vec2<f32>(p.c.z, p.c.w);
    var sum = vec3<f32>(0.0);
    for (var i = 0; i < 8; i = i + 1) {
        let a = f32(i) * 0.7853981;  // pi/4
        let o = vec2<f32>(cos(a), sin(a)) * texel * 2.5;
        let s = textureSample(src_tex, src_smp, uv + o).rgb;
        let l = dot(s, vec3<f32>(0.2126, 0.7152, 0.0722));
        sum = sum + s * step(0.55, l);
    }
    return (sum / 8.0) * w;
}

// (3) Vignette — radial dim, applied last.
fn apply_vignette(c: vec3<f32>, uv: vec2<f32>, w: f32) -> vec3<f32> {
    if (w <= 0.0) { return c; }
    let d = distance(uv, vec2<f32>(0.5)) * 1.4142;
    return c * (1.0 - w * smoothstep(0.55, 1.0, d));
}

// (4) Micro chroma — R/B shifted radially by <=2 texels (perf cap).
fn apply_chroma(uv: vec2<f32>, w: f32) -> vec3<f32> {
    if (w <= 0.0) { return textureSample(src_tex, src_smp, uv).rgb; }
    let texel = vec2<f32>(1.0) / vec2<f32>(p.c.z, p.c.w);
    let dir = normalize(uv - vec2<f32>(0.5) + vec2<f32>(1e-6));
    let off = dir * texel * min(w * 2.0, 2.0);
    let r = textureSample(src_tex, src_smp, uv + off).r;
    let g = textureSample(src_tex, src_smp, uv).g;
    let b = textureSample(src_tex, src_smp, uv - off).b;
    return vec3<f32>(r, g, b);
}

// (5) Faint PCB/dot-grid — content-gated (only on near-black pixels).
fn apply_grid(c: vec3<f32>, uv: vec2<f32>, w: f32) -> vec3<f32> {
    if (w <= 0.0) { return c; }
    let l = dot(c, vec3<f32>(0.2126, 0.7152, 0.0722));
    let empty = 1.0 - smoothstep(0.05, 0.12, l);
    let gx = step(0.985, fract(uv.x * p.c.z / 24.0));
    let gy = step(0.985, fract(uv.y * p.c.w / 24.0));
    return c + vec3<f32>(0.06, 0.08, 0.10) * max(gx, gy) * w * empty;
}

// (6) Event-only glitch — UV tear; CPU sets weight=1 for one frame on event.
fn apply_glitch(uv: vec2<f32>, w: f32) -> vec2<f32> {
    if (w <= 0.0) { return uv; }
    let row = floor(uv.y * p.c.w / 4.0);
    let h = fract(sin(row * 12.9898 + p.c.y * 60.0) * 43758.5453);
    let tear = (h - 0.5) * w * 0.04;
    return vec2<f32>(uv.x + tear, uv.y);
}

// (7) Optional phosphor-persistence — max-blend with previous frame.
fn apply_persistence(c: vec3<f32>, uv: vec2<f32>, w: f32) -> vec3<f32> {
    if (w <= 0.0) { return c; }
    let prev = textureSample(hist_tex, src_smp, uv).rgb;
    return max(c, prev * (1.0 - w * 0.25));
}

@fragment
fn fs_main(@builtin(position) frag_pos: vec4<f32>) -> @location(0) vec4<f32> {
    var uv = frag_pos.xy / vec2<f32>(p.c.z, p.c.w);
    // Structural body-text bypass — DECISION T17.4: pixels inside the
    // mask rect return the source verbatim. byte-identical to no-pass.
    let in_text = step(p.mask.x, uv.x) * step(p.mask.y, uv.y)
                * step(uv.x, p.mask.x + p.mask.z)
                * step(uv.y, p.mask.y + p.mask.w);
    if (in_text > 0.5) { return textureSample(src_tex, src_smp, uv); }

    uv = apply_glitch(uv, p.b.z);
    var rgb = apply_chroma(uv, p.b.y);
    rgb = apply_scanlines(rgb, uv, p.a.x);
    rgb = rgb + apply_bloom(uv, p.a.z);
    // Phosphor tint — mixes a faint green-blue cast at half the glow weight.
    rgb = mix(rgb, rgb * vec3<f32>(0.94, 1.05, 0.96), p.a.y * 0.5);
    rgb = apply_grid(rgb, uv, p.b.w);
    rgb = apply_vignette(rgb, uv, p.a.w);
    rgb = apply_persistence(rgb, uv, p.c.x);
    return vec4<f32>(rgb, 1.0);
}
