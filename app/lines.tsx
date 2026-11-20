// Test app for `Text` with `lines`: the same long sentence in a 120 px column, capped at one line and at two.
const sentence = "The quick brown fox jumps over the lazy dog while the patient engineer measures every line of wrapped text twice.";
function Lines(): Element {
    return (
        <Column>
            <Column style={{ width: 120 }}>
                <Text lines={1} font={{ size: 16 }}>{sentence}</Text>
                <Text lines={2} font={{ size: 16 }}>{sentence}</Text>
            </Column>
        </Column>
    );
}
mount(Lines);
