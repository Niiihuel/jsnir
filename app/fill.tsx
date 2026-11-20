// Test app for flex sizing: a scroll column of fifty 30 px rows above a 56 px footer, in a grow column. The arguments
// choose the layout: `direct` puts the scroll column straight in the grow column, otherwise it sits in a grow row,
// which takes `minHeight: 0` only when `shrink` is set. The footer keeps its height with `shrink: 0`, as CSS needs for a
// fixed footer: otherwise it shrinks to its text.
function Rows(): Element {
    return (
        <Column style={{ grow: 1, scroll: true }}>
            {[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30, 31, 32, 33, 34, 35, 36, 37, 38, 39, 40, 41, 42, 43, 44, 45, 46, 47, 48, 49].map((n) => (
                <Box style={{ height: 30 }}>
                    <Text>{"row " + n}</Text>
                </Box>
            ))}
        </Column>
    );
}
function Fill(): Element {
    var args_ = args<{ shrink: boolean; direct: boolean }>();
    var shrink = args_.shrink ?? false;
    var direct = args_.direct ?? false;
    return (
        <Column style={{ grow: 1 }}>
            {direct ? <Rows /> : <Row style={{ grow: 1, minHeight: shrink ? 0 : undefined }}><Rows /></Row>}
            <Box style={{ height: 56, shrink: 0 }} paint={{ background: 0x303040ff }}>
                <Text>{"footer"}</Text>
            </Box>
        </Column>
    );
}
mount(Fill);
