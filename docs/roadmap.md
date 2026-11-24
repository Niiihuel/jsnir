# JSNIR Roadmap

JSNIR aims to be a general-purpose, embeddable native desktop UI engine in Rust for TypeScript/TSX
applications — delivering native GPU efficiency comparable to GPUI/Iced/egui with the iteration speed
of TSX and sub-millisecond hot reload.

See **[docs/benchmarks.md](benchmarks.md)** for verified release measurements and
**[docs/research.md](research.md)** for the historical design notebook.

---

## Completed Milestones

- **Pluggable JS VM architecture & safety (`src/engine/`, `crates/engine-v8/`)**
  - Engine-agnostic `JsEngine` / `JsEngineFactory` traits; core `jsnir` compiles with `--no-default-features` (zero JS VM linked).
  - Lightweight default **QuickJS-ng** backend with `.jsbc` AOT bytecode (`0.51–1.20 ms` cold start) and `VmLimits` (memory ceiling + interrupt deadline).
  - Opt-in **V8** backend (`jsnir-v8`) with shared `v8::BackingStore`, V8 Fast API (`jsnir.set`/`commit`), in-place `Isolate` recycling on hot reload (`0.41 ms`), and watchdog termination.
- **High-throughput zero-JSON JS↔Rust bridge (`src/cmd.rs`, `src/host.rs`, `app/sdk.ts`)**
  - 100% binary command buffer (`Uint32Array`/`Float32Array` + string pool) across the entire TSX SDK (`JSON calls = 0`).
  - Zero-JSON typed event entry points (`jsnirDrag`, `jsnirScroll`, `jsnirContextMenu`), bounded `Host::poll()` wait (`100%` same-frame landing at `5.84 µs`), UI→app log coalescing, and `Arc<Batch>`/`Arc<Paint>` zero-clone sharing.
- **CPU frame & GPU rendering optimizations (`src/host.rs`, `backends/gpu/src/`)**
  - Text measurement cache, dirty layout short-circuiting, zero-clone tree walker, `Motion` fast path, and safe clip culling (`7.40 µs` steady frame on 1,007 nodes).
  - Persistent `4096×2048` shelf texture atlas, instanced draw-call batching (`1501 → 1` draw call), tight styled-text rasterization, and `O(1)` steady-state view cache (`533 µs` GPU frame on Radeon 780M).
- **Vite-style development tooling (`crates/dev/`)**
  - `jsnir` CLI (`init`, `dev`, `build [--bytecode]`, `run`, `check`, `capture`) with in-memory Oxc multi-file TSX transpilation, signal-preserving hot reload, native in-window error overlay, background `tsc --noEmit`, TTY shortcuts, and `--engine quickjs|v8` / `jsnir.toml` selection.
- **Native multiplatform IME (`src/editor.rs`, `src/host.rs`, `backends/gpu/src/ime/`)**
  - `SurroundingText` (UTF-8 & UTF-16), multi-clause `PreeditSegments`, `DeleteSurroundingBytes`, `InputPurpose` (`Normal`, `Password`, `Search`, `Email`, `Number`), and `ImeChangeCause`.
  - Native Linux Wayland `zwp_text_input_v3` (v1 & v2) client on an isolated `wl_event_queue`.
  - Native Windows `HWND` subclass (`GCS_COMPATTR`, `WM_IME_REQUEST` `IMR_DOCUMENTFEED`/`IMR_RECONVERTSTRING`/`IMR_QUERYCHARPOSITION`, `ImmNotifyIME(CPS_CANCEL)`, TSF `SetInputScope`).
  - Upgraded X11/macOS `winit` fallback with input-to-input focus switch reset.
- **Native Accessibility via AccessKit & Keyboard Focus (`src/a11y.rs`, `src/host.rs`, `backends/gpu/src/app.rs`)**
  - Full semantic accessibility tree exposed via **AccessKit 0.25** + **`accesskit_winit` 0.34** (AT-SPI on Linux, UIA on Windows, NSAccessibility on macOS) with zero per-frame overhead when no assistive technology is active and minimal incremental `TreeUpdate` diffing when active.
  - 100% binary `Paint.a11y` contract (`A11yRole`, `A11yLive`, `label`, `description`, `value`, `checked`, `selected`, `expanded`, `hidden`, `modal`, `level`, `focusable`, numeric ranges), automatic `labelled_by` inference for controls, `Role::TextRun` UTF-8 character/word runs & `TextSelection` for `TextInput` (with password masking), and full AccessKit action dispatch (`Focus`, `Blur`, `Click`, `SetValue`, `ReplaceSelectedText`, `SetTextSelection`, `ScrollUp`/`Down`, `SetScrollOffset`, `ScrollIntoView`, `ShowContextMenu`).
  - Generalized `Tab` / `Shift+Tab` keyboard focus traversal across focusable controls (`Input`, `Button`, pressable `Box`, `a11y.focusable`) with ARIA `modal` focus trapping, `Enter`/`Space` activation, and automatic scroll-into-view.
- **Virtual lists, rich text, overlays, animations & event coalescing**
  - Rust-owned virtual list indexing with keyed demand bridged to JS/SDK, rich `spans`, anchored `Popover` placement callbacks, `For`/`Dynamic` cleanup hooks, keyframe animations with reduced motion, multi-line wrapped `TextInput`, animatable per-corner `Property::Radius`, `paint.exiting` frozen-snapshot exit transitions, and `Host::push_keyed` / `Pusher::push_keyed` event coalescing.

---

## Next Priorities

### 1. Resilience & Fault Recovery
- Catch panics / fatal VM failures on the app thread cleanly so the UI thread stays alive, displays
  the native error overlay, and allows restarting or hot-reloading the VM without crashing the window.
- Preserve unsent field drafts if a JS `onSubmit` handler throws.

### 2. Unified Host Cleanup
- Migrate the remaining standalone chat/counter measurement demos onto the generic `Host` window and
  remove duplicate window code.

### 3. Multi-Window, Platform Polish & Packaging
- Multiple native windows sharing one or more `Host` instances, native OS menu bars / tray hooks,
  and OS reduced-motion / dark-mode preference reading.
- Verified CI matrix for Linux (Wayland/X11), Windows (Vulkan/DX12 + IMM32/TSF), and macOS (Metal).
- Publishable `jsnir` crate + `@jsnir/sdk` npm types package and built-in visual tree/batch inspector.
