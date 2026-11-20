use crate::renderer::{Color, Draw, Image, Rect, Renderer};
use jsnir::{Mode, chat::Chat, editor::EditorBitmapKey};
use std::{collections::HashMap, time::Duration};

pub struct Scene {
    pub chat: Chat,
    messages: HashMap<usize, CachedText>,
    status: Option<CachedText>,
    button: Option<CachedText>,
    editor: Option<Image>,
    editor_key: Option<EditorBitmapKey>,
}

struct CachedText {
    text: String,
    size: [f32; 2],
    scale: f32,
    image: Image,
}

struct TextParams {
    size: [f32; 2],
    scale: f32,
    max_side: usize,
    wrap: bool,
}

impl Scene {
    pub fn new(mode: Mode, delay: Duration) -> Self {
        Self {
            chat: Chat::new(mode, delay),
            messages: HashMap::new(),
            status: None,
            button: None,
            editor: None,
            editor_key: None,
        }
    }

    pub fn reset_images(&mut self) {
        self.messages.clear();
        self.status = None;
        self.button = None;
        self.editor = None;
        self.editor_key = None;
    }

    pub fn draws(
        &mut self,
        renderer: &Renderer,
        size: [u32; 2],
        scale: f32,
        focused: bool,
        send_focused: bool,
    ) -> (Vec<Draw>, Rect) {
        let [width, height] = size.map(|side| side as f32 / scale);
        self.chat.layout(width, height);
        let screen = [0., 0., width, height];
        let mut draws = vec![Draw::fill(screen, screen, rgb(19, 32, 38))];
        let side = renderer.device.limits().max_texture_dimension_2d as usize;

        let list = self.chat.history_view();
        self.messages.retain(|index, _| list.rows.contains(index));
        for index in list.rows.clone() {
            let rect = list.row_rect(index);
            let params = TextParams {
                size: [rect[2], rect[3]],
                scale,
                max_side: side,
                wrap: false,
            };
            let text = self.chat.message(index);
            if text.is_empty() {
                self.messages.remove(&index);
                continue;
            }
            if !self
                .messages
                .get(&index)
                .is_some_and(|cache| cache.matches(text, &params))
            {
                let text = text.to_owned();
                if let Some(bitmap) = params.rasterize(&mut self.chat, &text, renderer) {
                    self.messages.insert(index, bitmap);
                } else {
                    self.messages.remove(&index);
                }
            }
            if let Some(cache) = self.messages.get(&index) {
                draws.push(Draw::image(
                    &cache.image,
                    [rect[0], rect[1]],
                    scale,
                    list.content,
                ));
            }
        }
        if let Some(track) = list.track {
            draws.push(Draw::fill(track, screen, rgb(27, 43, 52)));
        }
        if let Some(thumb) = list.thumb {
            draws.push(Draw::fill(inset(thumb, 1., 1.), screen, rgb(81, 105, 120)));
        }

        let rect = self.chat.status_rect();
        let params = TextParams {
            size: [rect[2], rect[3]],
            scale,
            max_side: side,
            wrap: true,
        };
        let status = self.chat.status();
        if status.is_empty() {
            self.status = None;
        } else if !self
            .status
            .as_ref()
            .is_some_and(|cache| cache.matches(status, &params))
        {
            let status = status.to_owned();
            self.status = params.rasterize(&mut self.chat, &status, renderer);
        }
        if let Some(cache) = &self.status {
            draws.push(Draw::image(&cache.image, [rect[0], rect[1]], scale, rect));
        }

        let [field, button] = self.chat.controls();
        let content = editor_content(field);
        let frame = self
            .chat
            .editor_frame([content[2], content[3]], scale, side, self.editor_key);
        let border = if focused {
            rgb(138, 191, 215)
        } else {
            rgb(88, 107, 117)
        };
        draws.push(Draw::fill(field, screen, border));
        draws.push(Draw::fill(inset(field, 1., 1.), field, rgb(12, 20, 24)));
        if send_focused {
            draws.push(Draw::fill(button, screen, rgb(138, 191, 215)));
        }
        let button_fill = if send_focused {
            inset(button, 1., 1.)
        } else {
            button
        };
        draws.push(Draw::fill(button_fill, screen, rgb(48, 66, 76)));
        let params = TextParams {
            size: [button[2], frame.line_height.min(button[3])],
            scale,
            max_side: side,
            wrap: false,
        };
        if !self
            .button
            .as_ref()
            .is_some_and(|cache| cache.matches("Send", &params))
        {
            self.button = params.rasterize(&mut self.chat, "Send", renderer);
        }
        if let Some(cache) = &self.button {
            let [w, h] = cache.image.size.map(|side| side as f32 / scale);
            let center = [
                button[0] + (button[2] - w + 14.) / 2.,
                button[1] + (button[3] - h) / 2.,
            ];
            draws.push(Draw::image(&cache.image, center, scale, button));
        }

        if self.editor_key != Some(frame.bitmap_key) {
            self.editor_key = Some(frame.bitmap_key);
            self.editor = frame.bitmap.map(|bitmap| renderer.image(bitmap));
        }
        let [x, y] = [
            content[0],
            content[1] + (content[3] - frame.line_height) / 2.,
        ];
        for [dx, dy, w, h] in frame.selection {
            draws.push(Draw::fill(
                [x + dx, y + dy, w, h],
                content,
                rgb(36, 77, 104),
            ));
        }
        if let Some(image) = &self.editor {
            draws.push(Draw::image(image, [x, y], scale, content));
        }
        for [dx, dy, w, h] in frame.composition {
            draws.push(Draw::fill([x + dx, y + dy, w, h], content, border));
        }
        let cursor = [x + frame.caret_x, y, 1., frame.line_height];
        if focused && frame.caret_visible {
            draws.push(Draw::fill(cursor, content, rgb(215, 225, 231)));
        }
        (draws, cursor)
    }
}

impl CachedText {
    fn matches(&self, text: &str, params: &TextParams) -> bool {
        self.text == text && self.size == params.size && self.scale == params.scale
    }
}

impl TextParams {
    fn rasterize(&self, model: &mut Chat, text: &str, renderer: &Renderer) -> Option<CachedText> {
        let bitmap = model.raster_text(text, self.size, self.scale, self.wrap, self.max_side)?;
        Some(CachedText {
            text: text.to_owned(),
            size: self.size,
            scale: self.scale,
            image: renderer.image(bitmap),
        })
    }
}

pub fn editor_content(rect: Rect) -> Rect {
    inset(rect, 6., 4.)
}

fn inset([x, y, w, h]: Rect, horizontal: f32, vertical: f32) -> Rect {
    let a = horizontal.min(w / 2.);
    let b = vertical.min(h / 2.);
    [x + a, y + b, w - 2. * a, h - 2. * b]
}

pub fn contains([x, y, w, h]: Rect, [px, py]: [f32; 2]) -> bool {
    px >= x && px < x + w && py >= y && py < y + h
}

const fn rgb(r: u8, g: u8, b: u8) -> Color {
    [r, g, b, 255]
}
