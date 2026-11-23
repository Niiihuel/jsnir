// Compile with sdk.ts and jsnir.d.ts. A host message drops the row whose cleanup throws: the row must still leave.
const items = signal(["keep", "drop"]);
onHost(() => { items.value = ["keep"]; });
mount(() => <For each={items} key={k => k}>{item => {
    onCleanup(() => { throw new Error("cleanup failed"); });
    return <Text>{`row ${item()}`}</Text>;
}}</For>);
