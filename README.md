<div align="center">

# JSNIR · JavaScript Native In Rust

**A standalone native desktop UI engine written in Rust for TypeScript/TSX applications — with no WebView, no DOM, and no Chromium.**

<p>
  <a href="LICENSE"><img src="https://img.shields.io/badge/License-MIT-blue.svg?style=for-the-badge" alt="MIT License"></a>
  <a href="https://www.rust-lang.org"><img src="https://img.shields.io/badge/Rust-Edition_2024-orange.svg?style=for-the-badge&logo=rust&logoColor=white" alt="Rust Edition 2024"></a>
  <a href="app/sdk.ts"><img src="https://img.shields.io/badge/TypeScript-TSX_SDK-3178C6.svg?style=for-the-badge&logo=typescript&logoColor=white" alt="TypeScript TSX SDK"></a>
  <a href="docs/benchmarks.md"><img src="https://img.shields.io/badge/Bridge-100%25_Binary_(0_JSON)-2ea44f.svg?style=for-the-badge" alt="100% Binary Bridge"></a>
</p>

<p>
  <img src="https://img.shields.io/badge/JS_Engines-QuickJS--ng_%7C_V8_JIT-8957e5.svg?style=flat-square" alt="QuickJS-ng and V8 JIT">
  <img src="https://img.shields.io/badge/Renderer-wgpu_(Vulkan_%7C_DX12_%7C_Metal)-007ec6.svg?style=flat-square" alt="wgpu Renderer">
  <img src="https://img.shields.io/badge/Accessibility-AccessKit_(AT--SPI_%7C_UIA_%7C_NSAccessibility)-0e8a16.svg?style=flat-square" alt="AccessKit Native Accessibility">
  <img src="https://img.shields.io/badge/IME-Wayland_v3_%7C_Win32_TSF_%7C_X11_%7C_macOS-1f6feb.svg?style=flat-square" alt="Native Multiplatform IME">
  <a href="#support--donations"><img src="https://img.shields.io/badge/Sponsor-Support_JSNIR-ea4aaa.svg?style=flat-square&logo=githubsponsors&logoColor=white" alt="Sponsor JSNIR"></a>
</p>

[Quickstart](#quickstart-jsnir-cli) ·
[Benchmarks](docs/benchmarks.md) ·
[Stepper Comparison](comparison/stepper/README.md) ·
[Roadmap](docs/roadmap.md) ·
[Contributing (`AGENTS.md`)](AGENTS.md) ·
[Support](#support--donations)

</div>

---

Application logic runs on a dedicated off-UI thread inside a pluggable JavaScript VM (**QuickJS-ng**
by default for minimal footprint, or opt-in **V8** via `jsnir-v8` for TurboFan JIT workloads).
Rust owns the window, native IME, native accessibility (AccessKit), text editing, Taffy flexbox layout,
CSS-like transitions and keyframe animations, persistent GPU texture atlas, and `wgpu` rendering — so
heavy JS work never freezes scrolling, typing, or animations paced natively at your monitor's refresh
rate (vsync: 60 / 120 / 144 / 240+ Hz).

![The React Bits Stepper in Chromium and in JSNIR](docs/images/stepper-side-by-side.png)

---

## Performance at a Glance · Electron vs GPUI vs JSNIR

Measured in `--release` on AMD Ryzen + Radeon 780M (Vulkan/Wayland, `rustc 1.99.1`). Full methodology
and reproducible commands live in **[docs/benchmarks.md](docs/benchmarks.md)** and
**[comparison/stepper/README.md](comparison/stepper/README.md)**.

| Metric | Electron (Chromium + Node) | GPUI (Native Rust) | JSNIR · QuickJS-ng (default) | JSNIR · QuickJS `.jsbc` AOT | JSNIR · V8 JIT (`jsnir-v8`) |
| --- | --- | --- | --- | --- | --- |
| **Process model** | 4–6+ OS processes | 1 process (UI thread) | 1 process (JS off UI thread) | 1 process (JS off UI thread) | 1 process (JS off UI thread) |
| **Stripped binary (windowed)** | ~180–260 MB | `19.6 MB` | **`19.97 MB`** *(wgpu + AccessKit + IME + 20 demos)* ¹ | **`19.97 MB`** ¹ | `~64 MB` *(+44.2 MiB V8)* |
| **Stripped headless engine** | N/A | N/A | **`6.41 MiB`** | **`6.41 MiB`** | `50.62 MiB` |
| **Engine-only RSS (`VmRSS`)** | ~150+ MB | N/A | **`9.03 MiB`** (`477 KiB` JS heap) | **`~9.0 MiB`** | `24.49 MiB` (`891 KiB` JS heap) |
| **Cold start to first frame** | ~400–1,500 ms | ~15–40 ms | **`3.97 ms`** (JS) | **`1.42 ms`** (`.jsbc`) | **`5.22 ms`** *(7.78 ms incl. `V8::initialize`)* |
| **Dev hot reload** | ~50–300 ms | `5–74 s` (Rust rebuild) | **`2.21 ms`** (Oxc + signal preservation) | — | **`0.47 ms`** (in-place V8 `Context`) |
| **Steady CPU frame (`1,007` nodes)** | ~1–4 ms | Sub-ms | **`7.91 µs`** p50 | **`7.91 µs`** p50 | **`7.91 µs`** p50 |
| **Dense GPU frame (`1,501` items)** | Multi-pass Skia | Native Metal/Vulkan | **`1 draw call` / `798 µs`** | **`1 draw call` / `798 µs`** | **`1 draw call` / `798 µs`** |
| **Heavy JS compute (200-msg parser)** | Fast (V8) | Native Rust | `1,189 µs` | `1,189 µs` | **`33.2 µs`** (**35.8×** faster) |
| **Same Stepper screen code** | ~350–400 (JSX + CSS) | `1,736` lines Rust | **`390` lines TSX** | **`390` lines TSX** | **`390` lines TSX** |
| **Pixel diff vs Chromium ref** | `0%` (reference) | `0.18% – 0.36%` | **`0.07% – 0.12%`** | **`0.07% – 0.12%`** | **`0.07% – 0.12%`** |
| **500 ms app logic stall** | Freezes scroll, input & paint | Freezes UI thread | **0 dropped frames** | **0 dropped frames** | **0 dropped frames** |

> ¹ Pre-AccessKit binary was `15.0 MB`; adding full Linux AT-SPI (`zbus`) + Wayland `zwp_text_input_v3` IME brings the stripped `--features stepper` binary (which bundles 20 embedded example/test screens) to `19.97 MB`.

---

## Key Architecture Highlights

- **Single-process, off-UI-thread JS**: zero DOM/Blink/Node.js overhead; JS stalls never drop monitor-refresh frames or freeze typing and scrolling.
- **100% Binary Command Protocol**: zero-JSON typed events (`jsnirDrag`, `jsnirScroll`, `jsnirContextMenu`), a shared `Uint32Array`/`Float32Array` command buffer + string pool (`4.28×` faster than JSON batching, `JSON calls = 0` across the entire SDK), and keyed event coalescing (`Host::push_keyed` / `Pusher::push_keyed`).
- **Pluggable `JsEngine` / `JsEngineFactory` trait**:
  - **QuickJS-ng (default)**: `6.41 MiB` stripped headless binary, `9.03 MiB` RSS, `.jsbc` AOT bytecode (`0.55–1.37 ms` cold startup), `VmLimits` (memory ceiling + interrupt timeout).
  - **V8 opt-in (`crates/engine-v8`)**: TurboFan JIT + shared `v8::BackingStore` + V8 Fast API (`jsnir.set` / `jsnir.commit` at `~15 ns/op`) + in-place `Isolate` recycling (`0.47 ms` hot reload), `3.2–35.8×` faster on heavy JS/SDK workloads with zero impact on default builds.
- **High-throughput `wgpu` renderer**: persistent `4096×2048` shelf texture atlas, instanced draw-call batching (`1,501 → 1` draw call on a dense 2D grid), `O(1)` steady-state view cache (`5.95 µs`), rounded fills, gradients, inner/outer shadows, animatable per-corner `radius`, `paint.exiting` frozen-snapshot exit transitions, rounded clips, masks, and opacity layers.
- **Native multiplatform IME**:
  - **Linux Wayland**: native `zwp_text_input_v3` (v1 & v2) over an isolated `wl_event_queue` (`set_surrounding_text`, `set_text_change_cause`, semantic `content_hint`/`content_purpose`, `delete_surrounding_text`, atomic 7-step `done`).
  - **Windows**: Win32 `HWND` subclass decoding multi-clause CJK `GCS_COMPATTR` (`PreeditSegments`), `WM_IME_REQUEST` (`IMR_DOCUMENTFEED`, `IMR_RECONVERTSTRING`, `IMR_QUERYCHARPOSITION`), `ImmNotifyIME(CPS_CANCEL)`, and TSF `SetInputScope`.
  - **Linux X11 / macOS**: deduplicated `winit` fallback with input-to-input focus reset.
- **Native Accessibility (AccessKit) & Keyboard Navigation**: full semantic tree via `accesskit 0.25` + `accesskit_winit 0.34` (Linux AT-SPI, Windows UIA, macOS NSAccessibility) with lazy activation (`0 µs` when inactive), minimal incremental tree diffing, `Role::TextRun` selection/editing, 100% binary `Paint.a11y` metadata, and `Tab`/`Shift+Tab` focus traversal with ARIA `modal` focus trapping and `Enter`/`Space` activation.
- **Vite-style `jsnir` CLI (`crates/dev`)**: in-memory multi-file TSX/TS compilation via **Oxc** (`~2–5 ms`), signal-preserving hot reload, native in-window error overlay, background `tsc --noEmit`, TTY shortcuts, and `--bytecode` AOT builds.
- **Single-source Rust↔TypeScript contract**: declared once in [src/contract.rs](src/contract.rs) and generated into [app/jsnir.d.ts](app/jsnir.d.ts) (`JSNIR_UPDATE=1 cargo test contract`).

---

## Quickstart (`jsnir` CLI)

```sh
# 1. Build the lightweight CLI (QuickJS-ng default, zero V8)
cargo build --release --manifest-path crates/dev/Cargo.toml

# 2. Scaffold and start a new TSX project with instant Oxc hot reload
./crates/dev/target/release/jsnir init my-app
cd my-app
../crates/dev/target/release/jsnir dev

# 3. Build a standalone JS or AOT QuickJS bytecode (`.jsbc`) production bundle
../crates/dev/target/release/jsnir build
../crates/dev/target/release/jsnir build --bytecode

# 4. Run a compiled bundle directly
../crates/dev/target/release/jsnir run dist/app.jsbc
```

### Opt-in V8 JIT engine (`--features engine-v8`)

For apps with heavy JS logic (parsers, terminal emulators, large grids, massive dynamic lists), compile
the CLI or GPU runtime with `--features engine-v8` and pass `--engine v8` (or set `engine = "v8"` under
`[app]` in `jsnir.toml`):

```sh
cargo run --release --manifest-path crates/dev/Cargo.toml --features engine-v8 -- dev my-app --engine v8
```

---

## Example TSX Component

Components run once on mount; fine-grained signals update only the nodes that read them. Transitions,
hover, press, focus states, exit animations, and accessibility metadata run natively in Rust on the UI
thread ([app/stepper.tsx](app/stepper.tsx)):

```tsx
function Option(props: { title: string; detail: string; selected: () => boolean; onSelect: () => void }): Element {
    return (
        <Row
            style={{ grow: 1, basis: 0, align: "center", justify: "between", gap: 12, padding: [11, 14, 11, 14] }}
            onPress={props.onSelect}
            paint={() => {
                const selected = props.selected();
                return {
                    radius: 12,
                    background: selected ? undefined : well,
                    gradient: selected ? tile : undefined,
                    edge: selected ? ring : tileEdge,
                    shadows: selected ? [tileShadow] : [],
                    a11y: { role: "radioButton", selected },
                    transitions: [ease("background", 200), ease("edge", 200), ease("radius", 200)],
                };
            }}
        >
            <Box style={{ gap: 2 }}>
                <Text font={{ size: 14, weight: 600 }} paint={{ color: ink }}>{props.title}</Text>
                <Text font={{ size: 12.5 }} paint={{ color: muted }}>{props.detail}</Text>
            </Box>
        </Row>
    );
}
```

### Run the Stepper Demo

```sh
# Fetch pinned native TypeScript compiler once (or use `npm ci`)
JSNIR_TSC=$(bash scripts/fetch-tsc.sh target/typescript)
export JSNIR_TSC

# Default QuickJS-ng backend
cargo run --release --features stepper --manifest-path backends/gpu/Cargo.toml -- stepper

# Opt-in V8 JIT backend
cargo run --release --features "stepper engine-v8" --manifest-path backends/gpu/Cargo.toml -- stepper --engine v8
```

Flags: `--engine quickjs|v8` (when built with `--features engine-v8`), `--step N`, `--name NAME`,
`--plan pro|starter`, `--focus`, `--offscreen file.ppm`.

---

## Repository Layout

| Path | Contents |
| --- | --- |
| [src/](src) | Core engine: tree & batches ([tree.rs](src/tree.rs)), pluggable JS engine trait + QuickJS-ng ([engine/](src/engine)), binary command codec & Rust↔TS contract ([contract.rs](src/contract.rs)), UI host & layout ([host.rs](src/host.rs)), AccessKit tree & actions ([a11y.rs](src/a11y.rs)), transitions ([motion.rs](src/motion.rs)), line editor + IME core ([editor.rs](src/editor.rs)), `cosmic-text` engine ([text.rs](src/text.rs)), virtual list ([list.rs](src/list.rs)) |
| [crates/engine-v8](crates/engine-v8) | Standalone opt-in V8 JIT backend (`jsnir-v8`): TurboFan, shared `BackingStore`, V8 Fast API, in-place reload, `VmLimits` watchdog, parity suite & comparative benchmark |
| [crates/dev](crates/dev) | Vite-style `jsnir` CLI & embeddable dev watcher: Oxc multi-file TSX compiler, signal-preserving hot reload, error overlay, async `tsc`, `--bytecode` AOT, `--engine quickjs\|v8` |
| [backends/gpu](backends/gpu) | Native `winit` + `wgpu` backend: persistent shelf atlas, 1-draw-call instanced renderer, `accesskit_winit` adapter, native Wayland `zwp_text_input_v3` & Windows IMM32/TSF IME ([ime/](backends/gpu/src/ime)), `arboard` clipboard |
| [backends/egui](backends/egui) | Historical `egui` measurement baseline |
| [app/](app) | Fine-grained TSX [SDK](app/sdk.ts), generated [jsnir.d.ts](app/jsnir.d.ts), and example/test screens ([stepper.tsx](app/stepper.tsx), [chat.tsx](app/chat.tsx)) |
| [examples/](examples) | Reproducible release microbenchmarks (`bridge_bench`, `frame_bench`, `sdk_bench`) |
| [comparison/stepper](comparison/stepper) | Side-by-side Stepper port in Chromium, GPUI, and JSNIR with captures and metrics |
| [docs/](docs) | [Benchmarks](docs/benchmarks.md), [Roadmap](docs/roadmap.md), [Releases](docs/releases.md), and [Research notebook](docs/research.md) |
| [AGENTS.md](AGENTS.md) | Architecture invariants, contract workflow, performance guardrails, and contributor rules |

---

## Tests & Benchmarks

```sh
# Core engine (zero-engine check + full QuickJS/contract/host/AccessKit tests)
cargo check --no-default-features
cargo test --features "examples stepper" -- --test-threads=4

# Native GPU renderer + Wayland/Windows IME protocol tests
cargo test --manifest-path backends/gpu/Cargo.toml --features "examples stepper"

# Vite-style CLI (default QuickJS + opt-in V8)
cargo test --manifest-path crates/dev/Cargo.toml
cargo test --manifest-path crates/dev/Cargo.toml --features engine-v8

# Opt-in V8 JIT parity, Fast API, in-place reload & VmLimits suite
cargo test --manifest-path crates/engine-v8/Cargo.toml

# TypeScript contract check
npm run typecheck
```

---

## Support & Donations

JSNIR is open-source and developed independently. If JSNIR helps your project or you'd like to support
continued work on native desktop performance, multi-window support, and cross-platform polish:

<p align="left">
  <a href="https://github.com/sponsors/Niiihuel"><img src="https://img.shields.io/badge/GitHub_Sponsors-Sponsor_JSNIR-ea4aaa?style=for-the-badge&logo=githubsponsors&logoColor=white" alt="GitHub Sponsors"></a>
  <a href="https://ko-fi.com/niiihuel"><img src="https://img.shields.io/badge/Ko--fi-Buy_a_Coffee-FF5E5B?style=for-the-badge&logo=ko-fi&logoColor=white" alt="Ko-fi"></a>
  <a href="https://github.com/Niiihuel/jsnir/stargazers"><img src="https://img.shields.io/badge/GitHub-Star_the_Repo-f59e0b?style=for-the-badge&logo=github&logoColor=white" alt="Star on GitHub"></a>
</p>

## Contributing & Documentation

Before opening a PR, please read **[AGENTS.md](AGENTS.md)** for architectural invariants, the
single-source binary contract workflow, and required test/benchmark checks.

- **[Performance & Benchmarks (Electron vs GPUI vs JSNIR · QuickJS vs V8)](docs/benchmarks.md)**
- **[Stepper Comparison: Chromium vs GPUI vs JSNIR](comparison/stepper/README.md)**
- **[Roadmap](docs/roadmap.md)**
- **[Contributor & Agent Guide (`AGENTS.md`)](AGENTS.md)**
- **[Research Notebook](docs/research.md)**
- **[External App & Release Guide](docs/releases.md)**

## License

JSNIR is licensed under [MIT](LICENSE). Inter uses the [SIL Open Font License](assets/fonts/Inter-LICENSE.txt), and Hugeicons Free uses [MIT](assets/icons/Hugeicons-LICENSE.txt).
