//! Comprehensive comparative benchmark: QuickJS-ng (JS source & AOT `.jsbc`) vs V8 (TurboFan +
//! Shared BackingStore + Fast API + In-Place Isolate Reload).
//!
//! Run in release mode:
//!   cargo run --release --manifest-path crates/engine-v8/Cargo.toml --example engine_bench
use jsnir::{
    ScriptBundle,
    apps::{SDK, STEPPER},
    compile_bytecode,
    editor::PointSelection,
    engine::{JsEngineFactory, VmConfig, quickjs::QuickJsFactory},
    host::Host,
    text::TextEngine,
};
use jsnir_v8::V8Factory;
use std::{
    rc::Rc,
    sync::Arc,
    time::{Duration, Instant},
};

const INTER: &[u8] = include_bytes!("../../../assets/fonts/Inter-Regular.ttf");

#[derive(Clone, Copy)]
struct Stats {
    p50_us: f64,
    p95_us: f64,
    p99_us: f64,
}

fn summarize(mut samples_ns: Vec<u128>) -> Stats {
    samples_ns.sort_unstable();
    let n = samples_ns.len();
    let q = |p: f64| samples_ns[((n - 1) as f64 * p) as usize] as f64 / 1000.0;
    Stats {
        p50_us: q(0.50),
        p95_us: q(0.95),
        p99_us: q(0.99),
    }
}

fn wait_reply(host: &mut Host) {
    while host.busy() {
        host.poll();
        if host.busy() {
            std::thread::yield_now();
        }
    }
}

fn wait_reload(host: &mut Host) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        host.poll();
        if !host.busy() {
            return;
        }
        std::thread::yield_now();
    }
    panic!("reload timed out");
}

fn settle(host: &mut Host) {
    for _ in 0..300 {
        host.poll();
        let _ = host.frame(1280., 720., 1., 4096);
        if !host.busy() {
            return;
        }
        std::thread::yield_now();
    }
    panic!("host did not settle");
}

fn with_sdk(app: &str) -> String {
    let mut s = String::with_capacity(SDK.len() + app.len() + 64);
    s.push_str(SDK);
    s.push('\n');
    s.push_str(app);
    s
}

fn make_host(factory: Arc<dyn JsEngineFactory>, bundle: ScriptBundle) -> Host {
    let mut h = Host::try_new_with_engine(
        factory,
        bundle,
        "{}".into(),
        TextEngine::with_fonts(&[INTER], [255; 3]),
    )
    .expect("create host");
    h.set_wait_budget(Duration::from_millis(20));
    settle(&mut h);
    h
}

fn print_cmp(name: &str, qjs: Stats, v8: Stats) {
    let speedup = qjs.p50_us / v8.p50_us;
    println!(
        "  {:<42} | QJS {:>8.1} µs (p95 {:>8.1}) | V8 {:>8.1} µs (p95 {:>8.1}) | {:>5.2}x",
        name, qjs.p50_us, qjs.p95_us, v8.p50_us, v8.p95_us, speedup
    );
}

fn bench_push_scenario(
    script: &str,
    payload: &str,
    warmup: usize,
    iters: usize,
) -> (Stats, Stats) {
    let run = |factory: Arc<dyn JsEngineFactory>| -> Stats {
        let mut host = make_host(factory, ScriptBundle::Source(script.to_string()));
        let mut samples = Vec::with_capacity(iters);
        for i in 0..(warmup + iters) {
            let t = Instant::now();
            host.push(payload.to_string());
            wait_reply(&mut host);
            let dt = t.elapsed().as_nanos();
            if i >= warmup {
                samples.push(dt);
            }
        }
        summarize(samples)
    };
    (run(Arc::new(QuickJsFactory)), run(Arc::new(V8Factory)))
}

fn bench_section_1_ui_and_bridge() {
    println!("=== 1. UI SDK & JS↔Rust Bridge Workloads (QuickJS-ng vs V8 TurboFan+FastAPI) ===");
    println!(
        "  {:<42} | {:<27} | {:<26} | Ratio",
        "Scenario", "QuickJS-ng p50 (p95)", "V8 p50 (p95)"
    );
    println!("  {}", "-".repeat(110));

    // 1a. For swap 100 rich rows (transitions + shadows)
    let rich_rows = with_sdk(
        r#"
        var rows = signal([]);
        var epoch = 0;
        function makeRows(n, ep) {
            var out = [];
            for (var i = 0; i < n; i++) {
                out.push({ key: ep + "-" + i, author: "Dev #" + i, body: "Message " + ep + "." + i + " with transitions & shadows" });
            }
            return out;
        }
        onHost(function(msg) { if (msg.cmd === "swap") { epoch++; rows.value = makeRows(msg.n, epoch); } });
        mount(function() {
            return h(Column, { style: { width: 1280, height: 720 }, paint: { background: 0x11131aff } },
                h(TextInput, { placeholder: "Message #general", multiline: true, submitOnEnter: true, style: { height: 40, padding: 8 }, paint: { background: 0x1e2230ff, radius: 6, transitions: [{ property: "edge", duration: 150, easing: "easeOut" }] } }),
                h(For, {
                    each: rows,
                    key: function(r) { return r.key; },
                    style: { grow: 1, minHeight: 0, scroll: true, padding: 12, gap: 4 },
                    paint: { scrollbar: { width: 6, thumb: 0xffffff44, radius: 3, minThumb: 24, inset: 2, autoHide: true } },
                    children: function(item) {
                        return h(Row, {
                            style: { gap: 10, padding: [6, 10, 6, 10], group: true },
                            paint: {
                                background: 0x181b25ff,
                                radius: 6,
                                shadows: [{ y: 1, blur: 4, spread: 0, color: 0x00000033 }],
                                hover: { background: 0x222634ff },
                                transitions: [{ property: "background", duration: 120, easing: "easeOut" }],
                            },
                        },
                            h(Box, { style: { width: 32, height: 32, shrink: 0 }, paint: { background: 0x5865f2ff, radius: 16 } }),
                            h(Column, { style: { grow: 1, minWidth: 0, gap: 2 } },
                                h(Text, { font: { size: 13, weight: 600 }, paint: { color: 0xf8fafcff, hover: { underline: true } }, children: function() { return item().author; } }),
                                h(Text, { font: { size: 13 }, paint: { color: 0x94a3b8ff }, children: function() { return item().body; } }),
                            ),
                        );
                    },
                }),
            );
        });
        "#,
    );
    let (q, v) = bench_push_scenario(&rich_rows, r#"{"cmd":"swap","n":100}"#, 25, 80);
    print_cmp("1a. For swap 100 rich rows (trans+shadow)", q, v);

    // 1b. For swap 100 plain rows (Row+Column)
    let plain_rows = with_sdk(
        r#"
        var rows = signal([]);
        var epoch = 0;
        function makeRows(n, ep) {
            var out = [];
            for (var i = 0; i < n; i++) {
                out.push({ key: ep + "-" + i, author: "Dev #" + i, body: "Plain message " + ep + "." + i });
            }
            return out;
        }
        onHost(function(msg) { if (msg.cmd === "swap") { epoch++; rows.value = makeRows(msg.n, epoch); } });
        mount(function() {
            return h(Column, { style: { width: 1280, height: 720 }, paint: { background: 0x11131aff } },
                h(For, {
                    each: rows,
                    key: function(r) { return r.key; },
                    style: { grow: 1, minHeight: 0, scroll: true, padding: 12, gap: 4 },
                    children: function(item) {
                        return h(Row, {
                            style: { gap: 10, padding: [6, 10, 6, 10], group: true },
                            paint: { radius: 6, hover: { background: 0x222634ff } },
                        },
                            h(Box, { style: { width: 32, height: 32, shrink: 0 }, paint: { background: 0x5865f2ff, radius: 16 } }),
                            h(Column, { style: { grow: 1, minWidth: 0, gap: 2 } },
                                h(Text, { font: { size: 13, weight: 600 }, paint: { color: 0xf8fafcff }, children: function() { return item().author; } }),
                                h(Text, { font: { size: 13 }, paint: { color: 0x94a3b8ff }, children: function() { return item().body; } }),
                            ),
                        );
                    },
                }),
            );
        });
        "#,
    );
    let (q, v) = bench_push_scenario(&plain_rows, r#"{"cmd":"swap","n":100}"#, 25, 80);
    print_cmp("1b. For swap 100 plain rows (Row+Column)", q, v);

    // 1c. 50 rich Text setSpans + setText updates
    let spans_app = with_sdk(
        r#"
        var tick = signal(0);
        var items = [];
        for (var i = 0; i < 50; i++) items.push(i);
        onHost(function(msg) { if (msg.cmd === "tick") { tick.value++; } });
        mount(function() {
            return h(Column, { style: { width: 1280, height: 720, gap: 4 } },
                items.map(function(i) {
                    return h(Text, {
                        font: { size: 13 },
                        paint: { color: 0xe2e8f0ff },
                        spans: function() {
                            var t = tick.value;
                            return [
                                { text: "@user" + i, color: 0x60a5faff, weight: 600, background: 0x3b82f633, backgroundRadius: 4 },
                                { text: " pushed commit #" + t + " " },
                                { text: "v" + t + "." + i, underline: true, color: 0x34d399ff },
                            ];
                        },
                        children: function() { return "@user" + i + " pushed commit #" + tick.value + " v" + tick.value + "." + i; },
                    });
                }),
            );
        });
        "#,
    );
    let (q, v) = bench_push_scenario(&spans_app, r#"{"cmd":"tick"}"#, 40, 150);
    print_cmp("1c. 50 rich Text setSpans + setText", q, v);

    // 1d. 50 reactive SetStyle + SetPaint(trans) + SetField
    let reactive_app = with_sdk(
        r#"
        var tick = signal(0);
        var fieldRef = null;
        var ids = [];
        for (var i = 0; i < 50; i++) ids.push(i);
        onHost(function(msg) {
            if (msg.cmd === "tick") {
                tick.value++;
                if (fieldRef) fieldRef.replace("draft-" + tick.value);
            }
        });
        mount(function() {
            return h(Column, { style: { width: 1280, height: 720, gap: 2 } },
                h(TextInput, { value: "draft-0", ref: function(f) { fieldRef = f; }, style: { height: 32 } }),
                ids.map(function(i) {
                    return h(Box, {
                        style: function() { return { width: 120 + ((tick.value + i) % 80), height: 10 }; },
                        paint: function() {
                            var active = (tick.value + i) % 2 === 0;
                            return {
                                background: active ? 0x3b82f6ff : 0x1e293bff,
                                radius: 4,
                                shadows: [{ y: 2, blur: 6, spread: 0, color: 0x00000044 }],
                                transitions: [{ property: "background", duration: 150, easing: "easeOut" }],
                            };
                        },
                    });
                }),
            );
        });
        "#,
    );
    let (q, v) = bench_push_scenario(&reactive_app, r#"{"cmd":"tick"}"#, 40, 150);
    print_cmp("1d. 50 SetStyle+SetPaint(trans)+SetField", q, v);

    // 1e. Popover toggle
    let popover_app = with_sdk(
        r#"
        var open = signal(false);
        var anchorId = 0;
        onHost(function(msg) { if (msg.cmd === "toggle") open.value = !open.value; });
        mount(function() {
            return h(Column, { style: { width: 800, height: 600, padding: 20 } },
                h(Box, { ref: function(id) { anchorId = id; }, style: { width: 120, height: 32 }, paint: { background: 0x3b82f6ff, radius: 6 } }),
                h(Popover, {
                    open: open,
                    anchor: function() { return anchorId; },
                    side: "bottom",
                    align: "start",
                    gap: 8,
                    onPlacement: function() {},
                    style: { width: 220, padding: 10, gap: 6 },
                    paint: {
                        background: 0x181b25ff,
                        radius: 8,
                        shadows: [{ y: 8, blur: 24, spread: 0, color: 0x00000088 }],
                        starting: { opacity: 0, y: -4, scale: 0.96 },
                        transitions: [
                            { property: "opacity", duration: 120, easing: "easeOut" },
                            { property: "y", duration: 120, easing: "easeOut" },
                            { property: "scale", duration: 120, easing: "spring" },
                        ],
                    },
                    children: h(Column, { style: { gap: 4 } },
                        h(Text, { children: "Menu Action 1", font: { size: 13 } }),
                        h(Text, { children: "Menu Action 2", font: { size: 13 } }),
                        h(Text, { children: "Menu Action 3", font: { size: 13 } }),
                    ),
                }),
            );
        });
        "#,
    );
    let (q, v) = bench_push_scenario(&popover_app, r#"{"cmd":"toggle"}"#, 60, 250);
    print_cmp("1e. Popover toggle (anchor+shadow+start)", q, v);

    // 1f. 120 Hz drag-resize move (zero-JSON jsnirDrag + binary cmd)
    let drag_script = r#"
        var baseW = 240;
        function jsnirMount() {
            jsnir.batch(JSON.stringify({ ops: [
                { op: 'create', id: 1, node: { kind: 'box', style: { direction: 'row', width: 1280, height: 720 } } },
                { op: 'create', id: 2, parent: 1, node: { kind: 'box', style: { width: 240, height: 720, shrink: 0 } } },
                { op: 'create', id: 3, parent: 1, node: { kind: 'box', style: { width: 8, height: 720, shrink: 0 }, drag: 1 } },
                { op: 'create', id: 4, parent: 1, node: { kind: 'box', style: { grow: 1, height: 720 } } },
            ] }));
        }
        function jsnirDrag(cb, phase, x, y, sx, sy) {
            if (cb !== 1) return;
            if (phase === 0) { baseW = jsnir.columns.width[2]; return; }
            if (phase !== 1) return;
            var w = Math.max(120, Math.min(600, baseW + (x - sx)));
            var u = jsnir.cmd;
            var f = jsnir.cmdF32;
            u[0] = 8; u[1] = 2; u[2] = 1 | 2 | (1 << 3); u[3] = 0;
            f[4] = w; f[5] = 720; f[6] = 0;
            jsnir.commit(7);
        }
    "#;
    let run_drag = |factory: Arc<dyn JsEngineFactory>| -> Stats {
        let mut host = make_host(factory, ScriptBundle::Source(drag_script.into()));
        host.pointer_move(Some([244., 100.]));
        host.pointer_down(PointSelection::Place);
        wait_reply(&mut host);
        let warmup = 300;
        let iters = 1500;
        let mut samples = Vec::with_capacity(iters);
        for i in 0..(warmup + iters) {
            let x = 244. + ((i % 200) as f32 - 100.);
            let t = Instant::now();
            host.pointer_move(Some([x, 100.]));
            wait_reply(&mut host);
            let dt = t.elapsed().as_nanos();
            if i >= warmup {
                samples.push(dt);
            }
        }
        host.pointer_up();
        wait_reply(&mut host);
        summarize(samples)
    };
    print_cmp(
        "1f. 120Hz drag move (zero-JSON + bin cmd)",
        run_drag(Arc::new(QuickJsFactory)),
        run_drag(Arc::new(V8Factory)),
    );

    // 1g. Direct engine 200-op binary cmd commit loop (no thread IPC noise)
    let bin_200_script = r#"
        var tick = 0;
        function jsnirMount() {}
        function jsnirEvent(cb) {
            tick++;
            var u = jsnir.cmd;
            var f = jsnir.cmdF32;
            var p = 0;
            var w = 40 + (tick & 63);
            var bg = (0x220000ff + ((tick & 255) << 16)) >>> 0;
            var opacity = 0.5 + (tick & 31) * 0.01;
            for (var i = 0; i < 100; i++) {
                var id = 10 + i;
                u[p++] = 8; u[p++] = id; u[p++] = 1 | 2 | (1 << 5); u[p++] = 0;
                f[p++] = w; f[p++] = 12; f[p++] = 4; f[p++] = 8; f[p++] = 4; f[p++] = 8;
                u[p++] = 9; u[p++] = id; u[p++] = 1 | (1 << 2) | (1 << 7);
                u[p++] = bg; f[p++] = 6; f[p++] = 6; f[p++] = 6; f[p++] = 6; f[p++] = opacity;
            }
            jsnir.commit(p);
        }
    "#;
    let run_engine_event = |factory: &dyn JsEngineFactory, script: &str, warmup: usize, iters: usize| -> Stats {
        let mut engine = factory
            .create(
                &ScriptBundle::Source(script.into()),
                Rc::default(),
                &VmConfig::default(),
            )
            .expect("load");
        engine.call_mount().expect("mount");
        let mut samples = Vec::with_capacity(iters);
        for i in 0..(warmup + iters) {
            let t = Instant::now();
            let ops = engine.call_event(1, "", None).expect("event");
            std::hint::black_box(ops);
            let dt = t.elapsed().as_nanos();
            if i >= warmup {
                samples.push(dt);
            }
        }
        summarize(samples)
    };
    print_cmp(
        "1g. Direct engine: 200-op bin commit",
        run_engine_event(&QuickJsFactory, bin_200_script, 200, 1500),
        run_engine_event(&V8Factory, bin_200_script, 200, 1500),
    );

    // 1h. Direct engine 1,000x jsnir.set burst (isolates V8 Fast API FAST_SET vs QuickJS FFI)
    let fast_set_script = r#"
        var tick = 0;
        function jsnirMount() {}
        function jsnirEvent() {
            tick = (tick + 1) & 1023;
            for (var i = 0; i < 1000; i++) {
                jsnir.set(i + 1, (i + tick) & 65535);
            }
        }
    "#;
    print_cmp(
        "1h. Direct engine: 1000x jsnir.set (FastAPI)",
        run_engine_event(&QuickJsFactory, fast_set_script, 200, 1500),
        run_engine_event(&V8Factory, fast_set_script, 200, 1500),
    );
    println!();
}

fn bench_section_2_heavy_compute() {
    println!("=== 2. Heavy JS Compute Workloads (where TurboFan/Maglev JIT actually matters) ===");
    println!(
        "  {:<42} | {:<27} | {:<26} | Ratio",
        "Workload", "QuickJS-ng p50 (p95)", "V8 p50 (p95)"
    );
    println!("  {}", "-".repeat(110));

    let run_engine = |factory: &dyn JsEngineFactory, script: &str, warmup: usize, iters: usize| -> Stats {
        let mut engine = factory
            .create(
                &ScriptBundle::Source(script.into()),
                Rc::default(),
                &VmConfig::default(),
            )
            .expect("load");
        engine.call_mount().expect("mount");
        let mut samples = Vec::with_capacity(iters);
        for i in 0..(warmup + iters) {
            let t = Instant::now();
            let ops = engine.call_event(1, "", None).expect("event");
            std::hint::black_box(ops);
            let dt = t.elapsed().as_nanos();
            if i >= warmup {
                samples.push(dt);
            }
        }
        summarize(samples)
    };

    // 2a. Filter + sort 20,000 chat messages & emit top-50 visible rows
    let filter_sort_script = r#"
        var messages = [];
        var channels = ["general", "rust", "gpu", "design", "random"];
        var authors = ["alice", "bob", "carol", "dave", "eve", "frank"];
        for (var i = 0; i < 20000; i++) {
            messages.push({
                id: i,
                ts: (i * 2654435761) >>> 0,
                channel: channels[i % 5],
                author: authors[i % 6],
                score: (i * 73) % 1000,
                pinned: (i & 31) === 0,
                text: "Msg #" + i + " about " + channels[i % 5] + " performance and latency"
            });
        }
        var qIdx = 0;
        function jsnirMount() {}
        function jsnirEvent() {
            var ch = channels[qIdx++ % 5];
            var matched = [];
            for (var i = 0; i < messages.length; i++) {
                var m = messages[i];
                if (m.channel === ch && m.score >= 400) {
                    matched.push(m);
                }
            }
            matched.sort(function(a, b) { return (b.score - a.score) || (b.ts - a.ts); });
            var u = jsnir.cmd;
            var strs = [];
            var p = 0;
            var limit = Math.min(50, matched.length);
            for (var j = 0; j < limit; j++) {
                // OP_SET_TEXT (2): id, str_idx
                u[p++] = 2; u[p++] = 10 + j; u[p++] = strs.length;
                strs.push(matched[j].author + ": " + matched[j].text);
            }
            jsnir.commitStrings(p, strs);
        }
    "#;
    print_cmp(
        "2a. Filter+sort 20k msgs -> top 50 rows",
        run_engine(&QuickJsFactory, filter_sort_script, 15, 60),
        run_engine(&V8Factory, filter_sort_script, 15, 60),
    );

    // 2b. Markdown-like inline tokenizer producing rich spans for 200 messages
    let md_parse_script = r#"
        var samples = [];
        for (var i = 0; i < 200; i++) {
            samples.push("Hey @dev" + i + " check **PR #" + (1000 + i) + "** with `cargo bench --release` and *zero-copy* atlas #" + i);
        }
        function tokenize(line) {
            var out = [];
            var i = 0, n = line.length, start = 0;
            while (i < n) {
                var ch = line.charCodeAt(i);
                if (ch === 42 && i + 1 < n && line.charCodeAt(i + 1) === 42) {
                    if (i > start) out.push({ t: line.slice(start, i), k: 0 });
                    var end = line.indexOf("**", i + 2);
                    if (end > i + 2) {
                        out.push({ t: line.slice(i + 2, end), k: 1 });
                        i = end + 2; start = i; continue;
                    }
                } else if (ch === 96) {
                    if (i > start) out.push({ t: line.slice(start, i), k: 0 });
                    var end = line.indexOf("`", i + 1);
                    if (end > i + 1) {
                        out.push({ t: line.slice(i + 1, end), k: 2 });
                        i = end + 1; start = i; continue;
                    }
                } else if (ch === 64) {
                    if (i > start) out.push({ t: line.slice(start, i), k: 0 });
                    var end = i + 1;
                    while (end < n && line.charCodeAt(end) !== 32) end++;
                    out.push({ t: line.slice(i, end), k: 3 });
                    i = end; start = i; continue;
                }
                i++;
            }
            if (start < n) out.push({ t: line.slice(start), k: 0 });
            return out;
        }
        var round = 0;
        function jsnirMount() {}
        function jsnirEvent() {
            round++;
            var totalSpans = 0;
            for (var i = 0; i < samples.length; i++) {
                var spans = tokenize(samples[i]);
                totalSpans += spans.length;
            }
            jsnir.set(1, (totalSpans + round) & 65535);
        }
    "#;
    print_cmp(
        "2b. Markdown inline tokenizer (200 msgs)",
        run_engine(&QuickJsFactory, md_parse_script, 25, 100),
        run_engine(&V8Factory, md_parse_script, 25, 100),
    );

    // 2c. Spreadsheet / data-grid recalculation (5,000 formula cells + dirty diff)
    let sheet_script = r#"
        var N = 5000;
        var base = new Float64Array(N);
        var mult = new Float64Array(N);
        var prev = new Float64Array(N);
        var cur = new Float64Array(N);
        for (var i = 0; i < N; i++) {
            base[i] = (i % 97) * 1.37 + 10.0;
            mult[i] = 1.0 + (i % 13) * 0.05;
        }
        var tick = 0;
        function jsnirMount() {}
        function jsnirEvent() {
            tick++;
            var factor = 1.0 + (tick & 15) * 0.01;
            var changed = 0;
            var sum = 0.0;
            for (var i = 0; i < N; i++) {
                var dep = i > 0 ? cur[i - 1] * 0.001 : 0.0;
                var val = Math.round((base[i] * mult[i] * factor + dep) * 100.0) / 100.0;
                cur[i] = val;
                sum += val;
                if (val !== prev[i]) {
                    prev[i] = val;
                    changed++;
                }
            }
            jsnir.set(1, changed & 65535);
            jsnir.set(2, (sum | 0) & 65535);
        }
    "#;
    print_cmp(
        "2c. Spreadsheet recalc (5,000 cells+diff)",
        run_engine(&QuickJsFactory, sheet_script, 40, 200),
        run_engine(&V8Factory, sheet_script, 40, 200),
    );
    println!();
}

fn bench_section_3_system_costs() {
    println!("=== 3. System Costs: Cold Start, Hot Reload & VM Heap Memory ===");

    // 3a. Cold start (STEPPER mount + settle)
    let stepper_bc = compile_bytecode(STEPPER).expect("compile STEPPER bytecode");
    let measure_cold = |factory: Arc<dyn JsEngineFactory>, bundle_fn: &dyn Fn() -> ScriptBundle, warmup: usize, iters: usize| -> Stats {
        let mut samples = Vec::with_capacity(iters);
        for i in 0..(warmup + iters) {
            let t = Instant::now();
            let mut h = Host::try_new_with_engine(
                factory.clone(),
                bundle_fn(),
                "{}".into(),
                TextEngine::with_fonts(&[INTER], [255; 3]),
            )
            .expect("cold host");
            settle(&mut h);
            let dt = t.elapsed().as_nanos();
            if i >= warmup {
                samples.push(dt);
            }
        }
        summarize(samples)
    };
    let q_src = measure_cold(
        Arc::new(QuickJsFactory),
        &|| ScriptBundle::Source(STEPPER.to_string()),
        5,
        30,
    );
    let q_aot = measure_cold(
        Arc::new(QuickJsFactory),
        &|| ScriptBundle::Bytecode(stepper_bc.clone()),
        5,
        30,
    );
    let v8_src = measure_cold(
        Arc::new(V8Factory),
        &|| ScriptBundle::Source(STEPPER.to_string()),
        3,
        15,
    );
    println!(
        "  Cold start STEPPER (mount+settle):\n    QuickJS (JS source) : p50 {:>8.1} µs | p95 {:>8.1} µs | p99 {:>8.1} µs\n    QuickJS (.jsbc AOT) : p50 {:>8.1} µs | p95 {:>8.1} µs | p99 {:>8.1} µs\n    V8 (JS source)      : p50 {:>8.1} µs | p95 {:>8.1} µs | p99 {:>8.1} µs  ({:.2}x slower vs QJS AOT)",
        q_src.p50_us, q_src.p95_us, q_src.p99_us,
        q_aot.p50_us, q_aot.p95_us, q_aot.p99_us,
        v8_src.p50_us, v8_src.p95_us, v8_src.p99_us,
        v8_src.p50_us / q_aot.p50_us,
    );

    // 3b. Hot reload (Host::reload preserving signals — V8 recycles Isolate via reload_in_place!)
    let reload_script = with_sdk(
        r#"
        var count = signal(1);
        var items = signal(["a", "b", "c", "d", "e"]);
        mount(function() {
            return h(Column, { style: { width: 800, height: 600, gap: 4 } },
                h(Text, { children: function() { return "Count: " + count.value; }, font: { size: 14 } }),
                h(For, {
                    each: items,
                    key: function(x) { return x; },
                    children: function(x) { return h(Text, { children: function() { return x(); } }); },
                }),
            );
        });
        "#,
    );
    let measure_reload = |factory: Arc<dyn JsEngineFactory>| -> Stats {
        let mut host = make_host(factory, ScriptBundle::Source(reload_script.clone()));
        let warmup = 10;
        let iters = 50;
        let mut samples = Vec::with_capacity(iters);
        for i in 0..(warmup + iters) {
            let t = Instant::now();
            assert!(host.reload(reload_script.clone()));
            wait_reload(&mut host);
            assert!(host.take_reload_result().unwrap().is_ok());
            let dt = t.elapsed().as_nanos();
            if i >= warmup {
                samples.push(dt);
            }
        }
        summarize(samples)
    };
    let q_rel = measure_reload(Arc::new(QuickJsFactory));
    let v8_rel = measure_reload(Arc::new(V8Factory));
    println!(
        "  Hot reload (SDK app, preserve signals):\n    QuickJS (fresh VM)  : p50 {:>8.1} µs | p95 {:>8.1} µs | p99 {:>8.1} µs\n    V8 (in-place ctx)   : p50 {:>8.1} µs | p95 {:>8.1} µs | p99 {:>8.1} µs  ({:.2}x vs QJS)",
        q_rel.p50_us, q_rel.p95_us, q_rel.p99_us,
        v8_rel.p50_us, v8_rel.p95_us, v8_rel.p99_us,
        v8_rel.p50_us / q_rel.p50_us,
    );

    // 3c. VM Heap memory usage after mounting STEPPER
    let mut q_eng = QuickJsFactory
        .create(&ScriptBundle::Source(STEPPER.into()), Rc::default(), &VmConfig::default())
        .expect("qjs");
    q_eng.call_mount().expect("mount");
    let q_mem = q_eng.memory_usage().expect("qjs mem");

    let mut v_eng = V8Factory
        .create(&ScriptBundle::Source(STEPPER.into()), Rc::default(), &VmConfig::default())
        .expect("v8");
    v_eng.call_mount().expect("mount");
    let v_mem = v_eng.memory_usage().expect("v8 mem");

    println!(
        "  VM Heap after STEPPER mount:\n    QuickJS-ng : used {:>7.1} KiB\n    V8         : used {:>7.1} KiB | total {:>7.1} KiB ({:.1}x used heap)",
        q_mem.used_bytes as f64 / 1024.0,
        v_mem.used_bytes as f64 / 1024.0,
        v_mem.total_bytes as f64 / 1024.0,
        v_mem.used_bytes as f64 / q_mem.used_bytes.max(1) as f64,
    );
}

fn main() {
    println!("JSNIR JS Engine Comparative Benchmark (release build)\n");
    bench_section_1_ui_and_bridge();
    bench_section_2_heavy_compute();
    bench_section_3_system_costs();
}
