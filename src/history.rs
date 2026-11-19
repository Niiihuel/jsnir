//! Scroll position and visible rows of the history, in logical coordinates.
use std::ops::Range;

pub const ROW_HEIGHT: f32 = 25.;
const TEXT_HEIGHT: f32 = 22.;
const TRACK_WIDTH: f32 = 10.;
const MIN_THUMB: f32 = 18.;

#[derive(Clone, Copy)]
pub enum ScrollEvent {
    /// Scroll offset in logical points. Positive towards newer messages.
    Wheel(f32),
    Grab(f32),
    Drag(f32),
    Release,
}

pub struct HistoryView {
    pub rows: Range<usize>,
    pub content: [f32; 4],
    pub track: Option<[f32; 4]>,
    pub thumb: Option<[f32; 4]>,
    offset: f32,
}

impl HistoryView {
    pub fn row_rect(&self, row: usize) -> [f32; 4] {
        let [x, y, width, _] = self.content;
        [
            x,
            y + (row as f32 * ROW_HEIGHT - self.offset),
            width,
            TEXT_HEIGHT,
        ]
    }
}

pub(crate) struct HistoryScroll {
    offset: f32,
    follow_tail: bool,
    grab_offset: Option<f32>,
}

impl Default for HistoryScroll {
    fn default() -> Self {
        Self {
            offset: 0.,
            follow_tail: true,
            grab_offset: None,
        }
    }
}

impl HistoryScroll {
    fn maximum(rect: [f32; 4], count: usize) -> f32 {
        (count as f32 * ROW_HEIGHT - rect[3].max(0.)).max(0.)
    }

    pub(crate) fn view(&mut self, rect: [f32; 4], count: usize) -> HistoryView {
        let [x, y, width, height] = rect;
        let height = height.max(0.);
        let max = Self::maximum(rect, count);
        if max == 0. {
            self.follow_tail = true;
            self.grab_offset = None;
        }
        self.offset = if self.follow_tail {
            max
        } else {
            self.offset.min(max)
        };

        let rows = if height > 0. {
            let start = (self.offset / ROW_HEIGHT).floor() as usize;
            let stop = ((self.offset + height) / ROW_HEIGHT).ceil() as usize;
            start.min(count)..stop.min(count)
        } else {
            0..0
        };
        let mut content = [x, y, width.max(0.), height];
        let (track, thumb) = if max > 0. && width > TRACK_WIDTH && height > 0. {
            content[2] -= TRACK_WIDTH;
            let x = x + content[2];
            let thumb_height = (height * height / (count as f32 * ROW_HEIGHT))
                .clamp(MIN_THUMB.min(height), height);
            (
                Some([x, y, TRACK_WIDTH, height]),
                Some([
                    x,
                    y + (height - thumb_height) * (self.offset / max),
                    TRACK_WIDTH,
                    thumb_height,
                ]),
            )
        } else {
            (None, None)
        };
        HistoryView {
            content,
            rows,
            track,
            thumb,
            offset: self.offset,
        }
    }

    pub(crate) fn input(&mut self, rect: [f32; 4], count: usize, event: ScrollEvent) {
        let view = self.view(rect, count);
        let max = Self::maximum(rect, count);
        let offset = match (event, view.thumb) {
            (ScrollEvent::Wheel(delta), _) if delta.is_finite() => self.offset + delta,
            (ScrollEvent::Grab(y), Some([_, thumb_y, _, height])) if y.is_finite() => {
                let grab = if (thumb_y..=thumb_y + height).contains(&y) {
                    y - thumb_y
                } else {
                    height / 2.
                };
                self.grab_offset = Some(grab);
                self.drag_offset(rect, max, y, grab, height)
            }
            (ScrollEvent::Drag(y), Some([_, _, _, height])) if y.is_finite() => {
                let Some(grab) = self.grab_offset else { return };
                self.drag_offset(rect, max, y, grab, height)
            }
            (ScrollEvent::Release, _) => {
                self.grab_offset = None;
                return;
            }
            _ => return,
        };
        self.offset = offset.clamp(0., max);
        self.follow_tail = self.offset >= max - 0.5;
    }

    fn drag_offset(&self, rect: [f32; 4], max: f32, y: f32, grab: f32, height: f32) -> f32 {
        let travel = rect[3] - height;
        if travel <= 0. {
            self.offset
        } else {
            max * ((y - rect[1] - grab) / travel)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn follows_new_messages_until_the_reader_scrolls_up() {
        let mut scroll = HistoryScroll::default();
        let rect = [16., 16., 608., 398.];
        assert_eq!(scroll.view(rect, 10_000).rows, 9984..10_000);
        scroll.input(rect, 10_000, ScrollEvent::Wheel(-240.));
        let before = scroll.view(rect, 10_000);
        let after = scroll.view(rect, 10_001);
        assert_eq!(before.rows, after.rows);
        assert_eq!(
            before.row_rect(before.rows.start),
            after.row_rect(after.rows.start)
        );
        scroll.input(rect, 10_001, ScrollEvent::Wheel(500.));
        assert_eq!(scroll.view(rect, 10_002).rows.end, 10_002);
    }

    #[test]
    fn scrollbar_reaches_both_ends_and_clamps_after_resize() {
        let mut scroll = HistoryScroll::default();
        let rect = [16., 16., 608., 398.];
        let [_, y, _, h] = scroll.view(rect, 10_000).thumb.unwrap();
        scroll.input(rect, 10_000, ScrollEvent::Grab(y + h / 2.));
        scroll.input(rect, 10_000, ScrollEvent::Drag(-100.));
        assert_eq!(scroll.view(rect, 10_000).rows.start, 0);
        scroll.input(rect, 10_000, ScrollEvent::Drag(600.));
        assert_eq!(scroll.view(rect, 10_000).rows.end, 10_000);
        scroll.input(rect, 10_000, ScrollEvent::Release);
        assert!(scroll.view(rect, 2).track.is_none());
        assert_eq!(scroll.view([0.; 4], 2).rows, 0..0);
    }
}
