// Test app for rich `Text`: colored, bold and underlined runs; an inline image between two runs; and a
// long line with spans, cut to one line, and a background that wraps onto two lines.
function Spans(): Element {
    return (
        <Column>
            <Text spans={[{ text: "a", color: 0xff0000ff }, { text: "b", weight: 600 }, { text: "c", underline: true }]}>abc</Text>
            <Text spans={[{ text: "x" }, { text: "", image: { source: "tag", width: 20, height: 20 } }, { text: "y" }]}>xy</Text>
            <Text style={{ width: 60 }} lines={1} spans={[{ text: "The quick " }, { text: "brown", weight: 600 }, { text: " fox jumps over the lazy dog" }]}>The quick brown fox jumps over the lazy dog</Text>
            <Text style={{ width: 100 }} spans={[{ text: "xxxxxxxx yyyyyyyy", background: 0x2040_80ff, backgroundRadius: 3 }]}>xxxxxxxx yyyyyyyy</Text>
        </Column>
    );
}
mount(Spans);
