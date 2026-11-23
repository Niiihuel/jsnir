use clap::{Parser, Subcommand, ValueEnum};
use jsnir::{
    host::{DevError, Host},
    text::TextEngine,
};
use jsnir_dev::{
    DevConfig, JSNIR_DTS, ModuleGraph, SDK_TS, TypecheckMode, TypecheckOutcome, assets,
    compile_entry, compiled_sdk, find_tsc, parse_size, run_typecheck, session, shortcuts,
    start_dev,
};
use std::{
    fs,
    path::{Path, PathBuf},
    process::ExitCode,
    time::Instant,
};

#[derive(Parser)]
#[command(
    name = "jsnir",
    version,
    about = "Vite-style dev mode, fast Oxc compiler, and CLI for JSNIR apps"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Clone, Copy, ValueEnum)]
enum CliTypecheck {
    Async,
    Blocking,
    Off,
}

impl From<CliTypecheck> for TypecheckMode {
    fn from(value: CliTypecheck) -> Self {
        match value {
            CliTypecheck::Async => TypecheckMode::Async,
            CliTypecheck::Blocking => TypecheckMode::Blocking,
            CliTypecheck::Off => TypecheckMode::Off,
        }
    }
}

#[derive(Subcommand)]
enum Commands {
    /// Start development mode with instant Oxc hot reload, error overlay, and TTY shortcuts.
    #[command(alias = "serve")]
    Dev {
        /// Entry TSX/TS file or project directory (defaults to jsnir.toml or src/app.tsx).
        entry: Option<PathBuf>,
        /// Window title override.
        #[arg(long)]
        title: Option<String>,
        /// Window logical size as WIDTHxHEIGHT (e.g. 1024x700).
        #[arg(long)]
        size: Option<String>,
        /// JSON startup arguments exposed via `args()`.
        #[arg(long)]
        args: Option<String>,
        /// Typecheck strategy during dev.
        #[arg(long, value_enum)]
        typecheck: Option<CliTypecheck>,
        /// Disable the in-window error overlay.
        #[arg(long)]
        no_overlay: bool,
        /// Clear the terminal on every reload.
        #[arg(long)]
        clear: bool,
    },
    /// Typecheck and compile a standalone production JS or QuickJS bytecode (`.jsbc`) bundle.
    Build {
        /// Entry TSX/TS file or project directory.
        entry: Option<PathBuf>,
        /// Output bundle path (defaults to dist/app.js, or dist/app.jsbc with --bytecode).
        #[arg(short, long)]
        out: Option<PathBuf>,
        /// Skip `tsc --noEmit` before bundling.
        #[arg(long)]
        no_typecheck: bool,
        /// Emit precompiled QuickJS bytecode (`.jsbc`) to skip JS parsing at startup.
        #[arg(long)]
        bytecode: bool,
    },
    /// Run an already-compiled standalone JS or `.jsbc` bytecode bundle without dev tooling.
    Run {
        /// Compiled bundle `.js` or `.jsbc` file.
        bundle: PathBuf,
        /// Window title.
        #[arg(long, default_value = "JSNIR")]
        title: String,
        /// Window logical size as WIDTHxHEIGHT.
        #[arg(long, default_value = "960x640")]
        size: String,
        /// JSON startup arguments exposed via `args()`.
        #[arg(long, default_value = "{}")]
        args: String,
    },
    /// Run TypeScript type-checking against the JSNIR contract (`jsnir.d.ts`).
    Check {
        /// Entry TSX/TS file or project directory.
        entry: Option<PathBuf>,
    },
    /// Scaffold a new JSNIR TSX project with jsnir.toml, tsconfig.json, and multi-file example.
    Init {
        /// Target directory (defaults to current directory).
        dir: Option<PathBuf>,
        /// Project / window title.
        #[arg(long)]
        name: Option<String>,
    },
    /// Render a deterministic settled offscreen frame to a PPM image.
    Capture {
        /// Entry TSX/TS file, compiled JS bundle, or project directory.
        entry: Option<PathBuf>,
        /// Output PPM file path.
        #[arg(short, long)]
        out: PathBuf,
        /// Logical size as WIDTHxHEIGHT.
        #[arg(long)]
        size: Option<String>,
        /// Device pixel scale factor.
        #[arg(long, default_value_t = 1.0)]
        scale: f32,
        /// Focus the first TextInput before capture.
        #[arg(long)]
        focus: bool,
        /// JSON startup arguments exposed via `args()`.
        #[arg(long)]
        args: Option<String>,
    },
}

const PLACEHOLDER_APP: &str = r#"mount(() => Box({ style: { width: 1, height: 1 } }));"#;

fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match cli.command {
        None => cmd_dev(None, None, None, None, None, false, false),
        Some(Commands::Dev {
            entry,
            title,
            size,
            args,
            typecheck,
            no_overlay,
            clear,
        }) => cmd_dev(entry, title, size, args, typecheck, no_overlay, clear),
        Some(Commands::Build {
            entry,
            out,
            no_typecheck,
            bytecode,
        }) => cmd_build(entry, out, no_typecheck, bytecode),
        Some(Commands::Run {
            bundle,
            title,
            size,
            args,
        }) => cmd_run(bundle, title, size, args),
        Some(Commands::Check { entry }) => cmd_check(entry),
        Some(Commands::Init { dir, name }) => cmd_init(dir, name),
        Some(Commands::Capture {
            entry,
            out,
            size,
            scale,
            focus,
            args,
        }) => cmd_capture(entry, out, size, scale, focus, args),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("\x1b[1;31merror:\x1b[0m {err}");
            ExitCode::FAILURE
        }
    }
}

fn cmd_dev(
    entry: Option<PathBuf>,
    title: Option<String>,
    size: Option<String>,
    args: Option<String>,
    typecheck: Option<CliTypecheck>,
    no_overlay: bool,
    clear: bool,
) -> Result<(), String> {
    let started = Instant::now();
    let mut config = DevConfig::discover(entry.as_deref())?;
    if let Some(title) = title {
        config.app.title = title;
    }
    if let Some(size) = size {
        config.app.size = parse_size(&size)?;
    }
    if let Some(args) = args {
        config.app.args = args;
    }
    if let Some(tc) = typecheck {
        config.dev.typecheck = tc.into();
    }
    if no_overlay {
        config.dev.overlay = false;
    }
    if clear {
        config.dev.clear_screen = true;
    }

    let mut graph = ModuleGraph::new(&config.root);
    let mut initial_error: Option<DevError> = None;
    let initial_script = match graph.build(&config.app.entry) {
        Ok(bundle) => bundle.code,
        Err(err) => {
            session::print_dev_error(&err);
            initial_error = Some(err);
            format!("{}\n{PLACEHOLDER_APP}", compiled_sdk().map_err(|e| e.message)?)
        }
    };

    let mut host = match Host::try_new(initial_script, config.app.args.clone(), TextEngine::default()) {
        Ok(host) => host,
        Err(err) => {
            let dev_err = DevError {
                title: "Mount error".into(),
                message: err,
                location: Some(config.app.entry.display().to_string()),
                frame: None,
            };
            session::print_dev_error(&dev_err);
            initial_error = Some(dev_err);
            let fallback = format!("{}\n{PLACEHOLDER_APP}", compiled_sdk().map_err(|e| e.message)?);
            Host::try_new(fallback, config.app.args.clone(), TextEngine::default())?
        }
    };

    assets::load_into_host(
        &mut host,
        &config.assets.fonts,
        config.assets.default_family.as_deref(),
        config.assets.images_dir.as_deref(),
    );
    host.enable_dev_overlay(config.dev.overlay);
    if config.dev.overlay && initial_error.is_some() {
        host.set_dev_error(initial_error);
    }

    let tc_label = match config.dev.typecheck {
        TypecheckMode::Off => "off".to_string(),
        TypecheckMode::Async => match find_tsc(&config.root) {
            Some(_) => "tsc (async)".to_string(),
            None => "tsc not found (skipped)".to_string(),
        },
        TypecheckMode::Blocking => match find_tsc(&config.root) {
            Some(_) => "tsc (blocking)".to_string(),
            None => "tsc not found".to_string(),
        },
    };
    let entry_display = jsnir_dev::compiler::module_id_for(&config.root, &config.app.entry);
    shortcuts::print_banner(
        &entry_display,
        config.app.size,
        &tc_label,
        started.elapsed().as_millis(),
    );

    let title = config.app.title.clone();
    let size = config.app.size;
    // Keep DevSession alive until the window event loop exits.
    let mut session_slot = None;
    jsnir_gpu::open_with(host, &title, size, |pusher| {
        match start_dev(config, pusher, Some(graph), true) {
            Ok(session) => session_slot = Some(session),
            Err(err) => eprintln!("\x1b[1;31merror:\x1b[0m {err}"),
        }
    })
    .map_err(|e| e.to_string())?;
    drop(session_slot);
    Ok(())
}

fn cmd_build(
    entry: Option<PathBuf>,
    out: Option<PathBuf>,
    no_typecheck: bool,
    bytecode: bool,
) -> Result<(), String> {
    let config = DevConfig::discover(entry.as_deref())?;
    if !no_typecheck {
        match run_typecheck(&config.root, &config.app.entry) {
            TypecheckOutcome::Ok(elapsed) => {
                eprintln!("\x1b[32m✓\x1b[0m typecheck passed ({} ms)", elapsed.as_millis());
            }
            TypecheckOutcome::Errors { output, .. } => {
                return Err(format!("typecheck failed:\n{output}"));
            }
            TypecheckOutcome::Unavailable(msg) => {
                eprintln!("\x1b[33m!\x1b[0m {msg} — continuing without typecheck");
            }
        }
    }
    let bundle = compile_entry(&config.root, &config.app.entry).map_err(|e| {
        session::print_dev_error(&e);
        e.message
    })?;
    let default_name = if bytecode {
        "dist/app.jsbc"
    } else {
        "dist/app.js"
    };
    let out_path = out.unwrap_or_else(|| config.root.join(default_name));
    if let Some(parent) = out_path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    let (bytes, kind, elapsed_ms) = if bytecode {
        let bc_started = Instant::now();
        let bytes = jsnir::compile_bytecode(&bundle.code)?;
        let total = bundle.elapsed + bc_started.elapsed();
        (bytes, "bytecode", total.as_millis())
    } else {
        (bundle.code.into_bytes(), "js", bundle.elapsed.as_millis())
    };
    fs::write(&out_path, &bytes).map_err(|e| format!("{}: {e}", out_path.display()))?;
    let kb = bytes.len() as f64 / 1024.0;
    eprintln!(
        "\x1b[32m✓\x1b[0m built \x1b[1m{}\x1b[0m ({:.1} kB · {} · {} module{} · {} ms)",
        out_path.display(),
        kb,
        kind,
        bundle.files.len(),
        if bundle.files.len() == 1 { "" } else { "s" },
        elapsed_ms
    );
    Ok(())
}

fn cmd_run(bundle: PathBuf, title: String, size: String, args: String) -> Result<(), String> {
    let size = parse_size(&size)?;
    let bytes = fs::read(&bundle).map_err(|e| format!("{}: {e}", bundle.display()))?;
    let host = Host::try_new_auto(bytes, args, TextEngine::default())
        .map_err(|e| format!("{}: {e}", bundle.display()))?;
    jsnir_gpu::open(host, &title, size, false, None).map_err(|e| e.to_string())
}

fn cmd_check(entry: Option<PathBuf>) -> Result<(), String> {
    let config = DevConfig::discover(entry.as_deref())?;
    match run_typecheck(&config.root, &config.app.entry) {
        TypecheckOutcome::Ok(elapsed) => {
            eprintln!("\x1b[32m✓\x1b[0m no type errors ({} ms)", elapsed.as_millis());
            Ok(())
        }
        TypecheckOutcome::Errors { output, .. } => Err(format!("type errors found:\n{output}")),
        TypecheckOutcome::Unavailable(msg) => Err(msg),
    }
}

fn cmd_capture(
    entry: Option<PathBuf>,
    out: PathBuf,
    size: Option<String>,
    scale: f32,
    focus: bool,
    args: Option<String>,
) -> Result<(), String> {
    let mut config = DevConfig::discover(entry.as_deref())?;
    if let Some(size) = size {
        config.app.size = parse_size(&size)?;
    }
    if let Some(args) = args {
        config.app.args = args;
    }
    let mut host = if config
        .app
        .entry
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e == "js" || e == "jsbc")
    {
        let bytes = fs::read(&config.app.entry)
            .map_err(|e| format!("{}: {e}", config.app.entry.display()))?;
        Host::try_new_auto(bytes, config.app.args, TextEngine::default())?
    } else {
        let script = compile_entry(&config.root, &config.app.entry)
            .map_err(|e| {
                session::print_dev_error(&e);
                e.message
            })?
            .code;
        Host::try_new(script, config.app.args, TextEngine::default())?
    };
    assets::load_into_host(
        &mut host,
        &config.assets.fonts,
        config.assets.default_family.as_deref(),
        config.assets.images_dir.as_deref(),
    );
    jsnir_gpu::capture_at(host, config.app.size, scale, focus, &out).map_err(|e| e.to_string())?;
    eprintln!("\x1b[32m✓\x1b[0m captured {}", out.display());
    Ok(())
}

fn cmd_init(dir: Option<PathBuf>, name: Option<String>) -> Result<(), String> {
    let target = match dir {
        Some(d) => d,
        None => std::env::current_dir().map_err(|e| e.to_string())?,
    };
    let title = name.unwrap_or_else(|| {
        target
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("JSNIR App")
            .to_string()
    });
    fs::create_dir_all(target.join("src")).map_err(|e| e.to_string())?;
    fs::create_dir_all(target.join(".jsnir")).map_err(|e| e.to_string())?;

    write_if_missing(
        &target.join("jsnir.toml"),
        &format!(
            r#"[app]
entry = "src/app.tsx"
title = {title:?}
size = [960, 640]

[dev]
typecheck = "async"
overlay = true
clear_screen = false
debounce_ms = 40
"#
        ),
    )?;

    write_if_missing(
        &target.join("tsconfig.json"),
        r#"{
  "compilerOptions": {
    "strict": true,
    "target": "es2020",
    "lib": ["es2020"],
    "module": "es2020",
    "moduleResolution": "bundler",
    "jsx": "react",
    "jsxFactory": "h",
    "noEmit": true,
    "skipLibCheck": true,
    "types": ["./.jsnir/jsnir.d.ts"]
  },
  "include": [".jsnir/jsnir.d.ts", ".jsnir/sdk.ts", "src/**/*.ts", "src/**/*.tsx"]
}
"#,
    )?;

    fs::write(target.join(".jsnir/jsnir.d.ts"), JSNIR_DTS).map_err(|e| e.to_string())?;
    fs::write(target.join(".jsnir/sdk.ts"), SDK_TS).map_err(|e| e.to_string())?;

    write_if_missing(
        &target.join("src/counter.tsx"),
        r#"export function Counter(): Element {
    const count = signal(0);
    return (
        <Row style={{ gap: 12, align: "center" }}>
            <Box
                style={{ padding: [10, 16, 10, 16] }}
                paint={{
                    background: 0x3b82f6ff,
                    radius: 8,
                    hover: { brightness: 1.12 },
                    active: { scale: 0.97 },
                }}
                onPress={() => count.update((n) => n + 1)}
            >
                <Text font={{ size: 14, weight: 600 }} paint={{ color: 0xffffffff }}>
                    {() => `Clicks: ${count.value}`}
                </Text>
            </Box>
        </Row>
    );
}
"#,
    )?;

    write_if_missing(
        &target.join("src/app.tsx"),
        r#"import { Counter } from "./counter";

function App(): Element {
    const sidebarWidth = signal(220);
    let baseWidth = 220;

    return (
        <Row style={{ grow: 1, height: 640 }} paint={{ background: 0x11131aff }}>
            <Column
                style={() => ({ width: sidebarWidth.value, shrink: 0, padding: 20, gap: 12 })}
                paint={{ background: 0x181b25ff }}
            >
                <Text font={{ size: 16, weight: 700 }} paint={{ color: 0xf8fafcff }}>
                    {"JSNIR Dev"}
                </Text>
                <Text font={{ size: 13 }} paint={{ color: 0x94a3b8ff }}>
                    {"Edit src/app.tsx or src/counter.tsx and save for instant hot reload."}
                </Text>
            </Column>
            <Box
                style={{ width: 6, shrink: 0 }}
                paint={{
                    background: 0x262a3aff,
                    cursor: "colResize",
                    hover: { background: 0x3b82f6ff },
                    active: { background: 0x60a5faff },
                }}
                onDrag={(e) => {
                    if (e.phase === "start") baseWidth = sidebarWidth.value;
                    else if (e.phase === "move") {
                        sidebarWidth.value = Math.max(140, Math.min(420, baseWidth + e.dx));
                    }
                }}
            />
            <Column style={{ grow: 1, padding: 28, gap: 16 }}>
                <Text font={{ size: 22, weight: 700 }} paint={{ color: 0xf8fafcff }}>
                    {"Hello from JSNIR"}
                </Text>
                <Counter />
            </Column>
        </Row>
    );
}

mount(App);
"#,
    )?;

    write_if_missing(&target.join(".gitignore"), "dist/\n.jsnir/\n")?;

    eprintln!("\x1b[32m✓\x1b[0m created JSNIR project in {}", target.display());
    eprintln!("  Run: \x1b[1mjsnir dev\x1b[0m");
    Ok(())
}

fn write_if_missing(path: &Path, content: &str) -> Result<(), String> {
    if path.exists() {
        return Ok(());
    }
    fs::write(path, content).map_err(|e| format!("{}: {e}", path.display()))
}
