// Test app for per-side borders: a 100 by 40 box with a 1 px line under it, and a rounded one with a 2 px line.
function Borders(): Element {
    return (
        <Column>
            <Box style={{ width: 100, height: 40 }} paint={{ edge: 0xff0000ff, edgeSides: [0, 0, 1, 0] }} />
            <Box style={{ width: 100, height: 40 }} paint={{ edge: 0xff0000ff, radius: 8, edgeSides: [0, 0, 2, 0] }} />
        </Column>
    );
}
mount(Borders);
