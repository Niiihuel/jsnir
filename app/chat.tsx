// Chat with the SDK. Rust captures the text and clears the field; JS appends the message and writes the
// status with the draft and selection the person has right now, read from the UI without waiting.
// Taffy computes the layout in Rust from these styles.
function Chat(): Element {
    var messages = list<string>();
    var status = signal("");
    return (
        <Column>
            <Column style={{ padding: 16, gap: 16, grow: 1 }}>
                <List items={messages} style={{ grow: 1 }}>
                    {(m) => <Text>{m}</Text>}
                </List>
                <TextInput
                    style={{ height: 34 }}
                    onSubmit={({ text, field }) => {
                        messages.push(text);
                        var [start, end] = field.selection();
                        status.value = `draft «${field.text()}», selection ${start}–${end}`;
                    }}
                />
            </Column>
            <Text>{status}</Text>
        </Column>
    );
}
mount(Chat);
