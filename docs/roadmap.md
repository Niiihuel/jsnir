# JSNIR roadmap

The first priorities are keeping the UI responsive when the app VM fails, preserving input, and moving the chat and counter onto the shared host. The measurement milestones follow the original runtime proposal. [Decision criteria](runtime-proposal.md#decision-criteria) · [Research notebook](research.md)

## Now: make what exists dependable

1. **A failing app must not take the UI down.** A panic on the app thread (a script that fails to load, a broken
   replica invariant) disconnects the bridge and the UI thread panics with it. The UI already owns the editor,
   layout and drawing, so it can keep running, show the error, and restart the VM the way live reload swaps it.
   *Done when* a test kills the VM mid-session and the window keeps answering input.
2. **No user input is lost.** The chat clears the draft before the app accepts it; if the callback throws, the
   message disappears. Events need an answer (accepted or rejected) so the UI can give the text back.
   *Done when* a throwing submit handler leaves the draft intact.
3. **One host for every app.** The chat still has its own scene and window; the counter has its own runtime.
   Move both onto `host.rs` (which needs list virtualization) and delete the duplicates.
   *Done when* `backends/gpu` has one window type and the chat captures are unchanged.
4. **Develop external apps.** The engine opens an external bundle with `jsnir-gpu run path/to/app.js`.
   Add live reload for the generic host and support for compiling larger TSX projects. [External app setup](releases.md#run-an-external-app)
5. **Bounded queues.** The UI→app log grows until the next event; coalesce repeated `setinput` and `setrect`
   for the same node.

## Next: cover what real screens use

6. **SDK structure:** keyed lists with insert, remove and reorder; conditional subtrees; component cleanup hooks.
   Today `list` only grows and `Dynamic` remounts its whole child.
7. **Layout:** percentages, min/max sizes, wrapping, grid, z-order. **Text:** ellipsis, alignment, rich spans,
   font loading from the app, system font fallback. **Images.**
8. **Rendering:** rounded clipping, rotation, backdrop blur, a glyph atlas instead of one texture per text,
   and redrawing only what changed.
9. **Input:** Tab focus navigation with focus rings, scroll containers for any box, drag, multi-line text,
   and the input-method surrounding-text protocol (winit 0.30 lacks it; Wayland text-input v3 has it).
10. **Accessibility:** expose the node tree through AccessKit. This is the largest gap against egui and GPUI.

## Measure: the proposal's decision criteria

11. **Input → presented frame** on the GPU host with `wp_presentation`, using the egui results as a baseline. [egui presentation measurements](../metrics/presentation-02)
12. **Startup to first frame, idle CPU, and memory**, with a breakdown of the 121 MB peak (GPUI: 109 MB).
13. **Binary size** against the +5 MB threshold over a Rust-only build of the same screen.
14. **Animations frame by frame** against Chromium: record the Stepper's transitions in all three versions.
15. **Scale:** 10,000-node trees in the host, long lists, and batch cost per operation now that `Paint` is boxed.

## Later: make it usable by others

16. An npm package for the SDK and its types, with component documentation.
17. Developer tools: a tree inspector, a batch trace viewer, an error overlay in the window.
18. macOS and Windows runs, fractional scale factors, several windows.
19. Continuous integration running the tests and the Stepper captures on every push.
