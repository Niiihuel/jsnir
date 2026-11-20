// Test app for the chat scroll: rows in a 300 px box anchored at its end, with a spacer above them. `Host::push`
// drives it: {"rows": n} sets the row count, {"above": px} the spacer's height, and {"to": "start" | "end",
// "offset"?: px, "smooth"?: bool} scrolls the box. The box's `onScroll` writes the scroll columns it reads back.
function range(n: number): number[] {
    return Array.from({ length: n }, (_, i) => i);
}
function ScrollChat(): Element {
    var rows = signal(range(args<{ rows: number }>().rows ?? 50));
    var above = signal(0);
    var box = 0;
    var calls = signal(0);
    var last = signal("");
    onHost(function (message: { rows?: number; above?: number; to?: ScrollEdge; offset?: number; smooth?: boolean }) {
        if (message.rows !== undefined) rows.value = range(message.rows);
        if (message.above !== undefined) above.value = message.above;
        if (message.to !== undefined || message.offset !== undefined) {
            scrollTo(box, { to: message.to, offset: message.offset, smooth: message.smooth });
        }
    });
    return (
        <Column>
            <Text>{() => "calls " + calls.value + " " + last.value}</Text>
            <Column
                style={{ height: 300, scroll: true, scrollAnchor: "end" }}
                ref={(id) => {
                    box = id;
                }}
                onScroll={(s) => {
                    calls.update((n) => n + 1);
                    last.value = "top " + s.top + " height " + s.height + " client " + s.client + " columns " + jsnir.columns.scrollTop[box];
                }}
            >
                <Box style={() => ({ height: above.value })} />
                <For each={rows} key={(n: number) => n}>
                    {(item: () => number) => <Text>{() => "row " + item()}</Text>}
                </For>
            </Column>
        </Column>
    );
}
mount(ScrollChat);
