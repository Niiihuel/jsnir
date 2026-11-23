//! TypeScript type-checking (`tsc --noEmit`) for both blocking (`jsnir check` / `build`) and
//! non-blocking cancellable background checks during `jsnir dev` (like `vite-plugin-checker`).
use crate::compiler::{JSNIR_DTS, SDK_TS};
use std::{
    env, fs,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
        mpsc::{self, Sender},
    },
    thread,
    time::{Duration, Instant},
};

#[derive(Debug, Clone)]
pub enum TypecheckOutcome {
    Ok(Duration),
    Errors { output: String, elapsed: Duration },
    Unavailable(String),
}

/// Finds a `tsc` executable: `$JSNIR_TSC`, then `<root>/node_modules/.bin/tsc`, then `tsc` on `PATH`.
pub fn find_tsc(root: &Path) -> Option<PathBuf> {
    if let Some(env_tsc) = env::var_os("JSNIR_TSC") {
        let p = PathBuf::from(env_tsc);
        if p.is_file() {
            return Some(p);
        }
    }
    let local = root.join("node_modules/.bin/tsc");
    if local.is_file() {
        return Some(local);
    }
    // Check PATH by trying `which`-like lookup.
    if let Some(paths) = env::var_os("PATH") {
        for dir in env::split_paths(&paths) {
            let candidate = dir.join(if cfg!(windows) { "tsc.exe" } else { "tsc" });
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

/// Runs `tsc --noEmit` synchronously against `tsconfig.json` (or a generated temp tsconfig if none exists).
pub fn run_typecheck(root: &Path, entry: &Path) -> TypecheckOutcome {
    let Some(tsc) = find_tsc(root) else {
        return TypecheckOutcome::Unavailable(
            "tsc not found (set JSNIR_TSC, run `npm ci`, or install TypeScript in PATH)".into(),
        );
    };
    let started = Instant::now();
    let (mut cmd, _temp_guard) = match prepare_tsc_command(&tsc, root, entry) {
        Ok(pair) => pair,
        Err(e) => return TypecheckOutcome::Unavailable(e),
    };
    match cmd.output() {
        Ok(out) if out.status.success() => TypecheckOutcome::Ok(started.elapsed()),
        Ok(out) => {
            let stdout = String::from_utf8_lossy(&out.stdout);
            let stderr = String::from_utf8_lossy(&out.stderr);
            let combined = if stderr.trim().is_empty() {
                stdout.trim().to_string()
            } else if stdout.trim().is_empty() {
                stderr.trim().to_string()
            } else {
                format!("{}\n{}", stdout.trim(), stderr.trim())
            };
            TypecheckOutcome::Errors {
                output: combined,
                elapsed: started.elapsed(),
            }
        }
        Err(e) => TypecheckOutcome::Unavailable(format!("failed to run {}: {e}", tsc.display())),
    }
}

struct TempDirGuard(Option<PathBuf>);
impl Drop for TempDirGuard {
    fn drop(&mut self) {
        if let Some(dir) = self.0.take() {
            let _ = fs::remove_dir_all(dir);
        }
    }
}

fn prepare_tsc_command(
    tsc: &Path,
    root: &Path,
    entry: &Path,
) -> Result<(Command, TempDirGuard), String> {
    let tsconfig = root.join("tsconfig.json");
    if tsconfig.is_file() {
        // Keep `.jsnir/jsnir.d.ts` and `.jsnir/sdk.ts` fresh if the project uses them.
        let dot_jsnir = root.join(".jsnir");
        if dot_jsnir.is_dir() {
            let _ = fs::write(dot_jsnir.join("jsnir.d.ts"), JSNIR_DTS);
            let _ = fs::write(dot_jsnir.join("sdk.ts"), SDK_TS);
        }
        let mut cmd = Command::new(tsc);
        cmd.current_dir(root)
            .args(["-p"])
            .arg(&tsconfig)
            .arg("--noEmit");
        return Ok((cmd, TempDirGuard(None)));
    }

    // Zero-config fallback: generate an ephemeral tsconfig pointing to the embedded SDK + entry.
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let temp = env::temp_dir().join(format!("jsnir-tsc-{}-{stamp}", std::process::id()));
    fs::create_dir_all(&temp).map_err(|e| format!("create temp dir: {e}"))?;
    let dts_path = temp.join("jsnir.d.ts");
    let sdk_path = temp.join("sdk.ts");
    fs::write(&dts_path, JSNIR_DTS).map_err(|e| format!("write jsnir.d.ts: {e}"))?;
    fs::write(&sdk_path, SDK_TS).map_err(|e| format!("write sdk.ts: {e}"))?;
    let entry_abs = fs::canonicalize(entry).unwrap_or_else(|_| entry.to_path_buf());
    let cfg = serde_json::json!({
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
            "types": [dts_path.to_string_lossy()]
        },
        "files": [
            dts_path.to_string_lossy(),
            sdk_path.to_string_lossy(),
            entry_abs.to_string_lossy()
        ]
    });
    let cfg_path = temp.join("tsconfig.json");
    fs::write(&cfg_path, serde_json::to_string_pretty(&cfg).unwrap())
        .map_err(|e| format!("write temp tsconfig: {e}"))?;
    let mut cmd = Command::new(tsc);
    cmd.args(["-p"]).arg(&cfg_path).arg("--noEmit");
    Ok((cmd, TempDirGuard(Some(temp))))
}

/// Non-blocking background typechecker that cancels stale runs whenever a newer save arrives.
pub struct AsyncTypechecker {
    tx: Sender<(PathBuf, PathBuf, u64)>,
    generation: Arc<AtomicU64>,
    running_child: Arc<Mutex<Option<Child>>>,
}

impl AsyncTypechecker {
    pub fn spawn(on_result: impl Fn(TypecheckOutcome) + Send + 'static) -> Self {
        let (tx, rx) = mpsc::channel::<(PathBuf, PathBuf, u64)>();
        let generation = Arc::new(AtomicU64::new(0));
        let running_child: Arc<Mutex<Option<Child>>> = Arc::new(Mutex::new(None));
        let gen_worker = generation.clone();
        let child_worker = running_child.clone();

        thread::spawn(move || {
            while let Ok((mut root, mut entry, mut id)) = rx.recv() {
                // Coalesce burst triggers to only the latest request.
                while let Ok(next) = rx.try_recv() {
                    root = next.0;
                    entry = next.1;
                    id = next.2;
                }
                if id != gen_worker.load(Ordering::SeqCst) {
                    continue;
                }
                let Some(tsc) = find_tsc(&root) else {
                    on_result(TypecheckOutcome::Unavailable(
                        "tsc not found (set JSNIR_TSC or install TypeScript)".into(),
                    ));
                    continue;
                };
                let started = Instant::now();
                let (mut cmd, _guard) = match prepare_tsc_command(&tsc, &root, &entry) {
                    Ok(pair) => pair,
                    Err(e) => {
                        on_result(TypecheckOutcome::Unavailable(e));
                        continue;
                    }
                };
                cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
                let child = match cmd.spawn() {
                    Ok(child) => child,
                    Err(e) => {
                        on_result(TypecheckOutcome::Unavailable(format!(
                            "failed to spawn tsc: {e}"
                        )));
                        continue;
                    }
                };
                *child_worker.lock().unwrap() = Some(child);
                // Wait in a tiny loop so cancellation from `trigger()` kills cleanly.
                let outcome = loop {
                    let status = {
                        let mut guard = child_worker.lock().unwrap();
                        match guard.as_mut() {
                            Some(c) => c.try_wait(),
                            None => break None,
                        }
                    };
                    match status {
                        Ok(Some(_)) => {
                            let child = child_worker.lock().unwrap().take();
                            if let Some(c) = child {
                                break c.wait_with_output().ok();
                            }
                            break None;
                        }
                        Ok(None) => {
                            if id != gen_worker.load(Ordering::SeqCst) {
                                if let Some(mut c) = child_worker.lock().unwrap().take() {
                                    let _ = c.kill();
                                    let _ = c.wait();
                                }
                                break None;
                            }
                            thread::sleep(Duration::from_millis(15));
                        }
                        Err(_) => {
                            child_worker.lock().unwrap().take();
                            break None;
                        }
                    }
                };
                if id != gen_worker.load(Ordering::SeqCst) {
                    continue;
                }
                if let Some(out) = outcome {
                    if out.status.success() {
                        on_result(TypecheckOutcome::Ok(started.elapsed()));
                    } else {
                        let stdout = String::from_utf8_lossy(&out.stdout);
                        let stderr = String::from_utf8_lossy(&out.stderr);
                        let combined = if stderr.trim().is_empty() {
                            stdout.trim().to_string()
                        } else if stdout.trim().is_empty() {
                            stderr.trim().to_string()
                        } else {
                            format!("{}\n{}", stdout.trim(), stderr.trim())
                        };
                        on_result(TypecheckOutcome::Errors {
                            output: combined,
                            elapsed: started.elapsed(),
                        });
                    }
                }
            }
        });

        Self {
            tx,
            generation,
            running_child,
        }
    }

    /// Schedules a fresh typecheck, cancelling any older run still in progress.
    pub fn trigger(&self, root: PathBuf, entry: PathBuf) {
        let id = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        if let Some(mut child) = self.running_child.lock().unwrap().take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        let _ = self.tx.send((root, entry, id));
    }
}

impl Drop for AsyncTypechecker {
    fn drop(&mut self) {
        self.generation.fetch_add(1, Ordering::SeqCst);
        if let Some(mut child) = self.running_child.lock().unwrap().take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}
