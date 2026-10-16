//! Preview-only: draws the finished frame into part of a window, letterboxed.

use super::{Gpu, fullscreen_pipeline, sampler_entry, texture_entry};

const BLIT_SHADER: &str =
    concat!(include_str!("../../shaders/fullscreen.wgsl"), include_str!("../../shaders/blit.wgsl"),);

pub struct Blit {
    pipeline: wgpu::RenderPipeline,
    bind_group: wgpu::BindGroup,
    source_size: (u32, u32),
}

impl Blit {
    pub fn new(gpu: &Gpu, format: wgpu::TextureFormat, source: &wgpu::TextureView, source_size: (u32, u32)) -> Self {
        let device = &gpu.device;
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("blit"),
            entries: &[texture_entry(0), sampler_entry(1)],
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("blit"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("blit"),
            layout: &layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(source) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&sampler) },
            ],
        });
        let pipeline = fullscreen_pipeline(device, "blit", BLIT_SHADER, &layout, format);
        Self { pipeline, bind_group, source_size }
    }

    /// Clears the whole view, then fits the frame inside `area`
    /// (`[x, y, width, height]` in pixels).
    pub fn encode(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        view: &wgpu::TextureView,
        view_size: (u32, u32),
        area: [f32; 4],
    ) {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("blit"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::BLACK), store: wgpu::StoreOp::Store },
            })],
            ..Default::default()
        });
        // Clamp to the view: the viewport must lie inside the target.
        let (vw, vh) = (view_size.0 as f32, view_size.1 as f32);
        let (ax, ay) = (area[0].clamp(0.0, vw - 1.0), area[1].clamp(0.0, vh - 1.0));
        let (aw, ah) = (area[2].min(vw - ax).max(1.0), area[3].min(vh - ay).max(1.0));
        let scale = (aw / self.source_size.0 as f32).min(ah / self.source_size.1 as f32);
        let (w, h) = (self.source_size.0 as f32 * scale, self.source_size.1 as f32 * scale);
        pass.set_viewport(ax + (aw - w) / 2.0, ay + (ah - h) / 2.0, w, h, 0.0, 1.0);
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.bind_group, &[]);
        pass.draw(0..3, 0..1);
    }
}
