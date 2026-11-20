// Test app for `scrollbar`: three 120 by 100 scroll boxes of ten 30 px rows (content 300). The thumb is the dark
// theme's thin thumb from Discord's reference, on an 8 px bar with 4 px corners and no track. The minimum thumb,
// the inset and auto-hide are choices of this app: the reference gives none.
function Rows(): Element {
    return (
        <Column>
            {[0, 1, 2, 3, 4, 5, 6, 7, 8, 9].map((n) => (
                <Box style={{ height: 30 }}>
                    <Text>{"row " + n}</Text>
                </Box>
            ))}
        </Column>
    );
}

function Scrolling(): Element {
    var thumb = 0x5f606aff;
    return (
        <Row style={{ gap: 16 }}>
            <Box style={{ width: 120, height: 100, scroll: true }} paint={{ scrollbar: { width: 8, thumb: thumb, radius: 4, minThumb: 40, inset: 0, autoHide: false } }}>
                <Rows />
            </Box>
            <Box style={{ width: 120, height: 100, scroll: true }} paint={{ scrollbar: { width: 8, thumb: thumb, radius: 4, minThumb: 40, inset: 0, autoHide: true } }}>
                <Rows />
            </Box>
            <Box style={{ width: 120, height: 100, scroll: true }} paint={{ scrollbar: { width: 8, thumb: thumb, radius: 4, minThumb: 0, inset: 0, autoHide: false } }}>
                <Rows />
            </Box>
        </Row>
    );
}
mount(Scrolling);
