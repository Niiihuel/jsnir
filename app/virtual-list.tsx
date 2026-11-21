// Compile with sdk.ts and jsnir.d.ts; the Host supplies the indexed history and payloads.
interface FixtureMessage {
    body: string;
    author: string;
}
function fixtureRow(key: string, item: () => FixtureMessage): Element {
    const clicks = signal(0);
    return <Row style={{ height: 24, gap: 6 }}>
        <Text spans={() => [{ text: item().author, weight: 700 }, { text: ` ${item().body}` }]}>
            {() => `${item().author} ${item().body}`}
        </Text>
        <Dynamic value={() => item().body === "edited"}>
            {edited => <Image source="attachment" style={{ width: edited ? 12 : 8, height: 8 }} />}
        </Dynamic>
        <Box onPress={() => clicks.update(n => n + 1)}><Text>{() => `react ${key}: ${clicks.value}`}</Text></Box>
    </Row>;
}
mount(() => <Column><VirtualList name="messages" style={{ height: 48 }}>{fixtureRow}</VirtualList></Column>);
