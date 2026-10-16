//! Window-agnostic renderer. Draws each frame into an offscreen texture at the
//! output resolution; the preview blits that texture to a window, the headless
//! path reads it back. Both therefore see exactly the same pixels.
//!
//! Three passes per frame: the scene (photo, tears, jitter, snow) into a float
//! texture, an 8× downsample of its bright parts for bloom, then the CRT
//! treatment into the final sRGB target.

mod blit;

pub use blit::Blit;

use std::sync::mpsc;

use anyhow::{Context, Result};
use bytemuck::{Pod, Zeroable};
use image::RgbaImage;
use wgpu::util::DeviceExt;

use crate::look::Disturbance;
use crate::tears::{MAX_EVENTS, Tears};

/// Format of the offscreen frame. sRGB, so photo colours pass through
/// unchanged while shader maths happens in linear space.
pub const TARGET_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;
/// Linear colour between passes, with headroom above 1 for bloom.
const SCENE_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;
/// Bloom works on a copy this many times smaller in each direction.
const BLOOM_DOWNSAMPLE: u32 = 8;

const SCENE_SHADER: &str = concat!(
    include_str!("../../shaders/fullscreen.wgsl"),
    include_str!("../../shaders/common.wgsl"),
    include_str!("../../shaders/photo.wgsl"),
    include_str!("../../shaders/displace.wgsl"),
    include_str!("../../shaders/snow.wgsl"),
    include_str!("../../shaders/scene.wgsl"),
);
const BLOOM_SHADER: &str =
    concat!(include_str!("../../shaders/fullscreen.wgsl"), include_str!("../../shaders/bloom.wgsl"));
const CRT_SHADER: &str = concat!(
    include_str!("../../shaders/fullscreen.wgsl"),
    include_str!("../../shaders/common.wgsl"),
    include_str!("../../shaders/crt.wgsl"),
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
    tear: f32,
    chroma: f32,
    roll: f32,
    event_count: u32,
    scanlines: f32,
    vignette: f32,
    bloom: f32,
    persistence: f32,
    /// (seed, intensity, strength, unused) per event.
    events: [[f32; 4]; MAX_EVENTS],
}

/// The CRT treatment; each amount 0..1, 0 = off.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Crt {
    /// Darkness between scanlines (one line per snow row).
    pub scanlines: f32,
    pub vignette: f32,
    pub bloom: f32,
    /// How long snow lingers on the phosphor.
    pub persistence: f32,
}

impl Crt {
    pub const OFF: Self = Self { scanlines: 0.0, vignette: 0.0, bloom: 0.0, persistence: 0.0 };
}

impl Default for Crt {
    fn default() -> Self {
        Self { scanlines: 0.3, vignette: 0.25, bloom: 0.15, persistence: 0.0 }
    }
}

/// Fixed for a whole render.
#[derive(Clone, Copy, Debug)]
pub struct RenderSettings {
    pub fps: f32,
    pub seed: u32,
    /// Rows of the virtual snow grid (540 = 4×4-pixel snow at 4K). The CRT
    /// scanlines follow the same grid.
    pub snow_rows: f32,
    pub crt: Crt,
}

/// What changes from frame to frame.
#[derive(Clone, Copy, Debug, Default)]
pub struct FrameInputs {
    pub index: u32,
    pub disturbance: Disturbance,
    pub tears: Tears,
}

/// One fullscreen pass: its pipeline and inputs.
struct Pass {
    pipeline: wgpu::RenderPipeline,
    bind_group: wgpu::BindGroup,
}

impl Pass {
    fn encode(&self, encoder: &mut wgpu::CommandEncoder, label: &str, target: &wgpu::TextureView) {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some(label),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target,
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
}

pub struct Renderer {
    width: u32,
    height: u32,
    settings: RenderSettings,
    scene: Pass,
    bloom: Pass,
    crt: Pass,
    globals: wgpu::Buffer,
    scene_view: wgpu::TextureView,
    bloom_view: wgpu::TextureView,
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
        let texture = |label, size, format, usage| {
            device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size,
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING | usage,
                view_formats: &[],
            })
        };
        let target = texture("frame", size, TARGET_FORMAT, wgpu::TextureUsages::COPY_SRC);
        let target_view = target.create_view(&Default::default());
        let scene_view =
            texture("scene", size, SCENE_FORMAT, wgpu::TextureUsages::empty()).create_view(&Default::default());
        let bloom_size = wgpu::Extent3d {
            width: width.div_ceil(BLOOM_DOWNSAMPLE),
            height: height.div_ceil(BLOOM_DOWNSAMPLE),
            depth_or_array_layers: 1,
        };
        let bloom_view =
            texture("bloom", bloom_size, SCENE_FORMAT, wgpu::TextureUsages::empty()).create_view(&Default::default());

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

        // The later passes sample at pixel centres (exact) or just off them
        // (chroma split, bloom); clamping keeps the frame edges clean.
        let clamped = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("clamped"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        let photo_view = photo_tex.create_view(&Default::default());
        let scene = Pass::new(
            device,
            "scene",
            SCENE_SHADER,
            SCENE_FORMAT,
            &[Binding::Uniform(&globals), Binding::Texture(&photo_view), Binding::Sampler(&sampler)],
        );
        let bloom = Pass::new(
            device,
            "bloom",
            BLOOM_SHADER,
            SCENE_FORMAT,
            &[Binding::Texture(&scene_view), Binding::Sampler(&clamped)],
        );
        let crt = Pass::new(
            device,
            "crt",
            CRT_SHADER,
            TARGET_FORMAT,
            &[
                Binding::Uniform(&globals),
                Binding::Texture(&scene_view),
                Binding::Texture(&bloom_view),
                Binding::Sampler(&clamped),
            ],
        );

        Self { width, height, settings, scene, bloom, crt, globals, scene_view, bloom_view, target, target_view }
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
        let (d, t, crt) = (&frame.disturbance, &frame.tears, &self.settings.crt);
        let mut events = [[0.0; 4]; MAX_EVENTS];
        for (slot, e) in events.iter_mut().zip(&t.events[..t.count]) {
            // Click indices stay far below 2^24, so they survive as f32.
            *slot = [e.seed as f32, e.intensity, e.strength, 0.0];
        }
        let globals = Globals {
            resolution: [self.width as f32, self.height as f32],
            frame: frame.index,
            fps: self.settings.fps,
            snow: d.snow,
            jitter: d.jitter,
            snow_rows: self.settings.snow_rows,
            seed: self.settings.seed,
            tear: d.tear,
            chroma: d.chroma,
            roll: t.roll,
            event_count: t.count as u32,
            scanlines: crt.scanlines,
            vignette: crt.vignette,
            bloom: crt.bloom,
            persistence: crt.persistence,
            events,
        };
        gpu.queue.write_buffer(&self.globals, 0, bytemuck::bytes_of(&globals));

        self.scene.encode(encoder, "scene", &self.scene_view);
        if crt.bloom > 0.0 {
            self.bloom.encode(encoder, "bloom", &self.bloom_view);
        }
        self.crt.encode(encoder, "crt", &self.target_view);
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

enum Binding<'a> {
    Uniform(&'a wgpu::Buffer),
    Texture(&'a wgpu::TextureView),
    Sampler(&'a wgpu::Sampler),
}

impl Pass {
    /// A fullscreen pass whose group 0 holds `bindings`, numbered in order.
    fn new(
        device: &wgpu::Device,
        label: &str,
        shader: &str,
        format: wgpu::TextureFormat,
        bindings: &[Binding<'_>],
    ) -> Self {
        let layout_entries: Vec<_> = bindings
            .iter()
            .zip(0..)
            .map(|(b, i)| match b {
                Binding::Uniform(_) => wgpu::BindGroupLayoutEntry {
                    binding: i,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                Binding::Texture(_) => texture_entry(i),
                Binding::Sampler(_) => sampler_entry(i),
            })
            .collect();
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some(label),
            entries: &layout_entries,
        });
        let entries: Vec<_> = bindings
            .iter()
            .zip(0..)
            .map(|(b, i)| wgpu::BindGroupEntry {
                binding: i,
                resource: match b {
                    Binding::Uniform(buffer) => buffer.as_entire_binding(),
                    Binding::Texture(view) => wgpu::BindingResource::TextureView(view),
                    Binding::Sampler(sampler) => wgpu::BindingResource::Sampler(sampler),
                },
            })
            .collect();
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some(label),
            layout: &layout,
            entries: &entries,
        });
        Self { pipeline: fullscreen_pipeline(device, label, shader, &layout, format), bind_group }
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
        FrameInputs {
            index,
            disturbance: Disturbance { snow: 0.8, jitter: 0.6, tear: 0.0, chroma: 0.0 },
            ..Default::default()
        }
    }

    fn settings(crt: Crt) -> RenderSettings {
        RenderSettings { fps: 25.0, seed: 7, snow_rows: 90.0, crt }
    }

    fn max_diff(a: &RgbaImage, b: &RgbaImage) -> u8 {
        a.as_raw().iter().zip(b.as_raw()).map(|(x, y)| x.abs_diff(*y)).max().unwrap()
    }

    #[test]
    fn frames_are_deterministic_and_rest_is_the_photo() {
        let Ok(gpu) = Gpu::new(Gpu::instance(), None) else {
            eprintln!("no GPU adapter; skipping");
            return;
        };
        let photo = crate::assets::test_card(320, 180);
        let mut renderer = Renderer::new(&gpu, &photo, settings(Crt::default()));
        let render = |renderer: &Renderer, inputs: FrameInputs| {
            renderer.render(&gpu, &inputs);
            renderer.read_frame(&gpu).unwrap()
        };

        let a = render(&renderer, noisy(10));
        assert_eq!(a, render(&renderer, noisy(10)), "same frame must render identically");
        assert_ne!(a, render(&renderer, noisy(11)), "noise must change between frames");

        let rest = FrameInputs { index: 10, ..Default::default() };
        assert!(max_diff(&render(&renderer, rest), &photo) > 1, "the CRT treatment shows even at rest");
        renderer.set_settings(settings(Crt::OFF));
        let diff = max_diff(&render(&renderer, rest), &photo);
        assert!(diff <= 1, "at rest without CRT the frame should be the photo (max diff {diff})");
    }

    #[test]
    fn clicks_tear_deterministically() {
        let Ok(gpu) = Gpu::new(Gpu::instance(), None) else {
            eprintln!("no GPU adapter; skipping");
            return;
        };
        let photo = crate::assets::test_card(320, 180);
        let renderer = Renderer::new(&gpu, &photo, settings(Crt::OFF));
        let torn = |seed: u32| {
            let mut tears = Tears { count: 1, roll: 0.3, ..Default::default() };
            tears.events[0] = crate::tears::TearEvent { seed, strength: 1.0, intensity: 1.0 };
            let disturbance = Disturbance { tear: 1.0, chroma: 1.0, ..Default::default() };
            renderer.render(&gpu, &FrameInputs { index: 3, disturbance, tears });
            renderer.read_frame(&gpu).unwrap()
        };
        let a = torn(1);
        assert!(max_diff(&a, &photo) > 50, "a full-strength click visibly tears the photo");
        assert_eq!(a, torn(1), "same click, same tear");
        assert_ne!(a, torn(2), "each click tears differently");
    }
}
