// Test app for a multi-line `TextInput`: 200 px wide, 16 px text on 20 px lines, from one line to four. Enter submits
// and clears the field; the submitted text is shown below it.
function Composer(): Element {
    var sent = signal("");
    return (
        <Column>
            <TextInput
                multiline
                submitOnEnter
                style={{ width: 200, minHeight: 20, maxHeight: 80 }}
                font={{ size: 16, lineHeight: 1.25 }}
                onSubmit={({ text }) => {
                    sent.value = text;
                }}
            />
            <Text>{() => sent.value}</Text>
        </Column>
    );
}
mount(Composer);
