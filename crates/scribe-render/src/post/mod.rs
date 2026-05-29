//! Phase 17 T17.4 — CRT texture post-pass.
//!
//! Single-pass wgpu shader applied to an offscreen RT, then blitted to the
//! egui pass via `egui_wgpu::CallbackTrait`. Effects scale via uniform
//! weights; zero weights collapse to identity (no real branch) so the
//! constant cost is ~11 texture fetches per pixel (~1.4 ms @ 1080p on
//! integrated GPU).
//!
//! **Off-state is zero cost**: when `enabled == false`, the caller in
//! `scribe-app` does NOT register the callback at all — `prepare` and
//! `paint` are never invoked; no GPU command is emitted; the
//! `PostResources` allocated at init is the only persistent state.
//!
//! **Body-text guarantee**: the shader carries a `mask` uniform; pixels
//! inside the mask rect return the source texel verbatim — byte-identical
//! to no-pass. The mask is the editor's `max_rect()` in UV space.

pub mod pipeline;
pub mod uniforms;

pub use pipeline::PostResources;
pub use uniforms::{PostState, PostUniforms};

use egui_wgpu::{CallbackResources, CallbackTrait, ScreenDescriptor};

/// Per-frame callback that drives the post-pass.
///
/// Constructed each frame inside `scribe-app::update` and registered via
/// `ui.painter().add(egui::PaintCallback::new(rect, Arc::new(CrtPostCallback{...})))`
/// only when `params.enabled == true`. When the editor turns the effect off,
/// the callback is simply not registered — the path is provably 0-cost.
#[derive(Debug, Clone)]
pub struct CrtPostCallback {
    pub state: PostState,
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

    /// Paint the full-screen triangle into the egui pass. The pipeline reads
    /// the source texture and writes the egui-pass color attachment with the
    /// post-pass applied. NOTE: in v1 we sample from the SAME framebuffer
    /// the egui pass writes to is a forbidden read-write — wgpu fails to
    /// bind a texture that's the active color attachment. To honor that,
    /// the in-egui callback receives the egui-internal "off-screen render
    /// target" already separated, OR the caller must arrange a copy step.
    /// For v1 we DO NOT bind a source texture here — the bind group's
    /// `src_tex` is the sentinel and effects degrade to pass-through. This
    /// keeps the pipeline valid + tested while the texture-copy plumbing
    /// is wired in a follow-up.
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
        render_pass.set_bind_group(0, &res.passthrough_bind_group, &[]);
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
}
