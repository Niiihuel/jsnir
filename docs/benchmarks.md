# JSNIR Performance & Benchmarks

This document records reproducible `--release` benchmarks across every layer of the JSNIR engine
(including native **AccessKit** accessibility, native **Wayland/Win32 IME**, animatable `radius`,
`paint.exiting` snapshots, and `push_keyed` coalescing): JS↔Rust bridge, CPU frame pipeline,
reactive TSX SDK, `wgpu` GPU renderer, interchangeable JS engines (**QuickJS-ng** default vs
**QuickJS AOT `.jsbc`** vs **V8 `jsnir-v8`** opt-in), and a side-by-side comparison against
**Electron** and native **GPUI**.

## Test environment

- **Machine**: AMD Ryzen + AMD Radeon 780M Graphics (`RADV PHOENIX`, Vulkan, Wayland)
- **Toolchain**: `rustc 1.99.1`, `--release` builds
- **Measurement hygiene**: every latency table reports `p50` / `p95` / `p99` after explicit warmup.
  Headless engine binary size and process RSS (`VmRSS` / `VmHWM` from `/proc/self/status`) were
  measured using isolated `--release` (`strip = true, lto = "thin"`) probe executables linking
  **only** QuickJS-ng or **only** V8 (zero cross-contamination).
- **Display pacing**: animations and transitions run natively on the UI thread paced to the
  monitor's refresh rate (vsync: 60 / 120 / 144 / 240+ Hz — never capped to a fixed Hz).

---

## 1. JS↔Rust Bridge & Startup (`bridge_bench`)

Reproduce:

```sh
cargo run --release --features stepper --example bridge_bench
```

### Binary Command Buffer (`Uint32Array`/`Float32Array`) vs legacy JSON `batch`

| Batch size | JSON p50 | Binary p50 | JSON p99 | Binary p99 | Speedup |
|---|---:|---:|---:|---:|---:|
| 2 ops | `11.55 µs` | **`5.85 µs`** | `22.83 µs` | **`8.79 µs`** | **1.97×** |
| 20 ops | `59.06 µs` | **`17.48 µs`** | `82.56 µs` | **`47.33 µs`** | **3.38×** |
| 100 ops | `269.31 µs` | **`70.97 µs`** | `452.83 µs` | **`113.13 µs`** | **3.79×** |
| 200 ops | `575.00 µs` | **`134.45 µs`** | `1036.41 µs` | **`217.15 µs`** | **4.28×** |

### High-Refresh Drag-Resize Pipeline (Zero-JSON `jsnirDrag` + Binary `OP_SET_STYLE`)

| Stage | Legacy JSON p50 (p99) | Zero-JSON + Binary p50 (p99) | Speedup |
|---|---:|---:|---:|
| Drag move bridge only | `9.78 µs` (`23.30 µs`) | **`5.24 µs`** (`9.17 µs`) | **1.87×** |
| Drag move + Taffy `frame()` | `11.95 µs` (`29.34 µs`) | **`7.48 µs`** (`22.30 µs`) | **1.60×** |

### `Host::poll()` Bounded Wait (Same-Frame Landing Rate)

| Budget | Landed in 1st `poll()` | `poll()` p50 | p95 | p99 |
|---|---:|---:|---:|---:|
| `0 µs` (non-blocking) | `0 / 500` (`0.0%`) | `0.07 µs` | `0.09 µs` | `0.10 µs` |
| `800 µs` (default) | **`500 / 500` (`100.0%`)** | **`5.55 µs`** | `7.30 µs` | `11.98 µs` |

### Subtree Swap (`Create` N Interactive Rows: JSON vs Binary + String Pool)

| Subtree | JSON p50 | Binary p50 | JSON p99 | Binary p99 | Speedup |
|---|---:|---:|---:|---:|---:|
| 20 rows (60 nodes) | `314.84 µs` | **`107.39 µs`** | `503.95 µs` | **`194.87 µs`** | **2.93×** |
| 100 rows (300 nodes) | `1466.18 µs` | **`530.16 µs`** | `1948.28 µs` | **`835.18 µs`** | **2.77×** |
| 200 rows (600 nodes) | `3103.70 µs` | **`1057.78 µs`** | `4012.25 µs` | **`1702.68 µs`** | **2.93×** |

### Cold Startup: JS Source (`.js`) vs Precompiled QuickJS Bytecode (`.jsbc` AOT)

| App | JS size | `.jsbc` size | JS source p50 | `.jsbc` AOT p50 | Speedup |
|---|---:|---:|---:|---:|---:|
| `stepper` | `102.6 kB` | `203.0 kB` | `3.38 ms` | **`1.37 ms`** | **2.46×** |
| `overlay` | `92.7 kB` | `179.7 kB` | `2.81 ms` | **`1.01 ms`** | **2.78×** |
| `composer` | `90.2 kB` | `174.7 kB` | `2.60 ms` | **`0.55 ms`** | **4.69×** |
| `scrollchat` | `91.2 kB` | `177.2 kB` | `2.86 ms` | **`1.19 ms`** | **2.41×** |

---

## 2. CPU Frame Pipeline (`frame_bench`)

Reproduce:

```sh
cargo run --release --features stepper --example frame_bench
```

Includes text measurement caching, dirty layout short-circuiting, zero-clone tree walking,
`Motion` fast path (including `Property::Radius`), zero-cost `paint.exiting` skip on non-exiting
nodes, lazy AccessKit activation (`0 µs` when inactive), and safe clip culling.

### A. 2-Pane Feed (`1,007` nodes, 200 rows inside a scroll container)

| Scenario | p50 | p95 | p99 | Drawn items |
|---|---:|---:|---:|---:|
| Steady-state `frame()` | **`7.91 µs`** | `11.64 µs` | `14.07 µs` | `50` |
| Pointer hover sweep + `frame()` | **`8.81 µs`** | `9.28 µs` | `13.54 µs` | `63` |
| Wheel scroll + `frame()` | **`8.45 µs`** | `12.46 µs` | `15.45 µs` | `51` |
| Single `SetText` click + `frame()` | **`43.52 µs`** | `66.87 µs` | `106.84 µs` | `51` |
| Sidebar drag-resize + `frame()` | **`784.96 µs`** | `1243.77 µs` | `1439.97 µs` | `51` |
| Wrapped-200 multi-line drag-resize + `frame()` | **`2793.16 µs`** | `4128.08 µs` | `4990.39 µs` | `39` |

### B. Dense 100%-Visible 2D Grid (`1,231` nodes, zero clip culling)

| Scenario | p50 | p95 | p99 | Drawn items |
|---|---:|---:|---:|---:|
| All-visible steady `frame()` | **`96.03 µs`** | `110.24 µs` | `145.22 µs` | `1,201` |
| All-visible hover sweep + `frame()` | **`97.72 µs`** | `126.33 µs` | `151.85 µs` | `1,201` |

---

## 3. End-to-End Reactive SDK (`sdk_bench`)

Reproduce:

```sh
cargo run --release --features stepper --example sdk_bench
```

Runs the real [`app/sdk.ts`](../app/sdk.ts) on the default QuickJS-ng backend and verifies **`JSON calls = 0`** across every workload:

| Workload | QuickJS-ng p50 | p95 | p99 | JSON calls | Binary commits |
|---|---:|---:|---:|---:|---:|
| `For` swap 100 rich rows (`transitions` + `shadows`) | `3524.71 µs` | `4872.89 µs` | `5303.79 µs` | **`0`** | `60` |
| `For` swap 100 plain rows (`Row` + `Column`) | `3205.78 µs` | `4354.46 µs` | `4600.43 µs` | **`0`** | `60` |
| 50 rich `Text` `setSpans` + `setText` updates | `281.04 µs` | `464.41 µs` | `519.48 µs` | **`0`** | `120` |
| 50 reactive `SetStyle` + `SetPaint(trans)` + `SetField` | `298.74 µs` | `450.03 µs` | `503.99 µs` | **`0`** | `120` |
| `Popover` toggle (`anchor` + `shadows` + `starting`) | **`27.98 µs`** | `33.42 µs` | `52.69 µs` | **`0`** | `200` |
| Real `STEPPER` cold mount + settle | `4112.02 µs` | `4728.11 µs` | `4791.18 µs` | **`0`** | — |

---

## 4. Native `wgpu` GPU Renderer (`gpu_bench`)

Reproduce:

```sh
cargo run --release --manifest-path backends/gpu/Cargo.toml --features stepper --example gpu_bench
```

Uses a persistent `4096×2048` shelf texture atlas (downlevel-clamped to `max_texture_dimension_2d`)
with a reserved `4×4` white block and `1 px` gutters, instanced draw-call batching, and an `O(1)`
steady-state `View` cache with tight styled-text rasterization.

| Workload (AMD Radeon 780M, Vulkan) | `view.draws()` p50 | `renderer.draw()` CPU p50 | GPU total p50 | Draw calls |
|---|---:|---:|---:|---:|
| Steady 200-row feed (`1280×720`, 72 items) | **`5.95 µs`** | **`37.83 µs`** | **`632.46 µs`** | **1** |
| Dense 2D grid (300 cards, **1,501 items**, 0 culling) | **`99.57 µs`** | **`81.70 µs`** | **`797.69 µs`** | **1** |
| Scroll DOWN through new rows (`64 px`/step) | `277.88 µs` | — | `1006.95 µs` | **1** |
| Scroll UP revisiting cached rows (`64 px`/step) | **`6.62 µs`** | — | **`643.18 µs`** | **1** |
| 100 visible rich-span `Text` nodes (4 spans each, 791 items) | **`47.96 µs`** | — | **`519.38 µs`** | **1** |
| Layered + shadowed cards + opacity overlay (156 items) | **`9.24 µs`** | **`65.82 µs`** | **`1083.37 µs`** | **3** |

---

## 5. Pluggable JS Engines: QuickJS-ng vs QuickJS AOT vs V8 JIT (`engine_bench`)

Reproduce:

```sh
cargo run --release --manifest-path crates/engine-v8/Cargo.toml --example engine_bench
```

All SDK workloads run the real [`app/sdk.ts`](../app/sdk.ts) over the 100% binary command protocol
(`JSON calls = 0`). `jsnir-v8` adds shared `v8::BackingStore` zero-copy memory, V8 Fast API calls
(`jsnir.set`, `jsnir.commit`), and in-place `Isolate` recycling on hot reload.

### UI SDK & Bridge Workloads

| Scenario | QuickJS-ng p50 (p95) | V8 (`jsnir-v8`) p50 (p95) | Speedup |
|---|---:|---:|---:|
| `For` swap 100 rich rows (`transitions` + `shadows`) | `3498.5 µs` (`4427.0`) | **`1101.6 µs`** (`1496.1`) | **3.18×** |
| `For` swap 100 plain rows (`Row` + `Column`) | `3291.0 µs` (`3609.4`) | **`940.0 µs`** (`1243.3`) | **3.50×** |
| 50 rich `Text` `setSpans` + `setText` updates | `284.9 µs` (`357.3`) | **`63.5 µs`** (`93.8`) | **4.48×** |
| 50 reactive `SetStyle` + `SetPaint(trans)` + `SetField` | `304.9 µs` (`331.5`) | **`58.3 µs`** (`101.3`) | **5.23×** |
| `Popover` toggle (`anchor` + `shadows` + `starting`) | `28.7 µs` (`31.0`) | **`14.4 µs`** (`21.8`) | **1.99×** |
| High-refresh drag move (`jsnirDrag` + binary cmd) | `5.8 µs` (`7.2`) | **`4.9 µs`** (`6.3`) | **1.19×** |
| Direct engine: 200-op binary `commit` | `80.9 µs` (`100.8`) | **`12.2 µs`** (`14.6`) | **6.61×** |
| Direct engine: 1,000× `jsnir.set` (V8 Fast API) | `138.1 µs` (`170.2`) | **`15.1 µs`** (`15.6`) | **9.14×** |

### Heavy JS Compute Workloads (TurboFan / Maglev JIT)

| Workload | QuickJS-ng p50 (p95) | V8 (`jsnir-v8`) p50 (p95) | Speedup |
|---|---:|---:|---:|
| Filter + sort 20,000 items → emit top 50 rows | `2739.5 µs` (`3760.5`) | **`295.5 µs`** (`421.8`) | **9.27×** |
| Markdown inline tokenizer (200 messages → spans) | `1188.9 µs` (`1229.7`) | **`33.2 µs`** (`55.6`) | **35.81×** |
| Spreadsheet / grid recalculation (5,000 cells + diff) | `1089.1 µs` (`1204.2`) | **`37.3 µs`** (`44.4`) | **29.20×** |

### System Cost Trade-off (Isolated `--release`, `strip = true, lto = "thin"`)

| Metric | QuickJS-ng (default) | QuickJS `.jsbc` AOT | V8 (`jsnir-v8` opt-in) |
|---|---:|---:|---:|
| Stripped headless binary (`STEPPER` + `cosmic-text`) | **`6.41 MiB`** (`6.72 MB`) | **`6.41 MiB`** | `50.62 MiB` (`53.08 MB`, `+44.21 MiB`) |
| Isolated process RSS (`VmRSS` after mounting `STEPPER`) | **`9.03 MiB`** | **`~9.0 MiB`** | `24.49 MiB` (`+15.46 MiB`) |
| Live VM heap (`engine.memory_usage()`) | **`477.2 KiB`** | — | `891.0 KiB` (`1.75 MiB` total) |
| First-process cold start (`main` → settled frame) | **`4.64 ms`** | — | `7.78 ms` (includes `V8::initialize`) |
| Warm-process `STEPPER` mount + settle (p50) | `3.97 ms` | **`1.42 ms`** | `5.22 ms` |
| Hot reload SDK preserving signals (p50) | `2.21 ms` (fresh VM) | — | **`0.47 ms`** (in-place `Context`, **4.66× faster**) |

---

## 6. Architecture Comparison: Electron vs GPUI vs JSNIR

Windowed `Stepper` numbers for GPUI and JSNIR come from the same-machine comparison in
[`comparison/stepper/README.md`](../comparison/stepper/README.md).

| Dimension | Electron (Chromium + Node) | GPUI (native Rust) | JSNIR + QuickJS-ng (default) | JSNIR + QuickJS `.jsbc` AOT | JSNIR + V8 JIT (`jsnir-v8`) |
|---|---|---|---|---|---|
| Process model | 4–6+ processes (DOM/Blink/Skia/V8/Node) | 1 process (Rust) | 1 process (Rust UI + QuickJS off-thread) | 1 process (Rust UI + QuickJS off-thread) | 1 process (Rust UI + V8 off-thread) |
| Stripped release binary (windowed) | ~180–260 MB | `19.6 MB` | **`19.97 MB`** *(wgpu + AccessKit + IME + 20 demos)* ¹ | **`19.97 MB`** ¹ | `~64 MB` *(+44.2 MiB V8)* |
| Windowed Peak RSS (Vulkan/Wayland) | ~200–350+ MB | **`109 MB`** | `121 MB` *(~100+ MB is Vulkan/wgpu)* | `121 MB` | `~136 MB` (`121 + 15.5 MiB`) |
| Cold start to first frame | ~400–1,500 ms | ~15–40 ms | **`3.97 ms`** (JS) | **`1.42 ms`** (`.jsbc`) | **`5.22 ms`** *(7.78 ms incl. `V8::initialize`)* |
| Dev hot reload | ~50–300 ms (HMR + DOM reconcile) | `5–74 s` (Rust rebuild) | **`2.21 ms`** (Oxc + signal preservation) | — | **`0.47 ms`** (Oxc + in-place `Context`) |
| Native accessibility | Chromium a11y (heavyweight) | Partial / custom | **AccessKit** (AT-SPI / UIA / NSAccessibility, 0 µs idle) | **AccessKit** | **AccessKit** |
| Native IME | Chromium IME | Platform custom | **Wayland v3 + Win32 IMM32/TSF + X11/macOS** | **Wayland v3 + Win32 + X11/macOS** | **Wayland v3 + Win32 + X11/macOS** |
| Same Stepper screen source | ~350–400 (JSX + CSS) | `1,736` lines Rust | **`390` lines TSX** | **`390` lines TSX** | **`390` lines TSX** |
| Pixel diff vs Chromium reference | `0%` (reference) | `0.18% – 0.36%` | **`0.07% – 0.12%`** | **`0.07% – 0.12%`** | **`0.07% – 0.12%`** |
| 500 ms JS / app logic stall | Freezes scroll, typing & paint | Freezes UI thread | **0 dropped frames** (off UI thread) | **0 dropped frames** | **0 dropped frames** |

> ¹ Pre-AccessKit binary was `15.0 MB`; adding full Linux AT-SPI (`zbus`) + Wayland `zwp_text_input_v3` IME brings the stripped `--features stepper` binary (which also embeds 20 demo/test apps) to `19.97 MB` without LTO.

### Choosing the right JSNIR backend

- **Default (`QuickJS-ng` + `--bytecode` AOT)**: best for desktop utilities, launchers, system
  tools, and general apps targeting minimal binary size (`6.4–20 MB`), minimal RAM (`9 MiB` engine
  RSS), and sub-2 ms cold starts.
- **Opt-in (`jsnir-v8` / `--features engine-v8`)**: best for large, computation-heavy desktop apps
  (rich chat clients, terminal emulators, IDEs, Markdown/AST editors, large tables/spreadsheets)
  where `3.2–35.8×` faster JS execution and `0.47 ms` in-place hot reloads justify `+44 MiB` binary
  and `+15.5 MiB` RSS — while remaining dramatically smaller, leaner, and faster than Electron.
