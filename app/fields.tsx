// Test app for `TextInput` with a font: two fields reading "Hello" at 20 px. The first one is focused at mount.
function Fields(): Element {
    return (
        <Column>
            <TextInput style={{ width: 200, height: 32 }} font={{ size: 20 }} value="Hello" />
            <TextInput style={{ width: 200, height: 32 }} font={{ size: 20 }} value="Hello" />
        </Column>
    );
}
mount(Fields);
