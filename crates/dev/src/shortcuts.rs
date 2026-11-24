//! Interactive terminal shortcuts inspired by Vite's `packages/vite/src/node/shortcuts.ts`.
use std::{
    io::{self, BufRead, IsTerminal, Write},
    sync::mpsc::Sender,
    thread,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShortcutAction {
    HardReload,
    SoftReload,
    Typecheck,
    Clear,
    Help,
    Quit,
}

/// Prints the startup banner (Vite-style) to stderr.
pub fn print_banner(
    entry_display: &str,
    size: [u32; 2],
    engine_label: &str,
    typecheck_label: &str,
    ready_ms: u128,
) {
    let version = env!("CARGO_PKG_VERSION");
    eprintln!();
    eprintln!("  \x1b[1;36mJSNIR\x1b[0m \x1b[36mv{version}\x1b[0m  \x1b[2mready in\x1b[0m \x1b[1m{ready_ms} ms\x1b[0m");
    eprintln!();
    eprintln!("  \x1b[32m➜\x1b[0m  \x1b[1mEntry:\x1b[0m    {entry_display}");
    eprintln!("  \x1b[32m➜\x1b[0m  \x1b[1mEngine:\x1b[0m   {engine_label}");
    eprintln!("  \x1b[32m➜\x1b[0m  \x1b[1mWindow:\x1b[0m   {}×{}", size[0], size[1]);
    eprintln!("  \x1b[32m➜\x1b[0m  \x1b[1mTypes:\x1b[0m    {typecheck_label}");
    if io::stdin().is_terminal() {
        eprintln!("  \x1b[2m➜  press \x1b[0m\x1b[1mh + enter\x1b[0m\x1b[2m to show help\x1b[0m");
    }
    eprintln!();
}

pub fn print_help() {
    eprintln!();
    eprintln!("  \x1b[1mShortcuts\x1b[0m");
    eprintln!("  \x1b[2mpress \x1b[0m\x1b[1mr + enter\x1b[0m\x1b[2m to hard reload (reset signals)\x1b[0m");
    eprintln!("  \x1b[2mpress \x1b[0m\x1b[1ms + enter\x1b[0m\x1b[2m to soft reload (keep state)\x1b[0m");
    eprintln!("  \x1b[2mpress \x1b[0m\x1b[1mt + enter\x1b[0m\x1b[2m to run typecheck now\x1b[0m");
    eprintln!("  \x1b[2mpress \x1b[0m\x1b[1mc + enter\x1b[0m\x1b[2m to clear console\x1b[0m");
    eprintln!("  \x1b[2mpress \x1b[0m\x1b[1mh + enter\x1b[0m\x1b[2m to show this help\x1b[0m");
    eprintln!("  \x1b[2mpress \x1b[0m\x1b[1mq + enter\x1b[0m\x1b[2m to quit\x1b[0m");
    eprintln!();
}

pub fn clear_console() {
    let mut err = io::stderr();
    let _ = write!(err, "\x1b[2J\x1b[H");
    let _ = err.flush();
}

/// Spawns a background TTY reader thread if stdin is an interactive terminal.
pub fn bind_shortcuts(tx: Sender<ShortcutAction>) {
    if !io::stdin().is_terminal() {
        return;
    }
    thread::spawn(move || {
        let stdin = io::stdin();
        for line in stdin.lock().lines() {
            let Ok(line) = line else { break };
            let action = match line.trim() {
                "r" => Some(ShortcutAction::HardReload),
                "s" => Some(ShortcutAction::SoftReload),
                "t" => Some(ShortcutAction::Typecheck),
                "c" => Some(ShortcutAction::Clear),
                "h" => Some(ShortcutAction::Help),
                "q" => Some(ShortcutAction::Quit),
                _ => None,
            };
            if let Some(action) = action
                && tx.send(action).is_err()
            {
                break;
            }
        }
    });
}
