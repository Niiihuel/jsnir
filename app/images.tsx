// Test app for `Host::set_image`: a round avatar and an image nothing supplies, which draws nothing.
function Images(): Element {
    return (
        <Column>
            <Image source="a" style={{ width: 40, height: 40 }} paint={{ radius: 20, color: 0xff0000ff }} />
            <Image source="missing" style={{ width: 10, height: 10 }} />
        </Column>
    );
}
mount(Images);
