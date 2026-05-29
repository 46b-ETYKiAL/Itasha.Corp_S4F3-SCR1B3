//! Phase 17 T17.4 — CRT post-pass uniform layout.
//!
//! 64 bytes, std140-safe by construction (4 × vec4<f32> blocks). The
//! `PostUniforms::SIZE` constant is asserted equal to `size_of::<Self>()`
//! at build time via `static_assertions` so a future drift breaks the
//! build rather than silently mismatching the WGSL `Params` struct.
//!
//! Schema (Rust ↔ WGSL):
//!
//! | Field | WGSL location | Semantics |
//! |---|---|---|
//! | `a[0..4]` | `p.a` | scanline / phosphor_glow / bloom / vignette |
//! | `b[0..4]` | `p.b` | curvature / chromatic_aberration / glitch / grid |
//! | `c[0..4]` | `p.c` | persistence_decay / time_sec / screen_w / screen_h |
//! | `mask[0..4]` | `p.mask` | body-text rect in UV (x, y, w, h) |

use bytemuck::{Pod, Zeroable};

use crate::CrtParams;

/// GPU-side uniform buffer payload. `#[repr(C)]` + `Pod` makes the layout
/// stable across compilers; the `static_assertions` macro asserts the
/// total size matches the WGSL `Params` struct at build time.
#[repr(C)]
#[derive(Copy, Clone, Debug, Default, Pod, Zeroable)]
pub struct PostUniforms {
    pub a: [f32; 4],
    pub b: [f32; 4],
    pub c: [f32; 4],
    pub mask: [f32; 4],
}

impl PostUniforms {
    pub const SIZE: usize = 64;
}

static_assertions::const_assert_eq!(std::mem::size_of::<PostUniforms>(), PostUniforms::SIZE);

/// Driver-side state the renderer composes from the editor's per-frame view.
/// Per-frame inputs:
/// - `params`: the user-config-derived effect weights (CRT enable + every
///   per-effect intensity), already gated by reduced-motion + battery.
/// - `time_sec`: monotonic time since start; the WGSL glitch + persistence
///   terms read it.
/// - `screen`: framebuffer pixel size, used by every per-texel computation.
/// - `mask_uv`: the editor `max_rect()` in UV — pixels inside it bypass the
///   post-pass. This is the structural body-text guarantee.
/// - `glitch_frames_left`: simple one-frame-or-N-frames trigger; ticked
///   down by the driver each frame the post-pass renders.
/// - `grid_intensity` / `persistence_decay`: extra per-feature weights
///   that come from `EffectsConfig` extensions.
#[derive(Debug, Default, Clone, Copy)]
pub struct PostState {
    pub params: CrtParams,
    pub time_sec: f32,
    pub screen: [f32; 2],
    pub mask_uv: [f32; 4],
    pub glitch_frames_left: u8,
    pub grid_intensity: f32,
    pub persistence_decay: f32,
}

impl PostState {
    /// Project the driver-side state into the GPU `PostUniforms` payload.
    /// The mapping is direct — every field is a copy or a derived constant.
    pub fn to_uniforms(&self) -> PostUniforms {
        let glitch = if self.glitch_frames_left > 0 {
            1.0
        } else {
            0.0
        };
        PostUniforms {
            a: [
                self.params.scanline,
                self.params.phosphor_glow,
                self.params.bloom,
                self.params.vignette,
            ],
            b: [
                self.params.curvature,
                self.params.chromatic_aberration,
                glitch,
                self.grid_intensity,
            ],
            c: [
                self.persistence_decay,
                self.time_sec,
                self.screen[0],
                self.screen[1],
            ],
            mask: self.mask_uv,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uniforms_are_64_bytes() {
        assert_eq!(std::mem::size_of::<PostUniforms>(), 64);
        assert_eq!(std::mem::align_of::<PostUniforms>(), 4);
    }

    #[test]
    fn to_uniforms_default_is_zero() {
        let u = PostState::default().to_uniforms();
        assert_eq!(u.a, [0.0; 4]);
        assert_eq!(u.b, [0.0; 4]);
        assert_eq!(u.c, [0.0; 4]);
        assert_eq!(u.mask, [0.0; 4]);
    }

    #[test]
    fn to_uniforms_maps_scanline_to_a0() {
        let s = PostState {
            params: crate::CrtParams {
                scanline: 0.5,
                bloom: 0.25,
                ..Default::default()
            },
            ..Default::default()
        };
        let u = s.to_uniforms();
        assert_eq!(u.a[0], 0.5);
        assert_eq!(u.a[2], 0.25);
    }

    #[test]
    fn glitch_is_one_when_frames_left_nonzero() {
        let s_on = PostState {
            glitch_frames_left: 3,
            ..Default::default()
        };
        assert_eq!(s_on.to_uniforms().b[2], 1.0);
        let s_off = PostState::default();
        assert_eq!(s_off.to_uniforms().b[2], 0.0);
    }

    #[test]
    fn mask_uv_passes_through() {
        let s = PostState {
            mask_uv: [0.1, 0.2, 0.5, 0.6],
            ..Default::default()
        };
        assert_eq!(s.to_uniforms().mask, [0.1, 0.2, 0.5, 0.6]);
    }
}
