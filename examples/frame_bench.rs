//! Frame pipeline microbenchmark suite (Host::frame, Layout, Walker::draw, scroll clipping, hover, resize).
//!
//! Run in release mode:
//!   cargo run --release --features stepper --example frame_bench
use jsnir::{
    editor::PointSelection,
    host::Host,
    text::TextEngine,
};
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

fn wait_reply(host: &mut Host) {
    while host.busy() {
        host.poll();
        if host.busy() {
            std::thread::yield_now();
        }
    }
}

/// Builds a realistic Discord-like 2-pane app with:
/// - Resizable sidebar (id 2) + drag handle (id 3) + main column (id 4)
/// - Status bar text (id 5) + click trigger button (id 6)
/// - Scrollable message feed (id 7) containing `rows` message rows (each = 1 group Box + avatar Box + 2 Texts = 4 nodes)
/// Total nodes ≈ 8 + rows * 4 (for rows = 200 → 808 nodes!).
fn discord_like_script(rows: usize, long_wrapped: bool) -> String {
    let body_expr = if long_wrapped {
        "'Message payload ' + i + ' with an intentionally long multi-line paragraph that wraps across several lines inside the flexible chat column whenever the user drags the sidebar divider horizontally at 120 Hz. Testing zero-HarfBuzz Buffer re-wrapping!'"
    } else {
        "'Message payload ' + i + ' testing steady layout, scroll clipping, and hover walk.'"
    };
    format!(
        r#"
        var ROWS = {rows};
        var baseW = 240;
        var clicks = 0;
        function jsnirMount() {{
            var ops = [
                {{ op: 'create', id: 1, node: {{ kind: 'box', style: {{ direction: 'row', width: 1280, height: 720 }}, paint: {{ background: 0x11131aff }} }} }},
                {{ op: 'create', id: 2, parent: 1, node: {{ kind: 'box', style: {{ width: 240, height: 720, shrink: 0, padding: 12, gap: 6 }}, paint: {{ background: 0x181b25ff }} }} }},
                {{ op: 'create', id: 6, parent: 2, node: {{ kind: 'box', style: {{ height: 34, padding: [6, 10, 6, 10] }}, paint: {{ background: 0x3b82f6ff, radius: 6, hover: {{ brightness: 1.1 }} }}, press: 2 }} }},
                {{ op: 'create', id: 5, parent: 6, node: {{ kind: 'text', text: 'Status: 0', font: {{ size: 13, weight: 600 }}, paint: {{ color: 0xffffffff }} }} }},
                {{ op: 'create', id: 3, parent: 1, node: {{ kind: 'box', style: {{ width: 6, height: 720, shrink: 0 }}, paint: {{ background: 0x262a3aff, cursor: 'colResize', hover: {{ background: 0x3b82f6ff }} }}, drag: 1 }} }},
                {{ op: 'create', id: 4, parent: 1, node: {{ kind: 'box', style: {{ grow: 1, minWidth: 0, minHeight: 0, height: 720 }} }} }},
                {{ op: 'create', id: 7, parent: 4, node: {{ kind: 'box', style: {{ grow: 1, minHeight: 0, scroll: true, padding: 12, gap: 4 }}, paint: {{ scrollbar: {{ width: 6, thumb: 0xffffff44, radius: 3, minThumb: 24, inset: 2, autoHide: false }} }} }} }},
            ];
            var nextId = 10;
            for (var i = 0; i < ROWS; i++) {{
                var row = nextId++;
                var avatar = nextId++;
                var col = nextId++;
                var author = nextId++;
                var body = nextId++;
                ops.push({{
                    op: 'create', id: row, parent: 7,
                    node: {{
                        kind: 'box',
                        style: {{ direction: 'row', gap: 10, padding: [6, 10, 6, 10], group: true }},
                        paint: {{ radius: 6, hover: {{ background: 0x222634ff }} }},
                    }},
                }});
                ops.push({{
                    op: 'create', id: avatar, parent: row,
                    node: {{ kind: 'box', style: {{ width: 32, height: 32, shrink: 0 }}, paint: {{ background: 0x5865f2ff, radius: 16 }} }},
                }});
                ops.push({{
                    op: 'create', id: col, parent: row,
                    node: {{ kind: 'box', style: {{ grow: 1, minWidth: 0, gap: 2 }} }},
                }});
                ops.push({{
                    op: 'create', id: author, parent: col,
                    node: {{ kind: 'text', text: 'Developer #' + i, font: {{ size: 13, weight: 600 }}, paint: {{ color: 0xf8fafcff, hover: {{ underline: true }} }} }},
                }});
                ops.push({{
                    op: 'create', id: body, parent: col,
                    node: {{ kind: 'text', text: {body_expr}, font: {{ size: 13 }}, paint: {{ color: 0x94a3b8ff }} }},
                }});
            }}
            jsnir.batch(JSON.stringify({{ ops: ops }}));
        }}
        function jsnirDrag(cb, phase, x, y, sx, sy) {{
            if (cb !== 1) return;
            if (phase === 0) {{ baseW = jsnir.columns.width[2]; return; }}
            if (phase !== 1) return;
            var w = Math.max(140, Math.min(480, baseW + (x - sx)));
            var u = jsnir.cmd;
            var f = jsnir.cmdF32;
            // OP_SET_STYLE(8) id=2: width(0)|height(1)|shrink(3)|padding(5)|gap(7)
            u[0] = 8; u[1] = 2; u[2] = 1 | 2 | (1 << 3) | (1 << 5) | (1 << 7); u[3] = 0;
            f[4] = w; f[5] = 720; f[6] = 0;
            f[7] = 12; f[8] = 12; f[9] = 12; f[10] = 12; f[11] = 6;
            jsnir.commit(12);
        }}
        function jsnirEvent(cb) {{
            if (cb === 2) {{
                clicks++;
                var u = jsnir.cmd;
                u[0] = 10; u[1] = 5; u[2] = 0;
                jsnir.commitStrings(3, ['Status: ' + clicks]);
            }}
        }}
        "#
    )
}

/// Builds a true 100%-in-viewport 2D dashboard grid (cols × rows = 300 cards = 1,231 nodes,
/// ALL strictly inside 1280×720 with ZERO clip culling) to measure raw zero-clone walk + Motion skip.
fn visible_dashboard_grid_script(cols: usize, rows: usize) -> String {
    format!(
        r#"
        var COLS = {cols};
        var ROWS = {rows};
        function jsnirMount() {{
            var ops = [
                {{ op: 'create', id: 1, node: {{ kind: 'box', style: {{ width: 1280, height: 720, padding: 8, gap: 2 }}, paint: {{ background: 0x0f1117ff }} }} }},
            ];
            var nextId = 10;
            var idx = 0;
            for (var r = 0; r < ROWS; r++) {{
                var rowId = nextId++;
                ops.push({{
                    op: 'create', id: rowId, parent: 1,
                    node: {{ kind: 'box', style: {{ direction: 'row', height: 21, gap: 4 }} }},
                }});
                for (var c = 0; c < COLS; c++) {{
                    var card = nextId++;
                    var badge = nextId++;
                    var title = nextId++;
                    var metric = nextId++;
                    ops.push({{
                        op: 'create', id: card, parent: rowId,
                        node: {{
                            kind: 'box',
                            style: {{ direction: 'row', grow: 1, minWidth: 0, height: 21, gap: 4, padding: [2, 4, 2, 4], group: true }},
                            paint: {{ background: 0x1b1e28ff, hover: {{ background: 0x262b3aff }} }},
                        }},
                    }});
                    ops.push({{
                        op: 'create', id: badge, parent: card,
                        node: {{ kind: 'box', style: {{ width: 6, height: 14, shrink: 0 }}, paint: {{ background: 0x22c55eff, radius: 1 }} }},
                    }});
                    ops.push({{
                        op: 'create', id: title, parent: card,
                        node: {{ kind: 'text', text: 'M' + idx, font: {{ size: 10 }}, paint: {{ color: 0xe2e8f0ff }} }},
                    }});
                    ops.push({{
                        op: 'create', id: metric, parent: card,
                        node: {{ kind: 'text', text: (idx * 7) + 'ms', font: {{ size: 10 }}, paint: {{ color: 0x94a3b8ff }} }},
                    }});
                    idx++;
                }}
            }}
            jsnir.batch(JSON.stringify({{ ops: ops }}));
        }}
        "#
    )
}

fn print_row(name: &str, stats: &Stats, items: usize) {
    println!(
        "  {:<38} | p50 {:>7.2} µs | p95 {:>7.2} µs | p99 {:>7.2} µs | items: {:>4}",
        name, stats.p50_us, stats.p95_us, stats.p99_us, items
    );
}

fn main() {
    println!("JSNIR Frame Pipeline Microbenchmark Suite (release build)\n");

    let mut host = Host::new(discord_like_script(200, false), "{}".into(), TextEngine::default());
    host.set_wait_budget(Duration::from_millis(5));
    settle(&mut host);
    let node_count = host.tree.nodes.iter().count();
    println!("=== A. Discord-like 2-Pane Feed ({node_count} nodes, 200 chat rows in scroll box) ===");

    // 1. Steady-state frame (nothing changed)
    let warmup = 50;
    let iters = 400;
    let mut last_items = 0;
    let mut samples = Vec::with_capacity(iters);
    for i in 0..(warmup + iters) {
        let t = Instant::now();
        let frame = host.frame(1280., 720., 1., 4096);
        let dt = t.elapsed().as_nanos();
        last_items = frame.items.len();
        if i >= warmup {
            samples.push(dt);
        }
    }
    print_row("1. Steady-state frame()", &summarize(samples), last_items);

    // 2. Hover sweep across visible rows
    let mut samples = Vec::with_capacity(iters);
    for i in 0..(warmup + iters) {
        let y = 30. + ((i % 12) as f32) * 52.;
        let t = Instant::now();
        host.pointer_move(Some([450., y]));
        let frame = host.frame(1280., 720., 1., 4096);
        let dt = t.elapsed().as_nanos();
        last_items = frame.items.len();
        if i >= warmup {
            samples.push(dt);
        }
    }
    print_row("2. Pointer hover sweep + frame()", &summarize(samples), last_items);

    // 3. Continuous wheel scroll through the 200-row feed
    host.pointer_move(Some([450., 300.]));
    let mut samples = Vec::with_capacity(iters);
    for i in 0..(warmup + iters) {
        let dy = if (i / 100) % 2 == 0 { 28. } else { -28. };
        let t = Instant::now();
        host.wheel(dy);
        let frame = host.frame(1280., 720., 1., 4096);
        let dt = t.elapsed().as_nanos();
        last_items = frame.items.len();
        if i >= warmup {
            samples.push(dt);
        }
    }
    print_row("3. Wheel scroll + frame()", &summarize(samples), last_items);

    // 4. Single status-bar text update (click -> JS SetText -> poll -> frame)
    host.pointer_move(Some([40., 28.]));
    let mut samples = Vec::with_capacity(iters);
    for i in 0..(warmup + iters) {
        let t = Instant::now();
        host.pointer_down(PointSelection::Place);
        host.pointer_up();
        wait_reply(&mut host);
        let frame = host.frame(1280., 720., 1., 4096);
        let dt = t.elapsed().as_nanos();
        last_items = frame.items.len();
        if i >= warmup {
            samples.push(dt);
        }
    }
    print_row("4. Single SetText click + frame()", &summarize(samples), last_items);

    // 5. Live sidebar drag-resize (full Taffy relayout of 1000 nodes!)
    host.pointer_move(Some([243., 200.]));
    host.pointer_down(PointSelection::Place);
    wait_reply(&mut host);
    let resize_iters = 200;
    let mut samples = Vec::with_capacity(resize_iters);
    for i in 0..(warmup + resize_iters) {
        let x = 243. + ((i % 120) as f32 - 60.);
        let t = Instant::now();
        host.pointer_move(Some([x, 200.]));
        wait_reply(&mut host);
        let frame = host.frame(1280., 720., 1., 4096);
        let dt = t.elapsed().as_nanos();
        last_items = frame.items.len();
        if i >= warmup {
            samples.push(dt);
        }
    }
    host.pointer_up();
    wait_reply(&mut host);
    print_row("5. Sidebar drag-resize + frame()", &summarize(samples), last_items);

    // 5b. Live sidebar drag-resize with 200 LONG WRAPPED multi-line paragraphs!
    let mut wrapped_host = Host::new(
        discord_like_script(200, true),
        "{}".into(),
        TextEngine::default(),
    );
    wrapped_host.set_wait_budget(Duration::from_millis(5));
    settle(&mut wrapped_host);
    wrapped_host.pointer_move(Some([243., 200.]));
    wrapped_host.pointer_down(PointSelection::Place);
    wait_reply(&mut wrapped_host);
    let mut samples = Vec::with_capacity(resize_iters);
    for i in 0..(warmup + resize_iters) {
        let x = 243. + ((i % 120) as f32 - 60.);
        let t = Instant::now();
        wrapped_host.pointer_move(Some([x, 200.]));
        wait_reply(&mut wrapped_host);
        let frame = wrapped_host.frame(1280., 720., 1., 4096);
        let dt = t.elapsed().as_nanos();
        last_items = frame.items.len();
        if i >= warmup {
            samples.push(dt);
        }
    }
    wrapped_host.pointer_up();
    wait_reply(&mut wrapped_host);
    print_row("5b. Wrapped-200 drag-resize + frame()", &summarize(samples), last_items);
    println!();

    // B. Dense 100%-visible 2D dashboard grid (10x30 = 300 cards = 1231 nodes, ALL inside 1280x720, 0 culled!)
    let mut dash = Host::new(
        visible_dashboard_grid_script(10, 30),
        "{}".into(),
        TextEngine::default(),
    );
    settle(&mut dash);
    let dash_nodes = dash.tree.nodes.iter().count();
    println!("=== B. Dense 100%-Visible 2D Grid ({dash_nodes} nodes, 0 clip culling) ===");

    let mut samples = Vec::with_capacity(iters);
    for i in 0..(warmup + iters) {
        let t = Instant::now();
        let frame = dash.frame(1280., 720., 1., 4096);
        let dt = t.elapsed().as_nanos();
        last_items = frame.items.len();
        if i >= warmup {
            samples.push(dt);
        }
    }
    print_row("6. All-visible steady frame()", &summarize(samples), last_items);

    let mut samples = Vec::with_capacity(iters);
    for i in 0..(warmup + iters) {
        let x = 30. + ((i % 10) as f32) * 124.;
        let y = 16. + (((i / 10) % 30) as f32) * 23.;
        let t = Instant::now();
        dash.pointer_move(Some([x, y]));
        let frame = dash.frame(1280., 720., 1., 4096);
        let dt = t.elapsed().as_nanos();
        last_items = frame.items.len();
        if i >= warmup {
            samples.push(dt);
        }
    }
    print_row("7. All-visible hover sweep + frame()", &summarize(samples), last_items);
}
