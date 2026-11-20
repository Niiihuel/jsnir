// Test app for the containers: a Column with a background, a For with a thin scrollbar over ten rows, and a tooltip
// whose text has its own color and wraps at 60 px. Column and For take the props of a Box.
function Containers(): Element {
    var rows = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9];
    return (
        <Row style={{ gap: 16 }}>
            <Column style={{ width: 60, height: 60 }} paint={{ background: 0x3060c0ff }} />
            <For each={rows} key={(n: number) => n} style={{ width: 120, height: 100, scroll: true }} paint={{ scrollbar: { width: 8, thumb: 0x5f606aff, radius: 4, minThumb: 40, inset: 0, autoHide: false } }}>
                {(row: () => number) => <Box style={{ height: 30 }}><Text>{() => "row " + row()}</Text></Box>}
            </For>
            <Tooltip text="A tooltip that wraps onto several lines" maxWidth={60} textPaint={{ color: 0xff8000ff }}>
                <Box style={{ width: 20, height: 20 }} />
            </Tooltip>
        </Row>
    );
}
mount(Containers);
