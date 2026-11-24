use jsnir::{
    ScriptBundle,
    apps::{SDK, STEPPER},
    editor::PointSelection,
    engine::{JsEngineFactory, VmConfig, VmLimits, quickjs::QuickJsFactory},
    host::Host,
    text::TextEngine,
    tree::{Node, Op},
};
use jsnir_v8::V8Factory;
use std::{
    rc::Rc,
    sync::Arc,
    time::Duration,
};

const INTER: &[u8] = include_bytes!("../../../assets/fonts/Inter-Regular.ttf");

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

fn wait_reply(host: &mut Host) {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        host.poll();
        if !host.busy() {
            return;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    panic!("host reply timed out");
}

fn with_sdk(app: &str) -> String {
    let mut s = String::with_capacity(SDK.len() + app.len() + 512);
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

#[test]
fn v8_capabilities_and_bytecode_rejection() {
    let factory = V8Factory;
    assert_eq!(factory.name(), "v8");
    let caps = factory.capabilities();
    assert!(caps.jit);
    assert!(!caps.bytecode_aot);
    assert!(caps.in_place_reload);

    let err = factory
        .create(
            &ScriptBundle::Bytecode(b"JSNRBC01dummy".to_vec()),
            Rc::default(),
            &VmConfig::default(),
        )
        .err()
        .expect("V8 must reject QuickJS bytecode");
    assert!(
        err.contains("QuickJS") || err.contains("source"),
        "unexpected error: {err}"
    );
}

#[test]
fn v8_binary_sdk_roundtrip_zero_json_and_overflow_flush() {
    let script = with_sdk(
        r#"
        var rows = signal([]);
        var open = signal(false);
        var anchorId = 0;
        var fieldRef = null;
        var statsText = signal("STATS:0:0");
        onHost(function(msg) {
            if (msg.cmd === "flood") {
                var arr = [];
                for (var i = 0; i < 700; i++) {
                    arr.push({ key: "k" + i, title: "Row #" + i, desc: "Desc " + i });
                }
                rows.value = arr;
                open.value = true;
                if (fieldRef) fieldRef.replace("updated-draft");
            } else if (msg.cmd === "stats") {
                statsText.value = "STATS:" + __jsonCalls + ":" + __binCalls;
            }
        });
        mount(function() {
            return h(Column, { style: { width: 1280, height: 720 }, paint: { background: 0x11131aff } },
                h(Text, { children: statsText, font: { size: 11 } }),
                h(Box, { ref: function(id) { anchorId = id; }, style: { width: 100, height: 30 } }),
                h(TextInput, { value: "init", ref: function(r) { fieldRef = r; }, style: { height: 32 } }),
                h(Popover, {
                    open: open,
                    anchor: function() { return anchorId; },
                    side: "bottom",
                    style: { width: 200, padding: 8 },
                    paint: {
                        background: 0x1e2230ff,
                        radius: 8,
                        shadows: [{ y: 6, blur: 16, spread: 0, color: 0x00000088 }],
                        transitions: [{ property: "opacity", duration: 120, easing: "easeOut" }],
                    },
                    children: h(Text, {
                        spans: [
                            { text: "@v8", color: 0x60a5faff, weight: 600 },
                            { text: " binary ok", italic: true },
                        ],
                        children: "@v8 binary ok",
                    }),
                }),
                h(For, {
                    each: rows,
                    key: function(r) { return r.key; },
                    style: { grow: 1, scroll: true },
                    children: function(item) {
                        return h(Row, {
                            style: { gap: 8, padding: [4, 8, 4, 8] },
                            paint: {
                                background: 0x181b25ff,
                                radius: 6,
                                shadows: [{ y: 1, blur: 4, spread: 0, color: 0x00000033 }],
                                transitions: [{ property: "background", duration: 100, easing: "easeOut" }],
                            },
                        },
                            h(Text, { children: function() { return item().title; } }),
                            h(Text, { children: function() { return item().desc; } }),
                        );
                    },
                }),
            );
        });
        "#,
    );

    let mut host = Host::try_new_with_engine(
        Arc::new(V8Factory),
        ScriptBundle::Source(script),
        "{}".into(),
        TextEngine::default(),
    )
    .expect("v8 host");
    host.set_wait_budget(Duration::from_millis(50));
    settle(&mut host);

    host.push(r#"{"cmd":"flood"}"#.into());
    wait_reply(&mut host);
    let _ = host.frame(1280., 720., 1., 4096);

    host.push(r#"{"cmd":"stats"}"#.into());
    wait_reply(&mut host);

    let mut stats = None;
    let mut found_row_699 = false;
    let mut found_span_text = false;
    let mut found_updated_field = false;
    for (_, node) in host.tree.nodes.iter() {
        match node {
            Node::Text { text, spans, .. } => {
                if text.starts_with("STATS:") {
                    stats = Some(text.clone());
                }
                if text == "Row #699" {
                    found_row_699 = true;
                }
                if text == "@v8 binary ok" && spans.as_ref().is_some_and(|s| s.len() == 2) {
                    found_span_text = true;
                }
            }
            Node::Input { text, .. } if text == "updated-draft" => {
                found_updated_field = true;
            }
            _ => {}
        }
    }

    let stats = stats.expect("stats text node");
    let parts: Vec<u64> = stats
        .strip_prefix("STATS:")
        .unwrap()
        .split(':')
        .map(|p| p.parse().unwrap())
        .collect();
    assert_eq!(parts[0], 0, "expected ZERO JSON calls on V8, got {stats}");
    assert!(
        parts[1] >= 2,
        "expected >=2 binary commits (overflow flush), got {stats}"
    );
    assert!(found_row_699, "expected all 700 rows mounted across overflow flush");
    assert!(found_span_text, "expected styled spans in Popover");
    assert!(found_updated_field, "expected updated TextInput field");
}

#[test]
fn v8_fast_api_set_and_commit_turbofan_stress() {
    // Runs tight hot loops long enough for V8 Maglev/TurboFan to compile both `jsnir.set`
    // and `jsnir.commit` onto `FAST_SET` and `FAST_COMMIT`, plus verifies error throws.
    let script = r#"
        function hotSet(n) {
            for (var i = 0; i < n; i++) {
                jsnir.set((i & 1023) + 1, i & 65535);
            }
        }
        function hotCommit(n) {
            var u = jsnir.cmd;
            for (var i = 0; i < n; i++) {
                // OP_SET (1): id, value
                u[0] = 1;
                u[1] = (i & 1023) + 1;
                u[2] = i & 65535;
                jsnir.commit(3);
            }
        }
        function badSet() { jsnir.set(70000, 1); }
        function badCommit() { jsnir.commit(999999); }
        function jsnirMount() {}
        function jsnirEvent(cb) {
            if (cb === 1) { hotSet(25000); }
            else if (cb === 2) { hotCommit(25000); }
            else if (cb === 3) { badSet(); }
            else if (cb === 4) { badCommit(); }
        }
    "#;

    let factory = V8Factory;
    let mut engine = factory
        .create(
            &ScriptBundle::Source(script.into()),
            Rc::default(),
            &VmConfig::default(),
        )
        .expect("v8 load");
    assert_eq!(engine.name(), "v8");
    engine.call_mount().expect("mount");

    let ops_set = engine.call_event(1, "", None).expect("hotSet");
    assert_eq!(ops_set.len(), 25_000);
    assert_eq!(ops_set[0], Op::Set { id: 1, value: 0 });
    assert_eq!(
        ops_set[24_999],
        Op::Set {
            id: ((24_999 & 1023) + 1) as u16,
            value: 24_999
        }
    );

    let ops_commit = engine.call_event(2, "", None).expect("hotCommit");
    assert_eq!(ops_commit.len(), 25_000);
    assert_eq!(ops_commit, ops_set);

    // Trigger both error paths after TurboFan optimization to validate CallbackScope throw safety.
    let err_set = engine.call_event(3, "", None).unwrap_err();
    assert!(err_set.contains("u16"), "unexpected error: {err_set}");

    let err_commit = engine.call_event(4, "", None).unwrap_err();
    assert!(
        err_commit.contains("exceeds capacity"),
        "unexpected error: {err_commit}"
    );
}

#[test]
fn v8_host_drag_scroll_context_menu_columns_and_request() {
    let script = r#"
        var log = "";
        function pushText(t) {
            log = t;
            jsnir.setText(2, t);
        }
        function jsnirMount() {
            var ops = [
                { op: 'create', id: 1, node: { kind: 'box', style: { direction: 'column', width: 800, height: 600 } } },
                { op: 'create', id: 2, parent: 1, node: { kind: 'text', text: 'init', font: { size: 14 } } },
                { op: 'create', id: 3, parent: 1, node: { kind: 'box', style: { width: 200, height: 100 }, drag: 1, contextmenu: 2 } },
                { op: 'create', id: 4, parent: 1, node: { kind: 'box', style: { width: 200, height: 100, scroll: true }, scroll: 3 } },
            ];
            // Create > 80 nodes so `jsnir.columns` must grow dynamically past initial 64 capacity.
            for (var i = 0; i < 80; i++) {
                ops.push({ op: 'create', id: 10 + i, parent: 4, node: { kind: 'box', style: { width: 100, height: 20 } } });
            }
            jsnir.batch(JSON.stringify({ ops: ops }));
        }
        function jsnirDrag(cb, phase, x, y, sx, sy) {
            var w = jsnir.columns.width[85];
            var cur = jsnir.readText(2);
            pushText("drag:" + cb + ":" + phase + ":" + Math.round(x - sx) + ":w85=" + w + ":prev=" + cur);
        }
        function jsnirScroll(cb, top, height, client) {
            pushText("scroll:" + cb + ":" + Math.round(top));
        }
        function jsnirContextMenu(cb, x, y) {
            pushText("ctx:" + cb + ":" + Math.round(x) + "," + Math.round(y));
        }
        function jsnirEvent(cb, text) {
            if (cb === 0) {
                var reply = jsnirRequest(text);
                pushText("req:" + reply);
            }
        }
    "#;

    let mut host = Host::with_engine_and_request_handler(
        Arc::new(V8Factory),
        ScriptBundle::Source(script.into()),
        "{}".into(),
        TextEngine::with_fonts(&[INTER], [255; 3]),
        |req| Ok(format!("echo:{req}")),
    )
    .expect("v8 host with request handler");
    host.set_wait_budget(Duration::from_millis(50));
    settle(&mut host);

    // Verify jsnirRequest roundtrip.
    host.push("hello-v8".into());
    wait_reply(&mut host);
    assert_eq!(host.tree.text(2), Some("req:echo:hello-v8"));

    // Verify jsnirDrag + dynamic jsnir.columns read beyond index 64 + jsnir.readText.
    // Box 3 sits right below text 2 (~18-24 px tall), so y=60 hits box 3.
    host.pointer_move(Some([40., 60.]));
    host.pointer_down(PointSelection::Place);
    wait_reply(&mut host);
    host.pointer_move(Some([75., 60.]));
    wait_reply(&mut host);
    host.pointer_up();
    wait_reply(&mut host);
    let after_drag = host.tree.text(2).unwrap_or("").to_string();
    assert!(
        after_drag.starts_with("drag:1:") && after_drag.contains(":w85=100:"),
        "unexpected drag text: {after_drag}"
    );

    // Verify jsnirContextMenu.
    host.pointer_move(Some([40., 60.]));
    host.context_press();
    wait_reply(&mut host);
    assert_eq!(host.tree.text(2), Some("ctx:2:40,60"));
}

#[test]
fn v8_in_place_hot_reload_preserves_state_and_rolls_back_on_error() {
    let v1 = with_sdk(
        r#"
        var count = signal(1);
        onHost(function(msg) {
            if (msg.cmd === "inc") count.value++;
        });
        mount(function() {
            return h(Column, { style: { width: 400, height: 200 } },
                h(Text, { children: function() { return "v1:" + count.value + ":ep=" + (typeof __jsnirPlacementEpoch === "string" ? __jsnirPlacementEpoch : "0"); } }),
            );
        });
        "#,
    );
    let v2 = with_sdk(
        r#"
        var count = signal(1);
        onHost(function(msg) {
            if (msg.cmd === "inc") count.value += 10;
        });
        mount(function() {
            return h(Column, { style: { width: 400, height: 200 } },
                h(Text, { children: function() { return "v2:" + count.value + ":ep=" + (typeof __jsnirPlacementEpoch === "string" ? __jsnirPlacementEpoch : "0"); } }),
            );
        });
        "#,
    );
    let broken_syntax = "function jsnirMount() { {{{ broken";
    let broken_mount = r#"
        function jsnirMount() {
            jsnir.set(1, 99);
            throw new Error("boom in mount");
        }
    "#;

    let mut host = Host::try_new_with_engine(
        Arc::new(V8Factory),
        ScriptBundle::Source(v1),
        "{}".into(),
        TextEngine::with_fonts(&[INTER], [255; 3]),
    )
    .expect("v8 host");
    host.set_wait_budget(Duration::from_millis(50));
    settle(&mut host);

    host.push(r#"{"cmd":"inc"}"#.into());
    wait_reply(&mut host);
    let read_label = |h: &Host| -> String {
        h.tree
            .nodes
            .iter()
            .find_map(|(_, n)| match n {
                Node::Text { text, .. } => Some(text.clone()),
                _ => None,
            })
            .unwrap_or_default()
    };
    assert_eq!(read_label(&host), "v1:2:ep=0");

    // Failed reloads (syntax error & mount throw) MUST fail cleanly and keep v1 alive.
    assert!(host.reload(broken_syntax.into()));
    wait_reply(&mut host);
    assert!(
        host.take_reload_result().expect("syntax reload result").is_err(),
        "broken syntax must fail reload"
    );

    assert!(host.reload(broken_mount.into()));
    wait_reply(&mut host);
    assert!(
        host.take_reload_result().expect("mount reload result").is_err(),
        "broken mount must fail reload"
    );

    host.push(r#"{"cmd":"inc"}"#.into());
    wait_reply(&mut host);
    assert_eq!(read_label(&host), "v1:3:ep=0");

    // Successful in-place reload preserves signal count=3 and bumps placement_epoch!
    assert!(host.reload(v2.clone()));
    wait_reply(&mut host);
    assert!(
        host.take_reload_result().expect("v2 reload result").is_ok(),
        "v2 reload must succeed"
    );
    settle(&mut host);
    assert_eq!(read_label(&host), "v2:3:ep=1");

    // Subsequent events run on v2 (+10) and repeated reloads recycle the same Isolate cleanly.
    host.push(r#"{"cmd":"inc"}"#.into());
    wait_reply(&mut host);
    assert_eq!(read_label(&host), "v2:13:ep=1");

    assert!(host.reload(v2));
    wait_reply(&mut host);
    assert!(
        host.take_reload_result().expect("second v2 reload result").is_ok(),
        "second v2 reload must succeed"
    );
    settle(&mut host);
    assert_eq!(read_label(&host), "v2:13:ep=2");
}

#[test]
fn v8_vm_limits_interrupt_infinite_loop_and_recover() {
    let script = r#"
        var ok = 0;
        function jsnirMount() {
            jsnir.set(1, 1);
        }
        function jsnirEvent(cb) {
            if (cb === 1) {
                jsnir.set(1, 999);
                while (true) {}
            } else if (cb === 2) {
                ok++;
                jsnir.setText(1, "recovered:" + ok);
            }
        }
    "#;
    let factory = V8Factory;
    let config = VmConfig {
        args: "{}".into(),
        limits: VmLimits {
            max_memory_bytes: Some(32 * 1024 * 1024),
            max_call_duration: Some(Duration::from_millis(30)),
        },
        ..Default::default()
    };
    let mut engine = factory
        .create(&ScriptBundle::Source(script.into()), Rc::default(), &config)
        .expect("load v8 with limits");
    assert_eq!(engine.call_mount().expect("mount"), vec![Op::Set { id: 1, value: 1 }]);

    let mem = engine.memory_usage().expect("v8 memory_usage");
    assert!(mem.used_bytes > 0 && mem.total_bytes >= mem.used_bytes);

    // Infinite loop must be interrupted by Watchdog and discard partial ops.
    let err = engine
        .call_event(1, "", None)
        .expect_err("infinite loop must time out");
    assert!(
        err.contains("timed out") || err.contains("VmLimits"),
        "unexpected error: {err}"
    );

    // Isolate recovers cleanly after `cancel_terminate_execution` for subsequent events!
    let ops = engine.call_event(2, "", None).expect("recovery event");
    assert_eq!(
        ops,
        vec![Op::SetText {
            id: 1,
            text: "recovered:1".into()
        }]
    );
}

#[test]
fn v8_stepper_app_frame_parity_with_quickjs() {
    let mut qjs = Host::try_new_with_engine(
        Arc::new(QuickJsFactory),
        ScriptBundle::Source(STEPPER.into()),
        "{}".into(),
        TextEngine::with_fonts(&[INTER], [255; 3]),
    )
    .expect("qjs stepper");
    let mut v8 = Host::try_new_with_engine(
        Arc::new(V8Factory),
        ScriptBundle::Source(STEPPER.into()),
        "{}".into(),
        TextEngine::with_fonts(&[INTER], [255; 3]),
    )
    .expect("v8 stepper");

    qjs.set_wait_budget(Duration::from_millis(50));
    v8.set_wait_budget(Duration::from_millis(50));
    settle(&mut qjs);
    settle(&mut v8);

    let f_qjs = qjs.frame(800., 600., 1., 4096);
    let f_v8 = v8.frame(800., 600., 1., 4096);
    assert_eq!(qjs.tree.nodes.iter().count(), v8.tree.nodes.iter().count());
    assert_eq!(f_qjs.items.len(), f_v8.items.len());
    assert!(f_v8.items.len() > 0);
}

#[test]
fn v8_scroll_placement_and_virtual_list_callbacks() {
    let script = r#"
        function jsnirMount() {}
        function jsnirScroll(cb, top, height, client, guard) {
            jsnir.setText(1, "scroll:" + cb + ":" + top + ":" + height + ":" + client + ":" + (guard || ""));
        }
        function jsnirPlacement(text) {
            jsnir.setText(2, "place:" + text);
        }
        function jsnirVirtualList(delta) {
            jsnir.set(3, 42);
            return "reply:" + delta;
        }
    "#;
    let factory = V8Factory;
    let mut engine = factory
        .create(
            &ScriptBundle::Source(script.into()),
            Rc::default(),
            &VmConfig::default(),
        )
        .expect("v8 load");

    let scroll_ops = engine
        .call_scroll(7, 120.5, 2000.0, 400.0, Some("g1"))
        .expect("call_scroll");
    assert_eq!(
        scroll_ops,
        vec![Op::SetText {
            id: 1,
            text: "scroll:7:120.5:2000:400:g1".into()
        }]
    );

    let place_ops = engine.call_placement("bottom-start").expect("call_placement");
    assert_eq!(
        place_ops,
        vec![Op::SetText {
            id: 2,
            text: "place:bottom-start".into()
        }]
    );

    let (reply, vl_ops) = engine
        .call_virtual_list(r#"{"entering":[1,2]}"#)
        .expect("call_virtual_list");
    assert_eq!(reply, r#"reply:{"entering":[1,2]}"#);
    assert_eq!(vl_ops, vec![Op::Set { id: 3, value: 42 }]);
}

