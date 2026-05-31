// SCR1B3 F-027 — procedural CRT overlay shader (offscreen-RT-free).
//
// One pipeline, one draw, full-screen triangle via @builtin(vertex_index)
// (no vertex buffer). This shader does NOT sample the framebuffer — it is a
// pure *overlay* that egui_wgpu's `CallbackTrait::paint` blends OVER the
// already-painted editor using premultiplied-alpha blending.
//
// WHY NO SOURCE SAMPLE: eframe 0.34 owns the surface render loop and exposes
// no post-egui hook (`App::post_rendering` was removed in eframe 0.24), and
// `CallbackTrait::paint` runs INSIDE the egui surface render pass — wgpu
// forbids binding the active color attachment as a sampled texture. A
// full-frame post-process that *resamples* the composited frame is therefore
// architecturally impossible within eframe 0.34 (see
// `docs/audits/crt-post-pass-design.md`). The overlay form below is the
// subset of CRT artifacts that need NO source texel — they are all
// modulation (darken) or faint additive tint, which compose correctly via
// alpha blending and can never black out the editor.
//
// Output convention: PREMULTIPLIED alpha. `rgb` is already multiplied by `a`.
// The pipeline blend state is premultiplied-alpha
// (src=ONE, dst=ONE_MINUS_SRC_ALPHA), so `out = overlay.rgb + dst*(1-a)`.
// A fully transparent texel (`a == 0`, `rgb == 0`) is a byte-exact no-op.
//
// Body-text bypass: the `mask` vec4 carries the editor `max_rect()` in UV.
// Pixels inside `mask` emit a fully-transparent texel — byte-identical to
// no-pass for the body-text region (the structural "never on body text"
// guarantee). The mask defaults to zero-rect (no bypass) when unset.

struct Params {
    // a: scanline / phosphor_glow / bloom(unused-overlay) / vignette
    a: vec4<f32>,
    // b: curvature(unused-overlay) / chromatic_aberration(unused-overlay) / glitch / grid
    b: vec4<f32>,
    // c: persistence_decay(unused-overlay) / time_sec / screen_w / screen_h
    c: vec4<f32>,
    // mask: body-text rect in UV (x, y, w, h)
    mask: vec4<f32>,
};

@group(0) @binding(0) var<uniform> p: Params;

// Full-screen triangle — no vertex buffer.
@vertex
fn vs_main(@builtin(vertex_index) vid: u32) -> @builtin(position) vec4<f32> {
    let x = f32((vid << 1u) & 2u) * 2.0 - 1.0;
    let y = f32(vid & 2u) * 2.0 - 1.0;
    // -y to match egui's top-left origin
    return vec4<f32>(x, -y, 0.0, 1.0);
}

// Accumulated CRT overlay is built in *straight* (non-premultiplied) space as
// a (tint_rgb, coverage_alpha) pair, then premultiplied once at the end.

// (1) Scanlines — horizontal row-stripe darkening. Coverage scales with
//     weight; the darker stripe rows get alpha that dims the editor toward
//     black. Never fully opaque (cap 0.35) so text stays readable.
fn scanline_alpha(uv: vec2<f32>, w: f32) -> f32 {
    if (w <= 0.0) { return 0.0; }
    let stripe = step(0.5, fract(uv.y * p.c.w * 0.5));
    // stripe==0 → dark row gets up to 0.35*w darkening; stripe==1 → none.
    return (1.0 - stripe) * w * 0.35;
}

// (2) Vignette — radial darkening toward the corners.
fn vignette_alpha(uv: vec2<f32>, w: f32) -> f32 {
    if (w <= 0.0) { return 0.0; }
    let d = distance(uv, vec2<f32>(0.5)) * 1.4142;
    return w * smoothstep(0.55, 1.0, d) * 0.9;
}

// (3) Faint dot-grid — additive phosphor-cell tint on a sparse lattice.
//     Returns a premultiplied additive contribution (no darkening).
fn grid_tint(uv: vec2<f32>, w: f32) -> vec3<f32> {
    if (w <= 0.0) { return vec3<f32>(0.0); }
    let gx = step(0.985, fract(uv.x * p.c.z / 24.0));
    let gy = step(0.985, fract(uv.y * p.c.w / 24.0));
    return vec3<f32>(0.06, 0.08, 0.10) * max(gx, gy) * w;
}

// (4) Phosphor glow — a faint green-blue additive wash across the whole
//     overlay (the iconic CRT cast). Additive (premultiplied) contribution.
fn phosphor_tint(w: f32) -> vec3<f32> {
    if (w <= 0.0) { return vec3<f32>(0.0); }
    // Very faint so it tints rather than washes out.
    return vec3<f32>(0.0, 0.045, 0.02) * w;
}

// (5) Event-only glitch — per-row horizontal darken bands, gated to one frame
//     by the CPU (weight=1 only on the event frame). Additive darkening.
fn glitch_alpha(uv: vec2<f32>, w: f32) -> f32 {
    if (w <= 0.0) { return 0.0; }
    let row = floor(uv.y * p.c.w / 4.0);
    let h = fract(sin(row * 12.9898 + p.c.y * 60.0) * 43758.5453);
    // Tear bands: roughly half the rows darken briefly.
    return step(0.7, h) * w * 0.25;
}

@fragment
fn fs_main(@builtin(position) frag_pos: vec4<f32>) -> @location(0) vec4<f32> {
    let uv = frag_pos.xy / vec2<f32>(p.c.z, p.c.w);

    // Structural body-text bypass — pixels inside the mask rect emit a fully
    // transparent texel (byte-identical to no-pass). Mask defaults to the
    // zero rect, which this test never enters, so the bypass is inert unless
    // the driver supplies a real mask.
    let in_text = step(p.mask.x, uv.x) * step(p.mask.y, uv.y)
                * step(uv.x, p.mask.x + p.mask.z)
                * step(uv.y, p.mask.y + p.mask.w)
                * step(0.0000001, p.mask.z * p.mask.w);
    if (in_text > 0.5) { return vec4<f32>(0.0, 0.0, 0.0, 0.0); }

    // Darkening coverage (alpha over black) from the modulation effects.
    var dark = scanline_alpha(uv, p.a.x);
    dark = dark + vignette_alpha(uv, p.a.w);
    dark = dark + glitch_alpha(uv, p.b.z);
    dark = clamp(dark, 0.0, 0.85);

    // Additive tints (premultiplied: contribute color without coverage of
    // their own beyond what the alpha already carries).
    var add = grid_tint(uv, p.b.w);
    add = add + phosphor_tint(p.a.y);

    // Compose: the darkening is alpha over black (rgb contribution = 0 for the
    // dark part since color is black), plus the additive tints on top. We emit
    // premultiplied alpha. Total coverage alpha is the darkening coverage; the
    // additive tints raise rgb above the (black * alpha == 0) floor.
    let a = clamp(dark + max(add.r, max(add.g, add.b)), 0.0, 1.0);
    let rgb = add; // black darkening contributes 0 rgb; only tints add color
    return vec4<f32>(rgb, a);
}
