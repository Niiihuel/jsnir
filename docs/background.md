# Background · NSH and live UI in Rust · 19 November 2026

These notes record the prior art reviewed for JSNIR's bridge and live reload. The project roadmap tracks the work planned for JSNIR itself. [Roadmap](roadmap.md)

## NSH (Native Shell, Longbridge)

A native UI framework for interfaces delivered at runtime: signed TSX apps that run on a Rust runtime with QuickJS and Taffy. Apache 2.0 license. The code at commit `fc4d7a8` (19 November 2026) was read without building or running it. **[V]**: seen in the code. **[D]**: taken from its internal documentation, not checked line by line against the code.

### How they cross from JS to Rust

| Topic | NSH | JSNIR |
| --- | --- | --- |
| JS thread | Its own (`nsh-engine`); input arrives through an unbounded channel **[V]** | Its own; events through a bounded queue and an unbounded log |
| Engine | QuickJS-NG 0.16.2 through `quickjs-jit` 0.14.2, Longbridge's fork of rquickjs (`Cargo.toml:241`). **They do not use the JIT**: the JIT is another crate (`quickjs-jit-runtime`), which is not in their `Cargo.lock` **[V]** | QuickJS-NG 0.16.2 through `rquickjs` 0.14.0 |
| Writes | One native function per operation, decoded into a typed `Op`, without JSON **[V]** | `jsnir.set` and `jsnir.setText` call native functions. Structural changes go through `jsnir.batch`. |
| Signals | Reactive graph in Rust (Solid 1.9 scheduling); **every read and write is a native call** on the same thread **[V]** | Signals in JS; a read is bytecode, without crossing |
| Synchronous reads of native state | **None.** `measure()` returns a promise. A field's value and selection arrive as events, with an edit counter so a stale JS value does not overwrite what is being typed **[D]** | **Yes**: shared columns (≈ 4 ns per read) and a replica fed by a log. JS cannot write the field |
| Layout and text | Computed on the JS thread, at `commit`; the main thread only receives a `MountBatch` with what changed **[V]** | Layout is computed on the UI thread and returns to JS through the replica |
| Batches with errors | The invalid operation is skipped and the tree stays consistent; some errors stop the VM **[V]** | The whole batch is rejected atomically |
| Contract | TS declarations, specifications and codes generated from Rust, checked by tests **[D]** | The same (`contract.rs`, `jsnir.d.ts`) |
| Virtualized lists | Variable heights with prefix sums (Fenwick tree), recycled cells, at most 150 operations or 2 ms of binding per turn **[V]** | Fixed-height rows drawn by the backend |
| Performance | Counter +1: 10 operations, 7 items, ≈ 261–269 µs per commit in a debug build **[D]** | Counter +1 in release: 1 operation, ≈ 3–5 µs round trip |

### Live UI

- **Compilation** **[V]**: Oxc 0.153 compiles TSX and JSX in the CLI, rolldown bundles, and the result can optionally be compiled to QuickJS bytecode. The bytecode is used only if it matches the exact engine version; otherwise the source is loaded.
- **Signed packages** **[V]**:
  - Format `.nshpkg`: `NSHP`, a JSON header, files and a signature.
  - Ed25519 over `context \0 SHA-256(message)`, with one context per kind (package, certificate, revocations).
  - The trust store keeps the highest accepted build per app and rejects an older one (rollback protection).
  - **[D]** An offline root key certifies the publishing keys, per app pattern and validity window. Revocations carry a sequence number. Verification runs off the UI thread, and the new build is accepted once the host declares it healthy.
- **Loading**: a package replaces the whole app of a surface, and each surface has one VM **[V]/[D]**.
- **Sandbox** **[D]**:
  - capabilities declared in `nsh.json`, which the host app can narrow (network, files, processes, clipboard);
  - no `eval` or `Function`, with frozen prototypes;
  - 256 MB of heap and 2 s per call by default.
- **Development reload** **[V]**:
  - `notify` watches the files and changes arrive over WebSocket as one patch per module;
  - top-level components are wrapped in stable proxies;
  - top-level signals keep their value if their initializer did not change;
  - a patch takes ≈ 0.2–0.5 ms **[D]**; if it cannot be applied, the app reloads.

### What to take for JSNIR, and why

1. ~~Try `quickjs-jit`~~. Tried in Delivery 14 of [research.md](research.md): it speeds up pure JS 35×, but it does not compile functions that call native functions or that read typed arrays, adds 6.5 MB, and does not run on iOS or Android. Not adopted.
2. **VM limits.** Maximum memory and time per call (rquickjs has a memory limit and an interrupt handler). This is the "fault injection" debt noted at the start: today a hung callback leaves the app waiting.
3. **Signed packages for the live UI.** Ed25519 with a context per kind, rollback protection, two-level keys and acceptance after a health check. It is a proven design that is not worth reinventing.
4. **Precompiled bytecode**, tied to the exact engine version, with the source as a fallback. It improves startup.
5. **Oxc instead of `tsc`/Node to compile TSX.** It is a Rust crate: the build would stop depending on Node and compilation could happen in process, for example for development reload. `tsc` would remain for type checking.
6. **Variable-height lists** with prefix sums, for when messages span more than one line (after text measurement).

### Where JSNIR differs

- **Synchronous reads without crossing**: NSH does not have them (its reads are promises or events).
- **One owner per node** instead of edit counters: JS cannot write UI state.
- **Atomic batches.** In the egui presentation benchmark, Rust and JavaScript showed changes in the same frame with the bounded wait enabled. [Presentation results](../metrics/presentation-02)
- NSH's signals live in Rust, so every read crosses. In JSNIR they live in JS. It is a trade-off: the graph in Rust lets Rust components take part in reactivity; ours saves the native call per read.

## Live UI in the Rust ecosystem

| Tool | What it offers | Limit |
| --- | --- | --- |
| Dioxus + Subsecond | Hot patching of Rust code during development (`dx serve --hotpatch`), redirecting calls through a jump table | Experimental in 0.7. The Subsecond and Freya documentation says hook state resets on every patch, although the Dioxus 0.7 announcement says it is kept. Not for production ([subsecond docs](https://docs.rs/subsecond/0.7.10/), [Freya](https://docs.rs/crate/freya/0.4.0-rc.21/source/src/_docs/hot_reload.rs), [Dioxus tooling](https://www.mintlify.com/DioxusLabs/dioxus/advanced/devtools)) |
| Slint | Live preview with a `.slint` interpreter; `slint-interpreter` can load a `.slint` file at runtime | The preview is for development only. The interpreter updates the declarative UI, but the logic stays compiled in Rust ([live preview](https://docs.slint.dev/latest/docs/slint/guide/tooling/live-preview/), [interpreter](https://docs.rs/crate/slint-interpreter/1.16.0/source/lib.rs)) |
| Makepad | Design DSL reloadable without recompiling; Splash (2.0) as a runtime UI language | Changes in Rust need a recompile. The 2.0 details come from community sources, unverified ([documentation](https://publish.obsidian.md/makepad-docs)) |

**Reading.** Rust has hot reload during development, limited and experimental. What it lacks is updating **UI and logic** in an already installed app, which React Native solves with over-the-air (OTA) updates: Expo Updates and the replacements for CodePush, retired on 31 March 2025, several of them with package signing ([Expo](https://expo.dev/blog/what-to-do-without-codepush), [2026 comparison](https://blog.codemagic.io/react-native-ota-tools-in-2026/), [Revopush](https://revopush.org/revopush-vs-expo-updates)). JSNIR can offer both, and its architecture has an advantage of its own: **UI state (draft, selection, scroll, layout) lives in Rust and survives a JS reload by construction**. The person does not lose what they are typing.

**Store restriction (to be verified at the source).** According to third-party sources, Apple's license (3.3.1(B)) allows downloading interpreted code if it does not change the app's primary purpose and does not bypass signing, the sandbox or other system protections ([Capgo](https://capgo.app/blog/apple-remote-code-execution-capgo-live-updates/), [WebF](https://openwebf.com/docs/add-webf-to-flutter/store-guidelines)). The exact text must be read in Apple's documents before deciding the distribution model.
