// Test app for interaction states: a group row whose children take its hover and active variants, a text
// with a color transition, a pill that grows on hover, an image that a press switches, and a scroll column
// of group rows without press callbacks.
var rest = 0x102030ff;
var hot = 0xf0a050ff;
var pressed = 0x00ff00ff;
var rowHover = 0x80808080;
var rowFill = 0x4080c0ff;
function States(): Element {
    var source = signal("a");
    return (
        <Column>
            <Row
                style={{ group: true, width: 240, height: 60, align: "center", gap: 8, padding: 8 }}
                paint={{ hover: { background: rowHover }, transitions: [] }}
                onPress={() => {
                    source.value = source.value === "a" ? "b" : "a";
                }}
            >
                <Text paint={{ color: rest, hover: { color: hot }, active: { color: pressed }, transitions: [] }}>{"Label"}</Text>
                <Text paint={{ color: rest, hover: { color: hot }, transitions: [{ property: "color", duration: 100, easing: "ease" }] }}>
                    {"Fading"}
                </Text>
                <Box style={{ width: 40, height: 8 }} paint={{ radius: 4, background: 0xffffffff, hover: { height: 20 }, transitions: [] }} />
                <Image source={() => source.value} style={{ width: 10, height: 10 }} />
            </Row>
            <Column style={{ width: 240, height: 100, scroll: true }}>
                {[0, 1, 2, 3, 4, 5].map((n) => (
                    <Row style={{ group: true, height: 30, padding: 4 }} paint={{ hover: { background: rowFill }, transitions: [] }}>
                        <Text>{"row " + n}</Text>
                    </Row>
                ))}
            </Column>
        </Column>
    );
}
mount(States);
