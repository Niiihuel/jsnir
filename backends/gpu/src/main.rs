mod app;
mod chat;
mod view;
mod window;

use chat::Scene;
use jsnir::{Mode, editor::EditorEvent, host::Host, text::TextEngine};
use jsnir_gpu::renderer::{self, Renderer};
use std::{
    path::Path,
    time::{Duration, Instant},
};
use winit::event_loop::EventLoop;

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

const USAGE: &str = "usage: jsnir-gpu rust|js [--offscreen file.ppm]
       jsnir-gpu stepper [--step N] [--name NAME] [--plan pro|starter] [--focus] [--offscreen file.ppm]";

fn main() -> Result<()> {
    if std::env::args().nth(1).as_deref() == Some("stepper") {
        return stepper(std::env::args().skip(2).collect());
    }
    let mode = Mode::from_args();
    let mut args = std::env::args_os().skip(2);
    if let Some(command) = args.next() {
        if command == "--offscreen" {
            let output = args.next().ok_or("expected a new PPM output path")?;
            return offscreen(mode, Path::new(&output));
        }
        return Err(USAGE.into());
    }
    let event_loop = EventLoop::new()?;
    let mut app = window::App::new(mode);
    event_loop.run_app(&mut app)?;
    app.finish()
}

fn offscreen(mode: Mode, path: &Path) -> Result<()> {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let adapter = pollster::block_on(instance.request_adapter(&Default::default()))?;
    eprintln!("wgpu · {:?}", adapter.get_info());
    let mut renderer = Renderer::new(&adapter, wgpu::TextureFormat::Rgba8Unorm)?;
    let mut scene = Scene::new(mode, Duration::ZERO);
    scene.chat.edit(&[
        EditorEvent::Insert("Hello, Córdoba".into()),
        EditorEvent::Enter,
    ]);
    let deadline = Instant::now() + Duration::from_secs(3);
    while scene.chat.pending() {
        if Instant::now() > deadline {
            return Err("the chat did not resolve the send".into());
        }
        scene.chat.poll();
        std::thread::yield_now();
    }
    if scene.chat.history_len() != 1 || scene.chat.message(0) != "Hello, Córdoba" {
        return Err("the chat did not keep the message".into());
    }
    scene.chat.set_draft("second draft", [8, 16]);
    scene.chat.publish_input();
    let size = [640, 480];
    let (draws, _) = scene.draws(&renderer, size, 1., true, false);
    renderer.capture(size, &draws, path)
}

/// Inter, the only font of the stepper comparison (OFL, assets/fonts).
fn inter() -> TextEngine {
    const REGULAR: &[u8] = include_bytes!("../../../assets/fonts/Inter-Regular.ttf");
    const SEMIBOLD: &[u8] = include_bytes!("../../../assets/fonts/Inter-SemiBold.ttf");
    TextEngine::with_fonts(&[REGULAR, SEMIBOLD], [244, 244, 245])
}

/// The React Bits Stepper port (app/stepper.tsx), in a window or captured offscreen in a settled state.
fn stepper(args: Vec<String>) -> Result<()> {
    let mut state = serde_json::Map::new();
    let (mut focus, mut output) = (false, None);
    let mut args = args.into_iter();
    while let Some(flag) = args.next() {
        let mut value = || args.next().ok_or(USAGE);
        match flag.as_str() {
            "--step" => {
                let step: u32 = value()?.parse().map_err(|_| USAGE)?;
                state.insert("step".into(), step.into());
            }
            "--name" => {
                state.insert("name".into(), value()?.into());
            }
            "--plan" => {
                state.insert("plan".into(), value()?.into());
            }
            "--focus" => focus = true,
            "--offscreen" => output = Some(value()?),
            _ => return Err(USAGE.into()),
        }
    }
    let state = serde_json::Value::Object(state).to_string();
    let mut host = Host::new(jsnir::apps::STEPPER.into(), state, inter());
    if let Some(output) = output {
        return capture(host, focus, Path::new(&output));
    }
    let event_loop = EventLoop::new()?;
    host.poll();
    if focus {
        host.focus_first_input();
    }
    let mut window = app::AppWindow::new(host, "Stepper · JSNIR", [640., 560.]);
    event_loop.run_app(&mut window)?;
    window.finish()
}

/// Waits until the app has answered and every transition has settled, then captures 640 × 560 at scale 1.
fn capture(mut host: Host, focus: bool, path: &Path) -> Result<()> {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let adapter = pollster::block_on(instance.request_adapter(&Default::default()))?;
    let mut renderer = Renderer::new(&adapter, wgpu::TextureFormat::Rgba8Unorm)?;
    let max_side = renderer.device.limits().max_texture_dimension_2d as usize;
    host.hide_caret = true;
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut focused = false;
    let mut settled = 0;
    loop {
        host.poll();
        if focus && !focused {
            host.focus_first_input();
            focused = true;
        }
        let frame = host.frame(640., 560., 1., max_side);
        settled = if frame.animating || host.busy() {
            0
        } else {
            settled + 1
        };
        if settled >= 3 {
            break;
        }
        if Instant::now() > deadline {
            return Err("the stepper did not settle within 5 s".into());
        }
        std::thread::sleep(Duration::from_millis(16));
    }
    let frame = host.frame(640., 560., 1., max_side);
    let draws = view::View::default().draws(&mut host, &renderer, frame.items, 1.);
    renderer.capture([640, 560], &draws, path)
}
