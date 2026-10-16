//! Window-agnostic renderer. Draws each frame into an offscreen texture at the
//! output resolution; the preview blits that texture to a window, the headless
//! path reads it back. Both therefore see exactly the same pixels.

mod blit;

pub use blit::Blit;

use std::sync::mpsc;

use anyhow::{Context, Result};
use bytemuck::{Pod, Zeroable};
use image::RgbaImage;
use wgpu::util::DeviceExt;

use crate::look::Disturbance;

/// Format of the offscreen frame. sRGB, so photo colours pass through
/// unchanged while shader maths happens in linear space.
pub const TARGET_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

const SCENE_SHADER: &str = concat!(
    include_str!("../../shaders/fullscreen.wgsl"),
    include_str!("../../shaders/common.wgsl"),
    include_str!("../../shaders/photo.wgsl"),
    include_str!("../../shaders/snow.wgsl"),
    include_str!("../../shaders/scene.wgsl"),
);

pub struct Gpu {
    // Held so the instance lives as long as the device and surfaces.
    _instance: wgpu::Instance,
    pub adapter: wgpu::Adapter,
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
}

impl Gpu {
    pub fn instance() -> wgpu::Instance {
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env())
    }

    /// Pass the window surface when previewing, `None` when headless.
    pub fn new(instance: wgpu::Instance, surface: Option<&wgpu::Surface<'_>>) -> Result<Self> {
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            force_fallback_adapter: false,
            compatible_surface: surface,
            ..Default::default()
        }))
        .context("no suitable GPU adapter")?;
        let (device, queue) = pollster::block_on(
            adapter.request_device(&wgpu::DeviceDescriptor { label: Some("static"), ..Default::default() }),
        )
        .context("creating GPU device")?;
        Ok(Self { _instance: instance, adapter, device, queue })
    }
}

/// Mirrors `Globals` in shaders/common.wgsl.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Globals {
    resolution: [f32; 2],
    frame: u32,
    fps: f32,
    snow: f32,
    jitter: f32,
    snow_rows: f32,
    seed: u32,
}

/// Fixed for a whole render.
#[derive(Clone, Copy, Debug)]
pub struct RenderSettings {
    pub fps: f32,
    pub seed: u32,
    /// Rows of the virtual snow grid (540 = 4×4-pixel snow at 4K).
    pub snow_rows: f32,
}

/// What changes from frame to frame.
#[derive(Clone, Copy, Debug, Default)]
pub struct FrameInputs {
    pub index: u32,
    pub disturbance: Disturbance,
}

pub struct Renderer {
    width: u32,
    height: u32,
    settings: RenderSettings,
    pipeline: wgpu::RenderPipeline,
    bind_group: wgpu::BindGroup,
    globals: wgpu::Buffer,
    target: wgpu::Texture,
    target_view: wgpu::TextureView,
}

impl Renderer {
    /// The photo must already be framed to the output resolution; its size
    /// sets the size of every frame.
    pub fn new(gpu: &Gpu, photo: &RgbaImage, settings: RenderSettings) -> Self {
        let device = &gpu.device;
        let (width, height) = photo.dimensions();
        let size = wgpu::Extent3d { width, height, depth_or_array_layers: 1 };

        let photo_tex = device.create_texture_with_data(
            &gpu.queue,
            &wgpu::TextureDescriptor {
                label: Some("photo"),
                size,
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8UnormSrgb,
                usage: wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            },
            wgpu::util::TextureDataOrder::LayerMajor,
            photo.as_raw(),
        );
        let target = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("frame"),
            size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: TARGET_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let target_view = target.create_view(&Default::default());

        let globals = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("globals"),
            size: std::mem::size_of::<Globals>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("photo"),
            // Jitter samples slightly past the edges; mirroring hides that
            // better than smearing the edge pixels.
            address_mode_u: wgpu::AddressMode::MirrorRepeat,
            address_mode_v: wgpu::AddressMode::MirrorRepeat,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("scene"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                texture_entry(1),
                sampler_entry(2),
            ],
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("scene"),
            layout: &layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: globals.as_entire_binding() },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&photo_tex.create_view(&Default::default())),
                },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(&sampler) },
            ],
        });
        let pipeline = fullscreen_pipeline(device, "scene", SCENE_SHADER, &layout, TARGET_FORMAT);

        Self { width, height, settings, pipeline, bind_group, globals, target, target_view }
    }

    /// Settings are read every frame, so the preview can change them live.
    pub fn set_settings(&mut self, settings: RenderSettings) {
        self.settings = settings;
    }

    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// The finished frame, for the preview to display.
    pub fn target_view(&self) -> &wgpu::TextureView {
        &self.target_view
    }

    /// Records one frame into the offscreen target.
    pub fn encode(&self, gpu: &Gpu, encoder: &mut wgpu::CommandEncoder, frame: &FrameInputs) {
        let globals = Globals {
            resolution: [self.width as f32, self.height as f32],
            frame: frame.index,
            fps: self.settings.fps,
            snow: frame.disturbance.snow,
            jitter: frame.disturbance.jitter,
            snow_rows: self.settings.snow_rows,
            seed: self.settings.seed,
        };
        gpu.queue.write_buffer(&self.globals, 0, bytemuck::bytes_of(&globals));

        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("scene"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &self.target_view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::BLACK), store: wgpu::StoreOp::Store },
            })],
            ..Default::default()
        });
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.bind_group, &[]);
        pass.draw(0..3, 0..1);
    }

    pub fn render(&self, gpu: &Gpu, frame: &FrameInputs) {
        let mut encoder = gpu.device.create_command_encoder(&Default::default());
        self.encode(gpu, &mut encoder, frame);
        gpu.queue.submit([encoder.finish()]);
    }

    /// Copies the current frame back to the CPU.
    pub fn read_frame(&self, gpu: &Gpu) -> Result<RgbaImage> {
        let row_bytes = self.width * 4;
        let padded_row = row_bytes.div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("readback"),
            size: padded_row as u64 * self.height as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });

        let mut encoder = gpu.device.create_command_encoder(&Default::default());
        encoder.copy_texture_to_buffer(
            self.target.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded_row),
                    rows_per_image: Some(self.height),
                },
            },
            wgpu::Extent3d { width: self.width, height: self.height, depth_or_array_layers: 1 },
        );
        let submission = gpu.queue.submit([encoder.finish()]);

        let slice = buffer.slice(..);
        let (tx, rx) = mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |result| {
            let _ = tx.send(result);
        });
        gpu.device
            .poll(wgpu::PollType::Wait { submission_index: Some(submission), timeout: None })
            .context("waiting for GPU")?;
        rx.recv()?.context("mapping readback buffer")?;

        let mut pixels = Vec::with_capacity((row_bytes * self.height) as usize);
        {
            let data = slice.get_mapped_range().context("reading readback buffer")?;
            for row in data.chunks_exact(padded_row as usize) {
                pixels.extend_from_slice(&row[..row_bytes as usize]);
            }
        }
        buffer.unmap();
        RgbaImage::from_raw(self.width, self.height, pixels).context("readback size mismatch")
    }
}

fn texture_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: true },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    }
}

fn sampler_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
        count: None,
    }
}

/// A pipeline that draws `vs_main` (fullscreen triangle) + `fs_main`.
fn fullscreen_pipeline(
    device: &wgpu::Device,
    label: &str,
    source: &str,
    layout: &wgpu::BindGroupLayout,
    format: wgpu::TextureFormat,
) -> wgpu::RenderPipeline {
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some(label),
        source: wgpu::ShaderSource::Wgsl(source.into()),
    });
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some(label),
        bind_group_layouts: &[Some(layout)],
        immediate_size: 0,
    });
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(label),
        layout: Some(&pipeline_layout),
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
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn noisy(index: u32) -> FrameInputs {
        FrameInputs { index, disturbance: Disturbance { snow: 0.8, jitter: 0.6 } }
    }

    #[test]
    fn frames_are_deterministic_and_rest_is_the_photo() {
        let Ok(gpu) = Gpu::new(Gpu::instance(), None) else {
            eprintln!("no GPU adapter; skipping");
            return;
        };
        let photo = crate::assets::test_card(320, 180);
        let renderer = Renderer::new(&gpu, &photo, RenderSettings { fps: 25.0, seed: 7, snow_rows: 90.0 });
        let render = |inputs: FrameInputs| {
            renderer.render(&gpu, &inputs);
            renderer.read_frame(&gpu).unwrap()
        };

        let a = render(noisy(10));
        assert_eq!(a, render(noisy(10)), "same frame must render identically");
        assert_ne!(a, render(noisy(11)), "noise must change between frames");

        let rest = render(FrameInputs { index: 10, ..Default::default() });
        let max_diff = rest.as_raw().iter().zip(photo.as_raw()).map(|(x, y)| x.abs_diff(*y)).max().unwrap();
        assert!(max_diff <= 1, "at rest the frame should be the photo (max diff {max_diff})");
    }
}
