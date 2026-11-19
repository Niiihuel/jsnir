// Counter design v1. Coordinates in logical pixels; rectangles are half-open.
pub const WIDTH: u32 = 320;
pub const HEIGHT: u32 = 240;
#[derive(Clone, Copy, Debug)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
    pub rgb: u32,
}
impl Rect {
    pub fn contains(self, x: i32, y: i32) -> bool {
        x >= self.x && y >= self.y && x < self.x + self.w && y < self.y + self.h
    }
}
pub const fn rect(x: i32, y: i32, w: i32, h: i32, rgb: u32) -> Rect {
    Rect { x, y, w, h, rgb }
}
pub const BUTTON: Rect = rect(48, 146, 224, 54, 0xb5ef69);
pub fn paint(value: u16, mut fill: impl FnMut(Rect)) {
    fill(rect(0, 0, 320, 240, 0x132026));
    fill(rect(32, 24, 256, 192, 0x203239));
    const DIGITS: [u8; 10] = [0x3f, 6, 0x5b, 0x4f, 0x66, 0x6d, 0x7d, 7, 0x7f, 0x6f];
    let segments = [
        (6, 0, 26, 6),
        (32, 6, 6, 28),
        (32, 40, 6, 28),
        (6, 68, 26, 6),
        (0, 40, 6, 28),
        (0, 6, 6, 28),
        (6, 34, 26, 6),
    ];
    for (column, digit) in [value / 100, value / 10 % 10, value % 10]
        .into_iter()
        .enumerate()
    {
        for (i, (x, y, w, h)) in segments.iter().enumerate() {
            let lit = DIGITS[digit as usize] & (1 << i) != 0;
            fill(rect(
                89 + column as i32 * 52 + x,
                48 + y,
                *w,
                *h,
                if lit { 0xe5f6ee } else { 0x30454a },
            ));
        }
    }
    fill(BUTTON);
    fill(rect(148, 170, 24, 6, 0x132026));
    fill(rect(157, 161, 6, 24, 0x132026));
}
