#[cfg(feature = "examples")]
mod chat;
#[cfg(feature = "examples")]
mod window;

#[cfg(feature = "examples")]
use chat::Scene;
#[cfg(feature = "examples")]
use jsnir::{Mode, editor::EditorEvent};
use jsnir::{host::Host, text::TextEngine};
use jsnir_gpu::open;
#[cfg(feature = "examples")]
use jsnir_gpu::renderer::{self, Renderer};
#[cfg(any(feature = "examples", feature = "stepper"))]
use std::path::Path;
#[cfg(feature = "examples")]
use std::time::{Duration, Instant};
use std::{ffi::OsString, fs, path::PathBuf};
#[cfg(feature = "examples")]
use winit::event_loop::EventLoop;

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

const USAGE: &str =
    "usage: jsnir-gpu run APP.js [--title TITLE] [--size WIDTHxHEIGHT] [--offscreen file.ppm]
       jsnir-gpu --version";

fn main() -> Result<()> {
    let mut args = std::env::args_os().skip(1);
    match args.next().as_deref().and_then(|arg| arg.to_str()) {
        Some("--version" | "-V") => {
            println!("jsnir-gpu {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        Some("--help" | "-h") => {
            println!("{USAGE}");
            #[cfg(feature = "examples")]
            println!("       jsnir-gpu rust|js [--offscreen file.ppm]");
            #[cfg(feature = "stepper")]
            println!(
                "       jsnir-gpu stepper [--step N] [--name NAME] [--plan pro|starter] [--focus] [--offscreen file.ppm]"
            );
            Ok(())
        }
        Some("run") => run_external(args),
        #[cfg(feature = "stepper")]
        Some("stepper") => stepper(std::env::args().skip(2).collect()),
        #[cfg(feature = "examples")]
        None | Some("rust" | "js") => run_chat(),
        _ => Err(USAGE.into()),
    }
}

fn run_external(mut args: impl Iterator<Item = OsString>) -> Result<()> {
    let source = PathBuf::from(args.next().ok_or(USAGE)?);
    let mut title = String::from("JSNIR");
    let mut size = [800, 600];
    let mut output = None;
    while let Some(flag) = args.next() {
        let value = args.next().ok_or(USAGE)?;
        match flag.to_str() {
            Some("--title") => title = value.into_string().map_err(|_| "title must be UTF-8")?,
            Some("--size") => {
                size = value
                    .to_str()
                    .and_then(window_size)
                    .ok_or("expected a positive WIDTHxHEIGHT")?
            }
            Some("--offscreen") => output = Some(PathBuf::from(value)),
            _ => return Err(USAGE.into()),
        }
    }
    let script =
        fs::read_to_string(&source).map_err(|error| format!("{}: {error}", source.display()))?;
    let host = Host::try_new(script, "{}".into(), TextEngine::default())
        .map_err(|error| format!("{}: {error}", source.display()))?;
    open(host, &title, size, false, output.as_deref())
}

fn window_size(value: &str) -> Option<[u32; 2]> {
    let (width, height) = value.split_once('x')?;
    let size = [width.parse().ok()?, height.parse().ok()?];
    (!size.contains(&0)).then_some(size)
}

#[cfg(feature = "examples")]
fn run_chat() -> Result<()> {
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

#[cfg(feature = "examples")]
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
    renderer.capture(size, 1., &draws, path)
}

/// Inter, the only font of the stepper comparison (OFL, assets/fonts).
#[cfg(feature = "stepper")]
fn inter() -> TextEngine {
    const REGULAR: &[u8] = include_bytes!("../../../assets/fonts/Inter-Regular.ttf");
    const SEMIBOLD: &[u8] = include_bytes!("../../../assets/fonts/Inter-SemiBold.ttf");
    TextEngine::with_fonts(&[REGULAR, SEMIBOLD], [244, 244, 245])
}

/// The React Bits Stepper port (app/stepper.tsx), in a window or captured offscreen in a settled state.
#[cfg(feature = "stepper")]
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
    let host = Host::new(jsnir::apps::STEPPER.into(), state, inter());
    open(
        host,
        "Stepper · JSNIR",
        [640, 560],
        focus,
        output.as_deref().map(Path::new),
    )
}
