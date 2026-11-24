#[cfg(feature = "engine-v8")]
use jsnir::ScriptBundle;
use jsnir::{editor::PointSelection, host::Host, text::TextEngine, tree::Node};
use jsnir_dev::{DevConfig, EngineKind, ModuleGraph, compile_entry, create_engine_factory};
use std::{
    fs,
    path::PathBuf,
    process::Command,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

struct TempDir(PathBuf);
impl TempDir {
    fn new(prefix: &str) -> Self {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("{prefix}-{}-{stamp}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}
impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn settle(host: &mut Host) {
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut quiet = 0;
    while quiet < 3 {
        assert!(Instant::now() < deadline, "host did not settle");
        host.poll();
        let frame = host.frame(800., 600., 1., 4096);
        quiet = if frame.animating || host.busy() {
            0
        } else {
            quiet + 1
        };
        std::thread::sleep(Duration::from_millis(8));
    }
}

fn texts(host: &Host) -> Vec<String> {
    host.tree
        .nodes
        .iter()
        .filter_map(|(_, n)| match n {
            Node::Text { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn multi_file_oxc_compilation_caches_modules_and_preserves_keyed_signals_on_hot_reload() {
    let dir = TempDir::new("jsnir-multi");
    let src = dir.0.join("src");
    fs::create_dir_all(&src).unwrap();

    let fmt_path = src.join("format.ts");
    let counter_path = src.join("counter.tsx");
    let app_path = src.join("app.tsx");

    fs::write(
        &fmt_path,
        r#"
export interface LabelSpec { prefix: string; value: number }
export function formatCount(spec: LabelSpec): string {
    return `${spec.prefix}:${spec.value}`;
}
"#,
    )
    .unwrap();

    fs::write(
        &counter_path,
        r#"
import { Box, Text, signal } from "jsnir";
import { formatCount } from "./format";

export default function Counter(): Element {
    const count = signal(0);
    return (
        <Box style={{ width: 140, height: 40 }} onPress={() => count.update((n) => n + 1)}>
            <Text>{() => formatCount({ prefix: "v1", value: count.value })}</Text>
        </Box>
    );
}
"#,
    )
    .unwrap();

    fs::write(
        &app_path,
        r#"
import Counter from "./counter";
mount(() => <Column style={{ width: 300, height: 200 }}><Counter /></Column>);
"#,
    )
    .unwrap();

    let mut graph = ModuleGraph::new(&dir.0);
    let b1 = graph.build(&app_path).expect("first build");
    assert_eq!(b1.files.len(), 3);
    assert_eq!(b1.rebuilt, 3);

    // Second build with no changes is a full cache hit.
    let b2 = graph.build(&app_path).expect("cached build");
    assert_eq!(b2.rebuilt, 0);

    // Mount in QuickJS Host and click twice so `count` becomes 2.
    let mut host = Host::try_new(b1.code, "{}".into(), TextEngine::default()).unwrap();
    settle(&mut host);
    assert!(texts(&host).contains(&"v1:0".into()), "{:?}", texts(&host));
    for _ in 0..2 {
        host.pointer_move(Some([20., 20.]));
        host.pointer_down(PointSelection::Place);
        host.pointer_up();
        settle(&mut host);
    }
    assert!(texts(&host).contains(&"v1:2".into()), "{:?}", texts(&host));

    // Edit ONLY counter.tsx: insert a brand-new signal BEFORE `count` and change label to "v2".
    // Because Oxc injected stable keys ("src/counter.tsx:count"), `count` MUST stay 2 across reload!
    fs::write(
        &counter_path,
        r#"
import { Box, Text, signal } from "jsnir";
import { formatCount } from "./format";

export default function Counter(): Element {
    const addedAbove = signal(99);
    const count = signal(0);
    return (
        <Box style={{ width: 140, height: 40 }} onPress={() => count.update((n) => n + 1)}>
            <Text>{() => `${addedAbove.value}|${formatCount({ prefix: "v2", value: count.value })}`}</Text>
        </Box>
    );
}
"#,
    )
    .unwrap();
    graph.invalidate(&counter_path);
    let b3 = graph.build(&app_path).expect("incremental rebuild");
    assert_eq!(b3.rebuilt, 1, "only counter.tsx should recompile");

    assert!(host.reload(b3.code));
    settle(&mut host);
    assert!(
        texts(&host).contains(&"99|v2:2".into()),
        "expected preserved count=2 with new signal above it, got {:?}",
        texts(&host)
    );
}

#[test]
fn syntax_and_missing_import_errors_return_structured_code_frames() {
    let dir = TempDir::new("jsnir-errors");
    let bad_syntax = dir.0.join("bad.tsx");
    fs::write(&bad_syntax, "const x = ;\nmount(() => <Box />);\n").unwrap();
    let err = compile_entry(&dir.0, &bad_syntax).unwrap_err();
    assert_eq!(err.title, "Syntax error");
    assert!(err.location.as_deref().unwrap().contains(":1:"));
    assert!(err.frame.as_deref().unwrap().contains('^'));

    let bad_import = dir.0.join("missing.tsx");
    fs::write(
        &bad_import,
        "import { nope } from './does-not-exist';\nmount(() => nope());\n",
    )
    .unwrap();
    let err = compile_entry(&dir.0, &bad_import).unwrap_err();
    assert_eq!(err.title, "Import not found");
    assert!(err.message.contains("./does-not-exist"));
}

#[test]
fn jsnir_cli_init_build_and_check_work_end_to_end() {
    let bin = env!("CARGO_BIN_EXE_jsnir");
    let dir = TempDir::new("jsnir-cli");
    let project = dir.0.join("demo");

    // 1. `jsnir init`
    let out = Command::new(bin)
        .args(["init"])
        .arg(&project)
        .args(["--name", "Demo App"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "init failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(project.join("jsnir.toml").is_file());
    assert!(project.join("tsconfig.json").is_file());
    assert!(project.join(".jsnir/jsnir.d.ts").is_file());
    assert!(project.join("src/app.tsx").is_file());
    assert!(project.join("src/counter.tsx").is_file());

    // Config discovery reads jsnir.toml cleanly.
    let cfg = DevConfig::discover(Some(&project)).unwrap();
    assert_eq!(cfg.app.title, "Demo App");
    assert_eq!(cfg.app.size, [960, 640]);

    // 2. `jsnir check` using the repo's pinned tsc if present.
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    let repo_tsc = repo_root.join("node_modules/.bin/tsc");
    if repo_tsc.is_file() {
        let out = Command::new(bin)
            .env("JSNIR_TSC", &repo_tsc)
            .args(["check"])
            .arg(&project)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "check failed:\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }

    // 3. `jsnir build` -> standalone bundle runs directly in Host!
    let out = Command::new(bin)
        .args(["build"])
        .arg(&project)
        .arg("--no-typecheck")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "build failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let bundle_path = project.join("dist/app.js");
    assert!(bundle_path.is_file());
    let bundle = fs::read_to_string(&bundle_path).unwrap();

    let mut host = Host::try_new(bundle, "{}".into(), TextEngine::default()).unwrap();
    settle(&mut host);
    let rendered = texts(&host);
    assert!(rendered.contains(&"Hello from JSNIR".into()), "{rendered:?}");
    assert!(rendered.contains(&"Clicks: 0".into()), "{rendered:?}");

    // 4. `jsnir build --bytecode` -> AOT QuickJS `.jsbc` bundle mounts cleanly via `Host::try_new_auto`!
    let out = Command::new(bin)
        .args(["build"])
        .arg(&project)
        .args(["--no-typecheck", "--bytecode"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "bytecode build failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let bc_path = project.join("dist/app.jsbc");
    assert!(bc_path.is_file());
    let bc_bytes = fs::read(&bc_path).unwrap();
    assert!(bc_bytes.starts_with(jsnir::BYTECODE_MAGIC));

    let mut bc_host = Host::try_new_auto(bc_bytes, "{}".into(), TextEngine::default()).unwrap();
    settle(&mut bc_host);
    let rendered = texts(&bc_host);
    assert!(rendered.contains(&"Hello from JSNIR".into()), "{rendered:?}");
    assert!(rendered.contains(&"Clicks: 0".into()), "{rendered:?}");
}

#[test]
fn config_parses_engine_selection_and_factory_matches_feature_gate() {
    let dir = TempDir::new("jsnir-engine-cfg");
    fs::write(
        dir.0.join("jsnir.toml"),
        "[app]\nentry = \"src/app.tsx\"\nengine = \"v8\"\n",
    )
    .unwrap();
    let cfg = DevConfig::discover(Some(&dir.0)).unwrap();
    assert_eq!(cfg.app.engine, EngineKind::V8);

    let qjs = create_engine_factory(EngineKind::QuickJs).expect("quickjs factory");
    assert_eq!(qjs.name(), "quickjs-ng");

    #[cfg(not(feature = "engine-v8"))]
    {
        let err = create_engine_factory(EngineKind::V8)
            .err()
            .expect("v8 must be disabled in default lightweight build");
        assert!(
            err.contains("--features engine-v8"),
            "expected actionable rebuild hint, got: {err}"
        );

        let bin = env!("CARGO_BIN_EXE_jsnir");
        let out = Command::new(bin)
            .args([
                "capture",
                dir.0.to_str().unwrap(),
                "--out",
                dir.0.join("out.ppm").to_str().unwrap(),
                "--engine",
                "v8",
            ])
            .output()
            .unwrap();
        assert!(!out.status.success());
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains("--features engine-v8"),
            "unexpected CLI stderr: {stderr}"
        );
    }

    #[cfg(feature = "engine-v8")]
    {
        let v8 = create_engine_factory(EngineKind::V8).expect("v8 factory");
        assert_eq!(v8.name(), "v8");
        assert!(v8.capabilities().jit);
        assert!(v8.capabilities().in_place_reload);

        // Full Oxc TSX compile + V8 Host + keyed-signal in-place reload!
        let src = dir.0.join("src");
        fs::create_dir_all(&src).unwrap();
        let app = src.join("app.tsx");
        fs::write(
            &app,
            r#"
function App(): Element {
    const count = signal(0);
    return (
        <Box style={{ width: 160, height: 48 }} onPress={() => count.update((n) => n + 1)}>
            <Text>{() => `v8-count:${count.value}`}</Text>
        </Box>
    );
}
mount(App);
"#,
        )
        .unwrap();
        let b1 = compile_entry(&dir.0, &app).expect("compile v1");
        let mut host = Host::try_new_with_engine(
            v8,
            ScriptBundle::Source(b1.code),
            "{}".into(),
            TextEngine::default(),
        )
        .expect("v8 host");
        settle(&mut host);
        assert!(texts(&host).contains(&"v8-count:0".into()));

        host.pointer_move(Some([20., 20.]));
        host.pointer_down(PointSelection::Place);
        host.pointer_up();
        settle(&mut host);
        assert!(texts(&host).contains(&"v8-count:1".into()));

        // Edit app.tsx and hot-reload in-place on V8, keeping count=1.
        fs::write(
            &app,
            r#"
function App(): Element {
    const extra = signal(7);
    const count = signal(0);
    return (
        <Box style={{ width: 160, height: 48 }} onPress={() => count.update((n) => n + 1)}>
            <Text>{() => `v8-reloaded:${extra.value}:${count.value}`}</Text>
        </Box>
    );
}
mount(App);
"#,
        )
        .unwrap();
        let b2 = compile_entry(&dir.0, &app).expect("compile v2");
        assert!(host.reload(b2.code));
        settle(&mut host);
        assert!(
            texts(&host).contains(&"v8-reloaded:7:1".into()),
            "expected in-place V8 reload to preserve count=1, got {:?}",
            texts(&host)
        );

        // CLI `jsnir capture --engine v8` end-to-end offscreen render.
        let bin = env!("CARGO_BIN_EXE_jsnir");
        let ppm = dir.0.join("v8_frame.ppm");
        let out = Command::new(bin)
            .args([
                "capture",
                dir.0.to_str().unwrap(),
                "--out",
                ppm.to_str().unwrap(),
                "--engine",
                "v8",
                "--size",
                "320x180",
            ])
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "v8 capture failed:\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        let bytes = fs::read(&ppm).expect("read ppm");
        assert!(bytes.starts_with(b"P6\n320 180\n255\n"));
    }
}

