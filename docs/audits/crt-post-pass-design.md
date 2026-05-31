# CRT Post-Pass (F-027) — Architecture, eframe 0.34 Limitation, and the Offscreen-RT Plan

Status: **shipped (overlay subset) + design (resample subset)**
Date: 2026-05-30
Scope: `crates/scribe-render/src/post/**`, `crates/scribe-app/src/app.rs` CRT wiring

## 1. What F-027 shipped

The CRT effect now renders through a **wgpu overlay pass** wired into the
egui_wgpu callback system, gated on `config.effects.crt_enabled`:

- `crates/scribe-render/src/post/pipeline.rs` — `PostResources`: a compiled
  render pipeline (premultiplied-alpha blend), a 64-byte uniform buffer, and a
  uniform-only bind group. No source/history texture is bound.
- `crates/scribe-render/src/post/shaders/crt_post.wgsl` — a procedural overlay
  shader that emits **premultiplied-alpha** CRT artifacts.
- `crates/scribe-render/src/post/mod.rs` — `CrtPostCallback` (the
  `egui_wgpu::CallbackTrait` impl) + `crt_overlay_shape(rect, state)` helper.
- `crates/scribe-app/src/app.rs` — registers the callback on a top-most
  foreground-layer painter when `crt_enabled` AND the wgpu backend is live
  (`wgpu_post_available`). On the glow backend (or wgpu-init failure) it falls
  back to the pre-existing egui-painter overlay (`paint_crt_overlay`). Exactly
  one path runs, so the effect never doubles up.

The overlay covers the **modulation + additive-tint** subset of CRT effects:

| Effect | How it works in the overlay | Config slider |
|---|---|---|
| Scanlines | Per-row alpha darkening (capped 0.35×w so text stays readable) | `scanline` |
| Vignette | Radial alpha darkening toward corners | `vignette` |
| Phosphor glow | Faint green-blue additive wash | `phosphor_glow` |
| Dot grid | Sparse additive phosphor-cell lattice | (grid weight via `b.w`) |
| Glitch tear | Event-frame per-row darken bands | (glitch weight via `b.z`) |

Because the pass binds **no source texture**, a fully-transparent overlay texel
is a byte-exact no-op — the effect **cannot black out the editor**. This is the
load-bearing safety property that the prior sentinel-texture passthrough wiring
(which would have sampled a 1×1 black texture and blacked out the screen) did
not have.

## 2. The eframe 0.34 limitation (why a full-frame *resample* is impossible)

A "true" CRT post-process resamples the **already-composited frame** —
curvature warps the sampled UVs, bloom gathers neighboring texels, chromatic
aberration samples R/G/B at offset UVs. All three require reading the rendered
editor as an input texture. Within eframe 0.34.3 this is **not achievable**:

1. **No post-egui surface hook.** `eframe::App::post_rendering` — the only hook
   that ran after egui composited to the surface — was **removed in eframe
   0.24** (CHANGELOG: *"App::post_rendering is gone. Screenshots are taken with
   `ctx.send_viewport_cmd(ViewportCommand::Screenshot)` and are returned in
   `egui::Event`"*). The `App` trait in 0.34.3 exposes `ui`, `save`, `on_exit`,
   `clear_color`, `raw_input_hook`, … and **no** frame-post hook.

2. **`CallbackTrait::paint` runs inside the egui surface render pass.** Its
   signature is `paint(&self, info, render_pass: &mut RenderPass<'static>,
   resources)`. The `render_pass` is egui's own pass, already bound to the
   surface color attachment. **wgpu forbids binding the active color attachment
   as a sampled texture** — so a callback cannot read the pixels egui is writing
   to the surface this frame.

3. **`CallbackTrait::prepare` gets the egui encoder, not the editor scene.** It
   can open its own render passes into offscreen textures — but the editor is
   not a self-contained wgpu scene it could render there. The editor is a tree
   of egui widgets (`ScrollArea` + `ui.label`) that egui composites onto the
   surface during egui's own pass. There is no "editor render function" to
   redirect into a callback-owned RT.

4. **The screenshot path is async + read-only.** `ViewportCommand::Screenshot`
   returns the framebuffer one or more frames later via an event, and there is
   no API to write the post-processed result back to the surface. It is
   unusable as a per-frame post-process.

**Conclusion:** within eframe 0.34, the resample-class effects (curvature,
bloom, chromatic aberration, phosphor persistence) cannot sample the real
composited editor. They are intentionally **out of scope for the overlay**
shipped in §1. Shipping them as no-ops would be a broken effect; shipping them
against a black sentinel would black out the editor. Neither is acceptable per
the F-027 hard constraint, so the overlay ships the safe subset and this doc
specifies the architecture the rest requires.

## 3. The offscreen-RT architecture (required for the resample subset)

To get curvature / bloom / chromatic aberration, SCR1B3 must own the surface
render loop so the editor is rendered into an **offscreen render target** that a
second pass then resamples through the CRT shader. This means **replacing eframe
with a custom winit + wgpu + egui_wgpu loop**:

```
winit event loop
  └─ per frame:
       1. egui_ctx.run(raw_input, |ctx| frame_tick(ctx))   // existing UI code
       2. let offscreen = Texture(RENDER_ATTACHMENT | TEXTURE_BINDING)   // RGBA, surface-sized
       3. egui_wgpu::Renderer::render(&mut pass_into(offscreen), tessellated, ...)
       4. crt_pass: RenderPass(into = surface_view) {
            bind offscreen as src_tex (sampled)
            draw full-screen triangle through crt_post_resample.wgsl   // curvature/bloom/chroma here
          }
       5. queue.submit(); surface.present();
```

Step 2's offscreen RT is the input that step 4 resamples — the read-write hazard
of §2.2 disappears because the surface pass (step 4) reads `offscreen` (a
*different* texture) and writes the surface.

### Work breakdown (estimate)

| # | Task | Risk |
|---|---|---|
| 1 | Replace `eframe::run_native` with a `winit::EventLoop` + `wgpu::Surface` owned by `scribe-app`. Port window creation, DPI, theme, frameless decorations, OS glass effect, and viewport-command handling that eframe currently does for free. | **High** — eframe abstracts a large surface; every viewport command (resize, minimize, drag, screenshot, IME, accesskit) must be re-driven manually. |
| 2 | Drive `egui_winit::State` for input translation + `egui::Context::run`. | Medium |
| 3 | Allocate + resize the offscreen RT on surface-config change; rebuild the CRT bind group on resize. | Low |
| 4 | Add `crt_post_resample.wgsl` (the original resample shader, restored) + a `PostResourcesResample` with src-texture + sampler + history bindings. | Low |
| 5 | Wire phosphor persistence (ping-pong history texture across frames). | Medium |
| 6 | Re-validate accesskit, IME, clipboard, and the egui_kittest headless test harness against the custom loop. | **High** — the test harness drives `Context::run` directly today; a custom loop must keep that path hermetic. |

### Why it was NOT done in F-027

Replacing eframe's render loop is a window-system rewrite that touches every OS
integration (frameless chrome, glass/acrylic/mica, two-phase close, resize
overlays, IME, accesskit, screenshot CLI). It is far larger and riskier than the
F-027 scope and would regress the safe, working overlay if landed under time
pressure. The overlay subset delivers a real, visible, non-blacking-out CRT
effect today; the resample subset is specified here for a dedicated future
initiative that can carry the winit-loop migration with its own test budget.

## 4. Follow-ups (not deferrals — specified work for a future initiative)

1. **Body-text mask.** The shader already honors a `mask` UV rect (emit
   transparent inside it). The overlay currently passes a zero mask (inert). A
   follow-up can pass the editor `max_rect()` in UV so the overlay never touches
   the text region even with the capped darkening.
2. **Resample effects via the §3 architecture** (curvature, bloom, chromatic
   aberration, phosphor persistence) — gated on the winit-loop migration.
3. **Reduced-motion runtime detection.** Both overlay paths currently pass
   `reduced_motion = false` (matching the prior behavior). A follow-up can query
   the OS preference and zero the animated terms.

## 5. References

- eframe CHANGELOG — `App::post_rendering` removed in 0.24.0.
- `egui_wgpu::CallbackTrait` docs — `prepare`/`paint` execution context.
- `egui_wgpu::Callback::new_paint_callback` — the shape-construction API used by
  `crt_overlay_shape`.
- wgpu validation — a texture bound as a render-pass color attachment cannot be
  simultaneously bound as a sampled texture in the same pass.
