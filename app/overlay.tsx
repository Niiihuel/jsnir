// Test app for overlays: a tooltip on the first row of a scroll column, a menu popover over the rows, a popover that
// flips at the right edge and one that is clamped at the bottom, and a box that reports right-button presses. Sizes
// are chosen for a 640 by 560 window: the menu sits under the header, the tall popover under the low button.
var rowHover = 0x4080c0ff;
var panel = 0x202030ff;
var button = 0x303040ff;
function Overlay(): Element {
    var menu = signal(false);
    var edge = signal(false);
    var low = signal(false);
    var picked = signal("none");
    var menuButton = 0;
    var edgeButton = 0;
    var lowButton = 0;
    return (
        <Column style={{ padding: 16, gap: 16 }}>
            <Row style={{ justify: "between", align: "center" }}>
                <Box style={{ width: 100, height: 32 }} paint={{ background: button, radius: 4 }} ref={(id) => (menuButton = id)} onPress={() => { menu.value = !menu.value; }}>
                    <Text>{"menu"}</Text>
                </Box>
                <Box style={{ width: 100, height: 32 }} paint={{ background: button, radius: 4 }} ref={(id) => (edgeButton = id)} onPress={() => { edge.value = true; }}>
                    <Text>{"edge"}</Text>
                </Box>
                <Popover open={edge} anchor={() => edgeButton} side="right" align="start" style={{ width: 200, height: 100 }} paint={{ background: panel, radius: 6 }}>
                    <Text>{"edge panel"}</Text>
                </Popover>
            </Row>
            <Column style={{ width: 240, height: 240, scroll: true }}>
                {[0, 1, 2, 3, 4, 5, 6, 7, 8, 9].map((n) =>
                    n === 0 ? (
                        <Tooltip text={"tip " + n} side="right">
                            <Row style={{ group: true, height: 30, align: "center", padding: 6 }} paint={{ hover: { background: rowHover }, transitions: [] }} onPress={() => { picked.value = "row " + n; }}>
                                <Text>{"row " + n}</Text>
                            </Row>
                        </Tooltip>
                    ) : (
                        <Row style={{ group: true, height: 30, align: "center", padding: 6 }} paint={{ hover: { background: rowHover }, transitions: [] }} onPress={() => { picked.value = "row " + n; }}>
                            <Text>{"row " + n}</Text>
                        </Row>
                    ),
                )}
            </Column>
            <Popover open={menu} anchor={() => menuButton} side="bottom" align="start" style={{ width: 200, height: 120 }} paint={{ background: panel, radius: 6 }}>
                <Box style={{ height: 40, align: "center", padding: 8 }} onPress={() => { picked.value = "item"; }}>
                    <Text>{"item"}</Text>
                </Box>
            </Popover>
            <Row style={{ justify: "between", align: "center", height: 32 }}>
                <Box style={{ width: 100, height: 32 }} paint={{ background: button, radius: 4 }} ref={(id) => (lowButton = id)} onPress={() => { low.value = true; }}>
                    <Text>{"low"}</Text>
                </Box>
                <Popover open={low} anchor={() => lowButton} side="bottom" style={{ width: 160, height: 400 }} paint={{ background: panel, radius: 6 }}>
                    <Text>{"tall panel"}</Text>
                </Popover>
                <Box style={{ width: 200, height: 32 }} onContextMenu={(p) => { picked.value = "context " + p.x + "," + p.y; }}>
                    <Text>{() => "picked " + picked.value}</Text>
                </Box>
            </Row>
        </Column>
    );
}
mount(Overlay);
