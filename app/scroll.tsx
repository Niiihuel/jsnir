// Test app for `scroll`: ten 30 px rows in a 100 px column. Pressing a row shows its number above the column.
function Scrolling(): Element {
    var picked = signal(-1);
    return (
        <Column>
            <Text>{() => "picked " + picked.value}</Text>
            <Column style={{ height: 100, scroll: true }}>
                {[0, 1, 2, 3, 4, 5, 6, 7, 8, 9].map((n) => (
                    <Box style={{ height: 30 }} onPress={() => { picked.value = n; }}>
                        <Text>{"row " + n}</Text>
                    </Box>
                ))}
            </Column>
        </Column>
    );
}
mount(Scrolling);
