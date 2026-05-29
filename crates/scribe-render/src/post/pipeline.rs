//! Phase 17 T17.4 — wgpu pipeline + resources for the CRT post-pass.
//!
//! Owns the long-lived GPU state:
//! - The compiled `RenderPipeline` (one shader, one set of fragment uniforms)
//! - The 64-byte uniform `Buffer` (per-frame `write_buffer` overwrites)
//! - The bind group layout shared by every per-frame bind group
//! - The persistence-history texture (1×1 zero-sentinel when persistence OFF;
//!   resized to match the framebuffer when persistence ON)
//! - The sampler (Linear+ClampToEdge)
//!
//! The actual off-screen RT textures live next to egui's view because they
//! must track the framebuffer size — they're held by `CrtPostCallback` and
//! re-created on resize.

use wgpu::util::DeviceExt;

use super::uniforms::PostUniforms;

/// Long-lived GPU state. Constructed once via `PostResources::new()` at app
/// init (inside `eframe::App::new()`) and stored in `egui_wgpu::Renderer::
/// callback_resources`.
pub struct PostResources {
    pub pipeline: wgpu::RenderPipeline,
    pub bind_group_layout: wgpu::BindGroupLayout,
    pub uniform_buf: wgpu::Buffer,
    pub sampler: wgpu::Sampler,
    /// 1×1 zero-sentinel texture used when persistence is OFF. Real history
    /// textures live on the callback if the operator enables persistence.
    pub history_sentinel: wgpu::TextureView,
    /// Pre-built bind group using the sentinel for both src + history. v1
    /// passthrough wiring — the actual offscreen-RT plumbing is a follow-up
    /// (the `paint` callback receives the egui RenderPass already bound to
    /// the surface, so reading from it is forbidden by wgpu). Using a
    /// sentinel here keeps the pipeline valid + tested while the texture-
    /// copy plumbing is wired.
    pub passthrough_bind_group: wgpu::BindGroup,
}

impl PostResources {
    /// Initialise the long-lived GPU state. Call this once at app startup
    /// (inside `eframe::App::new(cc)` with the `RenderState` from
    /// `cc.wgpu_render_state`).
    ///
    /// `target_format` is the surface format the egui pass writes to; the
    /// post-pass writes to a matching format.
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        target_format: wgpu::TextureFormat,
    ) -> Self {
        // Compile the WGSL shader. Failure should never panic in a release
        // build (cosmetic effect); the caller is expected to handle a panic
        // here by simply not registering the callback. wgpu emits the error
        // diagnostic through its internal logger.
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("scr1b3-crt-post-shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/crt_post.wgsl").into()),
        });

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("scr1b3-crt-post-bgl"),
            entries: &[
                // Uniform buffer
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
                // Source texture
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                // Sampler
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                // History texture (for persistence). 1×1 sentinel when OFF.
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
            ],
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("scr1b3-crt-post-pl"),
            bind_group_layouts: &[Some(&bind_group_layout)],
            immediate_size: 0,
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("scr1b3-crt-post-pipeline"),
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
                    blend: Some(wgpu::BlendState::REPLACE),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            multiview_mask: None,
            cache: None,
        });

        let uniform_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("scr1b3-crt-post-uniforms"),
            contents: bytemuck::bytes_of(&PostUniforms::default()),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("scr1b3-crt-post-sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            ..Default::default()
        });

        // 1×1 zero-sentinel for the history binding when persistence is OFF.
        let history_tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("scr1b3-crt-post-history-sentinel"),
            size: wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: target_format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        // Zero-init the sentinel pixel (4 bytes for the common 8-bit sRGB/UNORM formats).
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &history_tex,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &[0u8, 0, 0, 0],
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(4),
                rows_per_image: Some(1),
            },
            wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
        );
        let history_sentinel = history_tex.create_view(&wgpu::TextureViewDescriptor::default());

        // Pre-build the passthrough bind group. v1 wires src + history to
        // the sentinel; future offscreen-RT plumbing replaces src on resize
        // by rebuilding the bind group when needed.
        let passthrough_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("scr1b3-crt-post-bg-passthrough"),
            layout: &bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: uniform_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&history_sentinel),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(&history_sentinel),
                },
            ],
        });

        PostResources {
            pipeline,
            bind_group_layout,
            uniform_buf,
            sampler,
            history_sentinel,
            passthrough_bind_group,
        }
    }
}
