# JSNIR Performance & Benchmarks

This document records reproducible `--release` benchmarks across every layer of the JSNIR engine:
JS↔Rust bridge, CPU frame pipeline, reactive TSX SDK, `wgpu` GPU renderer, interchangeable JS
engines (**QuickJS-ng** default vs **QuickJS AOT `.jsbc`** vs **V8 `jsnir-v8`** opt-in), and a
side-by-side comparison against **Electron** and native **GPUI**.

## Test environment

- **Machine**: AMD Ryzen + AMD Radeon 780M Graphics (`RADV PHOENIX`, Vulkan, Wayland)
- **Toolchain**: `rustc 1.99.1`, `--release` builds
- **Measurement hygiene**: every latency table reports `p50` / `p95` / `p99` after explicit warmup.
  Binary size and process RSS (`VmRSS` / `VmHWM` from `/proc/self/status`) were measured using
  isolated `--release` (`strip = true, lto = "thin"`) probe executables linking **only** QuickJS-ng
  or **only** V8 (zero cross-contamination).

---

## 1. JS↔Rust Bridge & Startup (`bridge_bench`)

Reproduce:

```sh
cargo run --release --features stepper --example bridge_bench
```

### Binary Command Buffer (`Uint32Array`/`Float32Array`) vs legacy JSON `batch`

| Batch size | JSON p50 | Binary p50 | JSON p99 | Binary p99 | Speedup |
|---|---:|---:|---:|---:|---:|
| 2 ops | `11.59 µs` | **`6.15 µs`** | `17.79 µs` | **`10.82 µs`** | **1.88×** |
| 20 ops | `62.49 µs` | **`18.82 µs`** | `110.39 µs` | **`23.04 µs`** | **3.32×** |
| 100 ops | `276.17 µs` | **`71.56 µs`** | `479.35 µs` | **`99.75 µs`** | **3.86×** |
| 200 ops | `562.95 µs` | **`120.99 µs`** | `762.32 µs` | **`176.07 µs`** | **4.65×** |

### 120 Hz Drag-Resize Pipeline (Zero-JSON `jsnirDrag` + Binary `OP_SET_STYLE`)

| Stage | Legacy JSON p50 (p99) | Zero-JSON + Binary p50 (p99) | Speedup |
|---|---:|---:|---:|
| Drag move bridge only | `9.79 µs` (`14.50 µs`) | **`5.69 µs`** (`9.06 µs`) | **1.72×** |
| Drag move + Taffy `frame()` | `12.53 µs` (`34.07 µs`) | **`7.61 µs`** (`16.17 µs`) | **1.65×** |

### `Host::poll()` Bounded Wait (Same-Frame Landing Rate)

| Budget | Landed in 1st `poll()` | `poll()` p50 | p95 | p99 |
|---|---:|---:|---:|---:|
| `0 µs` (non-blocking) | `0 / 500` (`0.0%`) | `0.08 µs` | `0.09 µs` | `0.11 µs` |
| `800 µs` (default) | **`500 / 500` (`100.0%`)** | **`5.84 µs`** | `7.02 µs` | `9.53 µs` |

### Subtree Swap (`Create` N Interactive Rows: JSON vs Binary + String Pool)

| Subtree | JSON p50 | Binary p50 | JSON p99 | Binary p99 | Speedup |
|---|---:|---:|---:|---:|---:|
| 20 rows (60 nodes) | `325.43 µs` | **`106.54 µs`** | `477.43 µs` | **`140.08 µs`** | **3.05×** |
| 100 rows (300 nodes) | `1545.16 µs` | **`538.86 µs`** | `2777.66 µs` | **`765.61 µs`** | **2.87×** |
| 200 rows (600 nodes) | `3132.94 µs` | **`1048.32 µs`** | `4269.80 µs` | **`1396.21 µs`** | **2.99×** |

### Cold Startup: JS Source (`.js`) vs Precompiled QuickJS Bytecode (`.jsbc` AOT)

| App | JS size | `.jsbc` size | JS source p50 | `.jsbc` AOT p50 | Speedup |
|---|---:|---:|---:|---:|---:|
| `stepper` | `98.9 kB` | `197.8 kB` | `3.27 ms` | **`1.20 ms`** | **2.73×** |
| `overlay` | `89.0 kB` | `174.5 kB` | `2.67 ms` | **`0.91 ms`** | **2.94×** |
| `composer` | `86.5 kB` | `169.5 kB` | `2.41 ms` | **`0.51 ms`** | **4.77×** |
| `scrollchat` | `87.5 kB` | `171.9 kB` | `2.71 ms` | **`1.03 ms`** | **2.62×** |

---

## 2. CPU Frame Pipeline (`frame_bench`)

Reproduce:

```sh
cargo run --release --features stepper --example frame_bench
```

Includes text measurement caching, dirty layout short-circuiting, zero-clone tree walking,
`Motion` fast path, and safe clip culling (preserving casting shadows and active transitions).

### A. 2-Pane Feed (`1,007` nodes, 200 rows inside a scroll container)

| Scenario | p50 | p95 | p99 | Drawn items |
|---|---:|---:|---:|---:|
| Steady-state `frame()` | **`7.40 µs`** | `7.74 µs` | `9.94 µs` | `50` |
| Pointer hover sweep + `frame()` | **`8.38 µs`** | `8.53 µs` | `9.16 µs` | `63` |
| Wheel scroll + `frame()` | **`7.87 µs`** | `8.66 µs` | `9.78 µs` | `51` |
| Single `SetText` click + `frame()` | **`41.45 µs`** | `48.10 µs` | `64.09 µs` | `51` |
| Sidebar drag-resize + `frame()` | **`741.43 µs`** | `1009.13 µs` | `1158.52 µs` | `51` |
| Wrapped-200 multi-line drag-resize + `frame()` | **`2530.87 µs`** | `3108.66 µs` | `3386.05 µs` | `39` |

### B. Dense 100%-Visible 2D Grid (`1,231` nodes, zero clip culling)

| Scenario | p50 | p95 | p99 | Drawn items |
|---|---:|---:|---:|---:|
| All-visible steady `frame()` | **`91.72 µs`** | `97.97 µs` | `104.52 µs` | `1,201` |
| All-visible hover sweep + `frame()` | **`93.69 µs`** | `120.11 µs` | `158.51 µs` | `1,201` |

---

## 3. Native `wgpu` GPU Renderer (`gpu_bench`)

Reproduce:

```sh
cargo run --release --manifest-path backends/gpu/Cargo.toml --features stepper --example gpu_bench
```

Uses a persistent `4096×2048` shelf texture atlas (downlevel-clamped to `max_texture_dimension_2d`)
with a reserved `4×4` white block and `1 px` gutters, instanced draw-call batching, and an `O(1)`
steady-state `View` cache with tight styled-text rasterization.

| Workload (AMD Radeon 780M, Vulkan) | `view.draws()` p50 | `renderer.draw()` CPU p50 | GPU total p50 | Draw calls |
|---|---:|---:|---:|---:|
| Steady 200-row feed (`1280×720`, 72 items) | **`6.57 µs`** | **`43.34 µs`** | **`533.21 µs`** | **1** |
| Dense 2D grid (300 cards, **1,501 items**, 0 culling) | **`134.92 µs`** | **`115.68 µs`** | **`768.74 µs`** | **1** |
| Scroll DOWN through new rows (`64 px`/step) | `334.92 µs` | — | `1073.25 µs` | **1** |
| Scroll UP revisiting cached rows (`64 px`/step) | **`6.99 µs`** | — | **`481.24 µs`** | **1** |
| 100 visible rich-span `Text` nodes (4 spans each, 791 items) | **`53.67 µs`** | — | **`480.51 µs`** | **1** |
| Layered + shadowed cards + opacity overlay (156 items) | **`9.07 µs`** | **`74.18 µs`** | **`851.08 µs`** | **3** |

---

## 4. Pluggable JS Engines: QuickJS-ng vs QuickJS AOT vs V8 (`engine_bench`)

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
| `For` swap 100 rich rows (`transitions` + `shadows`) | `3553.2 µs` (`4528.7`) | **`1012.2 µs`** (`1368.0`) | **3.51×** |
| `For` swap 100 plain rows (`Row` + `Column`) | `3142.1 µs` (`3490.8`) | **`823.7 µs`** (`1051.2`) | **3.81×** |
| 50 rich `Text` `setSpans` + `setText` updates | `280.9 µs` (`346.4`) | **`59.4 µs`** (`88.3`) | **4.73×** |
| 50 reactive `SetStyle` + `SetPaint(trans)` + `SetField` | `301.2 µs` (`383.1`) | **`56.4 µs`** (`100.3`) | **5.34×** |
| `Popover` toggle (`anchor` + `shadows` + `starting`) | `30.4 µs` (`48.6`) | **`19.8 µs`** (`25.9`) | **1.54×** |
| 120 Hz drag move (`jsnirDrag` + binary cmd) | `5.9 µs` (`7.9`) | **`5.2 µs`** (`7.1`) | **1.15×** |
| Direct engine: 200-op binary `commit` | `79.5 µs` (`87.7`) | **`12.8 µs`** (`13.3`) | **6.23×** |
| Direct engine: 1,000× `jsnir.set` (V8 Fast API) | `134.4 µs` (`140.1`) | **`15.2 µs`** (`15.8`) | **8.86×** |

### Heavy JS Compute Workloads

| Workload | QuickJS-ng p50 (p95) | V8 (`jsnir-v8`) p50 (p95) | Speedup |
|---|---:|---:|---:|
| Filter + sort 20,000 items → emit top 50 rows | `2470.6 µs` (`2920.4`) | **`310.5 µs`** (`411.4`) | **7.96×** |
| Markdown inline tokenizer (200 messages → spans) | `1178.8 µs` (`1275.4`) | **`33.3 µs`** (`46.5`) | **35.44×** |
| Spreadsheet / grid recalculation (5,000 cells + diff) | `1065.5 µs` (`1080.2`) | **`36.5 µs`** (`43.9`) | **29.22×** |

### System Cost Trade-off (Isolated `--release`, `strip = true, lto = "thin"`)

| Metric | QuickJS-ng (default) | QuickJS `.jsbc` AOT | V8 (`jsnir-v8` opt-in) |
|---|---:|---:|---:|
| Stripped headless binary (`STEPPER` + `cosmic-text`) | **`6.41 MiB`** (`6.72 MB`) | **`6.41 MiB`** | `50.62 MiB` (`53.08 MB`, `+44.21 MiB`) |
| Isolated process RSS (`VmRSS` after mounting `STEPPER`) | **`9.03 MiB`** | **`~9.0 MiB`** | `24.49 MiB` (`+15.46 MiB`) |
| Live VM heap (`engine.memory_usage()`) | **`468.1 KiB`** | — | `870.4 KiB` (`1.75 MiB` total) |
| First-process cold start (`main` → settled frame) | **`4.64 ms`** | — | `7.78 ms` (includes `V8::initialize`) |
| Warm-process `STEPPER` mount + settle (p50) | `4.04 ms` | **`1.37 ms`** | `5.07 ms` |
| Hot reload SDK preserving signals (p50) | `2.09 ms` (fresh VM) | — | **`0.41 ms`** (in-place `Context`, **5.10× faster**) |

---

## 5. Architecture Comparison: Electron vs GPUI vs JSNIR

Windowed `Stepper` numbers for GPUI and JSNIR come from the same-machine comparison in
[`comparison/stepper/README.md`](../comparison/stepper/README.md).

| Dimension | Electron (Chromium + Node) | GPUI (native Rust) | JSNIR + QuickJS-ng (default) | JSNIR + V8 (`jsnir-v8`) |
|---|---|---|---|---|
| Process model | 4–6+ processes (DOM/Blink/Skia/V8/Node) | 1 process (Rust) | 1 process (Rust UI + QuickJS off-thread) | 1 process (Rust UI + V8 off-thread) |
| Stripped release binary | ~180–260 MB | `19.6 MB` | **`15.0 MB`** (with `wgpu` + examples) | `~59 MB` (with `wgpu`) |
| Windowed Peak RSS (Vulkan/Wayland) | ~200–350+ MB | **`109 MB`** | `121 MB` (~100+ MB is Vulkan/wgpu) | `~136 MB` (`121 + 15.5 MiB`) |
| Cold start to first frame | ~400–1,500 ms | ~15–40 ms | **`1.37 ms`** AOT / **`4.64 ms`** JS | **`7.78 ms`** |
| Dev hot reload | ~50–300 ms (HMR + DOM reconcile) | `5–74 s` (Rust rebuild) | **`2.09 ms`** (Oxc + signal preservation) | **`0.41 ms`** (Oxc + in-place `Context`) |
| Same Stepper screen source | ~350–400 (JSX + CSS) | `1,736` lines Rust | **`390` lines TSX** | **`390` lines TSX** |
| Pixel diff vs Chromium reference | `0%` (reference) | `0.18% – 0.36%` | **`0.07% – 0.12%`** | **`0.07% – 0.12%`** |
| 500 ms JS / app logic stall | Freezes scroll, typing & paint | Freezes UI thread | **0 dropped frames** (off UI thread) | **0 dropped frames** (off UI thread) |

### Choosing the right JSNIR backend

- **Default (`QuickJS-ng` + `--bytecode` AOT)**: best for desktop utilities, launchers, system
  tools, and general apps targeting minimal binary size (`6.4–15 MB`), minimal RAM (`9 MiB` engine
  RSS), and sub-2 ms cold starts.
- **Opt-in (`jsnir-v8` / `--features engine-v8`)**: best for large, computation-heavy desktop apps
  (rich chat clients, IDEs, Markdown/AST editors, large tables/spreadsheets) where `3.5–35×` faster
  JS execution and `0.41 ms` in-place hot reloads justify `+44 MiB` binary and `+15.5 MiB` RSS —
  while remaining dramatically smaller, leaner, and faster than Electron.
