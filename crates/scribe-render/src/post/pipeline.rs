//! F-027 — wgpu pipeline + resources for the CRT overlay post-pass.
//!
//! Owns the long-lived GPU state:
//! - The compiled `RenderPipeline` (one shader, premultiplied-alpha blend)
//! - The 64-byte uniform `Buffer` (per-frame `write_buffer` overwrites)
//! - The bind group layout (uniform-only — the overlay samples no texture)
//! - The single bind group binding the uniform buffer
//!
//! ## Why uniform-only (no source texture)
//!
//! The CRT pass is an *overlay*, not a resample. eframe 0.34 exposes no
//! post-egui surface hook (`App::post_rendering` was removed in eframe 0.24),
//! and `egui_wgpu::CallbackTrait::paint` runs INSIDE the egui surface render
//! pass — wgpu forbids binding the active color attachment as a sampled
//! texture. A full-frame post-process that *resamples* the composited frame
//! is therefore architecturally impossible within eframe 0.34. See
//! `docs/audits/crt-post-pass-design.md` for the full analysis and the
//! offscreen-RT architecture the resample-class effects (bloom, chromatic
//! aberration, curvature) would require.
//!
//! The overlay form below blends procedural CRT artifacts (scanlines,
//! vignette, phosphor tint, grid, glitch) OVER the editor using
//! premultiplied-alpha blending. It binds no source texture, so it can
//! never black out the editor — a fully-transparent overlay texel is a
//! byte-exact no-op.

use wgpu::util::DeviceExt;

use super::uniforms::PostUniforms;

/// Long-lived GPU state. Constructed once via `PostResources::new()` at app
/// init (inside `eframe::App::new()`) and stored in `egui_wgpu::Renderer::
/// callback_resources`.
pub struct PostResources {
    pub pipeline: wgpu::RenderPipeline,
    pub bind_group_layout: wgpu::BindGroupLayout,
    pub uniform_buf: wgpu::Buffer,
    /// Uniform-only bind group. The overlay samples no texture, so this is
    /// the single bind group bound at paint time — no per-resize rebuild.
    pub bind_group: wgpu::BindGroup,
}

impl PostResources {
    /// Initialise the long-lived GPU state. Call this once at app startup
    /// (inside `eframe::App::new(cc)` with the `RenderState` from
    /// `cc.wgpu_render_state`).
    ///
    /// `target_format` is the surface format the egui pass writes to; the
    /// overlay writes to a matching format with premultiplied-alpha blend.
    pub fn new(device: &wgpu::Device, target_format: wgpu::TextureFormat) -> Self {
        // Compile the WGSL shader. Failure should never panic in a release
        // build (cosmetic effect); the caller is expected to handle a panic
        // here by simply not registering the callback. wgpu emits the error
        // diagnostic through its internal logger.
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("scr1b3-crt-overlay-shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/crt_post.wgsl").into()),
        });

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("scr1b3-crt-overlay-bgl"),
            entries: &[
                // Uniform buffer — the only binding the overlay needs.
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: wgpu::BufferSize::new(PostUniforms::SIZE as u64),
                    },
                    count: None,
                },
            ],
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("scr1b3-crt-overlay-pl"),
            bind_group_layouts: &[Some(&bind_group_layout)],
            immediate_size: 0,
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("scr1b3-crt-overlay-pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: Default::default(),
            },
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                strip_index_format: None,
                front_face: wgpu::FrontFace::Ccw,
                cull_mode: None,
                polygon_mode: wgpu::PolygonMode::Fill,
                unclipped_depth: false,
                conservative: false,
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: target_format,
                    // Premultiplied-alpha blend: the shader emits premultiplied
                    // texels, so src=ONE / dst=ONE_MINUS_SRC_ALPHA composes the
                    // overlay over the editor. A transparent texel is a no-op.
                    blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            multiview_mask: None,
            cache: None,
        });

        let uniform_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("scr1b3-crt-overlay-uniforms"),
            contents: bytemuck::bytes_of(&PostUniforms::default()),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("scr1b3-crt-overlay-bg"),
            layout: &bind_group_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: uniform_buf.as_entire_binding(),
            }],
        });

        PostResources {
            pipeline,
            bind_group_layout,
            uniform_buf,
            bind_group,
        }
    }
}
