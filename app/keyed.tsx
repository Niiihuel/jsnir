// Test app for `For`: the items come from the arguments, and each `Host::push` replaces them. The list is a 60 px
// scroll box, so its content height and offset follow the items.
interface Item {
    key: string;
    name: string;
}
function Keyed(): Element {
    var items = signal(args<{ items: Item[] }>().items ?? []);
    // The window also sends `{jsnirScale}`, which has no items.
    onHost(function (message: { items?: Item[] }) {
        if (message.items) items.value = message.items;
    });
    return (
        <Column>
            <For each={items} key={(item: Item) => item.key} style={{ height: 60, scroll: true }}>
                {(item: () => Item) => <Text>{() => item().name}</Text>}
            </For>
        </Column>
    );
}
mount(Keyed);
