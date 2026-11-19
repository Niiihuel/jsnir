//! A window that runs any TSX app through `jsnir::host::Host`: winit input in, wgpu frames out.
use crate::{Result, view::View};
use jsnir::{
    editor::EditorEvent,
    host::{Cursor, Host},
};
use jsnir_gpu::{
    input::{Clicks, Clipboard, ImeArea, KeyInput, editor_key, ime_event},
    surface::{self, Surface},
};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use winit::{
    application::ApplicationHandler,
    dpi::{LogicalSize, PhysicalPosition},
    event::{ElementState, MouseButton, WindowEvent},
    event_loop::{ActiveEventLoop, ControlFlow},
    keyboard::ModifiersState,
    window::{CursorIcon, Window, WindowId},
};

pub struct AppWindow {
    // Created with the window: the backend depends on what the GPU offers.
    instance: Option<wgpu::Instance>,
    surface: Option<Surface>,
    host: Host,
    view: View,
    title: String,
    size: [f32; 2],
    modifiers: ModifiersState,
    clicks: Clicks,
    clipboard: Clipboard,
    ime_area: ImeArea,
    window_focused: bool,
    ime_enabled: bool,
    wake: Option<Instant>,
    error: Option<Box<dyn std::error::Error>>,
}

impl AppWindow {
    pub fn new(host: Host, title: &str, size: [f32; 2]) -> Self {
        Self {
            instance: None,
            surface: None,
            host,
            view: View::default(),
            title: title.into(),
            size,
            modifiers: ModifiersState::empty(),
            clicks: Clicks::default(),
            clipboard: Clipboard::default(),
            ime_area: ImeArea::default(),
            window_focused: true,
            ime_enabled: false,
            wake: None,
            error: None,
        }
    }

    pub fn finish(self) -> Result<()> {
        self.error.map_or(Ok(()), Err)
    }

    fn request_redraw(&self) {
        if let Some(surface) = &self.surface {
            surface.window.request_redraw();
        }
    }

    // The platform input method is on only while a field has focus.
    fn sync_ime(&mut self) {
        let enabled = self.window_focused && self.host.focused();
        if enabled != self.ime_enabled
            && let Some(surface) = &self.surface
        {
            surface.window.set_ime_allowed(enabled);
            self.ime_enabled = enabled;
            self.ime_area.forget();
        }
    }

    fn edit(&mut self, events: &[EditorEvent]) {
        let output = self.host.edit(events);
        if let Some(text) = output.clipboard {
            self.clipboard.set_text(text);
        }
        if output.interrupt_ime
            && self.ime_enabled
            && let Some(surface) = &self.surface
        {
            self.ime_area.forget();
            surface.window.set_ime_allowed(false);
            surface.window.set_ime_allowed(true);
        }
        self.request_redraw();
    }

    fn redraw(&mut self, events: &ActiveEventLoop) {
        self.wake = None;
        self.host.poll();
        let Some(surface) = &mut self.surface else {
            return;
        };
        let size = surface.window.inner_size();
        if size.width == 0 || size.height == 0 {
            return;
        }
        let (frame, reconfigure) = match surface.target.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(frame) => (frame, false),
            wgpu::CurrentSurfaceTexture::Suboptimal(frame) => (frame, true),
            wgpu::CurrentSurfaceTexture::Timeout => {
                self.wake = Some(Instant::now() + Duration::from_millis(16));
                return;
            }
            wgpu::CurrentSurfaceTexture::Occluded => {
                self.wake = Some(Instant::now() + Duration::from_millis(250));
                return;
            }
            wgpu::CurrentSurfaceTexture::Outdated => {
                surface.resize();
                surface.window.request_redraw();
                return;
            }
            wgpu::CurrentSurfaceTexture::Lost => {
                match Surface::new(
                    self.instance.as_ref().expect("instance"),
                    surface.window.clone(),
                ) {
                    Ok(next) => {
                        *surface = next;
                        self.view.reset();
                        self.host.reset_images();
                        surface.window.request_redraw();
                    }
                    Err(error) => {
                        self.error = Some(error);
                        events.exit();
                    }
                }
                return;
            }
            wgpu::CurrentSurfaceTexture::Validation => {
                self.error = Some("wgpu rejected the window surface".into());
                events.exit();
                return;
            }
        };
        let scale = surface.window.scale_factor() as f32;
        let pixels = [frame.texture.width(), frame.texture.height()];
        let [width, height] = pixels.map(|side| side as f32 / scale);
        let max_side = surface.renderer.device.limits().max_texture_dimension_2d as usize;
        let scene = self.host.frame(width, height, scale, max_side);
        let draws = self
            .view
            .draws(&mut self.host, &surface.renderer, scene.items, scale);
        if let (true, Some(area)) = (self.ime_enabled, scene.ime_area) {
            self.ime_area.update(&surface.window, area);
        }
        surface.renderer.draw(&frame.texture, pixels, scale, &draws);
        surface.window.pre_present_notify();
        surface.renderer.queue.present(frame);
        if reconfigure {
            surface.resize();
        }
        // Transitions draw every frame; vsync paces them.
        if scene.animating || reconfigure {
            surface.window.request_redraw();
        }
    }

    fn cursor(&mut self, position: Option<PhysicalPosition<f64>>) {
        let Some(surface) = &self.surface else { return };
        let point = position.map(|p| {
            let p = p.to_logical::<f32>(surface.window.scale_factor());
            [p.x, p.y]
        });
        let icon = match self.host.pointer_move(point) {
            Cursor::Default => CursorIcon::Default,
            Cursor::Pointer => CursorIcon::Pointer,
            Cursor::Text => CursorIcon::Text,
            Cursor::NotAllowed => CursorIcon::NotAllowed,
        };
        surface.window.set_cursor(icon);
        self.request_redraw();
    }

    fn mouse(&mut self, state: ElementState, point: Option<[f32; 2]>) {
        match state {
            ElementState::Pressed => {
                let select = self
                    .clicks
                    .select(point.unwrap_or_default(), self.modifiers.shift_key());
                self.host.pointer_down(select);
            }
            ElementState::Released => self.host.pointer_up(),
        }
        self.sync_ime();
        self.request_redraw();
    }

    fn keyboard(&mut self, key: winit::event::KeyEvent) {
        if key.state == ElementState::Released || !self.host.focused() {
            return;
        }
        match editor_key(&key, self.modifiers) {
            Some(KeyInput::Edit(event)) => self.edit(&[event]),
            Some(KeyInput::Paste) => {
                if let Some(text) = self.clipboard.text() {
                    self.edit(&[EditorEvent::Insert(text)]);
                }
            }
            None => {}
        }
    }
}

impl ApplicationHandler for AppWindow {
    fn resumed(&mut self, events: &ActiveEventLoop) {
        if self.surface.is_some() {
            return;
        }
        let [width, height] = self.size;
        let window = events
            .create_window(
                Window::default_attributes()
                    .with_title(&self.title)
                    .with_inner_size(LogicalSize::new(width, height)),
            )
            .map_err(Box::from)
            .and_then(|window| surface::open(&events.owned_display_handle(), Arc::new(window)));
        match window {
            Ok((instance, surface)) => {
                self.instance = Some(instance);
                self.surface = Some(surface);
                self.sync_ime();
                self.request_redraw();
            }
            Err(error) => {
                self.error = Some(error);
                events.exit();
            }
        }
    }

    fn window_event(&mut self, events: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
        if !self
            .surface
            .as_ref()
            .is_some_and(|surface| surface.window.id() == id)
        {
            return;
        }
        match event {
            WindowEvent::CloseRequested => events.exit(),
            WindowEvent::Resized(_) | WindowEvent::ScaleFactorChanged { .. } => {
                if let Some(surface) = &mut self.surface {
                    surface.resize();
                }
                self.request_redraw();
            }
            WindowEvent::RedrawRequested => self.redraw(events),
            WindowEvent::CursorMoved { position, .. } => self.cursor(Some(position)),
            WindowEvent::CursorLeft { .. } => self.cursor(None),
            WindowEvent::Focused(focused) => {
                self.window_focused = focused;
                if !focused {
                    self.edit(&[EditorEvent::Preedit {
                        text: String::new(),
                        active: None,
                    }]);
                }
                self.sync_ime();
            }
            WindowEvent::MouseInput {
                state,
                button: MouseButton::Left,
                ..
            } => {
                let point = self.host_pointer();
                self.mouse(state, point);
            }
            WindowEvent::ModifiersChanged(modifiers) => self.modifiers = modifiers.state(),
            WindowEvent::KeyboardInput {
                event,
                is_synthetic: false,
                ..
            } => self.keyboard(event),
            WindowEvent::Ime(event) => {
                if self.host.focused()
                    && let Some(event) = ime_event(event)
                {
                    self.edit(&[event]);
                }
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, events: &ActiveEventLoop) {
        let now = Instant::now();
        if self.wake.is_some_and(|wake| now >= wake) {
            self.wake = None;
            self.request_redraw();
        }
        // While the app owes answers or timers are pending, look again soon.
        if self.wake.is_none() && self.host.busy() {
            self.wake = Some(now + Duration::from_millis(4));
        }
        events.set_control_flow(self.wake.map_or(ControlFlow::Wait, ControlFlow::WaitUntil));
    }
}

impl AppWindow {
    fn host_pointer(&self) -> Option<[f32; 2]> {
        self.host.pointer()
    }
}
