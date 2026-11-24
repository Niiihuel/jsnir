# AGENTS.md — JSNIR Contributor & Agent Guide

This document defines the architectural invariants, contract rules, verification suites, and
performance guardrails for contributing to **JSNIR** (`Niiihuel/jsnir`). Both human contributors
and automated coding agents must follow these rules.

---

## 1. Core Architectural Invariants (Non-Negotiable)

1. **JS never runs on the UI thread.**
   - All application JavaScript executes on a dedicated VM thread behind the `JsEngine` / `JsEngineFactory`
     traits ([src/engine/mod.rs](src/engine/mod.rs)).
   - Rust owns the window, event loop, Taffy flexbox layout, text editing (`LineEditor`), native IME,
     native accessibility (AccessKit), transitions/animations (`Motion` + `Leaving`), and `wgpu` rendering.
   - `Host::poll()` uses a bounded wait (`wait_budget`, default `800 µs`) so fast replies land in the
     same frame while slow JS stalls never drop monitor-refresh frames (vsync-paced to whatever refresh
     rate the display supports — 60 / 120 / 144 / 240+ Hz — never capped to a fixed Hz).
2. **100% Binary Command Protocol (`JSON calls = 0` across the SDK).**
   - Every SDK operation, style, paint property (including `transitions`, `animations`, `starting`,
     `exiting`, `scrollbar`, `a11y`), rich `Span`, and typed UI event (`jsnirDrag`, `jsnirScroll`,
     `jsnirContextMenu`) must round-trip through the shared `Uint32Array`/`Float32Array` binary command
     buffer + string pool in [src/contract.rs](src/contract.rs) and [app/sdk.ts](app/sdk.ts).
   - Never add a new SDK feature that falls back to `JSON.stringify` / `jsnir.batch`.
   - Keep `Op::Create` / `Op::SetPaint` / `Batch` zero-clone across the UI thread via `Arc<Batch>` and
     `Arc<Paint>`.
3. **Single-source Rust↔TypeScript contract.**
   - [app/jsnir.d.ts](app/jsnir.d.ts) is **generated from Rust** (`src/tree.rs` + `src/contract.rs`).
   - **Never hand-edit `app/jsnir.d.ts`.** Always regenerate it with `JSNIR_UPDATE=1 cargo test contract`.
4. **Lightweight default builds — zero V8 leakage.**
   - Core `jsnir` must compile cleanly with `--no-default-features` (zero JS VM linked).
   - Default builds (`jsnir`, `jsnir-gpu`, `jsnir-dev`) link **QuickJS-ng** only.
   - `jsnir-v8` (`crates/engine-v8`) is strictly opt-in via `--features engine-v8`. Verify default builds
     never pull `v8` into the dependency graph (`cargo tree --manifest-path crates/dev/Cargo.toml -i v8`
     must fail with "package not found").
5. **Zero steady-state cost for optional features.**
   - Static nodes without transitions/animations/starting must stay on the `Walker::shown` fast path
     (zero `Motion` HashMap lookups).
   - `Walker::draw` must only capture `ExitSnapshot` when `paint.exiting.is_some()`.
   - `Paint.a11y` is `Option<Box<A11y>>` (zero heap allocation when unset).
   - AccessKit tree building must run **only** while an assistive technology is active (`a11y_active`),
     and must emit minimal incremental `TreeUpdate` diffs via `A11yCache` (`None` when unchanged).
6. **Batch atomicity & immediate `u16` ID recycling.**
   - `Tree::apply_shared` validates an entire `Batch` before mutating state.
   - Side effects in `Host::receive` (such as promoting `exit_snapshots` to `self.leaving`, updating
     `bound_rows`, or invalidating focus) must only commit **after** `apply_shared` returns `Ok(())`.
   - Always assume a batch can `Op::Remove { id }` and immediately `Op::Create { id, ... }` in the
     same batch; never key long-lived exit animations or stale snapshots solely on recycled `u16` IDs.
7. **Focus vs. Text-Input IME separation.**
   - `Host::focused_node()` returns any focusable control (`Input`, `Button`, pressable `Box`,
     `a11y.focusable: true`) for `paint.focus`, `Tab`/`Shift+Tab`, `Enter`/`Space`, and AccessKit.
   - `Host::focused()` / `Host::focused_input()` / `Host::ime_state()` must enable platform IME and
     `LineEditor` **only** when a non-password `Node::Input` has focus — never when a button or box
     has focus.

---

## 2. Repository & Crate Map

| Path | Role |
| --- | --- |
| [Cargo.toml](Cargo.toml), [src/](src) | Core `jsnir` crate (engine-agnostic `Host`, `Tree`, `Layout`, `Motion`, `LineEditor`, `TextEngine`, `a11y`, `contract`, optional QuickJS-ng engine) |
| [src/tree.rs](src/tree.rs) | Contract types (`Style`, `Paint`, `Exiting`, `A11y`, `Node`, `Op`), validation, `Tree` & `Replica` |
| [src/contract.rs](src/contract.rs) | Binary command buffer decoder (`decode_batch`, `decode_paint`, `decode_style`, `decode_a11y`) + `Ts` declaration macros |
| [src/host.rs](src/host.rs) | UI-thread `Host`, Taffy layout integration, `Walker`, `Motion`/`Leaving` rendering, hit testing, keyboard `Tab` focus, `push_keyed` coalescing |
| [src/a11y.rs](src/a11y.rs) | AccessKit `0.25` tree snapshot, incremental `A11yCache`, `Role::TextRun` selection mapping, `handle_a11y_action` |
| [src/editor.rs](src/editor.rs) | Native `LineEditor`, UTF-8/UTF-16 `SurroundingText`, `PreeditSegments`, `DeleteSurroundingBytes`, password masking, `directed_selection` / `set_selection` |
| [src/motion.rs](src/motion.rs) | CSS-like transitions (`Value::Number`, `Value::Color`, `Value::Quad` for 4-corner `Radius`), `spring`/`bezier` easing, keyframe loops |
| [src/engine/](src/engine) | Pluggable `JsEngine` / `JsEngineFactory` traits, `VmLimits`, and default QuickJS-ng backend ([quickjs.rs](src/engine/quickjs.rs)) |
| [crates/engine-v8](crates/engine-v8) | Opt-in V8 JIT backend (`jsnir-v8`): shared `BackingStore`, V8 Fast API, in-place reload, watchdog, parity suite & `engine_bench` |
| [crates/dev](crates/dev) | Vite-style `jsnir` CLI (`init`, `dev`, `build`, `run`, `check`, `capture`): Oxc TSX compiler, hot reload, error overlay, `jsnir.toml` |
| [backends/gpu](backends/gpu) | Native `winit` + `wgpu` window, persistent shelf atlas, 1-draw-call instanced renderer, `accesskit_winit` adapter, `Pusher::push_keyed`, Wayland/Win32 IME ([ime/](backends/gpu/src/ime)) |
| [backends/egui](backends/egui) | Historical `egui` baseline — keep compiling cleanly when core `jsnir` changes |
| [app/sdk.ts](app/sdk.ts) | Reactive TSX SDK & binary command encoder (`tryWritePaint`, `tryWriteStyle`, `emitCreate*`) |
| [app/jsnir.d.ts](app/jsnir.d.ts) | Auto-generated TypeScript contract declarations (never edit manually) |
| [examples/](examples) | Release microbenchmarks: `bridge_bench.rs`, `frame_bench.rs`, `sdk_bench.rs` |
| [docs/](docs) | [benchmarks.md](docs/benchmarks.md), [roadmap.md](docs/roadmap.md), [releases.md](docs/releases.md), [research.md](docs/research.md) |

Note: `backends/gpu`, `backends/egui`, `crates/dev`, and `crates/engine-v8` have independent
`Cargo.toml` manifests (not a single root Cargo workspace). Always test them explicitly with
`--manifest-path`.

---

## 3. Checklist for Modifying the Rust↔TS Contract

Whenever you add or modify any field/variant in `Style`, `Paint`, `Node`, `Op`, or `Property`:

1. **Declare & validate in [src/tree.rs](src/tree.rs):**
   - Add the type/field via `contract_strings!` / `contract_struct!` / `contract_enum!`.
   - Validate all `f32` values with `.is_finite()` (and non-negative bounds where applicable) in `Style::valid()` / `Paint::valid()` / `Tree::apply`.
   - Export any new declaration in `ts_decls()` in dependency order.
2. **Decode in [src/contract.rs](src/contract.rs):**
   - Allocate the next unused bit in the binary mask (`Paint` currently uses bits `0..=25`; `Style` uses `0..=16`; `PROP_CODES` uses `0..=12`).
   - Add the binary decoder function and reject out-of-range enum codes with a descriptive `Err(...)`.
3. **Encode in [app/sdk.ts](app/sdk.ts):**
   - Add matching code tables and binary writer logic (`tryWritePaint` / `tryWriteStyle` / etc.).
   - Always check `if (p + MAX_WORDS > cmdCap) return -2;` **before** mutating the buffer or calling `pushStr(...)` so overflow flush-and-retry stays spotless.
   - Validate enum codes before calling `pushStr(...)` and return `-1` only on invalid input.
4. **Regenerate TypeScript definitions:**
   ```sh
   JSNIR_UPDATE=1 cargo test contract
   ```
5. **Add roundtrip coverage in [src/lib.rs](src/lib.rs):**
   - Exercise the new field/op inside `binary_sdk_roundtrip_covers_all_opcodes_rich_paint_spans_and_overflow_flush`
     and assert **`json_after_mount == 0`** (and field-by-field Rust equality).
   - If the change affects JS engine bindings or columns, also verify `crates/engine-v8/tests/parity.rs`.

---

## 4. Platform-Specific Safety Rules

- **AccessKit (`backends/gpu/src/app.rs`):**
  - Always create the `winit` window with `.with_visible(false)`, construct
    `accesskit_winit::Adapter::with_event_loop_proxy` **before** calling `window.set_visible(true)`,
    and forward every `WindowEvent` to `a11y.process_event(&surface.window, &event)` before app handling.
  - Every `TreeUpdate` must be validated against `accesskit_consumer::Tree` in unit tests (`src/a11y.rs`).
- **Windows `HWND` subclassing LIFO order (`backends/gpu/src/app.rs`, `backends/gpu/src/ime/windows.rs`):**
  - `accesskit_windows::SubclassingAdapter` installs via `SetWindowLongPtrW(GWLP_WNDPROC)`, while
    `WindowsIme` installs via comctl32 `SetWindowSubclass`.
  - **Attach order:** `accesskit_winit::Adapter` first → `PlatformIme::attach` second.
  - **Teardown / struct field drop order:** `ime` (`PlatformIme`) drops **first**, `a11y` drops
    **second**, `surface` (`Window` / `HWND`) drops **last**. Never reorder those fields in `AppWindow`.
- **Linux Wayland IME (`backends/gpu/src/ime/wayland.rs`):**
  - Keep `zwp_text_input_v3` on its own isolated `wl_event_queue` (never dispatch winit's main queue).
  - Apply atomic `done` events in strict spec order: `delete_surrounding_text` → `commit_string` → `preedit_string`.

---

## 5. Mandatory Verification Matrix

Run these checks before committing or opening a PR (add `--offline` if working from cached crates):

```sh
# 1. Core crate: zero-VM compile check + full unit/contract/host/AccessKit test suite
cargo check --no-default-features
cargo test --features "examples stepper" -- --test-threads=4

# 2. TypeScript SDK & apps typecheck
npm run typecheck

# 3. GPU backend (renderer, atlas, IME protocol, Pusher coalescing)
cargo test --manifest-path backends/gpu/Cargo.toml --features "examples stepper"

# 4. Egui baseline compile check
cargo check --manifest-path backends/egui/Cargo.toml

# 5. Dev CLI (default QuickJS + opt-in V8)
cargo test --manifest-path crates/dev/Cargo.toml
cargo test --manifest-path crates/dev/Cargo.toml --features engine-v8

# 6. Opt-in V8 JIT parity, Fast API, in-place reload & VmLimits suite
cargo test --manifest-path crates/engine-v8/Cargo.toml
```

Requirements:
- **Zero compiler warnings** across all crates.
- Prefer deterministic headless tests (e.g. `Host::render(..., explicit_now)`, `RequestHandler`
  channels, `accesskit_consumer::Tree`) over flaky wall-clock `sleep` assertions.

---

## 6. Performance Guardrails & Benchmarks

Any change touching `src/host.rs`, `src/layout.rs`, `src/motion.rs`, `src/text.rs`, `src/contract.rs`,
`app/sdk.ts`, `backends/gpu/src/`, or `crates/engine-v8/` must verify there is no regression using the
`--release` benchmark suite:

```sh
cargo run --release --features stepper --example frame_bench
cargo run --release --features stepper --example bridge_bench
cargo run --release --features stepper --example sdk_bench
cargo run --release --manifest-path backends/gpu/Cargo.toml --features stepper --example gpu_bench
cargo run --release --manifest-path crates/engine-v8/Cargo.toml --example engine_bench
```

Guardrails (baseline in [docs/benchmarks.md](docs/benchmarks.md)):
- `frame_bench` 2-pane steady-state `frame()` must remain **`< 10 µs` p50** (`~7.9 µs` baseline).
- `sdk_bench` must report **`JSON: 0`** on every single scenario.
- `gpu_bench` dense 2D grid (`1,501` items) must stay at **`1` draw call**.
- **Never fabricate or extrapolate benchmark numbers.** Every number recorded in `README.md`,
  `docs/benchmarks.md`, or `comparison/stepper/README.md` must come from an actual `--release` run.

---

## 7. Git, Branching & Documentation Rules

- **`main` is the single primary branch.** Do not leave stale feature branches pushed on `origin`.
- Use **Conventional Commits**: `feat(scope): ...`, `fix(scope): ...`, `perf(scope): ...`,
  `refactor(scope): ...`, `docs: ...`, `test(scope): ...`.
- Never commit untracked build outputs, temporary `.ppm` captures, local worktrees, or unrelated
  application-specific references (JSNIR is a general-purpose standalone UI engine).
- Keep [README.md](README.md), [docs/roadmap.md](docs/roadmap.md), and
  [docs/benchmarks.md](docs/benchmarks.md) synchronized whenever a major milestone lands.
