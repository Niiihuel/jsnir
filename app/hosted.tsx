// Test app for `Host::push`: shows the last message from Rust, or "waiting". Without `listen` the app
// never calls `onHost`, so a push reaches no handler.
function Hosted(): Element {
    var s = signal("waiting");
    if (args<{ listen: boolean }>().listen) {
        onHost((m) => {
            s.value = m.text;
        });
    }
    return (
        <Column>
            <Text>{s}</Text>
        </Column>
    );
}
mount(Hosted);
