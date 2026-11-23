//! Reproducible microbenchmark suite for JSNIR's JS↔Rust bridge, bounded wait,
//! zero-JSON drag/style pipeline, and QuickJS AOT bytecode startup.
//!
//! Run in release mode:
//!   cargo run --release --features stepper --example bridge_bench
use jsnir::{
    apps, compile_bytecode,
    editor::PointSelection,
    host::Host,
    text::TextEngine,
};
use std::time::{Duration, Instant};

struct Stats {
    p50_us: f64,
    p95_us: f64,
    p99_us: f64,
    mean_us: f64,
}

fn summarize(mut samples_ns: Vec<u128>) -> Stats {
    samples_ns.sort_unstable();
    let n = samples_ns.len();
    let q = |p: f64| samples_ns[((n - 1) as f64 * p) as usize] as f64 / 1000.0;
    let sum: u128 = samples_ns.iter().sum();
    Stats {
        p50_us: q(0.50),
        p95_us: q(0.95),
        p99_us: q(0.99),
        mean_us: (sum as f64 / n as f64) / 1000.0,
    }
}

fn settle_host(host: &mut Host) {
    for _ in 0..200 {
        host.poll();
        host.frame(1280., 720., 1., 4096);
        if !host.busy() {
            return;
        }
        std::thread::yield_now();
    }
    panic!("host did not settle");
}

fn wait_reply(host: &mut Host) {
    while host.busy() {
        host.poll();
        if host.busy() {
            std::thread::yield_now();
        }
    }
}

/// Generates a script with `count` styled boxes and one clickable trigger box (id = 2).
/// When `binary` is true, each click updates all `count` boxes (`SetStyle` + `SetPaint`)
/// via the shared binary command buffer (`jsnir.cmd`/`jsnir.cmdF32` + `jsnir.commit`).
/// When `binary` is false, it uses the legacy `jsnir.batch(JSON.stringify(...))` path.
fn mutation_script(count: usize, binary: bool) -> String {
    let mut s = format!(
        r#"
        var N = {count};
        var tick = 0;
        function jsnirMount() {{
            var ops = [
                {{ op: 'create', id: 1, node: {{ kind: 'box', style: {{ width: 1280, height: 720 }} }} }},
                {{ op: 'create', id: 2, parent: 1, node: {{ kind: 'box', style: {{ width: 100, height: 40 }}, press: 1 }} }},
            ];
            for (var i = 0; i < N; i++) {{
                ops.push({{ op: 'create', id: 10 + i, parent: 1, node: {{ kind: 'box', style: {{ width: 40, height: 10 }}, paint: {{ background: 0x334455ff }} }} }});
            }}
            jsnir.batch(JSON.stringify({{ ops: ops }}));
        }}
        "#
    );
    if binary {
        s.push_str(
            r#"
            function jsnirEvent(cb) {
                if (cb !== 1) return;
                tick++;
                var u = jsnir.cmd;
                var f = jsnir.cmdF32;
                var p = 0;
                var w = 40 + (tick & 63);
                var bg = (0x220000ff + ((tick & 255) << 16)) >>> 0;
                var opacity = 0.5 + (tick & 31) * 0.01;
                for (var i = 0; i < N; i++) {
                    var id = 10 + i;
                    // OP_SET_STYLE (8): width (bit 0) | height (bit 1) | padding (bit 5)
                    u[p++] = 8; u[p++] = id; u[p++] = 1 | 2 | (1 << 5); u[p++] = 0;
                    f[p++] = w; f[p++] = 12; f[p++] = 4; f[p++] = 8; f[p++] = 4; f[p++] = 8;
                    // OP_SET_PAINT (9): background (bit 0) | radius (bit 2) | opacity (bit 7)
                    u[p++] = 9; u[p++] = id; u[p++] = 1 | (1 << 2) | (1 << 7);
                    u[p++] = bg; f[p++] = 6; f[p++] = 6; f[p++] = 6; f[p++] = 6; f[p++] = opacity;
                }
                jsnir.commit(p);
            }
            "#,
        );
    } else {
        s.push_str(
            r#"
            function jsnirEvent(cb) {
                if (cb !== 1) return;
                tick++;
                var ops = [];
                var w = 40 + (tick & 63);
                var bg = (0x220000ff + ((tick & 255) << 16)) >>> 0;
                var opacity = 0.5 + (tick & 31) * 0.01;
                for (var i = 0; i < N; i++) {
                    var id = 10 + i;
                    ops.push({ op: 'setstyle', id: id, style: { width: w, height: 12, padding: [4, 8, 4, 8] } });
                    ops.push({ op: 'setpaint', id: id, paint: { background: bg, radius: [6, 6, 6, 6], opacity: opacity } });
                }
                jsnir.batch(JSON.stringify({ ops: ops }));
            }
            "#,
        );
    }
    s
}

fn bench_mutations() {
    println!("=== 1. JS→Rust Bridge Throughput: JSON batch vs Binary Command Buffer ===");
    println!(
        "{:<10} {:>12} {:>12} {:>12} {:>12} {:>10}",
        "Batch", "JSON p50", "Bin p50", "JSON p99", "Bin p99", "Speedup"
    );
    let warmup = 150;
    let iters = 1500;
    for count in [1usize, 10, 50, 100] {
        let measure = |binary: bool| -> Stats {
            let mut host = Host::new(
                mutation_script(count, binary),
                "{}".into(),
                TextEngine::default(),
            );
            host.set_wait_budget(Duration::from_millis(5));
            settle_host(&mut host);
            host.pointer_move(Some([20., 20.]));
            let mut samples = Vec::with_capacity(iters);
            for i in 0..(warmup + iters) {
                let t = Instant::now();
                host.pointer_down(PointSelection::Place);
                host.pointer_up();
                wait_reply(&mut host);
                let dt = t.elapsed().as_nanos();
                if i >= warmup {
                    samples.push(dt);
                }
            }
            summarize(samples)
        };
        let json = measure(false);
        let bin = measure(true);
        let ops_per_event = count * 2;
        println!(
            "{:<10} {:>9.2} µs {:>9.2} µs {:>9.2} µs {:>9.2} µs {:>8.2}x",
            format!("{ops_per_event} ops"),
            json.p50_us,
            bin.p50_us,
            json.p99_us,
            bin.p99_us,
            json.p50_us / bin.p50_us
        );
    }
    println!();
}

/// Compares the full 120 Hz drag-resize loop:
/// - Legacy JSON: `jsnirEvent` + `JSON.parse` + `jsnir.batch(JSON.stringify(setstyle))`
/// - Zero-JSON:   `jsnirDrag` (numeric args) + `jsnir.cmd` `OP_SET_STYLE` + `jsnir.commit`
fn drag_script(zero_json: bool) -> String {
    let common = r#"
        var baseW = 240;
        function jsnirMount() {
            jsnir.batch(JSON.stringify({ ops: [
                { op: 'create', id: 1, node: { kind: 'box', style: { direction: 'row', width: 1280, height: 720 } } },
                { op: 'create', id: 2, parent: 1, node: { kind: 'box', style: { width: 240, height: 720, shrink: 0 } } },
                { op: 'create', id: 3, parent: 1, node: { kind: 'box', style: { width: 8, height: 720, shrink: 0 }, paint: { cursor: 'colResize' }, drag: 1 } },
                { op: 'create', id: 4, parent: 1, node: { kind: 'box', style: { grow: 1, height: 720 } } },
            ] }));
        }
    "#;
    if zero_json {
        format!(
            "{common}\n{}",
            r#"
            function jsnirDrag(cb, phase, x, y, sx, sy) {
                if (cb !== 1) return;
                if (phase === 0) { baseW = jsnir.columns.width[2]; return; }
                if (phase !== 1) return;
                var w = Math.max(120, Math.min(600, baseW + (x - sx)));
                var u = jsnir.cmd;
                var f = jsnir.cmdF32;
                // OP_SET_STYLE (8): width (bit 0) | height (bit 1) | shrink (bit 3)
                u[0] = 8; u[1] = 2; u[2] = 1 | 2 | (1 << 3); u[3] = 0;
                f[4] = w; f[5] = 720; f[6] = 0;
                jsnir.commit(7);
            }
            "#
        )
    } else {
        format!(
            "{common}\n{}",
            r#"
            function jsnirEvent(cb, text) {
                if (cb !== 1) return;
                var e = JSON.parse(text);
                if (e.phase === 'start') { baseW = jsnir.columns.width[2]; return; }
                if (e.phase !== 'move') return;
                var w = Math.max(120, Math.min(600, baseW + e.dx));
                jsnir.batch(JSON.stringify({ ops: [
                    { op: 'setstyle', id: 2, style: { width: w, height: 720, shrink: 0 } }
                ] }));
            }
            "#
        )
    }
}

fn bench_drag_resize() {
    println!("=== 2. 120 Hz Drag-Resize Pipeline: Legacy JSON vs Zero-JSON + Binary Cmd ===");
    let warmup = 150;
    let iters = 1500;
    for include_frame in [false, true] {
        let run = |zero_json: bool| -> Stats {
            let mut host = Host::new(drag_script(zero_json), "{}".into(), TextEngine::default());
            host.set_wait_budget(Duration::from_millis(5));
            settle_host(&mut host);
            // Grab the handle at x = 244, y = 100.
            host.pointer_move(Some([244., 100.]));
            host.pointer_down(PointSelection::Place);
            wait_reply(&mut host);
            let mut samples = Vec::with_capacity(iters);
            for i in 0..(warmup + iters) {
                let x = 244. + ((i % 200) as f32 - 100.);
                let t = Instant::now();
                host.pointer_move(Some([x, 100.]));
                wait_reply(&mut host);
                if include_frame {
                    let _ = host.frame(1280., 720., 1., 4096);
                }
                let dt = t.elapsed().as_nanos();
                if i >= warmup {
                    samples.push(dt);
                }
            }
            host.pointer_up();
            wait_reply(&mut host);
            summarize(samples)
        };
        let legacy = run(false);
        let fast = run(true);
        let label = if include_frame {
            "Drag move + Taffy frame()"
        } else {
            "Drag move bridge only"
        };
        println!(
            "  {label:<26} | JSON: p50 {:>6.2} µs (p99 {:>6.2} µs) | Zero-JSON: p50 {:>6.2} µs (p99 {:>6.2} µs) | {:.2}x",
            legacy.p50_us,
            legacy.p99_us,
            fast.p50_us,
            fast.p99_us,
            legacy.p50_us / fast.p50_us
        );
    }
    println!();
}

fn bench_bounded_wait() {
    println!("=== 3. Host::poll() Bounded Wait (Same-Frame Landing Rate) ===");
    let script = mutation_script(1, true);
    let trials = 500usize;
    for (label, budget) in [
        ("0 µs (non-blocking)", Duration::ZERO),
        ("800 µs (default)", Duration::from_micros(800)),
    ] {
        let mut host = Host::new(script.clone(), "{}".into(), TextEngine::default());
        host.set_wait_budget(budget);
        settle_host(&mut host);
        host.pointer_move(Some([20., 20.]));
        // Warm up thread wakeup.
        for _ in 0..20 {
            host.pointer_down(PointSelection::Place);
            host.pointer_up();
            wait_reply(&mut host);
        }
        let mut landed_first_poll = 0usize;
        let mut poll_times = Vec::with_capacity(trials);
        for _ in 0..trials {
            host.pointer_down(PointSelection::Place);
            host.pointer_up();
            let t = Instant::now();
            host.poll();
            poll_times.push(t.elapsed().as_nanos());
            if !host.busy() {
                landed_first_poll += 1;
            } else {
                wait_reply(&mut host);
            }
        }
        let stats = summarize(poll_times);
        println!(
            "  {label:<20} | landed in 1st poll: {:>3}/{trials} ({:>5.1}%) | poll() p50 {:>6.2} µs, p95 {:>6.2} µs, p99 {:>6.2} µs",
            landed_first_poll,
            (landed_first_poll as f64 / trials as f64) * 100.0,
            stats.p50_us,
            stats.p95_us,
            stats.p99_us,
        );
    }
    println!();
}

fn bench_bytecode_startup() {
    println!("=== 4. Cold Startup: JS Source (.js) vs QuickJS Bytecode AOT (.jsbc) ===");
    println!(
        "{:<12} {:>9} {:>9} {:>12} {:>12} {:>10}",
        "App", "JS size", "BC size", "JS p50", "BC p50", "Speedup"
    );
    let runs = 40;
    for (name, src) in [
        ("stepper", apps::STEPPER),
        ("overlay", apps::OVERLAY),
        ("composer", apps::COMPOSER),
        ("scrollchat", apps::SCROLLCHAT),
    ] {
        let bc = compile_bytecode(src).expect("compile bytecode");
        let mut js_times = Vec::with_capacity(runs);
        let mut bc_times = Vec::with_capacity(runs);
        for _ in 0..runs {
            let text = TextEngine::default();
            let src_owned = src.to_string();
            let t = Instant::now();
            let host = Host::try_new(src_owned, "{}".into(), text).unwrap();
            js_times.push(t.elapsed().as_nanos());
            drop(host);

            let text = TextEngine::default();
            let bc_owned = bc.clone();
            let t = Instant::now();
            let host = Host::try_new_bytecode(bc_owned, "{}".into(), text).unwrap();
            bc_times.push(t.elapsed().as_nanos());
            drop(host);
        }
        let js = summarize(js_times);
        let bcs = summarize(bc_times);
        println!(
            "{:<12} {:>6.1} kB {:>6.1} kB {:>9.2} ms {:>9.2} ms {:>8.2}x",
            name,
            src.len() as f64 / 1024.0,
            bc.len() as f64 / 1024.0,
            js.p50_us / 1000.0,
            bcs.p50_us / 1000.0,
            js.p50_us / bcs.p50_us,
        );
    }
    println!();
}

fn bench_column_sync_churn() {
    println!("=== 5. Large Tree (500 nodes) In-Place Column Sync on Remove/Create + Resize ===");
    let script = r#"
        var toggle = 0;
        function jsnirMount() {
            var ops = [{ op: 'create', id: 1, node: { kind: 'box', style: { width: 800, height: 600 } } }];
            for (var i = 0; i < 498; i++) {
                ops.push({ op: 'create', id: 10 + i, parent: 1, node: { kind: 'box', style: { width: 20, height: 1 } } });
            }
            ops.push({ op: 'create', id: 2, parent: 1, node: { kind: 'box', style: { width: 100, height: 40 }, press: 1 } });
            ops.push({ op: 'create', id: 520, parent: 1, node: { kind: 'box', style: { width: 10, height: 10 } } });
            jsnir.batch(JSON.stringify({ ops: ops }));
        }
        function jsnirEvent(cb) {
            if (cb !== 1) return;
            toggle++;
            // Remove triggers `Columns::sync(..., None)` in-place refresh (no 11 ArrayBuffer reallocations).
            var u = jsnir.cmd;
            u[0] = 3; u[1] = 520;
            jsnir.commit(2);
            jsnir.batch(JSON.stringify({ ops: [
                { op: 'create', id: 520, parent: 1, node: { kind: 'box', style: { width: 10 + (toggle & 7), height: 10 } } }
            ] }));
        }
    "#;
    let mut host = Host::new(script.into(), "{}".into(), TextEngine::default());
    host.set_wait_budget(Duration::from_millis(5));
    settle_host(&mut host);
    // Find button 2's y coordinate (below 498 1px boxes = y 498..538).
    let rect = host.screen_rect(2).expect("button rect");
    host.pointer_move(Some([rect[0] + 10., rect[1] + 10.]));

    let warmup = 50;
    let iters = 500;
    let mut samples = Vec::with_capacity(iters);
    for i in 0..(warmup + iters) {
        let t = Instant::now();
        host.pointer_down(PointSelection::Place);
        host.pointer_up();
        wait_reply(&mut host);
        let dt = t.elapsed().as_nanos();
        if i >= warmup {
            samples.push(dt);
        }
    }
    let stats = summarize(samples);
    println!(
        "  500-node Remove+Create + full in-place Columns::sync: p50 {:>6.2} µs, p95 {:>6.2} µs, p99 {:>6.2} µs, mean {:>6.2} µs",
        stats.p50_us, stats.p95_us, stats.p99_us, stats.mean_us
    );
    println!();
}

fn main() {
    println!("JSNIR Engine Microbenchmark Suite (release build)\n");
    bench_mutations();
    bench_drag_resize();
    bench_bounded_wait();
    bench_bytecode_startup();
    bench_column_sync_churn();
}
