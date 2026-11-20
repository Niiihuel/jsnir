// Test app for `Host::reload`: a signal-driven label, a field, an image and a scroll column. The reload test
// makes its second version by replacing "v1:" with "v2:".
function Reloading(): Element {
    var n = signal(0);
    return (
        <Column>
            <Text>{() => "v1:" + n.value}</Text>
            <TextInput style={{ width: 200, height: 32 }} />
            <Image source="a" style={{ width: 40, height: 40 }} />
            <Box onPress={() => { n.value = 5; }}>
                <Text>five</Text>
            </Box>
            <Box onPress={() => { n.value += 1; }}>
                <Text>plus</Text>
            </Box>
            <Column style={{ height: 100, scroll: true }}>
                {[0, 1, 2, 3, 4, 5, 6, 7, 8, 9].map((i) => (
                    <Box style={{ height: 30 }}>
                        <Text>{"row " + i}</Text>
                    </Box>
                ))}
            </Column>
        </Column>
    );
}
mount(Reloading);
