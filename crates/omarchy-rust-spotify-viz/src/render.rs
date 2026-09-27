//! The GPU side. Two textures take turns: each frame the preset renders
//! into one while reading the other (the previous frame) for feedback, and
//! a final pass copies the result to the window with a soft glow.

use std::sync::Arc;

use anyhow::{Context, Result};
use winit::event_loop::OwnedDisplayHandle;
use winit::window::Window;

/// What every preset can read; laid out as vec4s for a uniform buffer.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct Uniforms {
    pub res: [f32; 4],
    pub clock: [f32; 4],
    pub levels: [f32; 4],
    pub motion: [f32; 4],
    pub spectrum: [[f32; 4]; 12],
    pub wave: [[f32; 4]; 64],
}

struct Target {
    view: wgpu::TextureView,
    size: (u32, u32),
}

pub struct Gpu {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    scene_format: wgpu::TextureFormat,
    layout: wgpu::BindGroupLayout,
    pipeline_layout: wgpu::PipelineLayout,
    sampler: wgpu::Sampler,
    uniforms: wgpu::Buffer,
    fade: wgpu::Buffer,
    blit: wgpu::RenderPipeline,
    scene: Option<wgpu::RenderPipeline>,
    targets: Vec<Target>,
    /// scene_binds[i] reads targets[i]; blit_binds[i] shows targets[i].
    scene_binds: Vec<wgpu::BindGroup>,
    blit_binds: Vec<wgpu::BindGroup>,
    cur: usize,
    /// Rendering resolution relative to the window.
    scale: f32,
    pub backend: String,
}

impl Gpu {
    pub async fn new(window: Arc<Window>, display: OwnedDisplayHandle, scale: f32) -> Result<Self> {
        let instance = wgpu::Instance::new(
            wgpu::InstanceDescriptor::new_with_display_handle(Box::new(display)).with_env(),
        );
        let surface = instance.create_surface(window.clone())?;
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: Some(&surface),
                ..Default::default()
            })
            .await
            .context("no GPU adapter can draw to this window")?;
        let info = adapter.get_info();
        let backend = format!("{} ({:?})", info.name, info.backend);
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("viz"),
                required_limits: adapter.limits(),
                ..Default::default()
            })
            .await?;

        let size = window.inner_size();
        let mut config = surface
            .get_default_config(&adapter, size.width.max(1), size.height.max(1))
            .context("the window's surface isn't supported by this GPU")?;
        // Presets write display colours; an sRGB surface would re-encode them.
        let caps = surface.get_capabilities(&adapter);
        if let Some(f) = caps.formats.iter().find(|f| !f.is_srgb()) {
            config.format = *f;
        }
        config.present_mode = wgpu::PresentMode::Fifo;
        surface.configure(&device, &config);

        // Half floats keep slow feedback fades smooth (8 bits leave smears).
        let scene_format = if adapter
            .get_texture_format_features(wgpu::TextureFormat::Rgba16Float)
            .allowed_usages
            .contains(wgpu::TextureUsages::RENDER_ATTACHMENT)
        {
            wgpu::TextureFormat::Rgba16Float
        } else {
            wgpu::TextureFormat::Rgba8Unorm
        };

        let entry = |binding, ty| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
            ty,
            count: None,
        };
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("viz"),
            entries: &[
                entry(
                    0,
                    wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                ),
                entry(
                    1,
                    wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                ),
                entry(
                    2,
                    wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                ),
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("viz"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let buffer = |label, size| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            })
        };
        let uniforms = buffer("uniforms", std::mem::size_of::<Uniforms>() as u64);
        let fade = buffer("fade", 16);
        let blit = pipeline(
            &device,
            &pipeline_layout,
            include_str!("shaders/blit.wgsl"),
            config.format,
        )
        .map_err(|e| anyhow::anyhow!("blit shader: {e}"))?;

        let mut gpu = Gpu {
            surface,
            device,
            queue,
            config,
            scene_format,
            layout,
            pipeline_layout,
            sampler,
            uniforms,
            fade,
            blit,
            scene: None,
            targets: Vec::new(),
            scene_binds: Vec::new(),
            blit_binds: Vec::new(),
            cur: 0,
            scale,
            backend,
        };
        gpu.make_targets();
        Ok(gpu)
    }

    /// Compiles a preset; on an error the previous one keeps running.
    pub fn set_scene(&mut self, source: &str) -> Result<(), String> {
        let p = pipeline(
            &self.device,
            &self.pipeline_layout,
            source,
            self.scene_format,
        )?;
        self.scene = Some(p);
        self.clear();
        Ok(())
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        if width == 0 || height == 0 {
            return;
        }
        self.config.width = width;
        self.config.height = height;
        self.surface.configure(&self.device, &self.config);
        self.make_targets();
    }

    pub fn scene_size(&self) -> (u32, u32) {
        self.targets.first().map_or((1, 1), |t| t.size)
    }

    fn make_targets(&mut self) {
        let size = (
            ((self.config.width as f32 * self.scale) as u32).max(1),
            ((self.config.height as f32 * self.scale) as u32).max(1),
        );
        self.targets = (0..2)
            .map(|i| {
                let tex = self.device.create_texture(&wgpu::TextureDescriptor {
                    label: Some(if i == 0 { "frame a" } else { "frame b" }),
                    size: wgpu::Extent3d {
                        width: size.0,
                        height: size.1,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: self.scene_format,
                    usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                        | wgpu::TextureUsages::TEXTURE_BINDING,
                    view_formats: &[],
                });
                Target {
                    view: tex.create_view(&Default::default()),
                    size,
                }
            })
            .collect();
        let bind = |buf: &wgpu::Buffer, view: &wgpu::TextureView| {
            self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: None,
                layout: &self.layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: buf.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::Sampler(&self.sampler),
                    },
                ],
            })
        };
        self.scene_binds = self
            .targets
            .iter()
            .map(|t| bind(&self.uniforms, &t.view))
            .collect();
        self.blit_binds = self
            .targets
            .iter()
            .map(|t| bind(&self.fade, &t.view))
            .collect();
        self.clear();
    }

    /// Both frames to black, so a new preset doesn't feed on the last one.
    fn clear(&mut self) {
        let mut enc = self.device.create_command_encoder(&Default::default());
        for t in &self.targets {
            enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("clear"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &t.view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
        }
        self.queue.submit([enc.finish()]);
    }

    /// One frame: the preset into the next texture, reading the current
    /// one; then that to the window, scaled by `fade`.
    pub fn render(&mut self, u: &Uniforms, fade: f32) {
        let frame = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(t)
            | wgpu::CurrentSurfaceTexture::Suboptimal(t) => t,
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                self.surface.configure(&self.device, &self.config);
                return;
            }
            _ => return,
        };
        self.queue
            .write_buffer(&self.uniforms, 0, bytemuck::bytes_of(u));
        self.queue
            .write_buffer(&self.fade, 0, bytemuck::bytes_of(&[fade, 0.0, 0.0, 0.0]));
        let (prev, next) = (self.cur, 1 - self.cur);
        let out = frame.texture.create_view(&Default::default());
        // No working preset yet (its shader failed): show the black frame,
        // so the window still appears and its title can say why.
        let next = if self.scene.is_some() { next } else { prev };
        let mut passes = Vec::new();
        if let Some(scene) = &self.scene {
            passes.push((&self.targets[next].view, scene, &self.scene_binds[prev]));
        }
        passes.push((&out, &self.blit, &self.blit_binds[next]));
        let mut enc = self.device.create_command_encoder(&Default::default());
        for (view, pipe, bind) in passes {
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: None,
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
            pass.set_pipeline(pipe);
            pass.set_bind_group(0, bind, &[]);
            pass.draw(0..3, 0..1);
        }
        self.queue.submit([enc.finish()]);
        self.queue.present(frame);
        self.cur = next;
    }
}

/// A full-screen pipeline from WGSL with `vs_main` and `fs_main`; shader
/// and pipeline errors come back as text instead of aborting.
fn pipeline(
    device: &wgpu::Device,
    layout: &wgpu::PipelineLayout,
    source: &str,
    format: wgpu::TextureFormat,
) -> Result<wgpu::RenderPipeline, String> {
    let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: None,
        source: wgpu::ShaderSource::Wgsl(source.into()),
    });
    let pipe = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: None,
        layout: Some(layout),
        vertex: wgpu::VertexState {
            module: &module,
            entry_point: Some("vs_main"),
            compilation_options: Default::default(),
            buffers: &[],
        },
        primitive: Default::default(),
        depth_stencil: None,
        multisample: Default::default(),
        fragment: Some(wgpu::FragmentState {
            module: &module,
            entry_point: Some("fs_main"),
            compilation_options: Default::default(),
            targets: &[Some(format.into())],
        }),
        multiview_mask: None,
        cache: None,
    });
    match pollster::block_on(scope.pop()) {
        Some(e) => Err(e.to_string()),
        None => Ok(pipe),
    }
}
