// Test app for masks and rounded clips: a masked box and a masked image, a box whose mask is missing,
// and a rounded clip that cuts a full-size child.
function Masks(): Element {
    return (
        <Column>
            <Box style={{ width: 40, height: 40 }} paint={{ background: 0xff0000ff, mask: "squircle" }} />
            <Image source="a" style={{ width: 40, height: 40 }} paint={{ mask: "cutout" }} />
            <Box style={{ width: 40, height: 40 }} paint={{ background: 0x0000ffff, mask: "missing" }} />
            <Box style={{ width: 40, height: 40, clip: true }} paint={{ radius: 16 }}>
                <Box style={{ width: 40, height: 40 }} paint={{ background: 0x00ff00ff }} />
            </Box>
        </Column>
    );
}
mount(Masks);
