# JSNIR runtime proposal

This proposal records the original TypeScript and Rust split and the thresholds set before benchmarking. The notebook records how the design evolved, and the roadmap tracks the remaining work. [Research notebook](research.md) · [Roadmap](roadmap.md)

## The experience we want to offer

A person writes a desktop application in TypeScript. They describe where a text field goes and what should happen when "Send" is pressed. When someone uses that application, Rust draws the window, receives the keyboard and keeps the draft, the selection and the focus. JavaScript runs when the application has work to do. The distributed executable includes its own VM and opens the interface without a WebView.

The first demonstration will be a local chat. In a single window you can type, send a message and scroll the history while the application logic is busy. That makes it possible to see and measure the benefit that would justify building the runtime.

## What programming it would feel like

Sketch of the public TSX API. The names are provisional.

```tsx
function Chat() {
  const messages = signal<Message[]>([])

  return (
    <Column gap={8}>
      <List items={messages} keyBy={m => m.id}>
        {m => <Text>{m.text}</Text>}
      </List>
      <TextInput
        placeholder="Write a message"
        onSubmit={({ text }) => {
          messages.update(ms => [...ms, createMessage(text)])
        }}
        clearOnSubmit
      />
    </Column>
  )
}
```

The list receives a signal and uses each message's key to keep its identity. The field hands the confirmed text to the callback. I propose that, when there is room to queue the send, Rust captures the text and clears the field in the same local operation. If the queue is full, it keeps the draft and indicates that the send is waiting to be retried. JavaScript's late answer only appends the message to the list. That way, if the person has already started typing another message, their new draft stays intact. During an IME composition, confirming a suggestion must keep the editor's behavior and avoid an accidental send.

## Implementation slice

| Piece | First responsibility | Observable test |
| --- | --- | --- |
| Rust core | Keep nodes with stable identity and apply batches with an expected version | A batch with an invalid reference leaves tree and version intact |
| egui host | Draw the tree, receive input and manage the field's local state | Changing a neighboring text keeps focus, caret and selection |
| JavaScript thread with QuickJS | Run the bundled script, receive events and produce batches | A slow callback delays its result and the user can keep typing |
| TypeScript SDK | Associate signals and callbacks with concrete nodes | Changing a counter emits a text update and keeps the button |

## Work order

1. **End-to-end counter.** A script mounts a column, a label and a button. Pressing the button generates an event. The callback increments the counter and sends a batch that changes the label. Record the event, the batch and the applied version. An explicit mount call is enough for this stage.
2. **Local chat.** Add the field, the history with stable keys and send handling. Introduce a deliberate 500 ms delay in the callback. Type a second draft during that delay and check that it is kept, along with the focus and the order of the sends.
3. **Reproducible comparison.** Build the same screen in pure egui, with identical fonts, window size, history and virtualization. On the same machine and in release, repeat a 10,000-message scroll. Record p50/p95/p99 percentiles of frame time, input-to-presentation latency, wait for each batch and resident memory. Publish the difference from the reference and record the machine and the display refresh rate.
4. **Architecture decision.** If the cost shows up in the batch wait, review dispatch and queues. If it shows up when walking visible rows, investigate the backend. Try the same screen on a second backend when a concrete limitation must be evaluated or the contract's portability shown.

## Decision criteria

The reference is the same screen built in native Rust, on the same machine, backend and release profile, with the renderer noted. The question is whether going through JavaScript costs little compared with that reference. Thresholds are set before measuring and only change with a note explaining why.

| Metric | Threshold for JS + Rust against native Rust | Reason |
| --- | --- | --- |
| Key → visible draft | No difference: p99 within ±5 % or ±0.1 ms | The editor does not go through JS. If this changes, the premise fails |
| p99 frame while scrolling 10,000 messages | Up to +10 % | The history is already in Rust |
| Event → applied batch, 1 to 100 operations | p99 ≤ 1 ms | Less than 6 % of a frame at 60 Hz |
| Event → applied batch, 1,000 operations | p99 ≤ 4 ms | A quarter of a frame. Informative: locates the knee of the curve |
| Event → first frame with the change | Up to one extra frame at 60 Hz at p99 | The answer shows in the next frame |
| Resident memory | Up to +20 MB | The VM must weigh much less than a WebView |
| Startup to first frame | Up to +100 ms | |
| Executable size | Up to +5 MB | |

Except for the informative row, all are mandatory. If one fails, the trace shows the responsible stage and a bounded fix is attempted. If it still fails after that fix, the split between Rust and JS is reworked or the idea is abandoned. The benefit against a WebView (Tauri or Electron) is measured separately with the same screen.

## First acceptable delivery

A desktop executable, the counter view's code and a trace of one click. The trace must show which node received the event, which batch updated the label and in which frame the value was presented. A second run with the delayed callback must let the window redraw during the wait. This delivery demonstrates the full path. The chat and its Rust reference then serve to quantify the cost.
