//! A window that runs any TSX app through `jsnir::host::Host`: winit input in, wgpu frames out.
use crate::{
    Result,
    ime::PlatformIme,
    input::{Clicks, Clipboard, KeyInput, editor_key},
    renderer::Renderer,
    surface::{self, Surface},
    view::View,
};
use jsnir::{
    editor::EditorEvent,
    host::{Cursor, DevError, Host},
};
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU32, Ordering},
        mpsc::{Receiver, Sender, channel},
    },
    time::{Duration, Instant},
};
use winit::{
    application::ApplicationHandler,
    dpi::{LogicalSize, PhysicalPosition},
    event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent},
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy},
    keyboard::{Key, ModifiersState, NamedKey},
    window::{CursorIcon, Window, WindowId},
};

pub struct AppWindow {
    // Field drop order is load-bearing: `ime` must detach BEFORE `surface`/`window` drops so
    // native Wayland (`wl_display`) and Win32 (`HWND`) handles are still live during cleanup.
    ime: PlatformIme,
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
    window_focused: bool,
    cursor_shape: Cursor,
    wake: Option<Instant>,
    // Messages from `Pusher`; None when the program has no channel (`open`).
    inbox: Option<Receiver<Message>>,
    shared: Arc<Shared>,
    dropped_files: Vec<PathBuf>,
    error: Option<Box<dyn std::error::Error>>,
}

impl AppWindow {
    pub fn new(host: Host, title: &str, size: [f32; 2]) -> Self {
        Self {
            ime: PlatformIme::default(),
            instance: None,
            surface: None,
            host,
            view: View::default(),
            title: title.into(),
            size,
            modifiers: ModifiersState::empty(),
            clicks: Clicks::default(),
            clipboard: Clipboard::default(),
            window_focused: true,
            cursor_shape: Cursor::Default,
            wake: None,
            inbox: None,
            shared: Arc::default(),
            dropped_files: Vec::new(),
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

    fn resize(&mut self) {
        if let Some(surface) = &mut self.surface {
            surface.resize();
        }
        self.request_redraw();
    }

    // Tells the embedder and the app the window's scale: once when the window opens, then on each change.
    fn set_scale(&mut self, scale: f32) {
        self.shared.set_scale(scale);
        self.host.push(format!(r#"{{"jsnirScale":{scale}}}"#));
    }

    // The platform input method is on only while a non-password field has focus.
    fn sync_ime(&mut self) {
        let interrupted = self.host.take_ime_interrupt();
        if let Some(surface) = &self.surface {
            if interrupted {
                self.ime.interrupt(&surface.window);
            }
            self.ime
                .sync(&surface.window, self.window_focused, self.host.ime_state());
        }
    }

    /// Drains any pending native Wayland (`zwp_text_input_v3`) or Windows (`WM_IME_*`) events.
    fn poll_ime(&mut self) {
        let events = self.ime.poll_events();
        if !events.is_empty() && self.host.focused() {
            self.edit(&events);
        }
    }

    fn edit(&mut self, events: &[EditorEvent]) {
        let output = self.host.edit(events);
        if let Some(text) = output.clipboard {
            self.clipboard.set_text(text);
        }
        self.sync_ime();
        self.request_redraw();
    }

    fn redraw(&mut self, events: &ActiveEventLoop) {
        self.wake = None;
        self.poll_ime();
        self.host.poll();
        if let Some(update) = self.shared.on_frame.lock().unwrap().as_mut() {
            update(&mut self.host);
        }
        for result in std::iter::from_fn(|| self.host.take_reload_result()) {
            if let Err(error) = result {
                eprintln!("jsnir: reload failed: {error}");
            }
        }
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
            .draws(&mut self.host, &mut surface.renderer, scene.items, scale);
        self.ime
            .sync(&surface.window, self.window_focused, self.host.ime_state());
        self.ime.update_area(&surface.window, scene.ime_area);
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
        self.sync_cursor(self.host.cursor());
    }

    fn sync_cursor(&mut self, next: Cursor) {
        if next != self.cursor_shape
            && let Some(surface) = &self.surface
        {
            surface.window.set_cursor(cursor_icon(next));
            self.cursor_shape = next;
        }
    }

    fn cursor(&mut self, position: Option<PhysicalPosition<f64>>) {
        let Some(surface) = &self.surface else { return };
        let point = position.map(|p| {
            let p = p.to_logical::<f32>(surface.window.scale_factor());
            [p.x, p.y]
        });
        let next = self.host.pointer_move(point);
        self.sync_cursor(next);
        self.request_redraw();
    }

    // Winit's wheel is positive up; the host's offset is positive down, so a wheel down moves the content up.
    fn wheel(&mut self, delta: MouseScrollDelta) {
        let Some(surface) = &self.surface else { return };
        // Mouse wheel notches glide; touchpad pixels are already smooth and apply at once.
        let changed = match delta {
            MouseScrollDelta::LineDelta(_, y) => self.host.wheel_smooth(-y * 40.),
            MouseScrollDelta::PixelDelta(p) => self
                .host
                .wheel(-(p.y / surface.window.scale_factor()) as f32),
        };
        if changed {
            self.request_redraw();
        }
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
        self.sync_cursor(self.host.cursor());
        self.request_redraw();
    }

    fn keyboard(&mut self, key: winit::event::KeyEvent) {
        if key.state == ElementState::Released {
            return;
        }
        // Escape reaches the app even when no field has focus, so menus and popovers can close,
        // EXCEPT while an IME preedit is active (first Escape cancels composition, not the popover).
        if matches!(key.logical_key, Key::Named(NamedKey::Escape)) && !self.host.composing() {
            self.host.escape();
            self.request_redraw();
        }
        if !self.host.focused() {
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
                let scale = surface.window.scale_factor() as f32;
                self.ime.attach(&surface.window);
                self.instance = Some(instance);
                self.surface = Some(surface);
                self.set_scale(scale);
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
            WindowEvent::CloseRequested => {
                self.ime.detach();
                events.exit();
            }
            WindowEvent::Resized(_) => self.resize(),
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                self.set_scale(scale_factor as f32);
                self.resize();
            }
            WindowEvent::RedrawRequested => self.redraw(events),
            WindowEvent::CursorMoved { position, .. } => self.cursor(Some(position)),
            WindowEvent::CursorLeft { .. } => self.cursor(None),
            WindowEvent::MouseWheel { delta, .. } => self.wheel(delta),
            WindowEvent::Focused(focused) => {
                self.window_focused = focused;
                if !focused {
                    self.host.pointer_cancel();
                    self.sync_cursor(self.host.cursor());
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
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Right,
                ..
            } => {
                self.host.context_press();
                self.request_redraw();
            }
            WindowEvent::ModifiersChanged(modifiers) => self.modifiers = modifiers.state(),
            WindowEvent::DroppedFile(path) => self.dropped_files.push(path),
            WindowEvent::KeyboardInput {
                event,
                is_synthetic: false,
                ..
            } => self.keyboard(event),
            WindowEvent::Ime(event) => {
                if self.host.focused()
                    && let Some(event) = self.ime.handle_winit_ime(event)
                {
                    self.edit(&[event]);
                }
            }
            _ => {}
        }
    }

    fn user_event(&mut self, events: &ActiveEventLoop, _: ()) {
        let Some(inbox) = &self.inbox else { return };
        let messages = coalesce_messages(inbox.try_iter().collect());
        for message in messages {
            match message {
                Message::Push(text) => self.host.push(text),
                Message::PushKeyed { key, text } => self.host.push_keyed(key, text),
                Message::Image { source, rgba, size } => self.host.set_image(source, rgba, size),
                Message::RemoveImage(source) => self.host.remove_image(&source),
                Message::Font(bytes) => self.host.add_font(bytes),
                Message::DefaultFamily(family) => self.host.set_default_family(family),
                Message::ReducedMotion(on) => self.host.set_reduced_motion(on),
                Message::Reload {
                    script,
                    preserve_state,
                } => {
                    let ok = if preserve_state {
                        self.host.reload(script)
                    } else {
                        self.host.hard_reload(script)
                    };
                    if !ok {
                        eprintln!(
                            "jsnir: reload failed: the app's event queue is full, send it again"
                        );
                    }
                }
                Message::DevOverlay(enabled) => self.host.enable_dev_overlay(enabled),
                Message::DevError(error) => self.host.set_dev_error(error),
                Message::Title(title) => {
                    self.title = title;
                    if let Some(surface) = &self.surface {
                        surface.window.set_title(&self.title);
                    }
                }
                Message::Close => {
                    self.ime.detach();
                    events.exit();
                    return;
                }
            }
        }
        self.request_redraw();
    }
    fn about_to_wait(&mut self, events: &ActiveEventLoop) {
        self.poll_ime();
        if !self.dropped_files.is_empty() {
            let paths = std::mem::take(&mut self.dropped_files);
            if let Some(on_drop) = self.shared.on_drop.lock().unwrap().as_ref() {
                on_drop(paths);
            }
        }
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

fn cursor_icon(cursor: Cursor) -> CursorIcon {
    match cursor {
        Cursor::Default => CursorIcon::Default,
        Cursor::Pointer => CursorIcon::Pointer,
        Cursor::Text => CursorIcon::Text,
        Cursor::NotAllowed => CursorIcon::NotAllowed,
        Cursor::Grab => CursorIcon::Grab,
        Cursor::Grabbing => CursorIcon::Grabbing,
        Cursor::Move => CursorIcon::Move,
        Cursor::ColResize => CursorIcon::ColResize,
        Cursor::RowResize => CursorIcon::RowResize,
        Cursor::EwResize => CursorIcon::EwResize,
        Cursor::NsResize => CursorIcon::NsResize,
        Cursor::NeswResize => CursorIcon::NeswResize,
        Cursor::NwseResize => CursorIcon::NwseResize,
    }
}

impl AppWindow {
    fn host_pointer(&self) -> Option<[f32; 2]> {
        self.host.pointer()
    }
}

/// Opens a native window or captures a settled frame to a new PPM file.
pub fn open(
    mut host: Host,
    title: &str,
    size: [u32; 2],
    focus: bool,
    output: Option<&Path>,
) -> Result<()> {
    if let Some(path) = output {
        return capture_at(host, size, 1., focus, path);
    }
    let event_loop = EventLoop::new()?;
    host.poll();
    if focus {
        host.focus_first_input();
    }
    let mut window = AppWindow::new(host, title, size.map(|side| side as f32));
    event_loop.run_app(&mut window)?;
    window.finish()
}

/// Like `open`, with a channel for `Host::push`: `start` receives a `Pusher` the program can move to other threads.
pub fn open_with(
    mut host: Host,
    title: &str,
    size: [u32; 2],
    start: impl FnOnce(Pusher),
) -> Result<()> {
    let event_loop = EventLoop::<()>::with_user_event().build()?;
    let (tx, inbox) = channel();
    let shared = Arc::<Shared>::default();
    start(Pusher {
        tx,
        proxy: event_loop.create_proxy(),
        shared: shared.clone(),
    });
    host.poll();
    let mut window = AppWindow::new(host, title, size.map(|side| side as f32));
    window.inbox = Some(inbox);
    window.shared = shared;
    event_loop.run_app(&mut window)?;
    window.finish()
}
/// What a `Pusher` sends to the window thread.
enum Message {
    Push(String),
    PushKeyed {
        key: String,
        text: String,
    },
    Image {
        source: String,
        rgba: Vec<u8>,
        size: [u32; 2],
    },
    RemoveImage(String),
    Font(Vec<u8>),
    DefaultFamily(String),
    ReducedMotion(bool),
    Reload {
        script: String,
        preserve_state: bool,
    },
    DevOverlay(bool),
    DevError(Option<DevError>),
    Title(String),
    Close,
}
type ScaleCallback = Box<dyn Fn(f32) + Send>;
type FrameCallback = Box<dyn FnMut(&mut Host) + Send>;
type DropCallback = Box<dyn Fn(Vec<PathBuf>) + Send>;
/// The window's scale factor and its `on_scale` callback, shared between the window and its `Pusher`s.
struct Shared {
    // f32 bits; 1.0 until the window opens.
    scale: AtomicU32,
    on_scale: Mutex<Option<ScaleCallback>>,
    on_frame: Mutex<Option<FrameCallback>>,
    on_drop: Mutex<Option<DropCallback>>,
}
impl Default for Shared {
    fn default() -> Self {
        Self {
            scale: AtomicU32::new(1f32.to_bits()),
            on_scale: Mutex::new(None),
            on_frame: Mutex::new(None),
            on_drop: Mutex::new(None),
        }
    }
}
impl Shared {
    fn scale(&self) -> f32 {
        f32::from_bits(self.scale.load(Ordering::Relaxed))
    }
    // Runs on the window thread.
    fn set_scale(&self, scale: f32) {
        self.scale.store(scale.to_bits(), Ordering::Relaxed);
        if let Some(on_scale) = self.on_scale.lock().unwrap().as_ref() {
            on_scale(scale);
        }
    }
}
/// Sends messages from any thread to the app's `onHost` handler and to its images, and wakes the window.
#[derive(Clone)]
pub struct Pusher {
    tx: Sender<Message>,
    proxy: EventLoopProxy<()>,
    shared: Arc<Shared>,
}
impl Pusher {
    /// Device pixels per logical pixel of the window; 1.0 until it opens. Decode images at this scale to keep them sharp.
    pub fn scale(&self) -> f32 {
        self.shared.scale()
    }
    /// Calls `f` on the window thread with the new scale whenever it changes, and once when the window opens. Replaces the previous callback.
    pub fn on_scale(&self, f: impl Fn(f32) + Send + 'static) {
        *self.shared.on_scale.lock().unwrap() = Some(Box::new(f));
    }
    /// Runs a nonblocking native data binding on the window thread before layout.
    pub fn on_frame(&self, update: impl FnMut(&mut Host) + Send + 'static) {
        *self.shared.on_frame.lock().unwrap() = Some(Box::new(update));
    }
    /// Delivers the native paths of files dropped into the window to the Rust embedder.
    pub fn on_file_drop(&self, handler: impl Fn(Vec<PathBuf>) + Send + 'static) {
        *self.shared.on_drop.lock().unwrap() = Some(Box::new(handler));
    }
    /// Requests redraw after native data changed without a JS message.
    pub fn redraw(&self) -> bool {
        self.proxy.send_event(()).is_ok()
    }
    /// Returns false once the window has closed.
    pub fn push(&self, json: String) -> bool {
        self.send(Message::Push(json))
    }
    /// Like `push`, but coalesces bursts and queued backlog entries sharing `key` so only the latest payload is delivered.
    pub fn push_keyed(&self, key: impl Into<String>, json: String) -> bool {
        self.send(Message::PushKeyed {
            key: key.into(),
            text: json,
        })
    }
    /// Sets the pixels of the image that `Node::Image` names as `source` (straight RGBA8).
    pub fn image(&self, source: String, rgba: Vec<u8>, size: [u32; 2]) -> bool {
        self.send(Message::Image { source, rgba, size })
    }
    pub fn remove_image(&self, source: String) -> bool {
        self.send(Message::RemoveImage(source))
    }
    /// Loads a TTF or OTF font file for `Font.family` (`Host::add_font`).
    pub fn font(&self, bytes: Vec<u8>) -> bool {
        self.send(Message::Font(bytes))
    }
    /// Sets the family of text without one (`Host::set_default_family`).
    /// Turns reduced motion on or off: infinite keyframe animations do not run and their nodes show their base values.
    // shortcut: the window does not read the OS reduced-motion preference, so the program passes it here; query the
    // preference in the window on each platform once that is needed.
    pub fn reduced_motion(&self, on: bool) -> bool {
        self.send(Message::ReducedMotion(on))
    }
    pub fn default_family(&self, family: String) -> bool {
        self.send(Message::DefaultFamily(family))
    }
    /// Replaces the app's code with `script` (SDK + app), keeping its state where it can (`Host::reload`). If the
    /// new code fails, the window prints `jsnir: reload failed` and the previous app keeps running.
    pub fn reload(&self, script: String) -> bool {
        self.send(Message::Reload {
            script,
            preserve_state: true,
        })
    }
    /// Replaces the app's code with `script` and resets all JS signals and native focus/scroll state (`Host::hard_reload`).
    pub fn hard_reload(&self, script: String) -> bool {
        self.send(Message::Reload {
            script,
            preserve_state: false,
        })
    }
    /// Enables or disables automatic display of reload failures in the native error overlay.
    pub fn enable_dev_overlay(&self, enabled: bool) -> bool {
        self.send(Message::DevOverlay(enabled))
    }
    /// Shows or clears the native development error overlay above the app.
    pub fn dev_error(&self, error: Option<DevError>) -> bool {
        self.send(Message::DevError(error))
    }
    /// Updates the native window title.
    pub fn title(&self, title: String) -> bool {
        self.send(Message::Title(title))
    }
    /// Closes the window cleanly from another thread (e.g. a dev CLI shortcut).
    pub fn close(&self) -> bool {
        self.send(Message::Close)
    }
    fn send(&self, message: Message) -> bool {
        self.tx.send(message).is_ok() && self.proxy.send_event(()).is_ok()
    }
}

/// Drops superseded `Message::PushKeyed` entries in a drained burst while preserving
/// chronological order relative to all other messages (`Push`, `Image`, `Reload`, etc.).
fn coalesce_messages(messages: Vec<Message>) -> Vec<Message> {
    let keyed_count = messages
        .iter()
        .filter(|m| matches!(m, Message::PushKeyed { .. }))
        .count();
    if keyed_count <= 1 {
        return messages;
    }
    let mut last_index: std::collections::HashMap<String, usize> =
        std::collections::HashMap::with_capacity(keyed_count);
    for (idx, msg) in messages.iter().enumerate() {
        if let Message::PushKeyed { key, .. } = msg {
            last_index.insert(key.clone(), idx);
        }
    }
    messages
        .into_iter()
        .enumerate()
        .filter_map(|(idx, msg)| match &msg {
            Message::PushKeyed { key, .. } if last_index.get(key) != Some(&idx) => None,
            _ => Some(msg),
        })
        .collect()
}
/// Waits until the app has answered and every transition has settled, then captures a frame of `size` logical
/// pixels at `scale` device pixels per logical pixel, into a new PPM file at `path`. Keyframe animations do not
/// run in a capture: each shows its value `host.keyframes_at` ms after its own start (0 unless the program set
/// it), so infinite animations settle too.
pub fn capture_at(
    mut host: Host,
    size: [u32; 2],
    scale: f32,
    focus: bool,
    path: &Path,
) -> Result<()> {
    if !scale.is_finite() || scale <= 0. {
        return Err("the capture scale must be positive".into());
    }
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let adapter = pollster::block_on(instance.request_adapter(&Default::default()))?;
    let mut renderer = Renderer::new(&adapter, wgpu::TextureFormat::Rgba8Unorm)?;
    let max_side = renderer.device.limits().max_texture_dimension_2d;
    let pixels = size.map(|side| (side as f32 * scale).round() as u32);
    if pixels.iter().any(|&side| side > max_side) {
        return Err(format!("capture dimensions exceed the GPU limit of {max_side} pixels").into());
    }
    let [width, height] = size.map(|side| side as f32);
    host.hide_caret = true;
    host.keyframes_at.get_or_insert(0.);
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut focused = false;
    let mut settled = 0;
    loop {
        host.poll();
        if focus && !focused {
            host.focus_first_input();
            focused = true;
        }
        let frame = host.frame(width, height, scale, max_side as usize);
        settled = if frame.animating || host.busy() {
            0
        } else {
            settled + 1
        };
        if settled >= 3 {
            break;
        }
        if Instant::now() > deadline {
            return Err("the app did not settle within 5 s".into());
        }
        std::thread::sleep(Duration::from_millis(16));
    }
    let frame = host.frame(width, height, scale, max_side as usize);
    let draws = View::default().draws(&mut host, &mut renderer, frame.items, scale);
    renderer.capture(pixels, scale, &draws, path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use jsnir::text::TextEngine;

    #[test]
    fn the_scale_starts_at_one_and_reports_each_change() {
        let shared = Shared::default();
        assert_eq!(shared.scale(), 1.);
        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink = seen.clone();
        *shared.on_scale.lock().unwrap() =
            Some(Box::new(move |scale| sink.lock().unwrap().push(scale)));
        shared.set_scale(1.5);
        assert_eq!(shared.scale(), 1.5);
        assert_eq!(*seen.lock().unwrap(), [1.5]);
    }

    #[test]
    fn coalesce_messages_keeps_latest_per_key_and_preserves_order() {
        let messages = vec![
            Message::PushKeyed {
                key: "screen".into(),
                text: "s1".into(),
            },
            Message::Push("fifo1".into()),
            Message::PushKeyed {
                key: "screen".into(),
                text: "s2".into(),
            },
            Message::PushKeyed {
                key: "sessions".into(),
                text: "a1".into(),
            },
            Message::Title("icy".into()),
            Message::PushKeyed {
                key: "screen".into(),
                text: "s3".into(),
            },
            Message::Push("fifo2".into()),
            Message::PushKeyed {
                key: "sessions".into(),
                text: "a2".into(),
            },
        ];
        let summary: Vec<String> = coalesce_messages(messages)
            .into_iter()
            .map(|m| match m {
                Message::Push(t) => format!("push:{t}"),
                Message::PushKeyed { key, text } => format!("keyed:{key}={text}"),
                Message::Title(t) => format!("title:{t}"),
                _ => "other".into(),
            })
            .collect();
        assert_eq!(
            summary,
            [
                "push:fifo1",
                "title:icy",
                "keyed:screen=s3",
                "push:fifo2",
                "keyed:sessions=a2",
            ]
        );
    }

    // A 20 by 20 logical box that pulses forever: the capture settles, and shows the pulse at its first keyframe.
    #[test]
    fn an_infinite_animation_is_captured_at_its_first_keyframe() {
        let script = r#"function jsnirMount() { jsnir.batch(JSON.stringify({ ops: [
            { op: 'create', id: 1, node: { kind: 'column' } },
            { op: 'create', id: 2, parent: 1, node: { kind: 'box', style: { width: 20, height: 20 },
                paint: { background: 4294967295, animations: [{ property: 'opacity', easing: 'easeInOut', duration: 3000,
                    keyframes: [{ offset: 0, value: 0.25 }, { offset: 0.5, value: 1 }, { offset: 1, value: 0.25 }] }] } } },
        ] })); }"#;
        let host = Host::new(script.into(), "{}".into(), TextEngine::default());
        let path = std::env::temp_dir().join(format!("jsnir-pulse-{}.ppm", std::process::id()));
        let _ = std::fs::remove_file(&path);
        capture_at(host, [20, 20], 1., false, &path).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        // White at opacity 0.25 over the black target.
        let pixel = &bytes[bytes.len() - 20 * 20 * 3..][..3];
        assert!(pixel.iter().all(|&c| c.abs_diff(64) <= 1), "{pixel:?}");
    }

    // A 40 by 40 device-pixel image at logical x 10.3 (device 20.6) is drawn at device 21, and its alpha mask, which
    // alternates columns, must move with it: each column of the image is red exactly where the mask is opaque.
    #[test]
    fn a_masked_image_drawn_1_to_1_is_snapped_with_its_mask() {
        let script = r#"function jsnirMount() { jsnir.batch(JSON.stringify({ ops: [
            { op: 'create', id: 1, node: { kind: 'column' } },
            { op: 'create', id: 2, parent: 1, node: { kind: 'image', source: 'red',
                style: { position: 'absolute', left: 10.3, top: 10.3, width: 20, height: 20 }, paint: { mask: 'columns' } } },
        ] })); }"#;
        let mut host = Host::new(script.into(), "{}".into(), TextEngine::default());
        let red: Vec<u8> = [255, 0, 0, 255].repeat(40 * 40);
        host.set_image("red".into(), red, [40, 40]);
        let columns: Vec<u8> = (0..40 * 40)
            .flat_map(|i| [255, 255, 255, if (i % 40) % 2 == 0 { 255 } else { 0 }])
            .collect();
        host.set_image("columns".into(), columns, [40, 40]);
        let path = std::env::temp_dir().join(format!("jsnir-mask-{}.ppm", std::process::id()));
        let _ = std::fs::remove_file(&path);
        capture_at(host, [40, 40], 2., false, &path).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        assert!(bytes.starts_with(
            b"P6
80 80
255
"
        ));
        let body = &bytes[bytes.len() - 80 * 80 * 3..];
        for (i, pixel) in body.as_chunks::<3>().0.iter().enumerate() {
            let (x, y) = (i % 80, i / 80);
            let inside = (21..61).contains(&x) && (21..61).contains(&y);
            let expected = if inside && (x - 21) % 2 == 0 {
                [255, 0, 0]
            } else {
                [0, 0, 0]
            };
            assert_eq!(pixel, &expected, "device pixel {x} {y}");
        }
    }

    // A 20 by 20 logical image at scale 2 covers the 40 by 40 checkerboard exactly, so every device pixel is its source pixel.
    #[test]
    fn an_image_at_its_pixel_size_is_drawn_one_to_one() {
        let mut host = Host::new(jsnir::SHARP_APP.into(), "{}".into(), TextEngine::default());
        let rgba: Vec<u8> = (0..40 * 40)
            .flat_map(|i| {
                let (x, y) = (i % 40, i / 40);
                let v = if (x + y) % 2 == 0 { 0 } else { 255 };
                [v, v, v, 255]
            })
            .collect();
        host.set_image("checker".into(), rgba.clone(), [40, 40]);
        let path = std::env::temp_dir().join(format!("jsnir-sharp-{}.ppm", std::process::id()));
        let _ = std::fs::remove_file(&path);
        capture_at(host, [20, 20], 2., false, &path).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        assert!(bytes.starts_with(b"P6\n40 40\n255\n"));
        let body = &bytes[bytes.len() - 40 * 40 * 3..];
        for (i, pixel) in body.as_chunks::<3>().0.iter().enumerate() {
            assert_eq!(
                pixel,
                &rgba[i * 4..i * 4 + 3],
                "device pixel {} {}",
                i % 40,
                i / 40
            );
        }
    }
}
