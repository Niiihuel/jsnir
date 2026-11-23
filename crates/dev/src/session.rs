//! Vite-style live development session: native OS watcher, incremental Oxc rebuilds,
//! asset hot reload, cancellable background typechecking, error overlay, and TTY shortcuts.
use crate::{
    assets,
    compiler::{ModuleGraph, canonical_or_normalized, module_id_for},
    config::{CONFIG_FILES, DevConfig, TypecheckMode},
    shortcuts::{self, ShortcutAction},
    typecheck::{AsyncTypechecker, TypecheckOutcome, find_tsc, run_typecheck},
};
use jsnir::host::DevError;
use jsnir_gpu::Pusher;
use notify_debouncer_mini::{DebouncedEvent, new_debouncer, notify::RecursiveMode};
use std::{
    collections::HashSet,
    path::{Component, Path, PathBuf},
    sync::mpsc::{self, Sender},
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

enum DevEvent {
    Files(Vec<PathBuf>),
    Shortcut(ShortcutAction),
    Stop,
}

/// Handle to a running development session. Dropping it stops the watcher and background threads.
pub struct DevSession {
    tx: Sender<DevEvent>,
}

impl DevSession {
    /// Triggers a soft reload (preserving signal and UI state) programmatically.
    pub fn soft_reload(&self) {
        let _ = self.tx.send(DevEvent::Shortcut(ShortcutAction::SoftReload));
    }

    /// Triggers a hard reload (resetting signals and native state) programmatically.
    pub fn hard_reload(&self) {
        let _ = self.tx.send(DevEvent::Shortcut(ShortcutAction::HardReload));
    }
}

impl Drop for DevSession {
    fn drop(&mut self) {
        let _ = self.tx.send(DevEvent::Stop);
    }
}

/// Starts a full Vite-style development session attached to a live `jsnir_gpu::Pusher`.
///
/// Works identically for the `jsnir dev` CLI and for external Rust applications that embed
/// `jsnir` + `jsnir-gpu` via `open_with`.
pub fn start_dev(
    config: DevConfig,
    pusher: Pusher,
    initial_graph: Option<ModuleGraph>,
    enable_shortcuts: bool,
) -> Result<DevSession, String> {
    pusher.enable_dev_overlay(config.dev.overlay);

    let (tx, rx) = mpsc::channel::<DevEvent>();

    // Native OS watcher with debounce (replaces 250ms polling).
    let watch_tx = tx.clone();
    let debounce = Duration::from_millis(config.dev.debounce_ms.max(10));
    let mut debouncer = new_debouncer(debounce, move |res: Result<Vec<DebouncedEvent>, _>| {
        if let Ok(events) = res {
            let paths: Vec<PathBuf> = events.into_iter().map(|e| e.path).collect();
            if !paths.is_empty() {
                let _ = watch_tx.send(DevEvent::Files(paths));
            }
        }
    })
    .map_err(|e| format!("failed to start file watcher: {e}"))?;

    let mut watched_dirs = HashSet::new();
    let mut add_watch = |path: &Path| {
        let dir = if path.is_dir() {
            path.to_path_buf()
        } else {
            path.parent().unwrap_or(path).to_path_buf()
        };
        if dir.is_dir() && watched_dirs.insert(canonical_or_normalized(&dir)) {
            let _ = debouncer.watcher().watch(&dir, RecursiveMode::Recursive);
        }
    };

    add_watch(&config.root);
    add_watch(&config.app.entry);
    if let Some(images) = &config.assets.images_dir {
        add_watch(images);
    }
    for font in &config.assets.fonts {
        add_watch(font);
    }

    if enable_shortcuts {
        let (stx, srx) = mpsc::channel();
        shortcuts::bind_shortcuts(stx);
        let fwd = tx.clone();
        thread::spawn(move || {
            while let Ok(action) = srx.recv() {
                if fwd.send(DevEvent::Shortcut(action)).is_err() {
                    break;
                }
            }
        });
    }

    let mut graph = initial_graph.unwrap_or_else(|| ModuleGraph::new(&config.root));

    let typechecker = AsyncTypechecker::spawn(|outcome| match outcome {
        TypecheckOutcome::Ok(elapsed) => {
            log_line(
                "\x1b[32mtypes ok\x1b[0m",
                &format!("({} ms)", elapsed.as_millis()),
            );
        }
        TypecheckOutcome::Errors { output, elapsed } => {
            log_line(
                "\x1b[33mtype errors\x1b[0m",
                &format!("({} ms)\n{output}", elapsed.as_millis()),
            );
        }
        TypecheckOutcome::Unavailable(reason) => {
            log_line("\x1b[2mtypes skipped\x1b[0m", &reason);
        }
    });

    // Kick off an initial non-blocking typecheck on startup if configured.
    if config.dev.typecheck == TypecheckMode::Async && find_tsc(&config.root).is_some() {
        typechecker.trigger(config.root.clone(), config.app.entry.clone());
    }

    let handle_tx = tx.clone();
    thread::spawn(move || {
        // Keep the debouncer alive for the lifetime of the dev coordinator loop.
        let _watcher = debouncer;
        let mut config = config;
        while let Ok(event) = rx.recv() {
            match event {
                DevEvent::Stop => break,
                DevEvent::Shortcut(action) => match action {
                    ShortcutAction::HardReload => {
                        graph.invalidate_all();
                        rebuild_and_push(&mut config, &mut graph, &pusher, &typechecker, false, "hard reload");
                    }
                    ShortcutAction::SoftReload => {
                        graph.invalidate_all();
                        rebuild_and_push(&mut config, &mut graph, &pusher, &typechecker, true, "soft reload");
                    }
                    ShortcutAction::Typecheck => {
                        log_line("\x1b[36mtypecheck\x1b[0m", "running tsc --noEmit...");
                        typechecker.trigger(config.root.clone(), config.app.entry.clone());
                    }
                    ShortcutAction::Clear => shortcuts::clear_console(),
                    ShortcutAction::Help => shortcuts::print_help(),
                    ShortcutAction::Quit => {
                        log_line("\x1b[2mbye\x1b[0m", "closing window");
                        pusher.close();
                        break;
                    }
                },
                DevEvent::Files(paths) => {
                    handle_file_changes(&mut config, &mut graph, &pusher, &typechecker, paths);
                }
            }
        }
    });

    Ok(DevSession { tx: handle_tx })
}

fn handle_file_changes(
    config: &mut DevConfig,
    graph: &mut ModuleGraph,
    pusher: &Pusher,
    typechecker: &AsyncTypechecker,
    paths: Vec<PathBuf>,
) {
    let mut unique = Vec::new();
    let mut seen = HashSet::new();
    for path in paths {
        if should_ignore(&path) {
            continue;
        }
        let canon = canonical_or_normalized(&path);
        if seen.insert(canon.clone()) {
            unique.push(canon);
        }
    }
    if unique.is_empty() {
        return;
    }

    let mut config_changed = false;
    let mut code_changed = Vec::new();

    for path in &unique {
        if is_config_file(config, path) {
            config_changed = true;
            continue;
        }
        if is_font_file(path) && is_configured_font(config, path) {
            match assets::push_font(pusher, path) {
                Ok(()) => log_line(
                    "\x1b[35masset\x1b[0m",
                    &format!("reloaded font {}", module_id_for(&config.root, path)),
                ),
                Err(e) => log_line("\x1b[31masset error\x1b[0m", &e),
            }
            continue;
        }
        if is_png_file(path)
            && let Some(images_dir) = &config.assets.images_dir
            && path.starts_with(canonical_or_normalized(images_dir))
        {
            match assets::push_image(pusher, &canonical_or_normalized(images_dir), path) {
                Ok(source) => log_line(
                    "\x1b[35masset\x1b[0m",
                    &format!("reloaded image {source:?} (no VM restart)"),
                ),
                Err(e) => log_line("\x1b[31masset error\x1b[0m", &e),
            }
            continue;
        }
        if is_source_file(path) {
            graph.invalidate(path);
            code_changed.push(path.clone());
        }
    }

    if config_changed {
        match config.reload_from_disk() {
            Ok(next) => {
                let entry_changed = canonical_or_normalized(&next.app.entry)
                    != canonical_or_normalized(&config.app.entry);
                if next.app.title != config.app.title {
                    pusher.title(next.app.title.clone());
                }
                if next.dev.overlay != config.dev.overlay {
                    pusher.enable_dev_overlay(next.dev.overlay);
                }
                for font in &next.assets.fonts {
                    let _ = assets::push_font(pusher, font);
                }
                if let Some(family) = &next.assets.default_family {
                    pusher.default_family(family.clone());
                }
                *config = next;
                log_line("\x1b[36mconfig\x1b[0m", "reloaded jsnir.toml");
                if entry_changed {
                    graph.set_root(&config.root);
                    rebuild_and_push(config, graph, pusher, typechecker, false, "config entry changed");
                    return;
                }
            }
            Err(e) => {
                let err = DevError {
                    title: "Config error".into(),
                    message: e.clone(),
                    location: config.config_path.as_ref().map(|p| p.display().to_string()),
                    frame: None,
                };
                log_line("\x1b[31mconfig error\x1b[0m", &e);
                if config.dev.overlay {
                    pusher.dev_error(Some(err));
                }
                return;
            }
        }
    }

    if !code_changed.is_empty() {
        let label = code_changed
            .iter()
            .map(|p| module_id_for(&config.root, p))
            .collect::<Vec<_>>()
            .join(", ");
        rebuild_and_push(config, graph, pusher, typechecker, true, &label);
    }
}

fn rebuild_and_push(
    config: &DevConfig,
    graph: &mut ModuleGraph,
    pusher: &Pusher,
    typechecker: &AsyncTypechecker,
    preserve_state: bool,
    reason: &str,
) {
    if config.dev.clear_screen {
        shortcuts::clear_console();
    }
    match graph.build(&config.app.entry) {
        Ok(bundle) => {
            if config.dev.typecheck == TypecheckMode::Blocking {
                if let TypecheckOutcome::Errors { output, elapsed } =
                    run_typecheck(&config.root, &config.app.entry)
                {
                    log_line(
                        "\x1b[31mtype error\x1b[0m",
                        &format!("({} ms) reload blocked\n{output}", elapsed.as_millis()),
                    );
                    if config.dev.overlay {
                        pusher.dev_error(Some(DevError {
                            title: "TypeScript error".into(),
                            message: output,
                            location: None,
                            frame: None,
                        }));
                    }
                    return;
                }
            }
            pusher.dev_error(None);
            let sent = if preserve_state {
                pusher.reload(bundle.code)
            } else {
                pusher.hard_reload(bundle.code)
            };
            if sent {
                let kind = if preserve_state {
                    "hmr update"
                } else {
                    "hard reload"
                };
                log_line(
                    &format!("\x1b[32m{kind}\x1b[0m"),
                    &format!(
                        "{reason} \x1b[2m({} ms · {}/{} modules)\x1b[0m",
                        bundle.elapsed.as_millis(),
                        bundle.rebuilt.max(1),
                        bundle.files.len()
                    ),
                );
            }
            if config.dev.typecheck == TypecheckMode::Async && find_tsc(&config.root).is_some() {
                typechecker.trigger(config.root.clone(), config.app.entry.clone());
            }
        }
        Err(err) => {
            print_dev_error(&err);
            if config.dev.overlay {
                pusher.dev_error(Some(err));
            }
        }
    }
}

pub fn print_dev_error(err: &DevError) {
    let loc = err
        .location
        .as_deref()
        .map(|l| format!(" \x1b[33m{l}\x1b[0m"))
        .unwrap_or_default();
    log_line("\x1b[31merror\x1b[0m", &format!("{}{loc}\n  {}", err.title, err.message));
    if let Some(frame) = &err.frame {
        eprintln!("{frame}");
    }
}

fn log_line(tag: &str, msg: &str) {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let h = (secs / 3600) % 24;
    let m = (secs / 60) % 60;
    let s = secs % 60;
    eprintln!("\x1b[2m{h:02}:{m:02}:{s:02}\x1b[0m \x1b[1;36m[jsnir]\x1b[0m {tag} {msg}");
}

fn should_ignore(path: &Path) -> bool {
    for comp in path.components() {
        if let Component::Normal(name) = comp {
            let s = name.to_string_lossy();
            if matches!(
                s.as_ref(),
                ".git" | "node_modules" | "target" | "dist" | ".jsnir"
            ) {
                return true;
            }
        }
    }
    if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
        if name.ends_with('~')
            || name.ends_with(".swp")
            || name.ends_with(".swo")
            || name.ends_with(".tmp")
            || name.ends_with(".part")
            || name.starts_with(".#")
            || name == ".DS_Store"
        {
            return true;
        }
    }
    false
}

fn is_config_file(config: &DevConfig, path: &Path) -> bool {
    if let Some(cfg) = &config.config_path
        && canonical_or_normalized(cfg) == *path
    {
        return true;
    }
    path.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| CONFIG_FILES.contains(&n))
}

fn is_source_file(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| matches!(e, "ts" | "tsx" | "js" | "jsx" | "mts" | "mjs"))
}

fn is_font_file(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("ttf") || e.eq_ignore_ascii_case("otf"))
}

fn is_configured_font(config: &DevConfig, path: &Path) -> bool {
    config
        .assets
        .fonts
        .iter()
        .any(|f| canonical_or_normalized(f) == *path)
}

fn is_png_file(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("png"))
}
