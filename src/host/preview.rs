//! Development window (any OS): the window is a virtual monitor, surfaces run inside it through
//! the [`Simulator`]. Move the cursor to the top edge to reveal a panel, click, scroll, Escape.

use std::num::NonZeroU32;
use std::rc::Rc;
use std::time::{Duration, Instant};

use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key, NamedKey};
use winit::window::{Window, WindowId};

use super::Entry;
use super::sim::Simulator;
use crate::Color;
use crate::surface::MonitorInfo;

const FRAME: Duration = Duration::from_millis(16);
const IDLE: Duration = Duration::from_millis(50);
const WHEEL_STEP: f32 = 48.0;

/// Opens a `width` × `height` (logical) window and runs the surfaces in it until it closes or a
/// surface quits.
pub fn run(entries: Vec<Entry>, width: f32, height: f32, background: Color) -> Result<(), String> {
    let event_loop = EventLoop::new().map_err(|e| e.to_string())?;
    let mut sim = Simulator::new(
        MonitorInfo {
            width,
            height,
            scale: 1.0,
        },
        background,
    );
    for entry in entries {
        sim.add(entry.surface);
    }
    let mut app = App {
        sim,
        size: (width, height),
        window: None,
        error: None,
        last_tick: Instant::now() - IDLE,
    };
    event_loop.run_app(&mut app).map_err(|e| e.to_string())?;
    app.error.map_or(Ok(()), Err)
}

struct Gpu {
    window: Rc<Window>,
    surface: softbuffer::Surface<Rc<Window>, Rc<Window>>,
}

struct App {
    sim: Simulator,
    size: (f32, f32),
    window: Option<Gpu>,
    error: Option<String>,
    last_tick: Instant,
}

impl App {
    fn fail(&mut self, event_loop: &ActiveEventLoop, error: String) {
        self.error = Some(error);
        event_loop.exit();
    }

    fn sync_monitor(&mut self) {
        let Some(gpu) = &self.window else {
            return;
        };
        let scale = gpu.window.scale_factor() as f32;
        let size = gpu.window.inner_size();
        self.sim.set_monitor(MonitorInfo {
            width: size.width.max(1) as f32 / scale,
            height: size.height.max(1) as f32 / scale,
            scale,
        });
    }

    fn present(&mut self) -> Result<(), String> {
        let frame = self.sim.render(Instant::now());
        let Some(gpu) = &mut self.window else {
            return Ok(());
        };
        let (Some(w), Some(h)) = (NonZeroU32::new(frame.width()), NonZeroU32::new(frame.height())) else {
            return Ok(());
        };
        gpu.surface.resize(w, h).map_err(|e| e.to_string())?;
        let mut buffer = gpu.surface.buffer_mut().map_err(|e| e.to_string())?;
        // The background is opaque, so the composite needs no un-premultiplying.
        for (dst, px) in buffer.iter_mut().zip(frame.pixels()) {
            *dst = (u32::from(px.red()) << 16) | (u32::from(px.green()) << 8) | u32::from(px.blue());
        }
        buffer.present().map_err(|e| e.to_string())
    }

    fn pointer(&self) -> Option<(f32, f32)> {
        self.sim.pointer()
    }

    fn respond(&mut self, event_loop: &ActiveEventLoop, response: crate::surface::Response) {
        if response.quit {
            event_loop.exit();
        } else if response.redraw
            && let Some(gpu) = &self.window
        {
            gpu.window.request_redraw();
        }
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let attributes = Window::default_attributes()
            .with_title("noren preview")
            .with_inner_size(LogicalSize::new(self.size.0, self.size.1));
        let result = event_loop
            .create_window(attributes)
            .map_err(|e| e.to_string())
            .and_then(|window| {
                let window = Rc::new(window);
                let context = softbuffer::Context::new(window.clone()).map_err(|e| e.to_string())?;
                let surface =
                    softbuffer::Surface::new(&context, window.clone()).map_err(|e| e.to_string())?;
                Ok(Gpu { window, surface })
            });
        match result {
            Ok(gpu) => {
                self.window = Some(gpu);
                self.sync_monitor();
            }
            Err(e) => self.fail(event_loop, e),
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _: WindowId, event: WindowEvent) {
        let now = Instant::now();
        let scale = self.sim.monitor().scale;
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(_) | WindowEvent::ScaleFactorChanged { .. } => {
                self.sync_monitor();
                let response = self.sim.tick(now);
                self.respond(event_loop, response);
                if let Some(gpu) = &self.window {
                    gpu.window.request_redraw();
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.sim
                    .set_pointer(Some((position.x as f32 / scale, position.y as f32 / scale)));
            }
            WindowEvent::CursorLeft { .. } => self.sim.set_pointer(None),
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                if let Some((x, y)) = self.pointer() {
                    let response = self.sim.click(x, y, now);
                    self.respond(event_loop, response);
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let dy = match delta {
                    MouseScrollDelta::LineDelta(_, lines) => -lines * WHEEL_STEP,
                    MouseScrollDelta::PixelDelta(p) => -(p.y as f32) / scale,
                };
                if let Some((x, y)) = self.pointer() {
                    let response = self.sim.scroll(x, y, dy);
                    self.respond(event_loop, response);
                }
            }
            WindowEvent::KeyboardInput { event, .. }
                if event.state == ElementState::Pressed
                    && event.logical_key == Key::Named(NamedKey::Escape) =>
            {
                let response = self.sim.escape(now);
                self.respond(event_loop, response);
            }
            WindowEvent::RedrawRequested => {
                if let Err(e) = self.present() {
                    self.fail(event_loop, e);
                }
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let now = Instant::now();
        let animating = self.sim.animating(now);
        let cadence = if animating { FRAME } else { IDLE };
        if now - self.last_tick >= cadence {
            self.last_tick = now;
            let response = self.sim.tick(now);
            let redraw = response.redraw || animating;
            self.respond(event_loop, response);
            if redraw && let Some(gpu) = &self.window {
                gpu.window.request_redraw();
            }
        }
        event_loop.set_control_flow(ControlFlow::WaitUntil(self.last_tick + cadence));
    }
}
