# JSNIR research notebook

The experiments start with the first counter and end with the native GPU backend and Stepper comparison. The project was renamed from Jurui to JSNIR on 19 November 2026. The notebook keeps historical identifiers where they describe an earlier implementation. Commands for a removed backend or experiment require its original source snapshot. [Recorded measurements](../metrics) · [Roadmap](roadmap.md)

## Question

If Rust keeps the state that is drawn and a click can make a round trip to JavaScript, is the image preserved exactly? What does that round trip cost under this load?

## First series · 18 November 2026

Linux 7.3.4 CachyOS, AMD Ryzen 9 270, rustc 1.99.1, release profile. egui used offscreen OpenGL with llvmpipe (LLVM 23.1.4). JSNIR drew into an RGB buffer in Rust and updated an SDL2 surface with the dummy driver. Both renderers ran in software. The integrated Radeon took no part in this series.

Three fresh processes per combination, 100 warm-up activations and 1,000 measurements per process. The order of the four cases was reversed in the second repetition. One event in flight, no synchronization with the display. Each row shows the median of the three per-process percentiles. All times are in **microseconds**.

| Backend | Mode | Change applied p50 / p95 / p99 | Draw p50 / p95 / p99 |
| --- | --- | ---: | ---: |
| egui | Rust | 0.030 / 0.071 / 0.170 | 113.670 / 128.447 / 146.911 |
| egui | JS | 8.896 / 15.448 / 19.235 | 115.863 / 134.909 / 297.128 |
| JSNIR own | Rust | 0.020 / 0.021 / 0.040 | 53.418 / 57.666 / 71.191 |
| JSNIR own | JS | 6.763 / 8.045 / 34.955 | 53.959 / 57.115 / 81.921 |

The first interval starts before dispatching the activation and ends when the new version is applied. In JS mode it includes two queues, thread scheduling, the callback, JSON and a busy wait that yields the thread in the benchmark. The pure Rust path is so short that the cost of reading the clock shows in its numbers. The second interval covers preparing and completing the draw, with `glFinish` in egui and the surface update in SDL. Reading pixels back and writing the CSV are outside the intervals.

**Measured parity.** Five captures per process, twelve processes, sixty captures. All matched their reference in all 76,800 pixels. The startup state and the return to zero had the same hash. The series directory contains the timing CSVs and the summary records the captured images' hashes. [Series summary](../metrics/first-series/summary.json)

**Interpretation.** For this counter, the JavaScript path added about 7–9 µs to the median of the applied change and preserved the final image. The draw p99 varied more than the median in egui. Three repetitions on one machine with these drivers leave the cause of that variation open. These measurements end before on-screen composition. Input-to-presentation latency, the physical GPU, RSS, cold start, idle power and scrolling with text are still pending.

## Notes per backend

- **egui.** The interactive UI uses a stable id for the button. The benchmark runs one egui pass, tessellates and reads back the egui_glow framebuffer. Edge feathering and dithering were disabled to keep exact colors. [Painter API](https://docs.rs/egui/0.36.2/egui/struct.Painter.html)
- **GPUI.** The entity holds the runtime. The canvas emits quads in window coordinates and an element with an identity receives the click. It compiled. Opening the window returned `NoCompositor`, and X11 was not reachable from this session either. The image is still to be verified. [GPUI architecture](https://docs.rs/gpui/0.2.2/gpui/)
- **Iced.** The adapter turns the pointer into an activation message. A time subscription checks the worker's answer while changes are in flight. Measuring that real wait will require capturing the window. [Iced guide](https://docs.rs/iced/0.13.1/iced/)
- **Masonry.** A fixed-size leaf widget was implemented. It receives events and emits rectangles in its paint pass. The repaint after the commit is requested explicitly. [Widget contract](https://docs.rs/masonry/0.4.0/masonry/core/trait.Widget.html)
- **Slint.** Each rectangle is a declarative element and the button receives events through a TouchArea. The adapter rebuilds the rectangle model after a change. That rebuild cost belongs to the adapter and deserves instrumentation before optimizing it. [Slint's Rust integration](https://docs.rs/slint.dev/latest/docs/rust/slint/)
- **JSNIR own.** Rust decides geometry, rasterization, hit area and redraw. SDL opens the window and delivers events. The first renderer uses the CPU to get reproducible pixel output. wgpu and Taffy remain alternatives for when a workload needs acceleration or dynamic layout. [SDL](https://www.libsdl.org/)

The Iced, Masonry and Slint dependencies were missing from the local cache and Cargo could not reach crates.io from this session. In a later run, `cargo fetch` per manifest completed the downloads and all three adapters compiled in release. Visual verification in a window is still pending because Xvfb failed to open its X11 socket. The verified scope of the own backend is the counter. IME editing, keyboard focus, accessibility, virtualization and platform portability need specific tests.

## Prototype decisions

The first VM is Duktape 2.x, available on this machine. The build step erases type annotations. The example uses the JavaScript subset this VM supports. The callback and the business logic are in TypeScript. The mount contract declares the identities of the display, the button and the callback, and the update contract changes the display's number with an expected version. The geometry of this example lives in the shared Rust design. The generic tree, TSX and structural mutations are later work. [Duktape](https://duktape.org/)

For this first comparison, Rust mode skips the VM and uses the same executable, visual state and renderer as JS mode. This controls for build differences between the two paths. Measuring installed size or linking cost also requires a binary built without the Duktape dependency.

Each queue holds up to 64 events or answers. The host caps the total in flight and reports saturation through the return value of `activate`. The benchmark checks that an activation was accepted before going on. The demo UI drops activations when the queue is full. The paint thread polls for answers and keeps the last applied state while it waits. The burst test checks order, saturation and the return to zero.

## Next experiment able to change the decision

The counter removes text and variable layouts so that a differing pixel can be blamed on a specific bug. A chat with the same bundled font, a fixed history and an editing script would test exactly what is left open here. Keep this counter as a regression test and add a second visual spec for the chat, including draft, selection, focus and scroll. The decision on the production renderer needs that second workload and a measurement in a window on real hardware.

## Additional counter verification and the chat decision

| Backend | Additional run | RGB result |
| --- | --- | --- |
| Iced | The window's Canvas drawn with tiny-skia, five states in Rust and five in JS | 10/10 exact |
| Slint | The same declarative component and its model in MinimalSoftwareWindow, both modes | 10/10 exact |
| Masonry | TestHarness injected 1,000 clicks per mode with press and release. Vello rendered the five checkpoints with software Vulkan (SwiftShader) | 10/10 exact, alpha channel included |
| GPUI | The adapter compiled. The TestWindow of version 0.2.2 skips drawing the scene | Pixel parity pending |

The Iced run under Xvfb started from the desktop also verified the Rust images for 0, 1 and 42 clicks, all with zero differences. DRI3 warnings coexisted with correct images. Only those three captures were in that series' directory at review time. The test without an X server uses the activation callback and the real Canvas, while the window series uses the X11 pointer.

### Reproducing the three additional tests

Run in Bash from the root of the historical source snapshot with the Iced, Slint and Masonry backends. Slint uses the fluent style to avoid initializing the desktop's Qt theme during offscreen rendering. The Masonry test uses a Vulkan device. This run used the SwiftShader ICD.

```bash
series=$(mktemp -d "$PWD/metrics/local-software-XXXXXX")
for b in iced slint; do
    SLINT_STYLE=fluent cargo build --locked --release --target-dir target --manifest-path "backends/$b/Cargo.toml" || exit 1
    for mode in rust js; do
        "target/release/jurui-$b" "$mode" "$series/$b/$mode" && python scripts/visual.py "$series/$b/$mode" || exit 1
    done
done
VK_DRIVER_FILES=/usr/lib/chromium/vk_swiftshader_icd.json JURUI_MASONRY_CAPTURES="$series/masonry" cargo test --release --locked --target-dir target --manifest-path backends/masonry/Cargo.toml counter_pixels
```

**Provisional decision.** Keep the editor and the scroll in the Rust toolkit. Send JavaScript one immutable message per submit and apply its answer only to the history. The counter tests portability of simple drawing across five adapters. The chat tests the editor's behavior in egui. Choosing a toolkit for production requires repeating the text scene on another adapter and evaluating IME, accessibility and presentation. [Chat results and limits](chat-test-02.md)

## Delivery 1 · Node tree, atomic batches and tracing · 19 November 2026

The fixed `{display, button, callback}` contract was replaced by a tree with stable identities ([tree.rs](../src/tree.rs)). Both the mount and each update are a `Batch { base, ops }`. Rust validates the whole batch before touching the tree. If an operation is invalid (stale base, missing node or node of another kind, parent that is not a column, duplicate id, value out of range), the batch is rejected and the tree and version stay intact. A test covers the seven cases. Rust mode builds the same batch as `montar()` in `app/counter.ts` (since removed; the counter is now [counter.tsx](../app/counter.tsx)), so both modes go through the same validation. Version 1 is the mount.

**Trace.** With `JSNIR_TRACE=file.csv` (must be a new file), the runtime writes one row per event: the node that received the event, the callback, the base and the number of operations in the batch, the resulting version, the status (`ok` or the rejection reason), frames drawn during the wait and the first frame drawn after the batch, with timestamps in µs. The adapter numbers frames with `Runtime::drawn()`. It is wired in egui, in the own backend and in the benchmark. That frame is the end of the adapter's draw pass, not the physical presentation.

**Delay.** `JSNIR_DELAY_MS` holds each event on the VM thread before the callback. Rust mode has no VM and ignores the variable.

### Results

| Test | Result |
| --- | --- |
| Offscreen egui benchmark, 1,100 events per mode, traced | 1,100 `ok` batches per mode. 0, 1, 42, 999 and 1,000 match the reference in all 76,800 pixels |
| egui window under Xvfb, JS mode, 500 ms delay, two clicks through the X11 pointer | Both batches `ok`. 1,686 frames were drawn while waiting for the first. The second click was queued and waited 796 ms |
| Test `slow_callback_keeps_drawing_and_traces_each_click` | With a 150 ms delay frames keep being drawn, a second click is accepted during the wait and the trace keeps the order |

Window trace ([CSV](../metrics/trace-01/window-js-500ms.csv)):

| Event | Node | Batch base → version | Event → batch | Frames waiting | Frame |
| ---: | ---: | --- | ---: | ---: | ---: |
| 1 | 3 | 1 → 2 | 500.2 ms | 1,686 | 1,692 |
| 2 | 3 | 2 → 3 | 796.3 ms | 2,748 | 3,440 |

Benchmark times for this run. One process per mode, in µs. They are indicative and do not follow the three-process protocol of the first series.

| Mode | Change applied p50 / p95 / p99 | Draw p50 / p95 / p99 |
| --- | ---: | ---: |
| Rust | 0.331 / 0.591 / 2.225 | 60.633 / 68.328 / 156.423 |
| JS | 6.723 / 8.806 / 12.394 | 23.404 / 24.997 / 42.309 |

**Reading.** The Rust path went from 0.030 to 0.331 µs. The increase is building and validating the batch. It is still about twenty times below the trip to JS. This time OpenGL picked the **NVIDIA RTX 5060 Laptop (driver 620.70.06)**, whereas the first series used llvmpipe, so the draw times are not comparable with that series. That drawing in Rust mode was slower than in JS mode within a single process needs a repetition with alternating order before it can be interpreted. GPU clock states may explain it.

**Findings that change the plan.**
- egui requests continuous repaints while a batch is in flight: 1,686 frames in 500 ms (≈3,400 fps without vsync under Xvfb). A real compositor caps it with vsync, but `request_repaint_after` should be used so the wait does not burn CPU.
- The VM processes events serially. A second click waits for the first one's delay plus its own (head-of-line blocking). That is correct for the chat, since it keeps the order of submits. If the SDK wants independent callbacks, the queue must stop being a single one.
- After a rejected batch, the following batches from the same script are also rejected for a stale base, because JS keeps its own counter. Reconciliation (resending the current version to JS, or remounting) must be defined before structural mutations.

The current egui counter can run the same trace scenario. Run the final command from a graphical session, click twice and close the window.

```bash
cargo build --release --locked --manifest-path backends/egui/Cargo.toml --target-dir target
d=$(mktemp -d "$PWD/metrics/local-trace-XXXXXX")
for m in rust js; do
    SDL_VIDEODRIVER=offscreen JSNIR_TRACE="$d/trace-$m.csv" target/release/jsnir-egui "$m" "$d/egui-$m" &&
        python3 scripts/visual.py "$d/egui-$m"
done
JSNIR_DELAY_MS=500 JSNIR_TRACE="$d/window.csv" target/release/jsnir-egui js
```

## Delivery 2 · Curve by batch size and the switch to QuickJS · 19 November 2026

The thresholds were set before this series, in the [proposal](runtime-proposal.md#decision-criteria). The reference is the native Rust mode of the same executable.

**Method.** `examples/curva.rs` (since removed) measured the round trip from the event to the applied batch, without drawing, with one event in flight. The callback returns a batch of 1, 10, 100 or 1,000 `set` operations (`Runtime::activate_ops`). Per size: 100 warm-up runs and 1,000 measurements. `scripts/curva.py` (since removed) opened three processes per mode, alternated the order in the second repetition and reported the median of the per-process percentiles. The same curve was measured with Duktape 2.7 (before the switch) and with QuickJS-ng 0.16.2 (`rquickjs` 0.14.0). Memory is the peak resident size (VmHWM) of the windowless process.

| Operations | Rust p50 / p99 | Duktape p50 / p99 | QuickJS p50 / p99 | p99 threshold |
| ---: | ---: | ---: | ---: | ---: |
| 1 | 0.05 / 0.06 | 5.19 / 8.70 | 4.54 / 6.56 | 1,000 |
| 10 | 0.10 / 0.11 | 15.51 / 28.52 | 12.23 / 21.44 | 1,000 |
| 100 | 0.43 / 0.44 | 112.39 / 128.38 | 87.07 / 98.01 | 1,000 |
| 1,000 | 4.00 / 4.69 | 1,045.21 / 1,111.89 | 843.75 / 1,242.76 | 4,000 (informative) |

Times in µs. [Duktape](../metrics/curve-duktape/summary.json) · [QuickJS](../metrics/curve-quickjs/summary.json)

| Resource | Rust | QuickJS | Difference | Threshold |
| --- | ---: | ---: | ---: | ---: |
| Initial mount | 6 µs | 635 µs | +0.6 ms | +100 ms |
| Peak resident memory, no window | 3,200 kB | 4,636 kB | +1.4 MB | +20 MB |
| Benchmark executable | — | 2.33 MB (Duktape: 0.84 MB, dynamically linked) | | |
| egui executable | — | 15.4 MB (with dynamic Duktape: 13.2 MB) | | +5 MB |

**Reading.** The cost grows linearly, with no knee up to 1,000 operations. There is a fixed cost of about 4.5 µs (two queues and waking the thread) and about 0.84 µs per operation with QuickJS (1.05 µs with Duktape), dominated by `JSON.stringify` and decoding in Rust. Per operation, JS costs about 200 times more than Rust. In absolute terms, a batch of 1,000 changes fits 13 times in a 60 Hz frame. QuickJS improves the median by 13 % to 23 % at every size. With 1,000 operations its p99 is above Duktape's. The GC is the likely cause, but this has not been checked. The size the VM adds still has no fair reference: Duktape was linked as a system library, and a binary without a VM is missing.

With QuickJS the core and egui tests were rerun, along with the counter's five states in each mode (0 differing pixels) and the chat script (50 verified captures). The `\uXXXX` escape in `app/chat.ts` was needed because of Duktape's CESU-8 strings. QuickJS does not need it, but it does no harm.

### Scrolling 10,000 messages with three processes per mode

| Mode | Frame p50 | p95 | p99 | p99 of each process |
| --- | ---: | ---: | ---: | --- |
| Rust | 0.112 ms | 0.242 ms | 0.265 ms | 0.262 · 0.372 · 0.265 |
| JS + Rust | 0.112 ms | 0.243 ms | 0.317 ms | 0.318 · 0.317 · 0.296 |

NVIDIA RTX 5060 Laptop, offscreen OpenGL, 1,100 frames measured per process. [CSV](../metrics/scroll-quickjs/)

**Fails the threshold.** The JS p99 is 20 % higher (the threshold is +10 %), although p50 and p95 match. That is 0.05 ms in a 16.7 ms frame. While scrolling, the history is already in Rust and the VM thread is blocked waiting for events, so the difference does not come from calls into JS. Hypothesis to investigate: in JS mode the 10,000 strings are created on the worker thread and end up in another allocator arena, with worse locality when drawing. Test of the hypothesis: copy the history on the UI thread after preloading and repeat. Per the proposal's rule, a bounded fix comes before reshaping the design. If the relative threshold is judged too strict for sub-millisecond frames, the change must be recorded in the proposal with this data as the reason.

### Scroll fix and A/A control

The allocator-arena hypothesis was tested with five processes per mode and alternating order. Median of the frame p99, same machine.

| Condition | Rust p99 | JS p99 | JS / Rust |
| --- | ---: | ---: | ---: |
| A · current binary | 0.253 ms | 0.282 ms | +11.6 % |
| A2 · the same binary, again | 0.272 ms | 0.278 ms | +2.3 % |
| B · `MALLOC_ARENA_MAX=1` | 0.247 ms | 0.262 ms | +6.2 % |
| C · history copied on the UI thread | 0.253 ms | 0.254 ms | +0.1 % |

[CSV per condition](../metrics/scroll-fix/)

**Reading.** A and A2 are the same executable, yet their difference went from +11.6 % to +2.3 %. Each process's p99 ranges between 0.24 and 0.35 ms in both modes. The process-to-process noise of the p99 is the size of the threshold, so the +20 % of the previous series is not evidence of a JS cost. C does not prove that the copy helps either. The copy was removed and the code stayed as in A. Conclusion: with this method no scroll difference between modes is detected. p50 and p95 match in every condition.

**Recommended change of method.** Every comparison against a relative threshold must be measured together with an A/A control of the same binary. A difference counts only if it exceeds the A/A variation. For the frame p99, a larger sample per process (10,000 frames instead of 1,100) or a bootstrap interval of the difference is preferable.

## Delivery 3 · Direct protocol, JSI style · 19 November 2026

**Question.** How much of the JS cost is JSON and how much is the thread hop? React Native left its asynchronous JSON bridge behind with JSI, which makes direct calls from JS to native functions. This experiment repeats that idea in JSNIR.

**Implementation.** When the VM is created, Rust registers the global `jsnir` object with native functions. `jsnir.set(id, value)` writes an `Op::Set` into a Rust buffer that lives on the VM thread, with no text or JSON in between. When the callback ends, the VM thread takes the buffer and sends an already typed `Batch`. The tree validates it as before: if it is invalid, it is rejected without touching anything. `rquickjs` rejects numbers outside the `u16` range. The mode was `directo` and lived next to `js` (JSON) and `rust`. To separate the cost of the hop, `local-json` and `local-directo` ran the same callback on the measuring thread, without queues. (Historical: the JSON and direct protocol modes were removed; the SDK kept direct calls.)

Times in µs, median of three processes with alternating order ([summary](../metrics/curve-direct/summary.json)):

| Ops | Rust | Direct | JSON | Local direct | Local JSON | JSON / direct |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 1 | 0.05 / 0.05 | 2.43 / 3.87 | 5.20 / 8.18 | 0.42 / 0.52 | 2.11 / 2.26 | 2.1× |
| 10 | 0.10 / 0.11 | 3.49 / 4.85 | 12.62 / 20.08 | 1.12 / 1.19 | 10.05 / 13.25 | 3.6× |
| 100 | 0.64 / 0.64 | 13.25 / 18.28 | 91.14 / 102.48 | 9.05 / 10.76 | 86.73 / 97.37 | 6.9× |
| 1,000 | 3.95 / 6.45 | 103.11 / 152.62 | 888.83 / 1,270.63 | 86.25 / 90.21 | 876.18 / 1,301.47 | 8.6× |

Each cell is p50 / p99. The counter's five states in direct mode matched the reference in every pixel. The trace recorded 1,100 `ok` batches.

**Breakdown with one operation.** Direct round trip: 2.43 µs. Thread hop (two queues, waking the thread and yielding it): ≈ 1.96 µs. Call into JS plus the native function: 0.42 µs. Applying in Rust: 0.05 µs.

**Cost per operation.** Direct: ≈ 0.10 µs. JSON: ≈ 0.88 µs. Rust: ≈ 0.004 µs.

**Reading.** JSON was 85 % of the per-operation cost. With direct calls, a batch of 1,000 changes costs 0.1 ms, 0.6 % of a 60 Hz frame. In small batches the thread hop dominates, which is the price of JS never blocking the UI. What remains per operation (≈ 86 ns) is the QuickJS interpreter running the loop plus the native call through `rquickjs`. It is still about 20 times Rust's per-operation cost, but it is no longer the important part of the total.

### Next steps for the protocol

1. **Shared command buffer.** JS writes the operations into an `ArrayBuffer` or `Int32Array` that Rust reads when the batch closes. This removes the cost of one native call per operation. Compare against `jsnir.set` with 1,000 operations.
2. **Synchronous reads of the tree.** What JSI allows and an asynchronous bridge does not: JS querying native state (text metrics, layout, selection) without waiting for a message. Proposal: Rust publishes an immutable, versioned snapshot of the tree (`Arc`) after each batch. JS reads it without locking from its thread, and writes still go through validated batches.
3. **Mount and structural mutations through the same path.** Add `jsnir.create` and `jsnir.remove`, take JSON out of the mount and define how the version is reconciled when a batch is rejected.
4. **Thread hop.** A lock-free queue with a brief spin before sleeping. Measure how much of the 1.96 µs is recovered and how much CPU it costs at idle.
5. **The chat through the direct protocol**, to measure text and Unicode without JSON.

## Delivery 4 · Command buffer · 19 November 2026

**Question.** With direct calls, is the per-operation cost (≈ 86 ns) due to crossing the boundary once per operation? If so, a binary buffer crossed once per batch should lower it.

**Sources.** React Native 0.88 lets a synchronous native method borrow an `ArrayBuffer`'s memory without copying it. Those bytes are valid until the method returns, and if they must be kept they are copied during the call ([React Native 0.88 blog](https://github.com/react/react-native-website/blob/main/website/blog/2026-10-19-react-native-0.88.mdx)). In rquickjs 0.14, `TypedArray::as_raw()` returns `None` if the buffer is detached, and the pointer is valid only until JS runs again. It is the same borrowing rule.

**Implementation.** Mode `buffer`. JS writes `[1, id, value]` per operation into an `Int32Array` that it reuses and doubles when it runs out. At the end of the callback it calls `jsnir.commit(buffer, length)` once. Rust borrows the bytes, decodes and validates the whole batch during the call, and copies it into a `Vec<Op>`. On error it throws a `TypeError` and leaves no partial operations. A test covers seven rejection cases: unknown code, id or value out of range, negative number, length larger than the buffer or not a multiple of 3, a buffer detached with `transfer()`, and a `Float64Array` instead of an `Int32Array`. It also covers a view with an offset (`subarray`). In egui, the counter's five states matched the reference in every pixel. The trace recorded 1,100 `ok` batches. (Historical: the buffer mode and `jsnir.commit` were removed.)

Times in µs, p50 / p99, median of three processes. The local part is measured on the VM thread, without queues.

| Ops | Direct | Buffer | Local direct | Local buffer with `emitir()` | Local buffer inlined | Local inlined with local variables |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 1 | 3.18 / 4.26 | 2.77 / 5.25 | 0.29 / 0.31 | 0.58 / 0.63 | 0.54 / 0.60 | 0.53 / 0.59 |
| 10 | 3.82 / 7.49 | 4.98 / 6.64 | 1.13 / 1.23 | 2.60 / 2.73 | 2.07 / 2.18 | 1.69 / 1.75 |
| 100 | 13.82 / 20.89 | 24.79 / 35.37 | 9.07 / 11.70 | 21.44 / 24.35 | 15.88 / 18.25 | 12.37 / 15.11 |
| 1,000 | 104.21 / 112.95 | 217.38 / 232.93 | 87.82 / 97.61 | 208.89 / 229.34 | 153.22 / 159.62 | 118.09 / 125.13 |

[Direct and buffer](../metrics/curve-buffer/summary.json) · [Inlined](../metrics/curve-buffer-inline/summary.json) · [With local variables](../metrics/curve-buffer-locals/summary.json)

**Negative result.** In QuickJS the buffer is slower than direct calls at every size above one. With 1,000 operations: 118 µs for the best buffer variant against 88 µs for direct calls. Removing the helper function saves 27 %. Turning globals into local variables saves another 23 %. Even so, the buffer does not catch up. According to the [QuickJS-ng documentation](https://quickjs-ng.github.io/quickjs/developer-guide/internals), the engine is a bytecode interpreter without a JIT, with fast function calls and optimized typed-array access. A native call is a few instructions. Three indexed writes are several times more. In this engine the per-operation cost is set by the amount of bytecode, not by crossing the boundary.

**Decision.** The mutation protocol stays with direct calls. `buffer` remained available and tested for bulk binary data (images, precomputed rows), which is how React Native uses `ArrayBuffer`. `activarBufferEnLinea` existed only for the benchmark.

**Lessons for the SDK.**
- Generate code that reads globals once per callback.
- Measure before adding helper functions on the hot path.

Two more ideas from the documentation, not yet measured:
- Keep `jsnir.set` in a local variable to avoid a property lookup per operation.
- Precompile the app to bytecode with `qjsc` (`js_std_eval_binary`), which according to QuickJS-ng avoids compiling at startup and allows leaving the compiler out of the binary.

## Delivery 5 · Synchronous reads of the tree from JS · 19 November 2026

**Question.** JS runs on its own thread and Rust applies batches on the UI thread. How can JS read the current state without waiting for a message and without seeing half-written data?

**Strategies** (`src/lectura.rs`, since removed):

| Strategy | Read from JS | Publishing on the UI thread | Consistency within a callback |
| --- | --- | --- | --- |
| request | Asks the UI thread for the value and blocks until the answer, like a bridge's synchronous method | Applies to its tree | Each read may see a different version |
| mutex | One lock per read on the shared tree | Takes the lock and applies in place | Each read may see a different version |
| snapshot | Takes an `Arc` of the tree when the callback starts. Reads take no lock | `Arc::make_mut`: clones the tree only if JS holds an open snapshot | A single version |
| replica | Reads its own copy on the JS thread, with no synchronization | Applies the batch and sends it to the JS thread. That thread applies the pending batches when the callback starts | A single version |

**Scenario.** A tree of 10,000 labels. The UI thread publishes a one-operation batch every 100 µs. The JS callback reads k values (`jsnir.leer` kept in a local variable). Three processes per strategy, with alternating order. 100 warm-up callbacks and 1,000 measurements per k. The benchmark's UI thread serves requests in a continuous loop, the best case for `request`: a real UI would serve them between frames.

Callback time in µs, p50 / p99 ([summary](../metrics/reads-01/summary.json)):

| Reads | request | mutex | snapshot | replica |
| ---: | ---: | ---: | ---: | ---: |
| 1 | 2.37 / 3.53 | 0.27 / 0.30 | 0.28 / 0.34 | 0.28 / 0.30 |
| 10 | 19.29 / 25.80 | 1.32 / 1.50 | 1.32 / 71.78 | 1.31 / 1.48 |
| 100 | 185.14 / 198.47 | 12.15 / 14.31 | 80.24 / 100.30 | 12.03 / 14.06 |
| 1,000 | 1,956.23 / 2,073.24 | 120.51 / 130.56 | 218.00 / 309.74 | 118.68 / 191.59 |

| Cost of publishing a batch | request | mutex | snapshot | replica |
| --- | ---: | ---: | ---: | ---: |
| p50 | 0.08 µs | 0.11 µs | **74.1 µs** | 0.19 µs |
| p99 | 0.22 µs | 0.25 µs | **135.9 µs** | 0.57 µs |
| max | 2.1 µs | 2.3 µs | **458.9 µs** | 3.3 µs |

**Reading.**
- **request** costs ≈ 1.95 µs per read: the thread hop on every value. A thousand reads take 2 ms in the best case. If the UI serves requests once per frame, each read may wait up to 16.7 ms. Ruled out.
- **mutex** and **replica** cost the same per read (≈ 0.12 µs): the native call plus the `BTreeMap` lookup. The uncontended lock does not show. The difference is consistency: with `mutex`, a callback that reads two values may see them from different versions. With `replica` it cannot.
- **snapshot** is consistent, but with a `BTreeMap` copy-on-write is O(n). Since the benchmark keeps JS always busy, almost every publish found an open snapshot and cloned 10,000 nodes: 74 µs on the UI thread. Freeing the old version also lands inside the JS callback, which explains the 80 µs with 100 reads. It would be viable with a persistent structure with O(log n) path copying, or with an app whose JS is idle almost all the time.
- **replica** wins on all three counts: cheaper reads, 0.19 µs publishing and a single version per callback. Its p99 with 1,000 reads (191 µs) includes catching up with the batches accumulated when the callback starts. The price is holding the tree twice in memory. At 10,000 labels it does not show in peak resident memory (6.8 MB against the mutex's 6.8 MB).

**Resulting design: a replica fed by an ordered log.** The UI thread remains the single source of truth. Every accepted batch, whether from JS or from the UI itself (draft, selection, scroll, metrics), enters an ordered log. The JS thread applies that same log to its replica when each callback starts. JS always reads a complete, known version (`replica.version`), without locks or thread hops. One question remains open: JS does not see its own changes until the UI accepts them and sends them back. An optimistic layer over the replica, discarded if the batch is rejected, can solve it.

**Next experiments.**
1. A replica with id-indexed storage (`Vec`) instead of a `BTreeMap`, to lower the 0.12 µs floor per read.
2. A snapshot over a persistent structure, to compare memory and publishing cost against the replica.
3. Bring the replica into the runtime: accepted batches sent back to the JS thread, `jsnir.leer` and `jsnir.version` in the SDK, and the optimistic layer to read one's own changes.

## Delivery 6 · Replica in the runtime and data structures · 19 November 2026

### Optimistic replica on the JS thread

`tree::Replica` keeps three things on the JS thread: the tree confirmed by the UI, the app's own batches still awaiting an answer (each remembers whether it was applied), and the **view**, which is the confirmed tree plus those pending batches. JS reads the view synchronously with `jsnir.leer(id)` and `jsnir.version()`, so it sees its own changes even while they are in flight. The UI answers each batch with a boolean over an unbounded channel, so it never blocks. It does not resend the batch: the replica already has it. On acceptance, the batch moves into the confirmed tree. If the UI rejects a batch the view counted on, the view is rebuilt from the confirmed tree and the pending batches that are still valid.

Each batch is validated on the JS thread first. If it is invalid there, the local version does not advance and the next batch leaves with the correct base. This solves the cascading rejection noted in Delivery 1.

`app/counter.ts` no longer kept its own state: the next value came from `jsnir.leer(2)` and the base from `jsnir.version()`. Rust is the single source of truth.

| Verification | Result |
| --- | --- |
| Replica test | Reads its own changes. An invalid batch does not advance the version. The view returns to the confirmed tree on a rejection. At the end, the view and the confirmed tree match the UI |
| Burst of 64 events in flight, js, direct and buffer modes | Version 1025, value 24. Each callback computed its value by reading the optimistic view |
| Offscreen egui, rust, js, direct and buffer | 5 of 5 states identical to the reference in each mode. 1,100 `ok` batches per mode |
| Chat script | 50 verified captures. The chat does not use the tree yet, so the replica plays no part in it |
| egui pointer test | Passes |

Round trip with the replica, in µs, p50 / p99 ([summary](../metrics/replica-01/curve/summary.json)): with 1 operation, 2.86 / 4.95 direct and 5.20 / 11.33 with JSON. With 1,000 operations, 108.4 / 156.6 direct, against 104.2 / 113.0 before the replica. The added cost is within the variation between series. This time `activarDirecto` keeps `jsnir.set` in a local variable.

### Structures for reading and publishing

`examples/estructuras.rs` (since removed), with 10,000 nodes. Three runs with similar results; the first one is shown ([outputs](../metrics/structures-01/)). `imbl` 7.0.2 was only a dev dependency. According to its documentation, `OrdMap::clone` is O(1) because it shares the root, and `insert` / `get_mut` copy only the shared nodes, in O(log n).

| Reading one value | ns |
| --- | ---: |
| `BTreeMap` (the current one) | 35–37 |
| `imbl::OrdMap` | 26–27 |
| `Vec` indexed by id | 1.1 |

| Publishing with an open snapshot (µs) | p50 | p99 | max | Dropping the snapshot, p50 |
| --- | ---: | ---: | ---: | ---: |
| Snapshot with `BTreeMap` | 20.50 | 28.38 | 153.89 | 31.61 |
| Snapshot with `imbl::OrdMap` | **0.31** | 1.44 | 5.49 | **0.08** |
| Replica with `BTreeMap`, two copies | 0.12 | 0.47 | 1.31 | — |
| Replica with `Vec`, two copies | **0.02** | 0.19 | 0.35 | — |

**Reading.**
- Of the ≈ 120 ns a read from JS costs, ≈ 36 are the `BTreeMap` lookup. The rest is QuickJS's native call through rquickjs. A `Vec` indexed by id would bring the read down to about 85 ns. That is an estimate: it still has to be measured from JS.
- With a persistent map, the snapshot stops being expensive: publishing costs 0.31 µs and dropping it 0.08 µs, against 20.5 and 31.6 µs with a `BTreeMap`.

**The two viable options:**

| | Replica with `Vec` | Snapshot with `imbl::OrdMap` |
| --- | --- | --- |
| Read | ≈ 1 ns plus the native call | ≈ 26 ns plus the native call |
| Publish | Apply twice, ≈ 0.02 µs | Path copying, ≈ 0.31 µs |
| Memory | The tree twice | Once, plus the shared nodes of open snapshots |
| Readers | One JS thread per replica | Any number of threads with the same snapshot |
| Requires | An ordered log of answers | A dependency and the optimistic layer for one's own changes |

With a single JS thread, the replica with `Vec` is the fastest. If JSNIR is going to have several JS workers reading the same UI, the persistent snapshot scales better.

## Delivery 7 · The chat in the tree, with two writers · 19 November 2026

**Question.** Can JS synchronously read state that the UI writes (the draft and the selection) while the UI remains the owner of that state?

**Design: a tree partitioned by writer.** The tree has two writers and each owns its nodes.

| Nodes | Writer | Operations |
| --- | --- | --- |
| Columns, labels, texts, buttons | The app (JS, or the Rust logic of the reference mode) | `create`, `set`, `settext` |
| `Input`: draft and selection | The UI | `setinput` |

The app declares the field at mount, but afterwards it cannot write it: an app batch with `setinput` is rejected (`ui_only_op`, then called `nodo_de_la_interfaz`). The UI can only emit `setinput`. Since no node has two writers, the batches of one and the other **commute**: order does not change the result, and no locks or rebasing are needed. A test checks this by applying the same two batches in both orders. The global version counts the batches of both writers. The base of an app batch is compared only with `app_seq`, the number of app batches applied, so typing in the field does not invalidate a JS batch in flight. The app thread fills in that base; the script no longer sends it.

**Log.** The UI adds two kinds of entries to the replica's log: answers (`Answer`) and its own batches (`Ui`). When each callback starts, the app thread applies the answers to the confirmed tree and the UI batches to both the confirmed tree and the view. The view does not need rebuilding, because they commute.

**Generic app thread.** `spawn_app` concentrates the mount, the replica, the log and the event loop. The JS counter (three protocols), the JS chat and the Rust chat use it. Events carry `Event { callback, arg, text }`.

**Chat.** The history is a column of `Text` nodes. The draft and the selection are `Input` 3. The egui adapter publishes the `TextEdit` draft and selection every frame as a UI batch, only if they changed. On submit, Rust captures the text, clears the field and writes that cleared field to the log before queueing the event. `app/chat.ts` added the message with `jsnir.crearTexto` and wrote the status label with what it read at that moment: `jsnir.readText(3)` (then `leerTexto`) and `jsnir.seleccion(3)`. Duktape's `\uXXXX` escape disappeared along with the message's JSON. Nodes moved to a `Vec` indexed by id.

| Verification | Result |
| --- | --- |
| 9 core tests | Pass. Partitions, commutativity, replica, chat in both modes |
| Chat test, 80 ms delay | In Rust and in JS the status reads «borrador «en edición», selección 3–10» (the Spanish status text of the time): the app read what was typed while it was busy |
| Script A–E, five repetitions per mode | 50 identical captures. The five states have **the same hash as before the change** ([parity](../metrics/chat-tree-01/chat/parity.json)). In E, the status is «borrador «segundo borrador», selección 8–16» |
| Submit → E | 501–504 ms in both modes, with a deliberate 500 ms delay ([times](../metrics/chat-tree-01/chat/times.csv)) |
| egui counter, four modes | 5 of 5 exact states and 1,100 `ok` batches per mode |

**Scrolling 10,000 messages from the tree** (five processes per mode, with an A/A control, [CSV](../metrics/chat-tree-01/)):

| Series | Rust p99 | JS p99 | JS / Rust |
| --- | ---: | ---: | ---: |
| A | 0.360 ms | 0.336 ms | −6.7 % |
| A2, the same binary | 0.317 ms | 0.325 ms | +2.7 % |

The difference changes sign between series of the same binary: there is no detectable difference. **Meets the threshold.**

**Reading from JS with a `Vec`** (replica strategy, 1,000 reads, p50): 112.4 µs, against 118.7 µs with a `BTreeMap`. A 5 % improvement, not the 30 % estimated in Delivery 6. In the isolated benchmark the `BTreeMap` lookup cost 36 ns, but inside the call from JS it barely matters. The real cost is the native call: ≈ 110 ns per read.

**Round trip** ([summary](../metrics/chat-tree-01/curve/summary.json)): with 1,000 operations, Rust 7.26 µs (before: 3.95), direct 117.1 µs (before: 104–108) and JSON 818.7 µs. `Op` and `Node` grew with the text variants, and copying and walking large batches costs about 3 ns more per operation in Rust and about 10 ns more in direct mode. With a single operation nothing changes.

**What it shows.** JS reads UI state instantly, without asking and without waiting. It sees one complete version per callback. It cannot write that state. And it does not lose what the person types while the app is busy.

**Pending.**
- Scroll and text metrics as UI state.
- Reduce the size of `Op` (for example, put the text in a `Box<str>`) if large batches justify it.
- The chat on the other adapters.

## Delivery 8 · Shared memory: JS reads the Rust heap without translation · 19 November 2026

**Idea.** The two languages represent the same data differently. In Rust, a `u16` sits at a fixed position. In JS, a tagged `JSValue` lives in the collector's heap. Crossing means translating, and that is what JSON (the whole batch) and native calls (each argument) cost. The stack cannot be shared, because each language's frames die with their call. The heap can: if numeric data is stored in flat columns, Rust reads it as a slice and JS as an `Int32Array`. **The same memory, two lenses, nothing copied or translated.**

**Implementation** (`src/lectura.rs`, since removed; the columns now live in [contract.rs](../src/contract.rs)). The column is a `Box<[i32]>` on the Rust heap, with one value per node id. It is exposed with `ArrayBuffer::from_source_immutable`: according to the rquickjs documentation, JS gets no write permission and Rust may keep the memory. `Persistent` keeps the collector from freeing the buffer. Before each callback, Rust applies the UI batches by writing through the pointer that `as_raw()` returns at that moment. This happens on the JS thread with no JS running, the condition the documentation sets for the pointer to be valid. If the buffer were detached, `as_raw()` returns `None` and Rust stops instead of writing freed memory. JS read `jsnir.valores[id]`. A test checks three things:
- JS sees the correct sum;
- a change written by Rust shows in JS without a copy;
- JS cannot modify the column.

**Finding.** In QuickJS-ng 0.16.2, writing to an immutable typed array does not throw, not even in strict mode: the write is silently ignored (`quickjs.c`, the `ta_immutable` branch). The single-writer property holds, but the SDK should warn in development.

**Measurement.** The same scenario as Delivery 5: 10,000 nodes, the UI publishes every 100 µs, and three processes per strategy. `empty` runs the same loop and the same index computation without reading anything; it is the interpreter's floor. Callback time in µs, p50 / p99 ([summary](../metrics/memory-01/summary.json)):

| Reads | empty (floor) | memory | replica (native call) | mutex |
| ---: | ---: | ---: | ---: | ---: |
| 1 | 0.21 / 0.23 | 0.27 / 0.29 | 0.28 / 0.30 | 0.28 / 0.31 |
| 10 | 0.81 / 0.83 | 0.90 / 0.99 | 1.26 / 1.58 | 1.29 / 1.54 |
| 100 | 6.85 / 7.82 | 7.27 / 9.51 | 11.14 / 14.14 | 11.32 / 14.49 |
| 1,000 | 66.72 / 72.28 | 70.74 / 76.40 | 109.61 / 137.00 | 111.63 / 130.71 |

| Cost per read, net of the floor | ns |
| --- | ---: |
| Shared memory | **≈ 4** |
| Native call (`jsnir.leer`) | ≈ 43 |
| The JS loop (index, sum, iteration) | ≈ 67 per iteration |

**Reading.**
- Shared memory lowers the cost of a read about 10 times compared with the native call. It is close to the cost of reading a JS `Array`.
- **The bridge stops mattering: what remains is the JS the developer writes.** With 1,000 reads, 94 % of the callback is the interpreter's loop.
- This corrects Delivery 7: the native call did not cost ≈ 110 ns per read but ≈ 43. The rest was the loop.
- Publishing to the column costs the same as to the replica (p50 151 ns).

**Consequences for the design.**
1. **Numeric reads through shared columns**: values, selection, scroll, layout rectangles. JS reads them without crossing.
2. **Text and variable structures by call**, per id. They cross once and only when needed.
3. **Writes through validated batches**, with an owner per node, as before.
4. The column layout is defined once, and the Rust accessors and the TypeScript `.d.ts` are generated from it.
5. With the bridge out of the way, **the next performance lever is the JS code**: what the SDK does per event and how much heavy logic is delegated to Rust primitives.

## Delivery 9 · A single contract and shared columns in the runtime · 19 November 2026

**Working rule.** Each piece of the bridge is justified by how the two engines work and by a measurement, never added for its own sake.

| Decision | Basis in the engine | Measurement |
| --- | --- | --- |
| Numbers through shared columns | In QuickJS, indexing a typed array is one interpreter instruction. A native call goes through a trampoline that converts each `JSValue` | ≈ 4 ns against ≈ 43 ns per read (Delivery 8) |
| Text by call and by id | JS stores strings with 8 or 16 bits per character and Rust uses UTF-8: crossing text always transcodes | — |
| Rust is the only writer of the columns, JS only reads | Rust borrowing (shared `&` / exclusive `&mut`) carried across the two languages. Rust writes only while no JS runs, on the same thread | Test: JS cannot modify the column |
| One declaration generates both lenses | Rust types are known at compile time: a macro can derive the TypeScript type | Test: `app/jsnir.d.ts` is generated and compared |

**Contract** ([contract.rs](../src/contract.rs), then `contrato.rs`):
- `contract_columns!` (then `columnas!`) declares each column once: its JS name, source node and field, and documentation. It generates `project` (each column's value for a node) and its part of the `.d.ts`. Columns at the time: `valor` (label), `hijos` (column), `seleccionDesde` and `seleccionHasta` (field); today they are `value`, `children`, `selectionStart` and `selectionEnd`.
- `def!` registers each native function and derives its TypeScript signature from the Rust types of the closure, through the `Ts` trait. The `Ctx` parameter does not appear in the signature.
- `app/jsnir.d.ts` is generated from there. A test fails if the file is stale (`JSNIR_UPDATE=1 cargo test contract` regenerates it). The scripts no longer declare `jsnir` by hand.

**Functions that remained, one per reason:**
- writing: `set`, `commit`, `crearTexto`, `setText` (then `ponerTexto`);
- reading text: `readText` (then `leerTexto`).

`leer`, `version`, `seleccion` and `hijos` were removed: they were numbers read by call and now come from the columns.

**Columns in the runtime.** Each VM installs `jsnir.columns` (then `jurui.col`) with one immutable `Int32Array` per column, backed by a `Box<[i32]>` on the Rust heap and indexed by node id. The app thread keeps them up to date at three moments, always with no JS running:
- when catching up with the log, for the nodes touched by UI batches;
- after producing each of its own batches (the optimistic view);
- in full, if a rejection forces the view to be rebuilt.

When an id larger than the length appears, the columns are recreated at double the size. That is why the `.d.ts` warns not to keep them across callbacks. `counter.ts` read `jsnir.columns.value[2]`. `chat.ts` read the children, selection-start and selection-end columns, and used `readText` only for the draft.

| Verification | Result |
| --- | --- |
| 11 core tests | Pass. They include the burst of 64 events in flight reading the optimistic column in three protocols, the chat reading the selection through columns, and the generated `.d.ts` |
| Chat script | 50 captures, **same hash as before the change** in all five states |
| egui counter, four modes | 5 of 5 exact in each |
| Scrolling 10,000 messages in JS mode | Complete. The columns grew to cover the 10,100 ids |
| Masonry, six adapters | Exact pixels. All compile |

**Cost of keeping the columns** ([summary](../metrics/contract-01/curve/summary.json)): in direct mode, 3.43 µs with 1 operation (3.20 in Delivery 7) and 120.8 µs with 1,000 (117.1). Syncing each batch, which restores the four buffers and writes the touched ids, costs a few tenths of a microsecond. It is at the edge of the noise between series.

**Pending.**
- Measure how much of those tenths is restoring four `Persistent` handles per batch. If it matters, keep the pointers in a single structure and only check that the buffer is still attached.
- Move scroll and layout rectangles into columns once they exist as UI state.
- Also generate the node and operation names of the mount (hand-written JSON in the scripts at the time).

## Delivery 10 · Types checked end to end · 19 November 2026

**Goal.** The shape of the data that crosses the boundary exists in one place (Rust), and the TypeScript compiler enforces it in the apps.

**TypeScript.** `typescript` 7.0.2 was installed as an exact-version dev dependency (`package.json`; `node_modules/` stays out of version control). Each app is checked separately against `jsnir.d.ts`, because both are global scripts that define the same functions (`app/tsconfig.counter.json` and `app/tsconfig.chat.json`, which extend `tsconfig.base.json`). The options come from the engine:
- `lib: ["es2020"]` without DOM, because QuickJS has no `window` or `document`;
- `erasableSyntaxOnly`, which accepts only syntax that `stripTypeScriptTypes` erases without transforming, which is what `build.rs` did at the time;
- `strict`, and no unused variables or parameters.

**Contract enums.** `Node` and `Op` are declared with `contract_enum!` (then `contrato_enum!`), which generates the Rust enum with serde and its TypeScript union from the same declaration. The tag and each variant's name come from `rename_all = "lowercase"` on both sides. Markers:
- `(skip)`: Rust derives the field and JS does not send it (a column's children);
- `(default)`: the field is optional in JS (a field's initial text and selection);
- `(ui)`: the variant belongs to the UI only and is kept out of the JS type (`SetInput`).

The `.d.ts` includes `Node`, `Operation` and `Batch` (then `Nodo`, `Operacion` and `Lote`). The mounts and JSON batches in `counter.ts` and `chat.ts` were declared as `Batch`.

**Defense in two layers.** JS trying to write the draft no longer compiles: `"setinput"` is not assignable to `Operation`. If it got through anyway, for example from unchecked code, Rust rejects it (`ui_only_op`).

| Verification | Result |
| --- | --- |
| `tsc` on `counter.ts` and `chat.ts` | No errors |
| `tsc` on incorrect uses | Catches the missing column, the wrong argument type, the `string \| undefined` assigned to `string`, the `setinput` from JS and the label without `value` |
| Test `contract_typescript_apps_typecheck` | Runs `tsc` inside `cargo test`. It fails when `valor` is changed to `valorr` in `counter.ts` and passes again when restored |
| 12 core tests, egui, Masonry | Pass |
| Chat, counter in four modes, six adapters | 50 captures with the same hash as before. 5 of 5 exact per mode. They compile |

**What is still written in two places.** `Batch` is a literal line in `contract::dts`, because the Rust `Batch` does not go through the macro (Rust fills in its `base` and JS does not send it). The Rust reference apps (`ChatRust`, `rust_mount`) duplicate the JS apps' logic on purpose, because they are the baseline for the measurements.

## Delivery 11 · Latency until light on screen, and the bounded wait · 19 November 2026

**Question.** Until the person sees it on screen, does an event that goes through JS arrive as fast as one resolved in Rust?

**Source of the timestamp.** `wp_presentation` (the stable Wayland *presentation-time* protocol). According to its specification, `presented` tells when the content "turned into light the first time" on the main output, approximated by the compositor from the framebuffer *flip*. It is read with the clock of `clock_id`; niri announced 1 (`CLOCK_MONOTONIC`). `feedback` refers to the surface's next content update, so it is requested during the frame, before eframe swaps.

**Implementation** ([presentation.rs](../backends/egui/src/presentation.rs), [measurement.rs](../backends/egui/src/measurement.rs)). It uses winit's own connection: its `wl_display` through `Backend::from_foreign_display` and its `wl_surface` through `ObjectId::from_ptr`, plus an event queue of its own. `wayland-client` 0.31.15, `wayland-backend` 0.3.17 and `wayland-protocols` 0.32.13 were already in the tree through winit, with the same features: no new crate was compiled.

**Bug found along the way.** The first version read the Wayland socket on its own (`prepare_read` + `read`). That left events in winit's queue that winit did not know it had to dispatch, and the window froze at the third frame. Now it only dispatches what winit already handed to our queue.

**Method.** `raw_input_hook` injects each event at the start of a frame and timestamps it with the compositor's clock. For the counter it is a click (press in one frame, release in the next; it counts from the release). For the chat it is a letter. Each frame records what it shows (the value or the draft's length) and requests its feedback. An event's latency goes from its injection to the first **presented** frame that shows the expected value. The kernel → compositor → app path is the same in every mode, so the comparison isolates what the bridge changes. 90 warm-up frames, 120 events every 15 frames and two runs per configuration. A real window in niri, vsync on and the display at 120 Hz (8.333 ms). [Data](../metrics/presentation-02/) · [analysis](../scripts/presentation.py)

**First finding: one extra frame in JS.** In JS mode, all 120 events appeared exactly one frame later than in Rust (32.9 against 24.6 ms). `Counter::show` activates and calls `poll` in the same pass. The JS answer (≈ 3 µs) has not arrived when the frame is painted, and the frame does not wait for it.

**Improvement: a bounded wait in `Runtime::poll`.** If events were activated less than `budget` ago, `poll` blocks on the channel with `recv_timeout`, without using CPU, until that deadline counted from the activation.
- **Basis:** the measured round trip is ≈ 3 µs and a frame at 120 Hz lasts 8,333 µs.
- **Default deadline:** 1 ms, 12 % of the frame. egui draws the counter in ≈ 0.4 ms.
- **If the callback is slow:** the deadline expires and the following frames do not wait at all.
- **Scope:** it lives in `Runtime`, so all six adapters got it without changes. It is set with `JSNIR_WAIT_US`; 0 disables it.
- **Test:** 100 consecutive answers land in the same `poll`. With a 200 ms callback, `poll` returns within the deadline, and the next one does not wait.

| Configuration | p50 | p95 | p99 | Frames late (240 events) |
| --- | ---: | ---: | ---: | --- |
| Counter, native Rust | 24.57–24.60 ms | 24.66–24.67 | 24.67–24.68 | 0 in all |
| Counter, JS without the wait | 32.91–32.92 ms | 32.99–33.02 | 33.00–33.03 | **1 in all** |
| **Counter, JS with the wait** | **24.55–24.57 ms** | 24.66–24.67 | 24.68–24.69 | **0 in all** |
| **Counter, direct with the wait** | **24.55–24.57 ms** | 24.65–24.66 | 24.66–24.68 | **0 in all** |
| Chat typing, Rust | 24.57–24.61 ms | 24.67 | 24.68–24.69 | 0 in all |
| Chat typing, JS | 24.55–24.58 ms | 24.67 | 24.67–24.68 | 0 in all |

No frame was dropped and all 2,880 events were measured. The differences between modes (≈ 0.03 ms) are smaller than those between two runs of the same mode. The ≈ 24.6 ms are this machine's pipeline: the egui frame, the swap, niri's composition and scan-out, about three refresh periods. With the bounded wait, the counter stays exact in all four modes. All six adapters compile and pass the egui and Masonry tests.

### Status of the proposal's thresholds

| Threshold | Result | Delivery |
| --- | --- | --- |
| Key → visible draft: no difference (±5 % or ±0.1 ms) | ✅ Difference ≈ 0.03 ms, within the A/A variation | 11 |
| Frame p99 with 10,000 messages: up to +10 % | ✅ No detectable difference with an A/A control (−6.7 % and +2.7 %) | 7 |
| Event → batch, 1 to 100 operations: p99 ≤ 1 ms | ✅ Direct: 4.9–23.9 µs | 9 |
| Event → batch, 1,000 operations: p99 ≤ 4 ms (informative) | ✅ Direct: 127 µs | 9 |
| Event → first frame: up to one extra frame | ✅ **Zero extra frames** with the bounded wait | 11 |
| Resident memory: up to +20 MB | ✅ +1.4 MB, measured without a window | 2 |
| Startup to first frame: up to +100 ms | ✅ +0.6 ms of mount, measured without a window | 2 |
| Executable size: up to +5 MB | ✅ ≈ +2.2 MB in egui (QuickJS statically linked) | 2 |

**Limits of the series.**
- A single machine, one compositor and one 120 Hz display.
- Input is injected into the app, so the hardware → compositor stretch is not measured (it is the same in every mode).
- Memory and startup were measured without a window. Measuring them with a window and cold is still pending.
- The comparison of the benefit against a WebView (Tauri or Electron) with the same screen is missing.

## Delivery 12 · A TSX SDK with signals · 19 November 2026

**Goal.** Make writing a UI comfortable (TSX, components, reactive state) without losing what was gained in the bridge.

**Decisions and their basis.**

| Decision | Basis | Measurement or source |
| --- | --- | --- |
| Fine-grained signals, no virtual DOM. Each component runs once | Diffing trees in JS is bytecode in the interpreter. A signal emits exactly its node's operation | JS loop ≈ 67 ns per iteration (Delivery 8) |
| Changes by direct call (`set`, `setText`) | It is the cheapest way to write | ≈ 0.1 µs per operation (Delivery 3) |
| Structure as a JSON batch: one subtree, one call (`jsnir.batch`) | Creating nodes is rare and the JSON is amortized over the subtree | Delivery 2 |
| Lists with explicit mutations (`items.push`) | A new item is one `create`, O(1), without diffing the list | — |
| Effects batched per event | Several writes in one callback produce a single change | Test |
| What the UI writes is read from Rust (`TextInput` → `field.text()`, `field.selection()`) | One owner per piece of data (Deliveries 7 to 9) | — |
| Types in two layers: the generated contract (wire) and the SDK written in TS, checked against it | If the SDK emits something Rust does not accept, it does not compile | Delivery 10 |
| JSX compiled by `tsc` (`jsx: react`, `jsxFactory: h`) | JSX is not erasable syntax and `stripTypeScriptTypes` does not transform it. TypeScript 7 dropped `outFile`, so `build.rs` emits the SDK and the app separately and concatenates them | — |

**The SDK** ([sdk.ts](../app/sdk.ts)): `signal`, `list`, the components `Column`, `List`, `Text`, `Label`, `Button` and `TextInput`, and `mount`. It implements the protocol with Rust through two functions: `jsnirMount()` (then `montar()`), which emits a batch with the tree, and `jsnirEvent(callback, text)` (then `evento`), which runs the handler and flushes the effects.

**The apps** became:
- [chat.tsx](../app/chat.tsx) is the sketch from the proposal: `<List items={messages}>`, `<TextInput onSubmit>` and `<Text>{status}</Text>`.
- [counter.tsx](../app/counter.tsx) runs with `jsnir-egui js` (then a separate `sdk` mode).

`counter.ts` remained as the protocol benchmark (`js`, `directo`, `buffer`); it was removed later.

**Contract changes.**
- `List` node: a scrolling list, distinct from `Column`, which is layout only.
- `Input.submit`: the submit callback.
- `batch(json)` (then `lote`) replaces `crearTexto`.

The Rust chat no longer uses fixed ids: after the mount it finds the list, the field with a submit and the status by their kind. On the first build, the SDK compiled against the previous `.d.ts` and `tsc` flagged exactly the three changes not yet published. Type errors do not stop the build, so `jsnir.d.ts` can still be regenerated, but they do make `cargo test` fail.

| Verification | Result |
| --- | --- |
| 16 core tests | Pass. They include the burst of 64 events in the SDK mode and three new SDK tests |
| TSX counter | Mounts **the same tree** as the Rust reference and emits **a single `set` per click**, without touching the button (the test the proposal asked for) |
| TSX chat | Each submit emits **exactly** one `create` and one `settext`. The status reads the live draft and selection |
| Batching | Three writes in one event produce a single `settext` with the final value |
| Invalid `batch` | Throws in JS and leaves no operations |
| TSX chat in egui, 50 captures | **Same hash as before** in all five states |
| TSX counter in egui | 5 of 5 exact states |
| Scrolling with the TSX chat | 10,000 messages complete (one batch per message) |
| `tsc` on the three apps | No errors |

**Cost.** Round trip with 1 operation: SDK 5.59 µs against 4.66 µs for the hand-written direct protocol in the same series ([summary](../metrics/sdk-01/curve/summary.json)). The SDK adds ≈ 0.9 µs per event (signal, effect, batching), 0.01 % of a 120 Hz frame.

**Latency to the real screen** (niri, 120 Hz, [data](../metrics/sdk-01/)):

| App | Events | p50 | Frames late |
| --- | ---: | ---: | --- |
| TSX chat typing | 120 | 24.56 ms | 0 in all |
| Rust chat typing | 120 | 24.58 ms | 0 in all |
| TSX counter | 960 in 8 runs | 24.53–24.59 ms | 0 in 957, 15 in 3 |
| Rust and direct counter | 1,560 across both series | 24.54–24.60 ms | 0 in all |

**Lost clicks (pending).** Three clicks of the TSX counter (out of 960 events) produced no change, and the value arrived with the next click, hence the 15 frames. In the traced run there were 119 activations for 120 clicks, and each activation produced its operation: **the bridge lost nothing; egui did not recognize the click**. The cause is not identified. The hypothesis that real system input got mixed with the injected input was tested: the harness now discards and counts it, but the following runs had no real event to discard. So the hypothesis is not confirmed, although the 600 later measurements had no losses. The difference against the other modes (3 of 960 against 0 of 1,560) is not conclusive.

## Delivery 13 · Layout with Taffy · 19 November 2026

**Goal.** Rust computes the layout from styles declared in TSX, and JS can read the metrics without crossing the boundary.

**Sources.** Taffy's documentation (Context7, `/dioxuslabs/taffy`) and its code at version 0.15.1:
- according to its benchmark, computing costs ≈ 0.25–0.47 µs per node (10,000 nodes ≈ 2.7–4.8 ms);
- it caches per node and marks as dirty what changes;
- `location` is relative to the parent;
- rounding is on by default (`use_rounding`);
- a leaf without a measure function measures 0×0.

**Decisions.**

| Decision | Basis |
| --- | --- |
| Layout is computed in Rust, on the UI thread | It depends on the window size and, later, on text measurement: it is derived UI state |
| It reaches JS as a UI batch (`setrect`) with only what changed, and from there the `x`, `y`, `width` and `height` columns (then `ancho` and `alto`) | The same path as the draft: one owner and reads in ≈ 4 ns (Delivery 8) |
| The items of a `List` do not go through Taffy: the list is a leaf | 10,000 rows would cost 2.7–4.8 ms, more than half a frame at 120 Hz. The adapter already draws only the visible rows |
| Only Taffy's `flexbox` feature | The chat needs no grid, block or float, and the executable has a size threshold |
| `Style` (then `Estilo`: `width`, `height`, `grow`, `padding`, `gap`) declared with `contract_struct!` | A single definition for Rust and TS. Serde rejects unknown fields and the tree rejects negative or non-finite values (`invalid_style`) |
| The root takes the window's size | Taffy treats the root's available space as a soft constraint |

**Implementation.**
- [layout.rs](../src/layout.rs) keeps a `TaffyTree` parallel to the tree. Each created node is added, except the descendants of a list. `compute(width, height)` sets the root's size only if it changed, lets Taffy use its cache, accumulates the relative positions from the root and returns only the rectangles that changed.
- `Chat::layout` publishes them as a UI batch: they enter the tree and the replica's log, and from there the columns.
- In the SDK, `Column`, `List`, `Text` and `TextInput` take `style?: Style`, which is passed to the node as is, so the list of fields does not have to be repeated. `field.rect()` reads the rectangle from the columns.
- [chat.tsx](../app/chat.tsx) declares the layout that egui had hand-coded: a column with `padding: 16`, `gap: 16` and `grow: 1`, a list with `grow: 1` and a field with `height: 34`. The status label sits outside that column and, without text measurement, measures 0 (step 3).
- The egui adapter dropped its position constants and draws with the core's rectangles. The "Send" button is still part of the field widget (92 px and a 12 px gap).

| Verification | Result |
| --- | --- |
| 19 core tests | Pass |
| Taffy against the hand-coded layout, Rust and JS | **Identical**: list at 16,16 of 608×398 and field at 16,430 of 608×34. At 800×600, 16,16 of 768×518 and 16,550 of 768×34. Recomputing without changes publishes nothing |
| JS reads the layout | `jsnir.columns.x/y/width/height` return 16, 430, 608 and 34 for the field, and 398 high for the list |
| Invalid styles | `paddng` fails serde and `padding: -1` fails the tree |
| Chat in egui drawn with Taffy, 50 captures | **Same hash as before** in all five states |
| Scrolling 10,000 messages, egui, Masonry, six adapters, `tsc` | Fine |

**Cost** ([measurement](../metrics/layout-01/cost.txt), three runs, 10,000 messages in the history):

| `Chat::layout` per frame | p50 | p99 |
| --- | ---: | ---: |
| Steady (no changes, Taffy's cache) | 0.22–0.23 µs | 0.24–0.48 µs |
| Resizing on every call (recomputes and publishes) | 1.14–1.19 µs | 2.6–3.5 µs |

The history's size does not matter because its rows do not go through Taffy. To the real screen, with the layout computed every frame, the chat gives JS 24.57 ms and Rust 24.60 ms (p50), with 0 frames late in 120 of 120 events. The egui executable went from 15.38 to 15.92 MB.

**Pending.**
- Text measurement (step 3): give `Text` its own size, which makes the status label visible.
- Reactive styles: they are fixed when the node is created.
- A horizontal container (`Row`).
- Alignment.
- Bring the layout to the counter, which keeps its fixed design as the pixel reference for the six adapters.

### Prior art: NSH (Native Shell)

While looking into Taffy, [NSH](https://deepwiki.com/longbridge/nsh/1-overview) by Longbridge came up. According to its DeepWiki documentation (we did not review the code):
- it is a native UI framework for AI-generated interfaces distributed at runtime, which loads signed TSX modules;
- it runs JS with QuickJS and computes layout with Taffy;
- its reactive graph (`signal`, `computed`, `effect`) lives in Rust, and components run once, with no virtual DOM;
- it sends mutations to native per-platform backends (and HTML on the web) as diffed batches (`MountBatch`).

It matches decisions that JSNIR reached by measuring: single-pass components, fine-grained signals, QuickJS and Taffy. According to the same documentation, the differences are in focus. NSH targets distributed, signed UI on each platform's native widgets. JSNIR aims to match native Rust performance, with shared memory for reads, a log-fed replica, a tree partitioned by owner and measurement until light on screen. Its code is worth reading before deciding step 4 (an own renderer), to compare where each one resolves the JS → Rust crossing.

## Delivery 14 · A JIT for QuickJS (`quickjs-jit`): measured and discarded · 19 November 2026

**Question.** What remains of JSNIR's cost is JS bytecode (≈ 67 ns per loop iteration, Delivery 8). Does a JIT lower it?

**The crate.** `quickjs-jit` 0.14.2 is Longbridge's fork of rquickjs (`github.com/longbridge/rquickjs`, MIT license). On crates.io it has a single owner, has existed since 1 September 2026 and has ≈ 50,000 downloads. It keeps the rquickjs API and QuickJS-NG `0fdea21` (0.16.2+), the same base we already use. The fork alone **does not include the JIT**: it exposes an ABI (`jit-abi`) where another crate, `quickjs-jit-runtime`, hooks in: a tiered JIT on Cranelift (feature `compiler`). Native execution only on macOS, Windows and Linux (x86_64 and aarch64): **no iOS or Android**, which forbid generating executable code anyway. Reviewing NSH's `Cargo.lock` revealed a mistake in the previous analysis: NSH uses the fork, but **not the JIT**.

**Method.** The experiment used the same executable with and without the historical `JURUI_JIT=1` flag. It used `Jit::attach` with the default configuration (automatic policy, 32 calls or 56 loop iterations before compilation, one compiler thread). A baseline with `rquickjs` 0.14.0 was taken in the same session. The JIT metrics (`native_entries`, `compile_failures`, `tier2_entries`) were read when each VM closed to confirm what ran as native code. [Data](../metrics/jit-01/)

**Correctness.** The full suite passed with the JIT in debug. In release, with tests in parallel, the test requiring a fast answer to land in the same `poll` (under 1 ms) failed once; alone it passed 3 of 3, and without the JIT it always passes. A hint, unconfirmed, of a pause when tiering up.

| Benchmark, 1,000 iterations or operations (p50, µs) | rquickjs 0.14.0 | Fork, interpreter | Fork + JIT |
| --- | ---: | ---: | ---: |
| `empty`: pure JS loop | 67.30 | 64.63 | **1.84** |
| `memory`: typed-array read | 71.26 | 69.39 | 70.16 |
| `replica`: native call | 113.88 | 111.98 | 113.12 |
| `local-direct`: callback with `jsnir.set` | — | 82.97 | 83.84 |
| `direct`, round trip | 150.83 | 152.42 | 138.13 |
| With 1 iteration or operation: `empty` / `local-direct` | 0.21 / — | 0.22 / 0.36 | 0.36 / 0.56 |

| JIT metrics | Compile failures | Native entries | Optimized tier |
| --- | ---: | ---: | ---: |
| `empty` | 0 | 1,873 | 1,862 |
| `memory` | 4 | 121 | 0 |
| `replica` | 4 | 66 | 0 |
| `local-direct` | 1 | 0 | 0 |

**Reading.**
- The JIT speeds up a pure JS loop **35×**.
- It **does not compile** functions that call native functions or read typed arrays, which are the bridge's primitives. Those stay interpreted and pay the JIT's bookkeeping cost (+0.14–0.20 µs per call before compiling).
- The fork without the JIT performs the same as rquickjs.
- egui executable: **22.39 MB with Cranelift**, against 15.92 MB. That is +6.47 MB even with the JIT off: the +5 MB threshold is exceeded.

**Decision.** Back to `rquickjs` 0.14.0. The JIT does not improve the bridge, breaks the size threshold, does not exist on mobile, comes from a young crate with a single owner and left a hint of pauses while compiling. It is a reference for an app with heavy pure-JS computation: once the JIT compiles native calls and typed arrays, the measurement is worth repeating. To reproduce:

```toml
rquickjs = { package = "quickjs-jit", version = "=0.14.2" }
rquickjs-jit = { package = "quickjs-jit-runtime", version = "=0.14.2", features = ["compiler"] }
```

with `rquickjs_jit::Jit::attach(&rt, rquickjs_jit::JitConfig::builder().build()?)` when creating the VM, keeping the `Jit` alive as long as the runtime.

### Can we keep only the useful part of the JIT?

Measured stripped (`strip`), which is what matters for distribution. Each variant was built in a separate directory and the project was restored afterwards.

| egui executable, stripped | MB | Difference from the interpreter |
| --- | ---: | ---: |
| Interpreter (`rquickjs` 0.14.0) | 11.97 | — |
| JIT as published (Cranelift for x86 and arm64) | 17.04 | +5.07 |
| JIT with Cranelift for x86 only (patched copy of the crate) | 16.33 | +4.36 |
| The above plus Cranelift built with `opt-level = "s"` | 15.99 | **+4.02** |

- **Two possible cuts:** building Cranelift only for the target architecture saves 0.71 MB. Optimizing the compiler itself for size saves 0.34 MB more. That option changes how fast the JIT compiles (on a background thread), not the quality of the code it generates. This variant's performance was not measured.
- **The floor:** ≈ 4 MB, which is Cranelift's core. Going below would need another kind of JIT, for example a template JIT (*copy-and-patch*, like CPython 3.13's). That is another project, not a cut.
- **The cost of maintaining it:** Cargo features are additive, so the first cut needs a patched copy of `quickjs-jit-runtime`. The sustainable route is to ask upstream for a per-architecture feature.
- **Conclusion:** it would only make sense as an optional desktop feature for apps with heavy pure-JS computation. For the bridge it changes nothing.

## Delivery 15 · Live UI during development · 19 November 2026

**Goal.** Edit the TSX and see the change in the open window, without restarting and without losing what the person is typing.

**Decisions.**

| Piece | Decision | Basis |
| --- | --- | --- |
| Replacing the app | A `remove` operation (subtree). The reload is **one atomic batch**: `[remove(old root), create(…new app)]` | There is never a frame with a half-replaced app. Validate all or nothing (Delivery 1) |
| UI state | Draft, selection and scroll live in Rust (`Chat` and egui). On remount, Rust republishes the draft into the new field | One owner per piece of data: a JS reload cannot touch what belongs to the UI |
| JS state | The old VM hands over a snapshot (`jsnirReloadState()`) and the new one reads it from `__jsnirPreviousState` before running. Each `signal` or `list` gets its value back if its position in creation order and its initial value match | If the initializer changes, the saved value is no longer trustworthy (the same rule as NSH) |
| Code with errors | If it does not compile or has type errors, nothing reloads. If the script does not load, the old VM continues. **An exception in a callback no longer brings the app down**: that event produces no changes and the operations it managed to emit are discarded | During development, saving a half-written file is normal. This closes the pending fault-injection item |
| Watching and compiling | [live.rs](../src/live.rs) (then `vivo.rs`) checks the modification times of `app/chat.tsx` and `app/sdk.ts` every 250 ms and compiles with the same `tsc` as the build. It is enabled with `JSNIR_LIVE=1` | No new dependencies. Good enough for development |

**The tree.**
- **Explicit root.** `Tree.root` replaces the "empty tree" inference, which stopped holding once nodes could be removed.
- **The parent of each node**, to detach a removed node in O(1).
- **`remove` only for the app**, and only on nodes that existed before the batch.
- **Validation with an overlay**: what the batch created or removed overrides the tree, so a batch can remove the root and create the same ids again.
- **The layout is rebuilt in full** (`Layout::from_tree`) when the structure changes, and the columns are fully resynced.

**Two bugs found along the way.**
1. **Log order.** When applying the remount, the UI published the draft into the new field **before** confirming the batch, and the replica received a `setinput` for a node that did not yet exist in its confirmed tree. Rule: the answer to a batch goes before any UI batch that depends on it. It now answers as soon as the batch is applied.
2. **O(n²) validation.** The overlay was a list and each `create` scanned it whole: mounting 10,000 nodes took ≈ 775 ms in debug. Reloading a chat with 10,000 saved messages is exactly that case, and it is validated three times (view, confirmed, UI). With a `HashMap` it drops to **9.7 ms in debug and ≈ 1–1.7 ms in release**. A one-operation batch allocates nothing (`HashMap::new()` is lazy): direct mode stays at 4.96 µs with 1 operation.

A third detail, from egui: with the window idle it does not redraw, and the reload is picked up in a frame. With live mode on, the chat requests a frame every 100 ms.

| Verification | Result |
| --- | --- |
| `remove` test | Detaches, removes subtrees, rejects what does not exist and what comes from the UI, and replaces the root reusing ids |
| Live reload test (JS mode) | After reloading with changed code: **both messages remain** (JS signal), **the draft "en edición" remains in the new field** (UI), the layout was rebuilt (16,430 of 608×34) and the status comes out in the new format. A broken script is rejected and the app continues. A callback that throws produces no changes and the app continues |
| Real window (niri, `JSNIR_LIVE=1`) | 7 saves: **compiles in 41–45 ms and applies in 1–8 ms**. The save with a syntax error was rejected with `tsc`'s errors and the app continued |
| 21 tests, egui, Masonry, chat (50 captures with the same hash), scroll, six adapters, `tsc` | Fine |

From saving to seeing the change takes up to ≈ 250 ms of the watcher's wait, plus ≈ 45 ms of compiling and a few ms to apply.

**Limits.**
- Signals are paired by creation order. If signals with the same initial value are reordered, their values get swapped.
- Signals created dynamically (inside items) are reset.
- Values must survive JSON.
- Reloading remounts the whole app; there are no per-module patches like NSH's yet.
- Only the chat has live reload; the counter does not.
- Production (signed packages, VM limits) is pending.

## Delivery 16 · Text measurement and rasterization in Rust

JSNIR uses cosmic-text 0.14.2 with the system fonts to shape and rasterize the chat's messages and status. Taffy queries the same engine to measure the layout's `Text` nodes. A `SetText` invalidates the node's measurement. The computed rectangles still reach JS through the shared columns. The core keeps its dependencies free of UI frameworks. [Text engine](../src/text.rs) · [Measurement in Taffy](../src/layout.rs)

The empty status measures zero. When text appears, it takes its space in the window and wraps to the available width. The history keeps virtualized single-line rows. The egui adapter uploads the RGBA images as textures and reuses those of rows that stay visible. The editor, its IME, the clipboard, the button and scrolling remain the adapter's job. [egui adapter](../backends/egui/src/chat.rs)

In release, with offscreen OpenGL and llvmpipe, the 10,000-message script went through every row, with at most 18 per frame. The table shows the median of three processes per case, with 1,100 frames measured per process and alternating order.

| Executable | Rust p50 / p99 | JS p50 / p99 | Stripped size |
| --- | ---: | ---: | ---: |
| Delivery 15 | 0.457 / 0.897 ms | 0.453 / 1.246 ms | 12.05 MB |
| JSNIR text | 0.725 / 1.258 ms | 0.713 / 1.150 ms | 13.73 MB |

Drawing adds about 0.27 ms to the median with this adapter. The executable grows by 1.68 MB stripped. The core test checks that updating the status triggers a new measurement in the same window, that narrowing the width wraps the line, and that both lines rasterize at 2× scale. The 50 captures of the chat script keep parity between Rust and JS. [Scroll benchmark](../backends/egui/src/offscreen.rs) · [Measurement test](../src/chat.rs)

## Delivery 17 · A single-line editor in Rust

The editor shares the fonts and rasterization of Delivery 16. cosmic-text keeps the buffer, shapes the glyphs and maps pointer positions to text positions. JSNIR adds grapheme-based editing, horizontal scrolling to the caret, undo and redo history, and selection geometry. The editor's indices are UTF-8 bytes. The SDK still receives ranges in characters. [Editor](../src/editor.rs) · [Publishing the draft](../src/chat.rs)

IME composition is drawn as a provisional view of the draft. `Commit` applies the text to the document, and a cancellation keeps the previous document and selection. In a delivery of events that contains a `Commit`, Enter is reserved for the IME in both orders. A later Enter sends the message. A JS reload keeps the editor in Rust, including an active composition. [egui-winit events](https://docs.rs/egui-winit/0.36.2/src/egui_winit/lib.rs.html#717) · [Editor tests](../src/chat.rs)

The adapter routes focus and system events, presents the textures, gives the IME the caret rectangle and sends clipboard requests to eframe. Those exchanges were tested with synthetic events. This session got a permission error connecting to the Wayland socket, so the system IME and clipboard in a real window are pending. The vertical history still uses the adapter's virtualized scroll. [Editor adapter](../backends/egui/src/editor.rs)

All 27 core tests and the three egui tests passed. The script's 50 captures kept Rust/JS parity. The 10,000-message benchmark still went through the whole history with at most 18 rows per frame. The figures are the median of three processes per case, with 1,100 frames per process on offscreen OpenGL and llvmpipe.

| Executable | Rust p50 / p99 | JS p50 / p99 | Stripped size |
| --- | ---: | ---: | ---: |
| Delivery 16 | 0.685 / 1.134 ms | 0.684 / 1.028 ms | 13.73 MB |
| JSNIR editor | 0.671 / 0.974 ms | 0.677 / 0.989 ms | 13.90 MB |

These runs measure scrolling with the editor idle. Input-to-presentation latency while editing is still to be measured. [Scroll benchmark](../backends/egui/src/offscreen.rs)

## Delivery 18 · Shared history and an own backend with winit and wgpu

Rust keeps the history's position, computes the visible range by division and updates the scrollbar. When a message arrives, it follows the end if the user was at the bottom and keeps the offset if they were reading further up. A JS reload keeps that UI state. egui and the new backend use the same history. [History in Rust](../src/history.rs)

`jsnir-gpu` has its own manifest. winit handles the window and input, arboard the clipboard, and wgpu runs an own shader that composes rectangles and glyph textures with clipping and premultiplied alpha. Layout, editor, virtualization and glyph rasterization come from JSNIR. The new binary's dependency tree is free of UI frameworks. The SDL2 counter remained as the CPU rasterization reference (it was removed later). [Window](../backends/gpu/src/window.rs) · [Renderer](../backends/gpu/src/renderer.rs) · [Surface presentation in wgpu](https://wgpu.rs/doc/wgpu/struct.Surface.html)

The offscreen wgpu capture matches in all its 307,200 pixels between Rust and JS. It includes a sent message, the editor with a selection and the status text. It ran on OpenGL with llvmpipe. The stripped `jsnir-gpu` executable is 13.64 MB on this machine.

```sh
cargo build --release --locked --features examples --manifest-path backends/gpu/Cargo.toml --target-dir target
out=$(mktemp -d)
for mode in rust js; do
  env -u DISPLAY -u WAYLAND_DISPLAY EGL_PLATFORM=surfaceless LIBGL_ALWAYS_SOFTWARE=1 WGPU_BACKEND=gl \
    target/release/jsnir-gpu "$mode" --offscreen "$out/$mode.ppm"
done
cmp "$out/rust.ppm" "$out/js.ppm"
```

All 29 core tests passed, along with the 50 chat captures in egui and the test converting winit's UTF-8 IME ranges. The egui benchmark went through 10,000 messages in each mode with at most 17 rows per frame. In a run of 1,100 frames per mode, p50/p99 was 0.671/0.931 ms in Rust and 0.670/1.068 ms in JS. The benchmark counts the whole pass for coverage, including the warm-up where the last rows are shown. Times are taken after the warm-up. [Scroll benchmark](../backends/egui/src/offscreen.rs)

Opening a real window returned `NoCompositor` in this session. Presentation on a physical GPU and the system IME and clipboard on Wayland or X11 remain to be validated. winit 0.30's IME API provides preedit and commit. Additional protocol operations, such as deleting surrounding text, require platform integration in this backend. [IME events in winit](https://docs.rs/winit/latest/winit/event/enum.Ime.html)

## Delivery 19 · A React Bits form ported to JSNIR and to GPUI · 19 November 2026

The React Bits Stepper (the "onboarding" demo: four steps, a text field, a sliding indicator and spring transitions) was ported twice under the same [spec](../comparison/stepper/SPEC.md): in TSX on JSNIR and in Rust on GPUI 0.2.2. The reference is the original, unchanged component in Chromium. All three versions use Inter 4.1 and the same 640 × 560 window. [Full comparison](../comparison/stepper/README.md)

**What JSNIR needed.** The tree gained a general container (`box`) with rows, alignment, margins, absolute positioning, clipping and nodes that follow another node's rectangle (`follow`); stroke icons; and a `paint` with gradient, inner ring, top highlight, shadows, opacity, offset, scale, brightness and hover, press and focus variants. JS declares the transitions and Rust runs them every frame ([motion.rs](../src/motion.rs)), including the spring's `linear()` curve and layout animations (height, position and width). A generic host ([host.rs](../src/host.rs)) runs any TSX app and returns a draw list; the wgpu renderer adds blurred shadows, inner shadows and layers with group opacity, and blends in the encoded color space, as browsers do.

**Fidelity.** Pixels that differ from Chromium by more than 6 %: 0.07–0.12 % in JSNIR and 0.18–0.36 % in GPUI, across the six states. What remains is glyph antialiasing. The GPUI captures come from a 2× window scaled down, which adds resampling error.

**Finding.** cosmic-text 0.14 ignores the color's alpha when rasterizing glyphs (`TODO: blend base alpha?` in `swash.rs`): muted text came out opaque. JSNIR now applies the alpha itself; the difference from Chromium halved.

**Finding.** Taffy rounds layout to whole pixels by default, which made a text box 27 px wide for a 27.2 px word, so the last glyph wrapped onto a hidden second line ("Pla" instead of "Plan"). JSNIR now keeps fractional layout like browsers (`disable_rounding`) and snaps only text and icon bitmaps to device pixels.

**Cost.** The screen uses 390 nonblank lines of TSX in JSNIR and 1,162 nonblank lines of Rust in GPUI, plus 574 lines for GPUI's adapted text field. The stripped binaries were 15.0 MB and 19.6 MB, respectively. Peak resident memory was 121 MB for JSNIR and 109 MB for GPUI. JSNIR's measurement uses Vulkan after making OpenGL a fallback, which saved about 29 MB. [Comparison methodology](../comparison/stepper/README.md#cost)

**Pending.** Compare the animations in real windows frame by frame against the browser; measure startup and input latency in the generic host; move the chat onto the generic host so there is a single window.

## Run the egui benchmarks

Build from the repository root after setting up the pinned TypeScript compiler. The egui backend builds a small C wrapper for SDL2. The scripts use Python 3 and the counter's pixel comparisons use ImageMagick 7. [Native compiler setup](../README.md#run-the-stepper)

```sh
cargo build --release --locked --manifest-path backends/egui/Cargo.toml --target-dir target
out=$(mktemp -d "$PWD/metrics/local-egui-XXXXXX")

python3 scripts/benchmark.py "$out/counter"
python3 scripts/chat_replay.py "$out/chat"

for mode in rust js; do
    SDL_VIDEODRIVER=offscreen target/release/jsnir-egui "$mode" scroll "$out/scroll-$mode.csv"
done
```

On a compositor that supports Wayland's `wp_presentation`, run a presentation benchmark for `counter` or `chat` and analyze the saved frames and events from the same shell.

```sh
target/release/jsnir-egui js presentation counter "$out/presentation-js"
python3 scripts/presentation.py "$out/presentation-js"
```

| Counter option | Effect |
| --- | --- |
| `JSNIR_DELAY_MS` | Adds a delay in milliseconds to each JS callback. |
| `JSNIR_WAIT_US` | Sets the time budget for `Runtime::poll` to receive an answer in the current frame. The default is 1 ms. Set it to 0 to disable the wait. |
| `JSNIR_TRACE` | Writes one row per event to a new CSV, including the node, batch, version and first frame drawn after application. |
