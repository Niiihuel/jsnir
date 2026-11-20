//! Live reload during development: watches an app's TSX, compiles it with the same `tsc` as the build
//! and hands over the new script. If it fails to compile or has type errors, nothing reloads and the app
//! keeps running as it was.
//! shortcut: polls modification times every 250 ms instead of using inotify; enough for development.
use crate::typescript;
use std::{
    fs,
    path::Path,
    process::Command,
    sync::mpsc::{Receiver, channel},
    thread,
    time::{Duration, Instant},
};

pub(crate) fn watch(app: &'static str) -> Receiver<String> {
    let root = env!("CARGO_MANIFEST_DIR");
    let (tx, rx) = channel();
    thread::spawn(move || {
        let sources = [
            format!("{root}/app/{app}.tsx"),
            format!("{root}/app/sdk.ts"),
        ];
        let stamp = || {
            sources
                .iter()
                .map(|p| fs::metadata(p).and_then(|m| m.modified()).ok())
                .collect::<Vec<_>>()
        };
        let mut last = stamp();
        loop {
            thread::sleep(Duration::from_millis(250));
            let now = stamp();
            if now == last {
                continue;
            }
            last = now;
            let start = Instant::now();
            match compile(root, app) {
                Ok(js) => {
                    eprintln!(
                        "live: {app}.tsx compiled in {} ms",
                        start.elapsed().as_millis()
                    );
                    if tx.send(js).is_err() {
                        break;
                    }
                }
                Err(e) => {
                    eprintln!("live: not reloading, the previous version keeps running.\n{e}")
                }
            }
        }
    });
    rx
}
// Same as build.rs: tsc emits the SDK and the app separately (TypeScript 7 has no outFile) and they are concatenated.
fn compile(root: &str, app: &str) -> Result<String, String> {
    let out = std::env::temp_dir().join(format!("jsnir-live-{}-{app}", std::process::id()));
    let tsc = typescript::compiler(Path::new(root));
    let result = Command::new(&tsc)
        .args([
            "-p",
            &format!("{root}/app/tsconfig.{app}.json"),
            "--noEmit",
            "false",
            "--outDir",
        ])
        .arg(&out)
        .output()
        .map_err(|e| format!("could not run tsc: {e}"))?;
    // A type error is a use of the contract that Rust does not accept: do not reload, just as `cargo test` fails.
    if !result.status.success() {
        return Err(String::from_utf8_lossy(&result.stdout).into_owned());
    }
    let read = |name: &str| {
        fs::read_to_string(out.join(name)).map_err(|e| format!("tsc did not emit {name}: {e}"))
    };
    Ok(read("sdk.js")? + &read(&format!("{app}.js"))?)
}
