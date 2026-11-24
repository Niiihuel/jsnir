use crate::{
    Result,
    chat::{Scene, contains, editor_content},
};
use jsnir::{
    Mode,
    editor::{EditorEvent, PointSelection},
    history::{ROW_HEIGHT, ScrollEvent},
    host::{ImeChangeCause, ImeState},
    tree::InputPurpose,
};
use jsnir_gpu::ime::PlatformIme;
use jsnir_gpu::input::{Clicks, Clipboard, KeyInput, editor_key};
use jsnir_gpu::surface::{self, Surface};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use winit::{
    application::ApplicationHandler,
    dpi::{LogicalSize, PhysicalPosition},
    event::{ElementState, Ime, MouseButton, MouseScrollDelta, WindowEvent},
    event_loop::{ActiveEventLoop, ControlFlow},
    keyboard::{Key, ModifiersState, NamedKey},
    window::{CursorIcon, Window, WindowId},
};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Focus {
    Editor,
    Send,
    Other,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Drag {
    Editor,
    Scrollbar,
    Send,
    None,
}

pub struct App {
    // Field drop order: `ime` detaches before `surface` drops.
    ime: PlatformIme,
    ime_revision: u64,
    // Created with the window: the backend depends on what the GPU offers.
    instance: Option<wgpu::Instance>,
    surface: Option<Surface>,
    scene: Scene,
    events: Vec<EditorEvent>,
    modifiers: ModifiersState,
    pointer: Option<[f32; 2]>,
    drag: Drag,
    focus: Focus,
    window_focused: bool,
    clipboard: Clipboard,
    clicks: Clicks,
    wake: Option<Instant>,
    error: Option<Box<dyn std::error::Error>>,
}

impl App {
    pub fn new(mode: Mode) -> Self {
        let mut scene = Scene::new(mode, Duration::from_millis(500));
        if std::env::var_os("JSNIR_LIVE").is_some_and(|value| value == "1") {
            scene.chat.watch();
        }
        Self {
            ime: PlatformIme::default(),
            ime_revision: 0,
            instance: None,
            scene,
            surface: None,
            events: Vec::new(),
            modifiers: ModifiersState::empty(),
            pointer: None,
            drag: Drag::None,
            focus: Focus::Editor,
            window_focused: true,
            clipboard: Clipboard::default(),
            clicks: Clicks::default(),
            wake: None,
            error: None,
        }
    }

    pub fn finish(self) -> Result<()> {
        self.error.map_or(Ok(()), Err)
    }

    fn fail(&mut self, events: &ActiveEventLoop, error: impl Into<Box<dyn std::error::Error>>) {
        self.ime.detach();
        self.error = Some(error.into());
        events.exit();
    }

    fn request_redraw(&self) {
        if let Some(surface) = &self.surface {
            surface.window.request_redraw();
        }
    }

    fn ime_allowed(&self) -> bool {
        self.window_focused && self.focus == Focus::Editor
    }

    fn ime_state(&self, change_cause: ImeChangeCause) -> ImeState {
        let allowed = self.focus == Focus::Editor;
        ImeState {
            focused_id: allowed.then_some(1),
            allowed,
            purpose: InputPurpose::Normal,
            multiline: false,
            submit_on_enter: true,
            change_cause,
            revision: self.ime_revision,
            surrounding: allowed.then(|| self.scene.chat.surrounding_text()),
        }
    }

    fn sync_ime(&mut self, change_cause: ImeChangeCause) {
        let state = self.ime_state(change_cause);
        if let Some(surface) = &self.surface {
            self.ime.sync(&surface.window, self.window_focused, state);
        }
    }

    fn poll_ime(&mut self) {
        let events = self.ime.poll_events();
        if !events.is_empty() && self.ime_allowed() {
            self.events.extend(events);
            self.request_redraw();
        }
    }

    fn focus(&mut self, next: Focus) {
        if self.focus == next {
            return;
        }
        if self.focus == Focus::Editor {
            self.events.push(EditorEvent::Preedit {
                text: String::new(),
                active: None,
            });
        }
        self.focus = next;
        self.ime_revision += 1;
        self.sync_ime(ImeChangeCause::Other);
    }

    fn apply_editor_events(&mut self) {
        if self.events.is_empty() {
            return;
        }
        let all_ime = self.events.iter().all(|e| {
            matches!(
                e,
                EditorEvent::Preedit { .. }
                    | EditorEvent::PreeditSegments { .. }
                    | EditorEvent::Commit(_)
                    | EditorEvent::DeleteSurrounding { .. }
                    | EditorEvent::DeleteSurroundingBytes { .. }
            )
        });
        let output = self.scene.chat.edit(&self.events);
        self.events.clear();
        self.ime_revision += 1;
        if let Some(text) = output.clipboard {
            self.clipboard.set_text(text);
        }
        if output.interrupt_ime
            && let Some(surface) = &self.surface
        {
            self.ime.interrupt(&surface.window);
        }
        let cause = if all_ime {
            ImeChangeCause::InputMethod
        } else {
            ImeChangeCause::Other
        };
        self.sync_ime(cause);
    }

    fn redraw(&mut self, event_loop: &ActiveEventLoop) {
        self.wake = None;
        self.poll_ime();
        self.scene.chat.poll();
        self.apply_editor_events();

        let focused = self.ime_allowed();
        let send_focused = self.window_focused && self.focus == Focus::Send;
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
                        self.scene.reset_images();
                        surface.window.request_redraw();
                    }
                    Err(error) => self.fail(event_loop, error),
                }
                return;
            }
            wgpu::CurrentSurfaceTexture::Validation => {
                self.fail(event_loop, "wgpu rejected the window surface");
                return;
            }
        };

        let scale = surface.window.scale_factor() as f32;
        let size = [frame.texture.width(), frame.texture.height()];
        let (draws, caret) =
            self.scene
                .draws(&surface.renderer, size, scale, focused, send_focused);
        if focused {
            self.ime.update_area(&surface.window, Some(caret));
        }
        surface.renderer.draw(&frame.texture, size, scale, &draws);
        surface.window.pre_present_notify();
        surface.renderer.queue.present(frame);
        if reconfigure {
            surface.resize();
            surface.window.request_redraw();
        }
    }

    fn layout(&mut self) {
        if let Some(surface) = &self.surface {
            let scale = surface.window.scale_factor();
            let size = surface.window.inner_size().to_logical::<f32>(scale);
            self.scene.chat.layout(size.width, size.height);
        }
    }

    fn cursor(&mut self, position: PhysicalPosition<f64>) {
        let Some(surface) = &self.surface else { return };
        let position = position.to_logical::<f32>(surface.window.scale_factor());
        let point = [position.x, position.y];
        self.pointer = Some(point);
        let [editor, _] = self.scene.chat.controls();
        surface.window.set_cursor(if contains(editor, point) {
            CursorIcon::Text
        } else {
            CursorIcon::Default
        });
        match self.drag {
            Drag::Editor => self.events.push(EditorEvent::Point {
                x: point[0] - editor_content(editor)[0],
                y: point[1] - editor_content(editor)[1],
                select: PointSelection::Extend,
            }),
            Drag::Scrollbar => self.scene.chat.scroll_history(ScrollEvent::Drag(point[1])),
            _ => return,
        }
        self.request_redraw();
    }

    fn mouse(&mut self, state: ElementState) {
        let [editor, send] = self.scene.chat.controls();
        if state == ElementState::Released {
            let drag = std::mem::replace(&mut self.drag, Drag::None);
            if drag == Drag::Scrollbar {
                self.scene.chat.scroll_history(ScrollEvent::Release);
            } else if drag == Drag::Send && self.pointer.is_some_and(|point| contains(send, point))
            {
                self.events.push(EditorEvent::Enter);
                self.focus(Focus::Editor);
            }
        } else if let Some(point) = self.pointer {
            let list = self.scene.chat.history_view();
            if contains(editor, point) {
                self.focus(Focus::Editor);
                self.drag = Drag::Editor;
                let select = self.clicks.select(point, self.modifiers.shift_key());
                self.events.push(EditorEvent::Point {
                    x: point[0] - editor_content(editor)[0],
                    y: point[1] - editor_content(editor)[1],
                    select,
                });
            } else if contains(send, point) {
                self.drag = Drag::Send;
            } else if list.track.is_some_and(|track| contains(track, point)) {
                self.drag = Drag::Scrollbar;
                self.scene.chat.scroll_history(ScrollEvent::Grab(point[1]));
            } else {
                self.focus(Focus::Other);
            }
        }
        self.request_redraw();
    }

    fn wheel(&mut self, delta: MouseScrollDelta) {
        if !self
            .pointer
            .is_some_and(|point| contains(self.scene.chat.history_rect(), point))
        {
            return;
        }
        let Some(surface) = &self.surface else { return };
        let points = match delta {
            MouseScrollDelta::LineDelta(_, y) => y * ROW_HEIGHT * 3.,
            MouseScrollDelta::PixelDelta(point) => {
                point.to_logical::<f32>(surface.window.scale_factor()).y
            }
        };
        self.scene.chat.scroll_history(ScrollEvent::Wheel(-points));
        self.request_redraw();
    }

    fn keyboard(&mut self, key: winit::event::KeyEvent) {
        if key.state == ElementState::Released {
            return;
        }
        let logical = key.logical_key.as_ref();
        if logical == Key::Named(NamedKey::Tab) && !key.repeat && !self.scene.chat.composing() {
            self.focus(if self.focus == Focus::Editor {
                Focus::Send
            } else {
                Focus::Editor
            });
            self.request_redraw();
            return;
        }
        if self.focus == Focus::Send {
            if matches!(logical, Key::Named(NamedKey::Enter | NamedKey::Space)) && !key.repeat {
                self.events.push(EditorEvent::Enter);
                self.focus(Focus::Editor);
                self.request_redraw();
            }
            return;
        }
        if !self.ime_allowed() {
            return;
        }
        let edit = match editor_key(&key, self.modifiers) {
            Some(KeyInput::Edit(event)) => Some(event),
            Some(KeyInput::Paste) => {
                // Keeps the order when the same batch already holds a cut or copy.
                self.apply_editor_events();
                self.clipboard.text().map(EditorEvent::Insert)
            }
            None => None,
        };
        if let Some(edit) = edit {
            self.events.push(edit);
            self.request_redraw();
        }
    }

    fn ime(&mut self, event: Ime) {
        if !self.ime_allowed() {
            return;
        }
        if let Some(event) = self.ime.handle_winit_ime(event) {
            self.events.push(event);
            self.request_redraw();
        }
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, events: &ActiveEventLoop) {
        if self.surface.is_some() {
            return;
        }
        let result = (|| {
            let window = Arc::new(
                events.create_window(
                    Window::default_attributes()
                        .with_title("JSNIR · Chat")
                        .with_inner_size(LogicalSize::new(640., 480.)),
                )?,
            );
            surface::open(&events.owned_display_handle(), window)
        })();
        match result {
            Ok((instance, surface)) => {
                self.ime.attach(&surface.window);
                self.instance = Some(instance);
                self.surface = Some(surface);
                self.layout();
                self.sync_ime(ImeChangeCause::Other);
                self.request_redraw();
            }
            Err(error) => self.fail(events, error),
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
            WindowEvent::CloseRequested => {
                self.ime.detach();
                events.exit();
            }
            WindowEvent::Resized(_) | WindowEvent::ScaleFactorChanged { .. } => {
                if let Some(surface) = &mut self.surface {
                    surface.resize();
                }
                self.layout();
                self.request_redraw();
            }
            WindowEvent::RedrawRequested => self.redraw(events),
            WindowEvent::CursorMoved { position, .. } => self.cursor(position),
            WindowEvent::CursorLeft { .. } => self.pointer = None,
            WindowEvent::Focused(focused) => {
                self.window_focused = focused;
                if !focused {
                    self.drag = Drag::None;
                    self.scene.chat.scroll_history(ScrollEvent::Release);
                    self.events.push(EditorEvent::Preedit {
                        text: String::new(),
                        active: None,
                    });
                }
                self.sync_ime(ImeChangeCause::Other);
                self.request_redraw();
            }
            WindowEvent::MouseInput {
                state,
                button: MouseButton::Left,
                ..
            } => self.mouse(state),
            WindowEvent::MouseWheel { delta, .. } => self.wheel(delta),
            WindowEvent::ModifiersChanged(modifiers) => self.modifiers = modifiers.state(),
            WindowEvent::KeyboardInput {
                event,
                is_synthetic: false,
                ..
            } => self.keyboard(event),
            WindowEvent::Ime(event) => self.ime(event),
            _ => {}
        }
    }

    fn about_to_wait(&mut self, events: &ActiveEventLoop) {
        self.poll_ime();
        let interval = if self.scene.chat.pending() {
            Some(Duration::from_millis(8))
        } else if self.scene.chat.watching() {
            Some(Duration::from_millis(100))
        } else {
            None
        };
        let now = Instant::now();
        if self.wake.is_some_and(|wake| now >= wake) {
            self.wake = None;
            self.request_redraw();
        }
        if self.wake.is_none() {
            self.wake = interval.map(|interval| now + interval);
        }
        events.set_control_flow(self.wake.map_or(ControlFlow::Wait, ControlFlow::WaitUntil));
    }
}
