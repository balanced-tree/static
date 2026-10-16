//! Live preview window. Renders through the same `Renderer` as the headless
//! path, then scales the frame into the window.
//!
//! Milestone 4 adds audio playback, scrubbing and parameter sliders; for now
//! the clock simply runs from when the window opens.

use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, Result};
use image::RgbaImage;
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{ElementState, KeyEvent, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::keyboard::{Key, NamedKey};
use winit::window::{Window, WindowId};

use crate::FPS;
use crate::renderer::{Blit, Gpu, Renderer};

pub fn run(photo: RgbaImage) -> Result<()> {
    let event_loop = EventLoop::new()?;
    let mut app = App { photo: Some(photo), state: None, error: None };
    event_loop.run_app(&mut app)?;
    app.error.map_or(Ok(()), Err)
}

struct App {
    photo: Option<RgbaImage>,
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
    started: Instant,
}

impl State {
    fn new(event_loop: &ActiveEventLoop, photo: RgbaImage) -> Result<Self> {
        let window = Arc::new(
            event_loop.create_window(
                Window::default_attributes()
                    .with_title("Static · still, charged")
                    .with_inner_size(LogicalSize::new(1280.0, 720.0)),
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

        let renderer = Renderer::new(&gpu, &photo, FPS);
        let blit = Blit::new(&gpu, config.format, renderer.target_view(), renderer.size());
        Ok(Self { window, surface, config, gpu, renderer, blit, started: Instant::now() })
    }

    fn resize(&mut self, width: u32, height: u32) {
        self.config.width = width.max(1);
        self.config.height = height.max(1);
        self.surface.configure(&self.gpu.device, &self.config);
    }

    fn redraw(&mut self) -> Result<()> {
        let surface_texture = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(t) | wgpu::CurrentSurfaceTexture::Suboptimal(t) => t,
            wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded => return Ok(()),
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                self.surface.configure(&self.gpu.device, &self.config);
                return Ok(());
            }
            wgpu::CurrentSurfaceTexture::Validation => anyhow::bail!("surface validation error"),
        };
        let view = surface_texture.texture.create_view(&Default::default());
        let frame = (self.started.elapsed().as_secs_f64() * FPS as f64) as u32;

        let mut encoder = self.gpu.device.create_command_encoder(&Default::default());
        self.renderer.encode(&self.gpu, &mut encoder, frame);
        self.blit.encode(&mut encoder, &view, (self.config.width, self.config.height));
        self.gpu.queue.submit([encoder.finish()]);
        self.window.pre_present_notify();
        surface_texture.present();
        Ok(())
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        let Some(photo) = self.photo.take() else { return };
        match State::new(event_loop, photo) {
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
        match event {
            WindowEvent::CloseRequested
            | WindowEvent::KeyboardInput {
                event: KeyEvent { logical_key: Key::Named(NamedKey::Escape), state: ElementState::Pressed, .. },
                ..
            } => event_loop.exit(),
            WindowEvent::Resized(size) => state.resize(size.width, size.height),
            WindowEvent::RedrawRequested => {
                if let Err(e) = state.redraw() {
                    self.error = Some(e);
                    event_loop.exit();
                    return;
                }
                state.window.request_redraw();
            }
            _ => {}
        }
    }
}
