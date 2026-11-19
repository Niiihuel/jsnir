# Test 02 · Typing while the chat processes a send

The A–E replay is implemented and measured in egui. The archived results come from the original Spanish-language prototype. The current replay uses English UI strings and checks that Rust and JS produce the same image at each checkpoint. [Research notebook](research.md) · [Current replay](../backends/egui/src/offscreen.rs)

## Question

Can a person keep typing and selecting text while the application logic takes 500 ms to accept the previous message? The original plan called for comparing the Rust and JavaScript modes on egui, GPUI, Iced, Masonry, Slint and the project's own backend.

## Proposed common screen

Client area of 640 × 480 pixels at scale 1. The counter's background and colors. History at the top, a one-line draft below and a "Send" button. Use the same bundled font file in all six backends, and fix its SHA-256, size and line height before generating the references. Also fix each control's box, the spacing, the selection style and the scroll direction. Captures must control the phase of the blinking caret. Each backend draws the text and the controls from the same state.

## Editing script

| Point | Action | State to check |
| --- | --- | --- |
| A | Open the empty chat and focus the field | Empty history, empty draft and focus in the field |
| B | Type "Hello, Córdoba" | Accent visible, caret at the end |
| C | Press Enter | Rust captures the text exactly once, queues the send and clears the field |
| D | While the send is held, type "a second draft" and select the last eight characters | Text visible and selection active before the send is released |
| E | Release the answer | "Hello, Córdoba" appears exactly once. Draft, focus and selection keep the state of D |

For the captures, an explicit synchronization point holds the answer until D is reached, so the machine's speed does not change the script's order. For timing, use a 500 ms delay in both modes off the UI thread. Record the time of each input and the arrival of the commit. In the JS variant the delay affects the worker that processes the send.

## What to record

- **Image.** Compare each point with its reference PNG and compare both modes of each backend. Keep both captures, the number of different pixels and a difference image for each failure. Pass criterion: zero different pixels. A glyph mismatch is kept as a result.
- **Behavior.** Record the messages with their identifiers, the draft text, the focus and the selection range. Check that every character typed during the delay shows before the commit.
- **Time.** Measure input → visible draft update and commit → first image with the message. State whether the end point is a framebuffer read or presentation. Report p50, p95, p99, sample size and whether the capture is included in the time.
- **Additional load.** In a separate run, scroll the same 10,000-message history with equivalent height and virtualization. Keep frame times, visible rows and resident memory. Keep the editing and scrolling scripts separate.
- **Traceability.** Note the code and dependency versions, system, font, renderer, GPU or software driver, scale and refresh rate. Attach raw times, captures and a Markdown summary. Mark as pending any backend that has not run the script.

## Result of the first prototype · egui

The full path was implemented in `src/chat.rs`, the chat app and the egui backend. The UI thread owns the draft, the selection and the focus. On send, it hands an independent logical copy of the message to a bounded queue. The worker waits 500 ms, runs the callback and returns an operation that appends the message to the history. The answer has no field that could overwrite the draft. If the queue is full, Rust puts the text back in the editor.

The runner injected text, Enter and eight Shift + Left Arrow presses into the real editor. In each process it checked that "borrador" in "segundo borrador" was selected (range 8–16), that the field kept focus and that "Hola, Córdoba" was appended exactly once. The runner used the real delay and checked that D was reached before the commit. It hid the caret during capture.

Five processes per mode, five states per process, 50 images of 640 × 480 read from the OpenGL framebuffer with llvmpipe. All 50 matched the corresponding state of the first Rust run. A and C also matched each other, as expected for the empty, focused editor. This reference allows comparing both modes of the same backend. The cross-backend text test remains open.

| From the send until the capture completes | Rust, min–max | JS + Rust, min–max |
| --- | ---: | ---: |
| D, second draft selected | 2–3 ms | 2–3 ms |
| E, first message appended and selection kept | 539–545 ms | 541–543 ms |

These measurements include reading the pixels and writing the capture. The scenario includes the intentional delay and uses an offscreen window. The five observations per mode serve to check the time order. [Raw times](../metrics/chat-02/times.csv) · [Hashes per state](../metrics/chat-02/parity.json)

### 10,000-message history

In another run, the same history was preloaded through the queue of the corresponding mode, without artificial delay. Wheel events were applied to the history. The union of the drawn ranges was checked to cover the 10,000 rows. During the scroll, 17 to 18 rows were drawn per frame. Per mode, 100 warm-up frames were discarded and 1,100 were measured, from the start of the egui pass until OpenGL finished drawing. The simulation advanced the UI clock in steps of 1/60 s and ran the frames without waiting for the display.

| Mode | Frame p50 | p95 | p99 |
| --- | ---: | ---: | ---: |
| Rust | 0.414 ms | 0.465 ms | 0.546 ms |
| JS + Rust | 0.406 ms | 0.465 ms | 0.661 ms |

One run per mode on this machine. During the scroll, the history was already in Rust. The resident memory test and the latency to physical presentation are still pending. [Rust frames](../metrics/chat-02/scroll-rust.csv) · [JS frames](../metrics/chat-02/scroll-js.csv)

### Run the current replay

```bash
npm ci
out=$(mktemp -d "$PWD/metrics/local-chat-XXXXXX")
python3 scripts/chat_replay.py "$out/replay"
SDL_VIDEODRIVER=offscreen target/release/jsnir-egui rust scroll "$out/scroll-rust.csv"
SDL_VIDEODRIVER=offscreen target/release/jsnir-egui js scroll "$out/scroll-js.csv"
```

To use the window on the desktop, run `target/release/jsnir-egui js chat` or `target/release/jsnir-egui rust chat`.
