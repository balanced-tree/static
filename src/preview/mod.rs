//! Live preview window: audio playback, scrubbing, live parameter overrides
//! and hot reload of `timeline.toml` and the shaders. Renders through the same
//! `Renderer` as the headless path, then scales the frame into the space left
//! by the panels.

mod audio;
mod transport;
mod ui;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use notify::Watcher;
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::window::{Window, WindowId};

use crate::FPS;
use crate::assets::{AssetCache, Assets};
use crate::project::Project;
use crate::renderer::{Blit, Gpu, RenderSettings, Renderer, Shaders};
use crate::show::Show;
use transport::Transport;
use ui::UiState;

/// Editors save in bursts (write, rename, touch); wait for quiet first.
const RELOAD_DEBOUNCE: Duration = Duration::from_millis(150);

/// The shader sources in the checkout this binary was built from.
fn shader_dir() -> Option<PathBuf> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("shaders");
    dir.is_dir().then_some(dir)
}

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

pub fn run(project: Project, show: Show, size: (u32, u32)) -> Result<()> {
    let event_loop = EventLoop::new()?;
    let mut app = App { setup: Some((project, show, size)), state: None, error: None };
    event_loop.run_app(&mut app)?;
    app.error.map_or(Ok(()), Err)
}

type Setup = (Project, Show, (u32, u32));

/// Set by the file watcher's thread, read by the render loop.
#[derive(Default)]
struct Changed {
    timeline: AtomicBool,
    shaders: AtomicBool,
}

/// Watches the timeline file and the shader sources.
struct HotReload {
    _watcher: Option<notify::RecommendedWatcher>,
    changed: Arc<Changed>,
    /// Changes seen but not yet applied: (since, timeline, shaders).
    pending: Option<(Instant, bool, bool)>,
}

impl HotReload {
    fn new(timeline: Option<&Path>, shaders: Option<&Path>) -> Self {
        let changed = Arc::new(Changed::default());
        let watcher = Self::watch(timeline, shaders, changed.clone()).unwrap_or_else(|e| {
            eprintln!("warning: no hot reload ({e:#})");
            None
        });
        Self { _watcher: watcher, changed, pending: None }
    }

    fn watch(
        timeline: Option<&Path>,
        shaders: Option<&Path>,
        changed: Arc<Changed>,
    ) -> Result<Option<notify::RecommendedWatcher>> {
        let timeline = timeline.map(std::fs::canonicalize).transpose()?;
        let shaders = shaders.map(std::fs::canonicalize).transpose()?;
        if timeline.is_none() && shaders.is_none() {
            return Ok(None);
        }
        let (t, s) = (timeline.clone(), shaders.clone());
        let mut watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
            let Ok(event) = event else { return };
            if matches!(event.kind, notify::EventKind::Access(_)) {
                return;
            }
            for path in &event.paths {
                // Editors often save via a temporary file and a rename, so
                // compare by name within the watched directory.
                if t.as_ref().is_some_and(|t| path.parent() == t.parent() && path.file_name() == t.file_name()) {
                    changed.timeline.store(true, Ordering::Release);
                }
                if s.as_ref().is_some_and(|s| path.parent() == Some(s.as_path()))
                    && path.extension().is_some_and(|e| e == "wgsl")
                {
                    changed.shaders.store(true, Ordering::Release);
                }
            }
        })?;
        if let Some(dir) = timeline.as_ref().and_then(|t| t.parent()) {
            watcher.watch(dir, notify::RecursiveMode::NonRecursive)?;
        }
        if let Some(dir) = &shaders {
            watcher.watch(dir, notify::RecursiveMode::NonRecursive)?;
        }
        Ok(Some(watcher))
    }

    /// What to reload now, if anything: (timeline, shaders).
    fn due(&mut self) -> Option<(bool, bool)> {
        let t = self.changed.timeline.swap(false, Ordering::AcqRel);
        let s = self.changed.shaders.swap(false, Ordering::AcqRel);
        if t || s {
            let (_, pt, ps) = self.pending.unwrap_or((Instant::now(), false, false));
            self.pending = Some((Instant::now(), pt || t, ps || s));
        }
        match self.pending {
            Some((since, t, s)) if since.elapsed() >= RELOAD_DEBOUNCE => {
                self.pending = None;
                Some((t, s))
            }
            _ => None,
        }
    }
}

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
    project: Project,
    cache: AssetCache,
    assets: Assets,
    shaders: Shaders,
    shader_dir: Option<PathBuf>,
    hot_reload: HotReload,
    egui_ctx: egui::Context,
    egui_state: egui_winit::State,
    egui_renderer: egui_wgpu::Renderer,
    /// Textures egui released last frame; freed once that frame is painted.
    textures_to_free: Vec<egui::TextureId>,
    session: Session,
}

impl State {
    fn new(event_loop: &ActiveEventLoop, (mut project, show, (width, height)): Setup) -> Result<Self> {
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

        let mut cache = AssetCache::new(width, height);
        let assets = cache.assets(show.timeline());
        let shader_dir = shader_dir();
        // Prefer the files on disk, so shader edits show without a rebuild.
        let shaders = shader_dir.as_deref().and_then(|d| Shaders::load(d).ok()).unwrap_or_else(Shaders::embedded);
        let settings = RenderSettings { fps: FPS, seed: project.seed };
        let renderer = match Renderer::new(&gpu, &assets, settings, &shaders) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("warning: shaders on disk don't compile ({e:#}); using the built-in ones");
                Renderer::new(&gpu, &assets, settings, &Shaders::embedded())?
            }
        };
        let blit = Blit::new(&gpu, config.format, renderer.target_view(), renderer.size());
        let hot_reload = HotReload::new(project.timeline_path.as_deref(), shader_dir.as_deref());
        let audio = project.audio.take();

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

        let session = Session {
            transport: Transport::new(audio, show.duration()),
            show,
            settings,
            ui: UiState::new(width as f32 / height as f32),
        };
        Ok(Self {
            window,
            surface,
            config,
            gpu,
            renderer,
            blit,
            project,
            cache,
            assets,
            shaders,
            shader_dir,
            hot_reload,
            egui_ctx,
            egui_state,
            egui_renderer,
            textures_to_free: Vec::new(),
            session,
        })
    }

    /// Applies saved changes to the timeline or shaders. Anything that fails
    /// is reported in the UI and the last working version stays.
    fn hot_reload(&mut self) {
        let Some((timeline, shaders)) = self.hot_reload.due() else { return };
        let mut rebuild = false;
        let mut messages = Vec::new();
        let mut error = false;
        if timeline {
            match self.project.reload() {
                Ok(Some(t)) => {
                    self.assets = self.cache.assets(&t);
                    self.session.show.set_timeline(t);
                    self.session.settings.seed = self.project.seed;
                    // The video length changes with the outro tail.
                    self.session.transport.set_duration(self.session.show.duration());
                    rebuild = true;
                    messages.push("reloaded timeline".to_string());
                }
                Ok(None) => {}
                Err(e) => {
                    messages.push(format!("timeline: {e:#}"));
                    error = true;
                }
            }
        }
        if shaders && let Some(dir) = &self.shader_dir {
            match Shaders::load(dir) {
                Ok(s) => {
                    self.shaders = s;
                    rebuild = true;
                }
                Err(e) => {
                    messages.push(format!("shaders: {e:#}"));
                    error = true;
                }
            }
        }
        if rebuild {
            match Renderer::new(&self.gpu, &self.assets, self.session.settings, &self.shaders) {
                Ok(renderer) => {
                    self.blit = Blit::new(&self.gpu, self.config.format, renderer.target_view(), renderer.size());
                    self.renderer = renderer;
                    if shaders {
                        messages.push("reloaded shaders".to_string());
                    }
                }
                Err(e) => {
                    messages.push(format!("shaders don't compile, keeping the last working ones:\n{e:#}"));
                    error = true;
                }
            }
        }
        let message = messages.join("\n");
        eprintln!("{message}");
        self.session.ui.set_status(message, error);
    }

    fn resize(&mut self, width: u32, height: u32) {
        self.config.width = width.max(1);
        self.config.height = height.max(1);
        self.surface.configure(&self.gpu.device, &self.config);
    }

    /// Returns `false` once the user asks to quit.
    fn redraw(&mut self) -> Result<bool> {
        self.hot_reload();
        self.session.transport.update();
        let input = self.egui_state.take_egui_input(&self.window);
        let ctx = self.egui_ctx.clone();
        let mut output = ctx.run_ui(input, |ui| ui::draw(ui, &mut self.session));
        self.egui_state.handle_platform_output(&self.window, output.platform_output);
        // Apply texture changes before any early return: egui only sends each
        // upload once (the font atlas on the first frame), and the delta must
        // not be dropped unapplied.
        let (device, queue) = (&self.gpu.device, &self.gpu.queue);
        for id in self.textures_to_free.drain(..) {
            self.egui_renderer.free_texture(&id);
        }
        for (id, deltas) in output.textures_delta.set.drain() {
            for delta in deltas {
                self.egui_renderer.update_texture(device, queue, id, &delta);
            }
        }
        self.textures_to_free.extend(output.textures_delta.free.drain());
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
