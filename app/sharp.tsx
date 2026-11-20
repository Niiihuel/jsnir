// Test app for image sampling: a 20 by 20 logical image with no radius, tint or mask.
function Sharp(): Element {
    return (
        <Column>
            <Image source="checker" style={{ width: 20, height: 20 }} />
        </Column>
    );
}
mount(Sharp);
