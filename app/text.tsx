// Test app for `Text` alignment: "Hi" centered in a 200 px box, and the same text at the start.
function Texts(): Element {
    return (
        <Column>
            <Text style={{ width: 200 }} align="center">Hi</Text>
            <Text style={{ width: 200 }}>Hi</Text>
        </Column>
    );
}
mount(Texts);
