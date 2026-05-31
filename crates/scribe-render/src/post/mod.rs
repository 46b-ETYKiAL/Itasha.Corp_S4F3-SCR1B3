//! F-027 — CRT overlay post-pass (wired).
//!
//! A wgpu shader that egui_wgpu's `CallbackTrait::paint` blends OVER the
//! already-painted editor using premultiplied-alpha blending. Effects scale
//! via uniform weights; zero weights collapse to a fully-transparent texel
//! (a byte-exact no-op), so the constant cost is one full-screen triangle of
//! cheap procedural math.
//!
//! **Overlay, not resample.** The pass samples NO framebuffer texture. eframe
//! 0.34 owns the surface render loop and exposes no post-egui hook
//! (`App::post_rendering` was removed in eframe 0.24), and
//! `CallbackTrait::paint` runs INSIDE the egui surface render pass — wgpu
//! forbids binding the active color attachment as a sampled texture. A
//! full-frame post-process that *resamples* the composited frame is therefore
//! architecturally impossible within eframe 0.34. See
//! `docs/audits/crt-post-pass-design.md` for the analysis and the
//! offscreen-RT architecture the resample-class effects (bloom, chromatic
//! aberration, curvature) would require. The overlay form here covers the
//! modulation + additive-tint subset (scanlines, vignette, phosphor glow,
//! grid, glitch) — all of which compose correctly via alpha blending and can
//! never black out the editor.
//!
//! **Off-state is zero cost**: when `enabled == false`, the caller in
//! `scribe-app` does NOT register the callback at all — `prepare` and
//! `paint` are never invoked; no GPU command is emitted; the `PostResources`
//! allocated at init is the only persistent state.
//!
//! **Body-text guarantee**: the shader carries a `mask` uniform; pixels
//! inside the mask rect emit a fully-transparent texel — byte-identical to
//! no-pass. The mask is the editor's `max_rect()` in UV space.

pub mod pipeline;
pub mod uniforms;

pub use pipeline::PostResources;
pub use uniforms::{PostState, PostUniforms};

use egui_wgpu::{CallbackResources, CallbackTrait, ScreenDescriptor};

/// Per-frame callback that drives the CRT overlay pass.
///
/// Constructed each frame inside `scribe-app`'s frame tick and registered via
/// `crt_overlay_shape(rect, state)` only when `state.params.enabled == true`.
/// When the editor turns the effect off, the callback is simply not
/// registered — the path is provably 0-cost.
#[derive(Debug, Clone)]
pub struct CrtPostCallback {
    pub state: PostState,
}

/// Build the egui `Shape` that registers the CRT overlay for `rect`.
///
/// Returns an `egui::Shape::Callback` wrapping a [`CrtPostCallback`]. Add it
/// to any painter (e.g. a foreground-layer painter spanning the window) to
/// have the overlay blended over the editor this frame. The caller must only
/// call this when the effect is enabled — registering the callback IS the
/// "render the overlay" instruction.
pub fn crt_overlay_shape(rect: egui::Rect, state: PostState) -> egui::Shape {
    egui::Shape::Callback(egui_wgpu::Callback::new_paint_callback(
        rect,
        CrtPostCallback { state },
    ))
}

impl CallbackTrait for CrtPostCallback {
    /// Upload uniforms to the GPU. We do NOT need to emit our own command
    /// buffer — `Queue::write_buffer` is enough.
    fn prepare(
        &self,
        _device: &wgpu::Device,
        queue: &wgpu::Queue,
        _screen_descriptor: &ScreenDescriptor,
        _egui_encoder: &mut wgpu::CommandEncoder,
        callback_resources: &mut CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        // Sanity-defense: if init failed and PostResources isn't in the
        // type-map, do nothing rather than panic. The callback registration
        // is the operator's "consent" to the effect; we silently skip if
        // the init contract was breached.
        let Some(res) = callback_resources.get::<PostResources>() else {
            return Vec::new();
        };
        queue.write_buffer(
            &res.uniform_buf,
            0,
            bytemuck::bytes_of(&self.state.to_uniforms()),
        );
        Vec::new()
    }

    /// Paint the full-screen triangle into the egui pass. The pipeline writes
    /// premultiplied-alpha CRT artifacts that blend OVER the editor content
    /// already in the color attachment. It binds no source texture, so it
    /// cannot black out the editor — a transparent texel is a no-op.
    fn paint(
        &self,
        _info: egui::PaintCallbackInfo,
        render_pass: &mut wgpu::RenderPass<'static>,
        callback_resources: &CallbackResources,
    ) {
        let Some(res) = callback_resources.get::<PostResources>() else {
            return;
        };
        render_pass.set_pipeline(&res.pipeline);
        render_pass.set_bind_group(0, &res.bind_group, &[]);
        render_pass.draw(0..3, 0..1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CrtParams;

    #[test]
    fn state_default_is_disabled_and_zero_uniforms() {
        let s = PostState::default();
        assert!(!s.params.enabled);
        let u = s.to_uniforms();
        assert_eq!(u.a, [0.0; 4]);
        assert_eq!(u.c[1], 0.0); // time_sec = 0
    }

    #[test]
    fn state_carries_params() {
        let s = PostState {
            params: CrtParams {
                enabled: true,
                scanline: 0.3,
                phosphor_glow: 0.2,
                bloom: 0.15,
                vignette: 0.25,
                curvature: 0.0,
                chromatic_aberration: 0.05,
            },
            ..Default::default()
        };
        let u = s.to_uniforms();
        assert_eq!(u.a[0], 0.3);
        assert_eq!(u.a[1], 0.2);
        assert_eq!(u.a[2], 0.15);
        assert_eq!(u.a[3], 0.25);
        assert_eq!(u.b[1], 0.05);
    }

    #[test]
    fn overlay_shape_is_a_paint_callback() {
        // The overlay is registered via an egui Callback shape; this asserts
        // the helper produces exactly that variant (and not, say, a noop/mesh)
        // so the wiring path can't silently regress to a non-rendering shape.
        let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(800.0, 600.0));
        let shape = crt_overlay_shape(rect, PostState::default());
        assert!(
            matches!(shape, egui::Shape::Callback(_)),
            "crt_overlay_shape must produce a Shape::Callback"
        );
    }

    #[test]
    fn overlay_time_sec_maps_to_uniform_c1() {
        // The glitch term reads time_sec from uniform slot c[1]; verify the
        // driver-side time is carried through so the animated band is live.
        let s = PostState {
            time_sec: 12.5,
            ..Default::default()
        };
        assert_eq!(s.to_uniforms().c[1], 12.5);
    }

    #[test]
    fn overlay_screen_size_maps_to_uniform_c23() {
        // Scanline/grid frequency depends on screen_w/screen_h (c[2]/c[3]);
        // a zeroed screen would collapse the procedural lattice, so verify the
        // driver-supplied framebuffer size is carried through.
        let s = PostState {
            screen: [1920.0, 1080.0],
            ..Default::default()
        };
        let u = s.to_uniforms();
        assert_eq!(u.c[2], 1920.0);
        assert_eq!(u.c[3], 1080.0);
    }
}
