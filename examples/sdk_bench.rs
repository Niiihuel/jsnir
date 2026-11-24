//! End-to-end SDK microbenchmark suite measuring REAL `app/sdk.ts` TSX component mounts,
//! `For` / `Dynamic` swaps, `Row`/`Column` composition, rich `spans`, `Popover`, `transitions`,
//! `shadows`, `scrollbar`, `TextInput`, and live signal updates in QuickJS + Host.
//!
//! Run in release mode:
//!   cargo run --release --features stepper --example sdk_bench
use jsnir::{apps::SDK, host::Host, text::TextEngine};
use std::time::{Duration, Instant};

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

fn settle(host: &mut Host) {
    for _ in 0..200 {
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
    let mut s = String::with_capacity(SDK.len() + app.len() + 512);
    // Wrap jsnir.batch / commit / commitStrings to count exact JSON vs binary bridge calls.
    s.push_str(
        r#"
var __jsonCalls = 0, __binCalls = 0;
(function() {
    var origBatch = jsnir.batch;
    var origCommit = jsnir.commit;
    var origCommitStrings = jsnir.commitStrings;
    jsnir.batch = function(json) { __jsonCalls++; return origBatch(json); };
    jsnir.commit = function(w) { __binCalls++; return origCommit(w); };
    jsnir.commitStrings = function(w, s) { __binCalls++; return origCommitStrings(w, s); };
})();
"#,
    );
    s.push_str(SDK);
    s.push('\n');
    s.push_str(app);
    s
}

fn read_counts(host: &mut Host) -> (u64, u64) {
    host.push(r#"{"cmd":"stats"}"#.into());
    wait_reply(host);
    // Stats are written into label id 9999 via host callback? Even simpler: use with_request_handler or read from text node!
    let mut json_calls = 0;
    let mut bin_calls = 0;
    for (_, node) in host.tree.nodes.iter() {
        if let jsnir::tree::Node::Text { text, .. } = node
            && let Some(rest) = text.strip_prefix("STATS:")
        {
            let mut parts = rest.split(':');
            json_calls = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
            bin_calls = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
        }
    }
    (json_calls, bin_calls)
}

fn print_row(name: &str, stats: &Stats, json_calls: u64, bin_calls: u64) {
    println!(
        "  {:<42} | p50 {:>8.2} µs | p95 {:>8.2} µs | p99 {:>8.2} µs | JSON:{:>5} Bin:{:>5}",
        name, stats.p50_us, stats.p95_us, stats.p99_us, json_calls, bin_calls
    );
}

fn main() {
    println!("JSNIR End-to-End SDK Microbenchmark Suite (release build, real app/sdk.ts)\n");

    // 1. Realistic 2-pane feed swap via `For` (100 interactive rows using Row/Column,
    //    transitions, shadows, hover, scrollbar, TextInput).
    let feed_app = with_sdk(
        r#"
        var rows = signal([]);
        var statsText = signal("STATS:0:0");
        var epoch = 0;
        function makeRows(n, ep) {
            var out = [];
            for (var i = 0; i < n; i++) {
                out.push({ key: ep + "-" + i, author: "Dev #" + i, body: "Message " + ep + "." + i + " with transitions & shadows" });
            }
            return out;
        }
        onHost(function(msg) {
            if (msg.cmd === "reset") { __jsonCalls = 0; __binCalls = 0; return; }
            if (msg.cmd === "stats") { statsText.value = "STATS:" + __jsonCalls + ":" + __binCalls; return; }
            if (msg.cmd === "swap") { epoch++; rows.value = makeRows(msg.n, epoch); }
        });
        mount(function() {
            return h(Column, { style: { width: 1280, height: 720 }, paint: { background: 0x11131aff } },
                h(Text, { children: statsText, font: { size: 11 } }),
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

    let mut host = Host::new(feed_app, "{}".into(), TextEngine::default());
    host.set_wait_budget(Duration::from_millis(20));
    settle(&mut host);

    let warmup = 10;
    let iters = 60;
    let mut samples = Vec::with_capacity(iters);
    for i in 0..(warmup + iters) {
        if i == warmup {
            host.push(r#"{"cmd":"reset"}"#.into());
            wait_reply(&mut host);
        }
        let t = Instant::now();
        host.push(r#"{"cmd":"swap","n":100}"#.into());
        wait_reply(&mut host);
        let dt = t.elapsed().as_nanos();
        if i >= warmup {
            samples.push(dt);
        }
    }
    let (jc, bc) = read_counts(&mut host);
    print_row("1. For swap 100 rich rows (trans+shadow)", &summarize(samples), jc, bc);

    // 2. Plain `For` swap (100 rows WITHOUT transitions/shadows — measures Row/Column + direct binary streaming!)
    let plain_app = with_sdk(
        r#"
        var rows = signal([]);
        var statsText = signal("STATS:0:0");
        var epoch = 0;
        function makeRows(n, ep) {
            var out = [];
            for (var i = 0; i < n; i++) {
                out.push({ key: ep + "-" + i, author: "Dev #" + i, body: "Plain message " + ep + "." + i });
            }
            return out;
        }
        onHost(function(msg) {
            if (msg.cmd === "reset") { __jsonCalls = 0; __binCalls = 0; return; }
            if (msg.cmd === "stats") { statsText.value = "STATS:" + __jsonCalls + ":" + __binCalls; return; }
            if (msg.cmd === "swap") { epoch++; rows.value = makeRows(msg.n, epoch); }
        });
        mount(function() {
            return h(Column, { style: { width: 1280, height: 720 }, paint: { background: 0x11131aff } },
                h(Text, { children: statsText, font: { size: 11 } }),
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

    let mut plain_host = Host::new(plain_app, "{}".into(), TextEngine::default());
    plain_host.set_wait_budget(Duration::from_millis(20));
    settle(&mut plain_host);
    let mut samples = Vec::with_capacity(iters);
    for i in 0..(warmup + iters) {
        if i == warmup {
            plain_host.push(r#"{"cmd":"reset"}"#.into());
            wait_reply(&mut plain_host);
        }
        let t = Instant::now();
        plain_host.push(r#"{"cmd":"swap","n":100}"#.into());
        wait_reply(&mut plain_host);
        let dt = t.elapsed().as_nanos();
        if i >= warmup {
            samples.push(dt);
        }
    }
    let (jc, bc) = read_counts(&mut plain_host);
    print_row("2. For swap 100 plain rows (Row+Column)", &summarize(samples), jc, bc);

    // 3. Rich text `spans` mount + live `setSpans` updates (50 rich messages per event)
    let spans_app = with_sdk(
        r#"
        var statsText = signal("STATS:0:0");
        var tick = signal(0);
        var items = [];
        for (var i = 0; i < 50; i++) items.push(i);
        onHost(function(msg) {
            if (msg.cmd === "reset") { __jsonCalls = 0; __binCalls = 0; return; }
            if (msg.cmd === "stats") { statsText.value = "STATS:" + __jsonCalls + ":" + __binCalls; return; }
            if (msg.cmd === "tick") { tick.value++; }
        });
        mount(function() {
            return h(Column, { style: { width: 1280, height: 720, gap: 4 } },
                h(Text, { children: statsText, font: { size: 11 } }),
                items.map(function(i) {
                    return h(Text, {
                        font: { size: 13 },
                        paint: { color: 0xe2e8f0ff },
                        spans: function() {
                            var t = tick.value;
                            return [
                                { text: "@user" + i, color: 0x60a5faff, weight: 600, background: 0x3b82f633, backgroundRadius: 4 },
                                { text: " pushed commit #" + t + " ", italic: true },
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

    let mut spans_host = Host::new(spans_app, "{}".into(), TextEngine::default());
    spans_host.set_wait_budget(Duration::from_millis(20));
    settle(&mut spans_host);
    let mut samples = Vec::with_capacity(120);
    for i in 0..(20 + 120) {
        if i == 20 {
            spans_host.push(r#"{"cmd":"reset"}"#.into());
            wait_reply(&mut spans_host);
        }
        let t = Instant::now();
        spans_host.push(r#"{"cmd":"tick"}"#.into());
        wait_reply(&mut spans_host);
        let dt = t.elapsed().as_nanos();
        if i >= 20 {
            samples.push(dt);
        }
    }
    let (jc, bc) = read_counts(&mut spans_host);
    print_row("3. 50 rich Text setSpans + setText updates", &summarize(samples), jc, bc);

    // 4. Live reactive SetPaint (with transitions+shadows) + SetStyle + SetField (50 widgets/event)
    let reactive_app = with_sdk(
        r#"
        var statsText = signal("STATS:0:0");
        var tick = signal(0);
        var fieldRef = null;
        var ids = [];
        for (var i = 0; i < 50; i++) ids.push(i);
        onHost(function(msg) {
            if (msg.cmd === "reset") { __jsonCalls = 0; __binCalls = 0; return; }
            if (msg.cmd === "stats") { statsText.value = "STATS:" + __jsonCalls + ":" + __binCalls; return; }
            if (msg.cmd === "tick") {
                tick.value++;
                if (fieldRef) fieldRef.replace("draft-" + tick.value);
            }
        });
        mount(function() {
            return h(Column, { style: { width: 1280, height: 720, gap: 2 } },
                h(Text, { children: statsText, font: { size: 11 } }),
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

    let mut reactive_host = Host::new(reactive_app, "{}".into(), TextEngine::default());
    reactive_host.set_wait_budget(Duration::from_millis(20));
    settle(&mut reactive_host);
    let mut samples = Vec::with_capacity(120);
    for i in 0..(20 + 120) {
        if i == 20 {
            reactive_host.push(r#"{"cmd":"reset"}"#.into());
            wait_reply(&mut reactive_host);
        }
        let t = Instant::now();
        reactive_host.push(r#"{"cmd":"tick"}"#.into());
        wait_reply(&mut reactive_host);
        let dt = t.elapsed().as_nanos();
        if i >= 20 {
            samples.push(dt);
        }
    }
    let (jc, bc) = read_counts(&mut reactive_host);
    print_row("4. 50 SetStyle+SetPaint(trans)+SetField", &summarize(samples), jc, bc);

    // 5. Popover toggle (Style.anchor + Paint.shadows + Paint.starting + Paint.transitions)
    let popover_app = with_sdk(
        r#"
        var statsText = signal("STATS:0:0");
        var open = signal(false);
        var anchorId = 0;
        onHost(function(msg) {
            if (msg.cmd === "reset") { __jsonCalls = 0; __binCalls = 0; return; }
            if (msg.cmd === "stats") { statsText.value = "STATS:" + __jsonCalls + ":" + __binCalls; return; }
            if (msg.cmd === "toggle") { open.value = !open.value; }
        });
        mount(function() {
            return h(Column, { style: { width: 800, height: 600, padding: 20 } },
                h(Text, { children: statsText, font: { size: 11 } }),
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

    let mut pop_host = Host::new(popover_app, "{}".into(), TextEngine::default());
    pop_host.set_wait_budget(Duration::from_millis(20));
    settle(&mut pop_host);
    let mut samples = Vec::with_capacity(200);
    for i in 0..(30 + 200) {
        if i == 30 {
            pop_host.push(r#"{"cmd":"reset"}"#.into());
            wait_reply(&mut pop_host);
        }
        let t = Instant::now();
        pop_host.push(r#"{"cmd":"toggle"}"#.into());
        wait_reply(&mut pop_host);
        let dt = t.elapsed().as_nanos();
        if i >= 30 {
            samples.push(dt);
        }
    }
    let (jc, bc) = read_counts(&mut pop_host);
    print_row("5. Popover toggle (anchor+shadow+start)", &summarize(samples), jc, bc);

    // 6. Cold full app mount of real STEPPER app (SDK + stepper.tsx, bundled Inter font to exclude system font scan)
    const INTER: &[u8] = include_bytes!("../assets/fonts/Inter-Regular.ttf");
    let mut samples = Vec::with_capacity(40);
    for i in 0..(5 + 40) {
        let t = Instant::now();
        let mut h = Host::new(
            jsnir::apps::STEPPER.into(),
            "{}".into(),
            TextEngine::with_fonts(&[INTER], [255; 3]),
        );
        settle(&mut h);
        let dt = t.elapsed().as_nanos();
        if i >= 5 {
            samples.push(dt);
        }
    }
    print_row("6. Real STEPPER cold mount + settle", &summarize(samples), 0, 0);
}
