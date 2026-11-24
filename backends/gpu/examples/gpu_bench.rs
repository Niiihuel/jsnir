//! Release microbenchmark for the JSNIR GPU backend (`View` + `Renderer`).
//! Run with:
//!   cargo run --offline --manifest-path backends/gpu/Cargo.toml --release --example gpu_bench

use jsnir::{host::Host, text::TextEngine};
use jsnir_gpu::{renderer::Renderer, view::View};
use std::time::{Duration, Instant};

const INTER_TTF: &[u8] = include_bytes!("../../../assets/fonts/Inter-Regular.ttf");

fn host_with_font(script: String) -> Host {
    let mut host = Host::new(script, "{}".into(), TextEngine::default());
    host.add_font(INTER_TTF.to_vec());
    host.set_default_family("Inter".into());
    host.hide_caret = true;
    host.keyframes_at = Some(0.);
    host
}

fn settle(host: &mut Host, w: f32, h: f32, scale: f32, max_side: usize) {
    for _ in 0..10 {
        host.poll();
        let f = host.frame(w, h, scale, max_side);
        if !f.animating && !host.busy() {
            break;
        }
    }
}

fn percentile(sorted: &[Duration], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let idx = (((sorted.len() - 1) as f64) * p).round() as usize;
    sorted[idx.min(sorted.len() - 1)].as_secs_f64() * 1_000_000.0
}

fn print_stats(label: &str, mut samples: Vec<Duration>, extra: &str) {
    samples.sort_unstable();
    let p50 = percentile(&samples, 0.50);
    let p95 = percentile(&samples, 0.95);
    let p99 = percentile(&samples, 0.99);
    println!(
        "  {label:<44} | p50 {:8.2} µs | p95 {:8.2} µs | p99 {:8.2} µs | {extra}",
        p50, p95, p99
    );
}

fn create_target(device: &wgpu::Device, size: [u32; 2], format: wgpu::TextureFormat) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some("bench target"),
        size: wgpu::Extent3d {
            width: size[0],
            height: size[1],
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    })
}

fn chat_script(rows: usize) -> String {
    format!(
        r#"
        function jsnirMount() {{
            var ops = [
                {{ op: 'create', id: 1, node: {{ kind: 'box', style: {{ width: 1280, height: 720, direction: 'column', scroll: true, clip: true }}, paint: {{ background: 0x11131aff, scrollbar: {{ width: 6, thumb: 0x475569ff, radius: 3, minThumb: 24, inset: 2 }} }} }} }}
            ];
            var id = 2;
            for (var i = 0; i < {rows}; i++) {{
                var rowId = id++;
                var badgeId = id++;
                var colId = id++;
                var authorId = id++;
                var bodyId = id++;
                ops.push({{ op: 'create', id: rowId, parent: 1, node: {{ kind: 'box', style: {{ direction: 'row', height: 52, padding: [6, 12, 6, 12], gap: 10, align: 'center' }}, paint: {{ background: i % 2 === 0 ? 0x181c26ff : 0x151822ff, radius: 6, edge: 0x262c3aff, edgeWidth: 1 }} }} }});
                ops.push({{ op: 'create', id: badgeId, parent: rowId, node: {{ kind: 'icon', icon: i % 2 === 0 ? 'checkCircle' : 'tick', size: 16, stroke: 2, paint: {{ color: 0x5865f2ff }} }} }});
                ops.push({{ op: 'create', id: colId, parent: rowId, node: {{ kind: 'box', style: {{ direction: 'column', grow: 1, minWidth: 0, gap: 2 }} }} }});
                ops.push({{ op: 'create', id: authorId, parent: colId, node: {{ kind: 'text', text: 'User #' + i + '  12:' + (i % 60 < 10 ? '0' : '') + (i % 60), font: {{ size: 13, weight: 600 }}, paint: {{ color: 0xf8fafcff }} }} }});
                ops.push({{ op: 'create', id: bodyId, parent: colId, node: {{ kind: 'text', text: 'Message #' + i + ': Streaming binary commands into GPU batched quads!', font: {{ size: 13 }}, paint: {{ color: 0x94a3b8ff }} }} }});
            }}
            jsnir.batch(JSON.stringify({{ ops: ops }}));
        }}
        "#
    )
}

fn dashboard_grid_script(cols: usize, rows: usize) -> String {
    format!(
        r#"
        function jsnirMount() {{
            var ops = [
                {{ op: 'create', id: 1, node: {{ kind: 'box', style: {{ width: 1280, height: 720, direction: 'column', padding: 8, gap: 4 }}, paint: {{ background: 0x0f1117ff }} }} }}
            ];
            var id = 2;
            for (var r = 0; r < {rows}; r++) {{
                var rowId = id++;
                ops.push({{ op: 'create', id: rowId, parent: 1, node: {{ kind: 'box', style: {{ direction: 'row', grow: 1, gap: 4 }} }} }});
                for (var c = 0; c < {cols}; c++) {{
                    var idx = r * {cols} + c;
                    var cardId = id++;
                    var iconId = id++;
                    var titleId = id++;
                    var valId = id++;
                    ops.push({{ op: 'create', id: cardId, parent: rowId, node: {{ kind: 'box', style: {{ direction: 'row', grow: 1, padding: [2, 4, 2, 4], gap: 4, align: 'center' }}, paint: {{ background: 0x1a1f2cff, radius: 4, edge: 0x2c3347ff, edgeWidth: 1 }} }} }});
                    ops.push({{ op: 'create', id: iconId, parent: cardId, node: {{ kind: 'icon', icon: 'tick', size: 10, stroke: 1.5, paint: {{ color: 0x22c55eff }} }} }});
                    ops.push({{ op: 'create', id: titleId, parent: cardId, node: {{ kind: 'text', text: 'M' + idx, font: {{ size: 10 }}, paint: {{ color: 0x94a3b8ff }} }} }});
                    ops.push({{ op: 'create', id: valId, parent: cardId, node: {{ kind: 'text', text: '' + (1000 + idx * 7), font: {{ size: 10 }}, paint: {{ color: 0xf8fafcff }} }} }});
                }}
            }}
            jsnir.batch(JSON.stringify({{ ops: ops }}));
        }}
        "#
    )
}

fn rich_spans_script(count: usize) -> String {
    format!(
        r#"
        function jsnirMount() {{
            var ops = [
                {{ op: 'create', id: 1, node: {{ kind: 'box', style: {{ width: 1280, height: 720, direction: 'column', padding: 8, gap: 2 }}, paint: {{ background: 0x11131aff }} }} }}
            ];
            var id = 2;
            var perRow = 4;
            for (var r = 0; r < {count} / perRow; r++) {{
                var rowId = id++;
                ops.push({{ op: 'create', id: rowId, parent: 1, node: {{ kind: 'box', style: {{ direction: 'row', height: 24, gap: 8 }} }} }});
                for (var c = 0; c < perRow; c++) {{
                    var idx = r * perRow + c;
                    var tid = id++;
                    ops.push({{ op: 'create', id: tid, parent: rowId, node: {{
                        kind: 'text',
                        text: '@user' + idx + ' shipped #pr' + idx + ' ok',
                        font: {{ size: 12 }},
                        style: {{ grow: 1 }},
                        spans: [
                            {{ text: '@user' + idx + ' ', color: 0x38bdf8ff, weight: 600, background: 0x1e293bff, backgroundRadius: 3 }},
                            {{ text: 'shipped ', color: 0xe2e8f0ff }},
                            {{ text: '#pr' + idx + ' ', color: 0xa78bfaff, underline: true }},
                            {{ text: 'ok', color: 0x4ade80ff, italic: true }}
                        ]
                    }} }});
                }}
            }}
            jsnir.batch(JSON.stringify({{ ops: ops }}));
        }}
        "#
    )
}

fn layered_cards_script(count: usize) -> String {
    format!(
        r#"
        function jsnirMount() {{
            var ops = [
                {{ op: 'create', id: 1, node: {{ kind: 'box', style: {{ width: 1280, height: 720, direction: 'column', padding: 16, gap: 8 }}, paint: {{ background: 0x0f172aff }} }} }}
            ];
            var id = 2;
            for (var r = 0; r < {count} / 5; r++) {{
                var rowId = id++;
                ops.push({{ op: 'create', id: rowId, parent: 1, node: {{ kind: 'box', style: {{ direction: 'row', grow: 1, gap: 10 }} }} }});
                for (var c = 0; c < 5; c++) {{
                    var idx = r * 5 + c;
                    var cardId = id++;
                    var icoId = id++;
                    var txtId = id++;
                    ops.push({{ op: 'create', id: cardId, parent: rowId, node: {{
                        kind: 'box',
                        style: {{ direction: 'row', grow: 1, padding: 10, gap: 8, align: 'center' }},
                        paint: {{
                            background: 0x1e293bff,
                            radius: 8,
                            edge: 0x334155ff,
                            edgeWidth: 1,
                            shadows: [{{ y: 4, blur: 10, spread: 0, color: 0x00000055 }}]
                        }}
                    }} }});
                    ops.push({{ op: 'create', id: icoId, parent: cardId, node: {{ kind: 'icon', icon: 'checkCircle', size: 16, stroke: 2, paint: {{ color: 0x38bdf8ff }} }} }});
                    ops.push({{ op: 'create', id: txtId, parent: cardId, node: {{ kind: 'text', text: 'Card #' + idx + ' status', font: {{ size: 13 }}, paint: {{ color: 0xf1f5f9ff }} }} }});
                }}
            }}
            // Semi-transparent modal layer overlay
            var layerId = id++;
            var modalTxt = id++;
            ops.push({{ op: 'create', id: layerId, parent: 1, node: {{
                kind: 'box',
                style: {{ position: 'fixed', left: 420, top: 240, width: 440, height: 200, padding: 20, gap: 8, layer: 10 }},
                paint: {{ background: 0x1e1b4bff, radius: 12, opacity: 0.92, shadows: [{{ y: 12, blur: 24, spread: 0, color: 0x00000088 }}] }}
            }} }});
            ops.push({{ op: 'create', id: modalTxt, parent: layerId, node: {{ kind: 'text', text: 'Translucent Overlay Layer', font: {{ size: 16, weight: 600 }}, paint: {{ color: 0xffffffff }} }} }});
            jsnir.batch(JSON.stringify({{ ops: ops }}));
        }}
        "#
    )
}

fn main() {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let adapter = pollster::block_on(instance.request_adapter(&Default::default()))
        .expect("GPU adapter required");
    let info = adapter.get_info();
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let mut renderer = Renderer::new(&adapter, format).expect("renderer");
    let max_side = renderer.device.limits().max_texture_dimension_2d as usize;
    let size = [1280u32, 720u32];
    let scale = 1.0f32;
    let target = create_target(&renderer.device, size, format);

    println!(
        "JSNIR GPU Backend Microbenchmark Suite (release build)\nAdapter: {} ({:?}, {:?})\n",
        info.name, info.device_type, info.backend
    );

    // -------------------------------------------------------------------------
    // 1. Steady 200-row chat feed (1280x720, ~14 visible rows)
    // -------------------------------------------------------------------------
    {
        let mut host = host_with_font(chat_script(200));
        let mut view = View::default();
        settle(&mut host, 1280., 720., scale, max_side);
        // Warm cache
        for _ in 0..5 {
            let scene = host.frame(1280., 720., scale, max_side);
            let draws = view.draws(&mut host, &mut renderer, scene.items, scale);
            renderer.draw(&target, size, scale, &draws);
            let _ = renderer.device.poll(wgpu::PollType::wait_indefinitely());
        }
        let mut view_times = Vec::with_capacity(60);
        let mut submit_times = Vec::with_capacity(60);
        let mut gpu_times = Vec::with_capacity(60);
        let mut draw_count = 0usize;
        let mut stats = renderer.last_stats();
        for _ in 0..60 {
            let scene = host.frame(1280., 720., scale, max_side);
            let t0 = Instant::now();
            let draws = view.draws(&mut host, &mut renderer, scene.items, scale);
            let t1 = Instant::now();
            draw_count = draws.len();
            renderer.draw(&target, size, scale, &draws);
            let t2 = Instant::now();
            stats = renderer.last_stats();
            let _ = renderer.device.poll(wgpu::PollType::wait_indefinitely());
            let t3 = Instant::now();
            view_times.push(t1 - t0);
            submit_times.push(t2 - t1);
            gpu_times.push(t3 - t1);
        }
        let info = format!(
            "items={draw_count} calls={} bg={} sc={}",
            stats.draw_calls, stats.bind_group_switches, stats.scissor_switches
        );
        println!("=== 1. Steady 200-row chat feed (1280x720) ===");
        print_stats("view.draws() (CPU cache lookup)", view_times, &info);
        print_stats("renderer.draw() (CPU encode+submit)", submit_times, &info);
        print_stats("renderer.draw() + GPU wait", gpu_times, &info);
    }

    // -------------------------------------------------------------------------
    // 2. Dense 100%-visible 2D dashboard grid (12x25 = 300 cards, 900 texts/icons, ~1500 quads)
    // -------------------------------------------------------------------------
    {
        renderer.clear_atlas();
        let mut host = host_with_font(dashboard_grid_script(12, 25));
        let mut view = View::default();
        settle(&mut host, 1280., 720., scale, max_side);

        // Cold frame (rasterize + upload all 900 tiles)
        let scene = host.frame(1280., 720., scale, max_side);
        let t0 = Instant::now();
        let draws = view.draws(&mut host, &mut renderer, scene.items, scale);
        let t1 = Instant::now();
        renderer.draw(&target, size, scale, &draws);
        let t2 = Instant::now();
        let _ = renderer.device.poll(wgpu::PollType::wait_indefinitely());
        let t3 = Instant::now();
        let draw_count = draws.len();

        // Warm steady frames
        let mut view_times = Vec::with_capacity(40);
        let mut submit_times = Vec::with_capacity(40);
        let mut gpu_times = Vec::with_capacity(40);
        let mut stats = renderer.last_stats();
        for _ in 0..40 {
            let scene = host.frame(1280., 720., scale, max_side);
            let s0 = Instant::now();
            let draws = view.draws(&mut host, &mut renderer, scene.items, scale);
            let s1 = Instant::now();
            renderer.draw(&target, size, scale, &draws);
            let s2 = Instant::now();
            stats = renderer.last_stats();
            let _ = renderer.device.poll(wgpu::PollType::wait_indefinitely());
            let s3 = Instant::now();
            view_times.push(s1 - s0);
            submit_times.push(s2 - s1);
            gpu_times.push(s3 - s1);
        }
        let info = format!(
            "items={draw_count} calls={} bg={} sc={}",
            stats.draw_calls, stats.bind_group_switches, stats.scissor_switches
        );
        println!("\n=== 2. Dense 100%-visible 2D grid (300 cards, 900 texts/icons, 0 culling) ===");
        println!(
            "  COLD frame: view.draws={:.2} ms, encode+submit={:.2} ms, total+GPU={:.2} ms ({info})",
            (t1 - t0).as_secs_f64() * 1e3,
            (t2 - t1).as_secs_f64() * 1e3,
            (t3 - t0).as_secs_f64() * 1e3
        );
        print_stats("WARM view.draws() (CPU cache)", view_times, &info);
        print_stats("WARM renderer.draw() (CPU encode+submit)", submit_times, &info);
        print_stats("WARM renderer.draw() + GPU wait", gpu_times, &info);
    }

    // -------------------------------------------------------------------------
    // 3. Scroll sweep: 80 steps down (new rows) + 80 steps back up (revisited rows)
    // -------------------------------------------------------------------------
    {
        renderer.clear_atlas();
        let mut host = host_with_font(chat_script(200));
        let mut view = View::default();
        settle(&mut host, 1280., 720., scale, max_side);
        host.pointer_move(Some([400., 360.]));
        // Initial frame
        let scene = host.frame(1280., 720., scale, max_side);
        let draws = view.draws(&mut host, &mut renderer, scene.items, scale);
        renderer.draw(&target, size, scale, &draws);
        let _ = renderer.device.poll(wgpu::PollType::wait_indefinitely());

        let mut down_view = Vec::with_capacity(80);
        let mut down_total = Vec::with_capacity(80);
        for _ in 0..80 {
            host.wheel(64.0);
            let scene = host.frame(1280., 720., scale, max_side);
            let t0 = Instant::now();
            let draws = view.draws(&mut host, &mut renderer, scene.items, scale);
            let t1 = Instant::now();
            renderer.draw(&target, size, scale, &draws);
            let _ = renderer.device.poll(wgpu::PollType::wait_indefinitely());
            let t2 = Instant::now();
            down_view.push(t1 - t0);
            down_total.push(t2 - t0);
        }
        let mut up_view = Vec::with_capacity(80);
        let mut up_total = Vec::with_capacity(80);
        for _ in 0..80 {
            host.wheel(-64.0);
            let scene = host.frame(1280., 720., scale, max_side);
            let t0 = Instant::now();
            let draws = view.draws(&mut host, &mut renderer, scene.items, scale);
            let t1 = Instant::now();
            renderer.draw(&target, size, scale, &draws);
            let _ = renderer.device.poll(wgpu::PollType::wait_indefinitely());
            let t2 = Instant::now();
            up_view.push(t1 - t0);
            up_total.push(t2 - t0);
        }
        let stats = renderer.last_stats();
        let info = format!("64px/step calls={}", stats.draw_calls);
        println!("\n=== 3. Scroll sweep through 200-row chat (down 80 steps, then back up 80 steps) ===");
        print_stats("Scroll DOWN view.draws() (new rows)", down_view, &info);
        print_stats("Scroll DOWN view+GPU total", down_total, &info);
        print_stats("Scroll UP   view.draws() (revisited rows)", up_view, &info);
        print_stats("Scroll UP   view+GPU total", up_total, &info);
    }

    // -------------------------------------------------------------------------
    // 4. 100 visible rich-span Text nodes (4 styled spans each)
    // -------------------------------------------------------------------------
    {
        renderer.clear_atlas();
        let mut host = host_with_font(rich_spans_script(100));
        let mut view = View::default();
        settle(&mut host, 1280., 720., scale, max_side);
        for _ in 0..3 {
            let scene = host.frame(1280., 720., scale, max_side);
            let draws = view.draws(&mut host, &mut renderer, scene.items, scale);
            renderer.draw(&target, size, scale, &draws);
            let _ = renderer.device.poll(wgpu::PollType::wait_indefinitely());
        }
        let mut view_times = Vec::with_capacity(60);
        let mut gpu_times = Vec::with_capacity(60);
        let mut draw_count = 0usize;
        let mut stats = renderer.last_stats();
        for _ in 0..60 {
            let scene = host.frame(1280., 720., scale, max_side);
            let t0 = Instant::now();
            let draws = view.draws(&mut host, &mut renderer, scene.items, scale);
            let t1 = Instant::now();
            draw_count = draws.len();
            renderer.draw(&target, size, scale, &draws);
            stats = renderer.last_stats();
            let _ = renderer.device.poll(wgpu::PollType::wait_indefinitely());
            let t2 = Instant::now();
            view_times.push(t1 - t0);
            gpu_times.push(t2 - t1);
        }
        let info = format!(
            "items={draw_count} calls={} bg={} sc={}",
            stats.draw_calls, stats.bind_group_switches, stats.scissor_switches
        );
        println!("\n=== 4. 100 visible rich-span Text nodes (4 spans each) ===");
        print_stats("WARM view.draws() (spans key lookup)", view_times, &info);
        print_stats("WARM renderer.draw() + GPU wait", gpu_times, &info);
    }

    // -------------------------------------------------------------------------
    // 5. Layered + shadowed cards + translucent modal overlay (30 cards + layer)
    // -------------------------------------------------------------------------
    {
        renderer.clear_atlas();
        let mut host = host_with_font(layered_cards_script(30));
        let mut view = View::default();
        settle(&mut host, 1280., 720., scale, max_side);
        for _ in 0..3 {
            let scene = host.frame(1280., 720., scale, max_side);
            let draws = view.draws(&mut host, &mut renderer, scene.items, scale);
            renderer.draw(&target, size, scale, &draws);
            let _ = renderer.device.poll(wgpu::PollType::wait_indefinitely());
        }
        let mut view_times = Vec::with_capacity(60);
        let mut submit_times = Vec::with_capacity(60);
        let mut gpu_times = Vec::with_capacity(60);
        let mut draw_count = 0usize;
        let mut stats = renderer.last_stats();
        for _ in 0..60 {
            let scene = host.frame(1280., 720., scale, max_side);
            let t0 = Instant::now();
            let draws = view.draws(&mut host, &mut renderer, scene.items, scale);
            let t1 = Instant::now();
            draw_count = draws.len();
            renderer.draw(&target, size, scale, &draws);
            let t2 = Instant::now();
            stats = renderer.last_stats();
            let _ = renderer.device.poll(wgpu::PollType::wait_indefinitely());
            let t3 = Instant::now();
            view_times.push(t1 - t0);
            submit_times.push(t2 - t1);
            gpu_times.push(t3 - t1);
        }
        let info = format!(
            "items={draw_count} calls={} bg={} sc={}",
            stats.draw_calls, stats.bind_group_switches, stats.scissor_switches
        );
        println!("\n=== 5. Layered + shadowed cards + opacity overlay ===");
        print_stats("WARM view.draws()", view_times, &info);
        print_stats("WARM renderer.draw() (CPU encode+submit)", submit_times, &info);
        print_stats("WARM renderer.draw() + GPU wait", gpu_times, &info);
    }
}
