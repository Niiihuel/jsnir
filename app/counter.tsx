// The counter with the SDK: the app state is a JS signal; Rust draws the label and the button.
function Counter(): Element {
    var n = signal(0);
    return (
        <Column>
            <Label value={n} />
            <Button onClick={() => n.update((v) => (v + 1) % 1000)} />
        </Column>
    );
}
mount(Counter);
