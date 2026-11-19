# JSNIR · JavaScript Native In Rust

Desktop apps written in TypeScript/TSX with a native UI owned by Rust and no WebView. The app runs in an embedded QuickJS VM on its own thread. Rust owns the window, input, text editing, layout and drawing, and it never waits for JS to draw a frame. The app and the UI share a tree of nodes with stable ids. The app sends versioned batches, and the UI applies each batch whole or rejects it.

![Counter reference at 042](design/0042.png)

## Run it

From this directory, in a Linux graphical session (Wayland or X11):

```sh
npm ci   # TypeScript 7.0.2, pinned in package.json; needed to compile the TSX apps

# The React Bits Stepper port (app/stepper.tsx): a four-step form with transitions run by Rust
cargo run --release --locked --manifest-path backends/gpu/Cargo.toml --target-dir target -- stepper

# Chat on the native GPU backend (wgpu + winit). `js` runs app/chat.tsx; `rust` runs the reference app without a VM.
cargo run --release --locked --manifest-path backends/gpu/Cargo.toml --target-dir target -- js
```

The stepper is compared with the original in Chromium and with a GPUI port in [comparison/stepper](comparison/stepper/README.md).
`jsnir-gpu js --offscreen file.ppm` captures the chat without opening a window. The path must be new.

## Layout

| Path | Contents |
| --- | --- |
| [src/](src) | Core: node tree and batches ([tree.rs](src/tree.rs)), QuickJS bridge ([lib.rs](src/lib.rs)), Rust↔TS contract ([contract.rs](src/contract.rs)), the app host ([host.rs](src/host.rs)) with transitions ([motion.rs](src/motion.rs)), chat, line editor, history, Taffy layout, text with cosmic-text |
| [app/](app) | [SDK](app/sdk.ts) with fine-grained signals, the [stepper](app/stepper.tsx), [chat](app/chat.tsx) and [counter](app/counter.tsx) apps, and the generated [jsnir.d.ts](app/jsnir.d.ts) |
| [backends/gpu](backends/gpu) | The native backend: winit window and input, arboard clipboard, a wgpu renderer with its own shader (rounded fills, gradients, inner and outer shadows, opacity layers). No UI framework in its dependency tree |
| [assets/fonts](assets/fonts) | Inter 4.1 (OFL), used by the stepper |
| [comparison/stepper](comparison/stepper) | The same screen in Chromium, GPUI and JSNIR, with captures and measurements |
| [backends/egui](backends/egui) | The same screens in egui, kept only as the measurement baseline |
| [scripts/](scripts) | Benchmark and capture runners |
| [docs/](docs) | Research notebook, in Spanish |

The Rust↔TypeScript contract is declared once in `src/contract.rs`, and `app/jsnir.d.ts` is generated from it. Apps are type-checked against that file, so using something Rust does not offer fails to compile. Regenerate it with `JSNIR_UPDATE=1 cargo test contract`. The VM is QuickJS-ng 0.16.2, bundled and statically linked through the `rquickjs` crate.

## Tests

```sh
cargo test --release --locked                                              # core, includes type-checking app/*.tsx
cargo test --release --locked --manifest-path backends/gpu/Cargo.toml --all-targets
cargo test --release --locked --manifest-path backends/egui/Cargo.toml
```

The counter checks pixel parity. The client area is 320 × 240 at scale 1, and the geometry and colors are in [design.rs](src/design.rs). Digits use seven segments, so system fonts and antialiasing cannot change the image. The checkpoints are 0, 1, 42, 999 and 1,000 activations, and every RGB pixel must match the frozen references in `design/`.

## Development options

| Variable | Effect |
| --- | --- |
| `JSNIR_LIVE=1` | Live reload: saving `app/chat.tsx` reloads the app without a restart. Draft, selection and scroll belong to the UI and are kept; JS signals carry over. If the code does not compile, the previous version keeps running |
| `JSNIR_DELAY_MS` | Artificial delay of each JS callback, to check the UI stays responsive |
| `JSNIR_WAIT_US` | How long `Runtime::poll` waits for an answer to land in the current frame (default 1 ms; 0 disables it) |
| `JSNIR_TRACE` | New CSV file with one row per event: node, batch, version and first frame drawn after it |

## Measurements against the egui baseline

```sh
target/release/jsnir-egui rust|js <new dir>                                   # offscreen counter benchmark
target/release/jsnir-egui rust|js chat [<new dir>]                            # chat window, or offscreen replay
target/release/jsnir-egui rust|js scroll <new.csv>                            # scroll over 10,000 messages
target/release/jsnir-egui rust|js presentation counter|chat <new dir>         # input → presented frame, Wayland wp_presentation
python3 scripts/benchmark.py [out]
python3 scripts/chat_replay.py <out> [--repeats N]
python3 scripts/presentation.py <dir>...
```

The scripts need Python 3 and ImageMagick 7. Output directories must be new, so earlier measurements are never overwritten.

## Results and research

- [Measurement and decision notebook](docs/investigacion.md)
- [Background: NSH and live UI in Rust](docs/antecedentes.md)
- [Runtime proposal and work plan](docs/Propuesta_de_runtime_TS_Rust_MVP.md)
- [Trace of a click with a delayed callback](metrics/traza-01/ventana-js-500ms.csv)
- [Percentiles, environment and capture hashes](metrics/primera-serie/summary.json)
- [Chat test 02: editing during a delayed send](docs/prueba-02-chat.md)
- [Setup on CachyOS](docs/instalacion-cachyos.md)

The project was called Jurui until 19 November 2026. The research notes keep that name.
