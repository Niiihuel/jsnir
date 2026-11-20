# JSNIR · JavaScript Native In Rust

JSNIR runs TypeScript and TSX desktop apps with a native Rust UI and no WebView. App logic runs in an embedded QuickJS VM on its own thread. Rust handles the window, input, text editing, layout, transitions and drawing. The native GPU backend can keep drawing while JavaScript processes an event. The app and the UI share a tree with stable node IDs, and the UI applies or rejects each versioned batch as a whole.

![The React Bits Stepper in Chromium and in JSNIR](docs/images/stepper-side-by-side.png)

The JSNIR port of the React Bits Stepper uses 390 nonblank lines of TSX. Across six settled states, 0.07–0.12% of its pixels differ from Chromium at the 6% comparison threshold. The GPUI port uses 1,162 nonblank lines of Rust plus a 574-line adapted text field, with 0.18–0.36% differing pixels. [Stepper comparison](comparison/stepper/README.md)

The Linux preview archive ships the engine and the TSX SDK. Load an app compiled outside the repository with `jsnir-gpu run app.js`. [Install the engine and build an app](docs/releases.md)

## Run the Stepper

Build the comparison from a Git checkout in a Linux graphical session (Wayland or X11). On Linux x86-64, fetch the pinned native compiler once to build without Node or npm.

```sh
JSNIR_TSC=$(bash scripts/fetch-tsc.sh target/typescript)
export JSNIR_TSC
cargo run --release --locked --features stepper --manifest-path backends/gpu/Cargo.toml --target-dir target -- stepper
```

Reuse it in another shell with `export JSNIR_TSC="$PWD/target/typescript/lib/tsc"`. The source build also accepts the compiler installed by `npm ci` when the variable is unset.

Use `--step`, `--name`, `--plan` and `--focus` to open a particular state. Append `--offscreen file.ppm` with a new output path to capture that state without opening a window.

## Stepper example code

Components run once, and signals update the nodes that read them. This option card from the Stepper declares its visual state in `paint`. Rust runs the transitions independently of the app thread. [Stepper source](app/stepper.tsx)

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
                    transitions: [ease("background", 200), ease("edge", 200)],
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

## Run the chat

```sh
cargo run --release --locked --features examples --manifest-path backends/gpu/Cargo.toml --target-dir target -- js
```

Use `rust` in place of `js` to run the Rust reference app. Set `JSNIR_LIVE=1` in the interactive JS chat to reload edits to `app/chat.tsx` and keep the draft, selection, scroll and signal values. If an edit fails to compile, the previous version keeps running.

## Layout

| Path | Contents |
| --- | --- |
| [src/](src) | Core: node tree and batches ([tree.rs](src/tree.rs)), QuickJS bridge ([lib.rs](src/lib.rs)), Rust↔TS contract ([contract.rs](src/contract.rs)), the app host ([host.rs](src/host.rs)) with transitions ([motion.rs](src/motion.rs)), line editor, Taffy layout, text with cosmic-text, icons |
| [app/](app) | [SDK](app/sdk.ts) with fine-grained signals, the [Stepper](app/stepper.tsx) and [chat](app/chat.tsx) examples, and the generated [jsnir.d.ts](app/jsnir.d.ts) |
| [backends/gpu](backends/gpu) | The native backend: winit window and input, arboard clipboard, a wgpu renderer with its own shader (rounded fills, gradients, inner and outer shadows, opacity layers). No UI framework in its dependency tree |
| [backends/egui](backends/egui) | egui measurement baseline |
| [comparison/stepper](comparison/stepper) | The same screen in Chromium, GPUI and JSNIR, with captures, code and measurements |
| [assets/fonts](assets/fonts) | Inter 4.1 (OFL) |
| [scripts/](scripts) | Benchmark and capture runners |
| [docs/](docs) | Research notebook, proposal and roadmap |

The Rust↔TypeScript contract is declared once in `src/contract.rs`, and `app/jsnir.d.ts` is generated from it. Apps are type-checked against that file, so using something Rust does not offer fails to compile. Regenerate it with `JSNIR_UPDATE=1 cargo test contract`. The VM is QuickJS-ng 0.16.2, bundled and statically linked through the `rquickjs` crate.

## Tests

```sh
cargo test --release --locked --features examples,stepper                   # core and TSX checks, including the comparison
cargo test --release --locked --features examples,stepper --manifest-path backends/gpu/Cargo.toml --target-dir target --all-targets
cargo test --release --locked --manifest-path backends/egui/Cargo.toml --target-dir target
```

The core tests cover batch validation, the replica on the JS thread, transitions against the CSS curves, and an end-to-end run of the Stepper through the host: clicks, typing, timers and removed panes, with JS answering from its own thread.

## Results and research

- [Stepper: React Bits in Chromium, GPUI and JSNIR](comparison/stepper/README.md)
- [Measurement and decision notebook](docs/research.md)
- [Roadmap: what is missing](docs/roadmap.md)
- [Runtime proposal and work plan](docs/runtime-proposal.md)
- [Background: NSH and live UI in Rust](docs/background.md)
- [Chat test 02: editing during a delayed send](docs/chat-test-02.md)
- [Run the egui benchmarks](docs/research.md#run-the-egui-benchmarks)
- [Historical backend setup on CachyOS](docs/setup-cachyos.md)

The project was called Jurui until 19 November 2026.

## License

JSNIR is under [MIT](LICENSE). Inter uses the [SIL Open Font License](assets/fonts/Inter-LICENSE.txt), and the Hugeicons Free icons use [MIT](assets/icons/Hugeicons-LICENSE.txt). The Chromium reference build downloads the pinned React Bits component. [Reference build](comparison/stepper/react/README.md)
