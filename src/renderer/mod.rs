//! Window-agnostic renderer. Draws each frame into an offscreen texture at the
//! output resolution; the preview blits that texture to a window, the headless
//! path reads it back. Both therefore see exactly the same pixels.
//!
//! Three passes per frame: the scene (photos, title, tears, jitter, snow) into
//! a float texture, an 8× downsample of its bright parts for bloom, then the
//! CRT treatment and colour grade into the final sRGB target.

mod blit;

pub use blit::Blit;

use std::path::Path;
use std::sync::mpsc;

use anyhow::{Context, Result, bail};
use bytemuck::{Pod, Zeroable};
use image::RgbaImage;

use crate::assets::Assets;
use crate::look::Disturbance;
use crate::tears::{MAX_EVENTS, Tears};
use crate::timeline::{Scene, SceneKind, SceneMix, TitleState};

/// Format of the offscreen frame. sRGB, so photo colours pass through
/// unchanged while shader maths happens in linear space.
pub const TARGET_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;
/// Linear colour between passes, with headroom above 1 for bloom.
const SCENE_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;
/// Bloom works on a copy this many times smaller in each direction.
const BLOOM_DOWNSAMPLE: u32 = 8;

/// The WGSL files each pass is built from, concatenated in order.
const SCENE_FILES: &[&str] =
    &["fullscreen.wgsl", "common.wgsl", "photo.wgsl", "displace.wgsl", "snow.wgsl", "scene.wgsl"];
const BLOOM_FILES: &[&str] = &["fullscreen.wgsl", "bloom.wgsl"];
const CRT_FILES: &[&str] = &["fullscreen.wgsl", "common.wgsl", "crt.wgsl"];

/// The shader sources: compiled in, or read from disk for hot reload.
#[derive(Clone, Hash)]
pub struct Shaders {
    scene: String,
    bloom: String,
    crt: String,
}

impl Shaders {
    pub fn embedded() -> Self {
        let join = |files: &[&str]| files.iter().map(|f| embedded(f)).collect::<String>();
        Self { scene: join(SCENE_FILES), bloom: join(BLOOM_FILES), crt: join(CRT_FILES) }
    }

    /// Reads the `.wgsl` files from `dir` (the preview's hot reload).
    pub fn load(dir: &Path) -> Result<Self> {
        let join = |files: &[&str]| -> Result<String> {
            files
                .iter()
                .map(|f| std::fs::read_to_string(dir.join(f)).with_context(|| format!("reading shaders/{f}")))
                .collect()
        };
        Ok(Self { scene: join(SCENE_FILES)?, bloom: join(BLOOM_FILES)?, crt: join(CRT_FILES)? })
    }
}

fn embedded(file: &str) -> &'static str {
    match file {
        "fullscreen.wgsl" => include_str!("../../shaders/fullscreen.wgsl"),
        "common.wgsl" => include_str!("../../shaders/common.wgsl"),
        "photo.wgsl" => include_str!("../../shaders/photo.wgsl"),
        "displace.wgsl" => include_str!("../../shaders/displace.wgsl"),
        "snow.wgsl" => include_str!("../../shaders/snow.wgsl"),
        "scene.wgsl" => include_str!("../../shaders/scene.wgsl"),
        "bloom.wgsl" => include_str!("../../shaders/bloom.wgsl"),
        "crt.wgsl" => include_str!("../../shaders/crt.wgsl"),
        other => unreachable!("no embedded shader {other}"),
    }
}

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

/// Mirrors `Globals` in shaders/common.wgsl: 31 scalars and a pad, then the
/// vec4s (which must start on a 16-byte boundary).
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
    from_kind: u32,
    from_layer: u32,
    to_kind: u32,
    to_layer: u32,
    transition: u32,
    transition_seed: u32,
    progress: f32,
    soften: f32,
    title_alpha: f32,
    title_noise: u32,
    title_disturb: f32,
    saturation: f32,
    contrast: f32,
    black_level: f32,
    grade_on: u32,
    _pad: u32,
    /// (seed, intensity, strength, unused) per event.
    events: [[f32; 4]; MAX_EVENTS],
    title_colour: [f32; 4],
    tint: [f32; 4],
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
        crate::params::Params::default().crt()
    }
}

/// Colour grade (PLAN §6), applied last.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Grade {
    /// 0 = monochrome.
    pub saturation: f32,
    pub contrast: f32,
    /// Lifts black towards grey.
    pub black_level: f32,
    /// Multiplies the image (sRGB 0..1; white = neutral).
    pub tint: [f32; 3],
}

impl Grade {
    pub const NEUTRAL: Self = Self { saturation: 1.0, contrast: 1.0, black_level: 0.0, tint: [1.0; 3] };
}

/// Fixed for a whole render.
#[derive(Clone, Copy, Debug)]
pub struct RenderSettings {
    pub fps: f32,
    pub seed: u32,
}

/// What changes from frame to frame.
#[derive(Clone, Copy, Debug)]
pub struct FrameInputs {
    pub index: u32,
    pub disturbance: Disturbance,
    pub tears: Tears,
    pub scene: SceneMix,
    pub title: TitleState,
    pub crt: Crt,
    pub grade: Grade,
    /// Rows of the virtual snow grid (540 = 4×4-pixel snow at 4K). The CRT
    /// scanlines follow the same grid.
    pub snow_rows: f32,
}

impl Default for FrameInputs {
    /// Photo layer 0 at rest, no CRT or grade.
    fn default() -> Self {
        Self {
            index: 0,
            disturbance: Disturbance::default(),
            tears: Tears::default(),
            scene: SceneMix::steady(Scene { kind: SceneKind::Photo, layer: 0, soften: 0.0, gain: 1.0 }),
            title: TitleState::default(),
            crt: Crt::OFF,
            grade: Grade::NEUTRAL,
            snow_rows: 540.0,
        }
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
    /// Fails (rather than panicking) if a shader doesn't compile, so the
    /// preview can keep the last working version during hot reload.
    pub fn new(gpu: &Gpu, assets: &Assets, settings: RenderSettings, shaders: &Shaders) -> Result<Self> {
        let device = &gpu.device;
        let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
        let renderer = Self::build(gpu, assets, settings, shaders);
        if let Some(error) = pollster::block_on(scope.pop()) {
            bail!("{error}");
        }
        Ok(renderer)
    }

    fn build(gpu: &Gpu, assets: &Assets, settings: RenderSettings, shaders: &Shaders) -> Self {
        let device = &gpu.device;
        let (width, height) = (assets.width, assets.height);
        let size = wgpu::Extent3d { width, height, depth_or_array_layers: 1 };
        let texture = |label, size, format, usage| {
            device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size,
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | usage,
                view_formats: &[],
            })
        };
        let attachment = wgpu::TextureUsages::RENDER_ATTACHMENT;

        // Photos: one layer each, all framed to the output size.
        let layers = assets.photos.len() as u32;
        let photos = texture(
            "photos",
            wgpu::Extent3d { depth_or_array_layers: layers, ..size },
            wgpu::TextureFormat::Rgba8UnormSrgb,
            wgpu::TextureUsages::COPY_DST,
        );
        for (layer, photo) in assets.photos.iter().enumerate() {
            write_layer(gpu, &photos, layer as u32, photo.as_raw(), width * 4, size);
        }
        let photos_view = photos.create_view(&wgpu::TextureViewDescriptor {
            dimension: Some(wgpu::TextureViewDimension::D2Array),
            ..Default::default()
        });

        // Title coverage mask; a blank pixel when there is no title.
        let (title_size, title_data) = match &assets.title {
            Some(t) => (size, t.as_raw().as_slice()),
            None => (wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 }, &[0u8][..]),
        };
        let title = texture("title", title_size, wgpu::TextureFormat::R8Unorm, wgpu::TextureUsages::COPY_DST);
        write_layer(gpu, &title, 0, title_data, title_size.width, title_size);
        let title_view = title.create_view(&Default::default());

        let target = texture("frame", size, TARGET_FORMAT, attachment | wgpu::TextureUsages::COPY_SRC);
        let target_view = target.create_view(&Default::default());
        let scene_view = texture("scene", size, SCENE_FORMAT, attachment).create_view(&Default::default());
        let bloom_size = wgpu::Extent3d {
            width: width.div_ceil(BLOOM_DOWNSAMPLE),
            height: height.div_ceil(BLOOM_DOWNSAMPLE),
            depth_or_array_layers: 1,
        };
        let bloom_view = texture("bloom", bloom_size, SCENE_FORMAT, attachment).create_view(&Default::default());

        let globals = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("globals"),
            size: std::mem::size_of::<Globals>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mirrored = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("photo"),
            // Jitter and tears sample past the edges; mirroring hides that
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

        let scene = Pass::new(
            device,
            "scene",
            &shaders.scene,
            SCENE_FORMAT,
            &[
                Binding::Uniform(&globals),
                Binding::TextureArray(&photos_view),
                Binding::Sampler(&mirrored),
                Binding::Texture(&title_view),
                Binding::Sampler(&clamped),
            ],
        );
        let bloom = Pass::new(
            device,
            "bloom",
            &shaders.bloom,
            SCENE_FORMAT,
            &[Binding::Texture(&scene_view), Binding::Sampler(&clamped)],
        );
        let crt = Pass::new(
            device,
            "crt",
            &shaders.crt,
            TARGET_FORMAT,
            &[
                Binding::Uniform(&globals),
                Binding::Texture(&scene_view),
                Binding::Texture(&bloom_view),
                Binding::Sampler(&clamped),
                Binding::Texture(&title_view),
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
        let (d, t, crt, g, s) = (&frame.disturbance, &frame.tears, &frame.crt, &frame.grade, &frame.scene);
        let mut events = [[0.0; 4]; MAX_EVENTS];
        for (slot, e) in events.iter_mut().zip(&t.events[..t.count]) {
            // Click indices stay far below 2^24, so they survive as f32.
            *slot = [e.seed as f32, e.intensity, e.strength, 0.0];
        }
        let [r, gr, b] = frame.title.colour;
        let [tr, tg, tb] = g.tint;
        let globals = Globals {
            resolution: [self.width as f32, self.height as f32],
            frame: frame.index,
            fps: self.settings.fps,
            snow: d.snow,
            jitter: d.jitter,
            snow_rows: frame.snow_rows,
            seed: self.settings.seed,
            tear: d.tear,
            chroma: d.chroma,
            roll: t.roll,
            event_count: t.count as u32,
            scanlines: crt.scanlines,
            vignette: crt.vignette,
            bloom: crt.bloom,
            persistence: crt.persistence,
            from_kind: s.from.kind as u32,
            from_layer: s.from.layer,
            to_kind: s.to.kind as u32,
            to_layer: s.to.layer,
            transition: s.transition as u32,
            transition_seed: s.seed,
            progress: s.progress,
            soften: s.from.soften + (s.to.soften - s.from.soften) * s.progress,
            title_alpha: frame.title.alpha,
            title_noise: frame.title.noise as u32,
            title_disturb: frame.title.disturb,
            saturation: g.saturation,
            contrast: g.contrast,
            black_level: g.black_level,
            // Skipped when neutral, so an ungraded frame is bit-exact.
            grade_on: (*g != Grade::NEUTRAL) as u32,
            _pad: 0,
            events,
            title_colour: [r, gr, b, 1.0],
            tint: [tr, tg, tb, 1.0],
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

fn write_layer(gpu: &Gpu, texture: &wgpu::Texture, layer: u32, data: &[u8], row_bytes: u32, size: wgpu::Extent3d) {
    gpu.queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture,
            mip_level: 0,
            origin: wgpu::Origin3d { x: 0, y: 0, z: layer },
            aspect: wgpu::TextureAspect::All,
        },
        data,
        wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(row_bytes), rows_per_image: Some(size.height) },
        wgpu::Extent3d { depth_or_array_layers: 1, ..size },
    );
}

/// One fullscreen pass: its pipeline and inputs.
struct Pass {
    pipeline: wgpu::RenderPipeline,
    bind_group: wgpu::BindGroup,
}

enum Binding<'a> {
    Uniform(&'a wgpu::Buffer),
    Texture(&'a wgpu::TextureView),
    TextureArray(&'a wgpu::TextureView),
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
                Binding::TextureArray(_) => wgpu::BindGroupLayoutEntry {
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2Array,
                        multisampled: false,
                    },
                    ..texture_entry(i)
                },
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
                    Binding::Texture(view) | Binding::TextureArray(view) => wgpu::BindingResource::TextureView(view),
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
    use std::sync::Arc;

    use super::*;
    use crate::timeline::Transition;

    const SETTINGS: RenderSettings = RenderSettings { fps: 25.0, seed: 7 };

    fn setup(photos: Vec<RgbaImage>) -> Option<(Gpu, Renderer)> {
        let Ok(gpu) = Gpu::new(Gpu::instance(), None) else {
            eprintln!("no GPU adapter; skipping");
            return None;
        };
        let (width, height) = photos[0].dimensions();
        let assets = Assets { width, height, photos: photos.into_iter().map(Arc::new).collect(), title: None };
        let renderer = Renderer::new(&gpu, &assets, SETTINGS, &Shaders::embedded()).unwrap();
        Some((gpu, renderer))
    }

    fn render(gpu: &Gpu, renderer: &Renderer, inputs: FrameInputs) -> RgbaImage {
        renderer.render(gpu, &inputs);
        renderer.read_frame(gpu).unwrap()
    }

    fn noisy(index: u32) -> FrameInputs {
        FrameInputs {
            index,
            disturbance: Disturbance { snow: 0.8, jitter: 0.6, tear: 0.0, chroma: 0.0 },
            snow_rows: 90.0,
            ..Default::default()
        }
    }

    fn max_diff(a: &RgbaImage, b: &RgbaImage) -> u8 {
        a.as_raw().iter().zip(b.as_raw()).map(|(x, y)| x.abs_diff(*y)).max().unwrap()
    }

    #[test]
    fn frames_are_deterministic_and_rest_is_the_photo() {
        let photo = crate::assets::test_card(320, 180);
        let Some((gpu, renderer)) = setup(vec![photo.clone()]) else { return };

        let a = render(&gpu, &renderer, noisy(10));
        assert_eq!(a, render(&gpu, &renderer, noisy(10)), "same frame must render identically");
        assert_ne!(a, render(&gpu, &renderer, noisy(11)), "noise must change between frames");

        let rest = FrameInputs { index: 10, ..Default::default() };
        let with_crt = FrameInputs { crt: Crt::default(), ..rest };
        assert!(max_diff(&render(&gpu, &renderer, with_crt), &photo) > 1, "the CRT treatment shows even at rest");
        let diff = max_diff(&render(&gpu, &renderer, rest), &photo);
        assert!(diff <= 1, "at rest without CRT the frame should be the photo (max diff {diff})");
        let mono = FrameInputs { grade: Grade { saturation: 0.0, ..Grade::NEUTRAL }, ..rest };
        assert_eq!(max_diff(&render(&gpu, &renderer, mono), &photo), diff, "the test card is grey already");
    }

    #[test]
    fn clicks_tear_deterministically() {
        let photo = crate::assets::test_card(320, 180);
        let Some((gpu, renderer)) = setup(vec![photo.clone()]) else { return };
        let torn = |seed: u32| {
            let mut tears = Tears { count: 1, roll: 0.3, ..Default::default() };
            tears.events[0] = crate::tears::TearEvent { seed, strength: 1.0, intensity: 1.0 };
            let disturbance = Disturbance { tear: 1.0, chroma: 1.0, ..Default::default() };
            render(&gpu, &renderer, FrameInputs { index: 3, disturbance, tears, ..Default::default() })
        };
        let a = torn(1);
        assert!(max_diff(&a, &photo) > 50, "a full-strength click visibly tears the photo");
        assert_eq!(a, torn(1), "same click, same tear");
        assert_ne!(a, torn(2), "each click tears differently");
    }

    #[test]
    fn scenes_and_transitions() {
        let white = RgbaImage::from_pixel(64, 36, image::Rgba([255, 255, 255, 255]));
        let black = RgbaImage::from_pixel(64, 36, image::Rgba([0, 0, 0, 255]));
        let Some((gpu, renderer)) = setup(vec![white.clone(), black.clone()]) else { return };
        let photo = |layer| Scene { kind: SceneKind::Photo, layer, soften: 0.0, gain: 1.0 };
        let show = |scene| render(&gpu, &renderer, FrameInputs { scene, ..Default::default() });

        assert_eq!(show(SceneMix::steady(photo(1))), black, "layers are separate");
        assert_eq!(show(SceneMix::steady(Scene::BLACK)), black);
        let stat = show(SceneMix::steady(Scene::STATIC));
        assert!(stat.pixels().any(|p| p.0[0] > 40) && stat.pixels().any(|p| p.0[0] < 20), "static is noise");

        let half = |transition| show(SceneMix { from: photo(0), to: photo(1), progress: 0.5, transition, seed: 1 });
        let dissolve = half(Transition::Dissolve);
        assert!(dissolve.pixels().all(|p| (100..220).contains(&p.0[0])), "a dissolve blends evenly");
        let noise = half(Transition::NoiseDissolve);
        let (lit, dark) =
            (noise.pixels().filter(|p| p.0[0] == 255).count(), noise.pixels().filter(|p| p.0[0] == 0).count());
        assert!(lit > 200 && dark > 200, "a noise dissolve swaps cell by cell ({lit} white, {dark} black)");
    }
}
