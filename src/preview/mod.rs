//! Live preview window: audio playback, scrubbing and live parameter
//! controls. Renders through the same `Renderer` as the headless path, then
//! scales the frame into the space left by the panels.

mod audio;
mod transport;
mod ui;

use std::sync::Arc;

use anyhow::{Context, Result};
use image::RgbaImage;
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::window::{Window, WindowId};

use crate::FPS;
use crate::analysis::Interleaved;
use crate::renderer::{Blit, Gpu, RenderSettings, Renderer};
use crate::show::Show;
use transport::Transport;
use ui::UiState;

/// Everything the UI may change.
pub struct Session {
    pub transport: Transport,
    pub show: Show,
    pub settings: RenderSettings,
    pub ui: UiState,
}

impl Session {
    /// The frame under the playhead.
    pub fn frame_index(&self) -> u32 {
        let frame = (self.transport.position() * FPS as f64).floor().max(0.0) as u32;
        self.show.len().map_or(frame, |len| frame.min(len.saturating_sub(1)))
    }
}

pub fn run(photo: RgbaImage, show: Show, settings: RenderSettings, audio: Option<Interleaved>) -> Result<()> {
    let event_loop = EventLoop::new()?;
    let mut app = App { setup: Some((photo, show, settings, audio)), state: None, error: None };
    event_loop.run_app(&mut app)?;
    app.error.map_or(Ok(()), Err)
}

type Setup = (RgbaImage, Show, RenderSettings, Option<Interleaved>);

struct App {
    /// Consumed when the window is created.
    setup: Option<Setup>,
    state: Option<State>,
    error: Option<anyhow::Error>,
}

struct State {
    window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    gpu: Gpu,
    renderer: Renderer,
    blit: Blit,
    egui_ctx: egui::Context,
    egui_state: egui_winit::State,
    egui_renderer: egui_wgpu::Renderer,
    session: Session,
}

impl State {
    fn new(event_loop: &ActiveEventLoop, (photo, show, settings, audio): Setup) -> Result<Self> {
        let window = Arc::new(
            event_loop.create_window(
                Window::default_attributes()
                    .with_title("Static · still, charged")
                    .with_inner_size(LogicalSize::new(1600.0, 900.0)),
            )?,
        );
        let instance = Gpu::instance();
        let surface = instance.create_surface(window.clone())?;
        let gpu = Gpu::new(instance, Some(&surface))?;

        let size = window.inner_size();
        let mut config = surface
            .get_default_config(&gpu.adapter, size.width.max(1), size.height.max(1))
            .context("surface not supported by the GPU adapter")?;
        // An sRGB surface keeps the colours identical to the headless PNGs.
        let caps = surface.get_capabilities(&gpu.adapter);
        config.format = caps.formats.iter().copied().find(|f| f.is_srgb()).unwrap_or(caps.formats[0]);
        config.present_mode = wgpu::PresentMode::AutoVsync;
        surface.configure(&gpu.device, &config);

        let renderer = Renderer::new(&gpu, &photo, settings);
        let blit = Blit::new(&gpu, config.format, renderer.target_view(), renderer.size());

        let egui_ctx = egui::Context::default();
        egui_ctx.set_visuals(egui::Visuals::dark());
        let egui_state = egui_winit::State::new(
            egui_ctx.clone(),
            egui::ViewportId::ROOT,
            &window,
            Some(window.scale_factor() as f32),
            None,
            Some(gpu.device.limits().max_texture_dimension_2d as usize),
        );
        let egui_renderer = egui_wgpu::Renderer::new(&gpu.device, config.format, Default::default());

        let (w, h) = renderer.size();
        let preset = show.look.preset;
        let session = Session {
            transport: Transport::new(audio, show.duration()),
            show,
            settings,
            ui: UiState::new(preset, w as f32 / h as f32),
        };
        Ok(Self { window, surface, config, gpu, renderer, blit, egui_ctx, egui_state, egui_renderer, session })
    }

    fn resize(&mut self, width: u32, height: u32) {
        self.config.width = width.max(1);
        self.config.height = height.max(1);
        self.surface.configure(&self.gpu.device, &self.config);
    }

    /// Returns `false` once the user asks to quit.
    fn redraw(&mut self) -> Result<bool> {
        self.session.transport.update();
        let input = self.egui_state.take_egui_input(&self.window);
        let ctx = self.egui_ctx.clone();
        let output = ctx.run_ui(input, |ui| ui::draw(ui, &mut self.session));
        self.egui_state.handle_platform_output(&self.window, output.platform_output);
        if self.session.ui.quit {
            return Ok(false);
        }

        let surface_texture = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(t) | wgpu::CurrentSurfaceTexture::Suboptimal(t) => t,
            wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded => return Ok(true),
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                self.surface.configure(&self.gpu.device, &self.config);
                return Ok(true);
            }
            wgpu::CurrentSurfaceTexture::Validation => anyhow::bail!("surface validation error"),
        };
        let view = surface_texture.texture.create_view(&Default::default());
        let (device, queue) = (&self.gpu.device, &self.gpu.queue);
        let mut encoder = device.create_command_encoder(&Default::default());

        self.renderer.set_settings(self.session.settings);
        let inputs = self.session.show.inputs(self.session.frame_index());
        self.renderer.encode(&self.gpu, &mut encoder, &inputs);

        let ppp = output.pixels_per_point;
        let area = self.session.ui.view_rect;
        let area = [area.min.x * ppp, area.min.y * ppp, area.width() * ppp, area.height() * ppp];
        self.blit.encode(&mut encoder, &view, (self.config.width, self.config.height), area);

        let screen = egui_wgpu::ScreenDescriptor {
            size_in_pixels: [self.config.width, self.config.height],
            pixels_per_point: ppp,
        };
        let primitives = ctx.tessellate(output.shapes, ppp);
        for (id, delta) in &output.textures_delta.set {
            self.egui_renderer.update_texture(device, queue, *id, delta);
        }
        let egui_commands = self.egui_renderer.update_buffers(device, queue, &mut encoder, &primitives, &screen);
        {
            let mut pass = encoder
                .begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("egui"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &view,
                        depth_slice: None,
                        resolve_target: None,
                        ops: wgpu::Operations { load: wgpu::LoadOp::Load, store: wgpu::StoreOp::Store },
                    })],
                    ..Default::default()
                })
                .forget_lifetime();
            self.egui_renderer.render(&mut pass, &primitives, &screen);
        }
        queue.submit(egui_commands.into_iter().chain([encoder.finish()]));
        self.window.pre_present_notify();
        queue.present(surface_texture);
        for id in &output.textures_delta.free {
            self.egui_renderer.free_texture(id);
        }
        Ok(true)
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        let Some(setup) = self.setup.take() else { return };
        match State::new(event_loop, setup) {
            Ok(state) => {
                state.window.request_redraw();
                self.state = Some(state);
            }
            Err(e) => {
                self.error = Some(e);
                event_loop.exit();
            }
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        let Some(state) = self.state.as_mut() else { return };
        let _ = state.egui_state.on_window_event(&state.window, &event);
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => state.resize(size.width, size.height),
            WindowEvent::RedrawRequested => match state.redraw() {
                Ok(true) => state.window.request_redraw(),
                Ok(false) => event_loop.exit(),
                Err(e) => {
                    self.error = Some(e);
                    event_loop.exit();
                }
            },
            _ => {}
        }
    }
}
