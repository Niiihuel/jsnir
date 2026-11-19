use crate::editor::{self, EditorView};
use eframe::egui::{self, Pos2, Rect, Ui};
use jsnir::{Mode, chat::Chat, history::ScrollEvent};
use std::{collections::HashMap, time::Duration};

pub struct ChatView {
    pub model: Chat,
    initial_focus: bool,
    editor: EditorView,
    status_texture: Option<TextTexture>,
    message_textures: HashMap<usize, Option<TextTexture>>,
}

struct TextTexture {
    text: String,
    size: [f32; 2],
    scale: f32,
    max_side: usize,
    texture: egui::TextureHandle,
}

fn paint_text(
    ui: &Ui,
    model: &mut Chat,
    cached: &mut Option<TextTexture>,
    rect: Rect,
    message: Option<usize>,
) {
    let text = match message {
        Some(i) => model.message(i),
        None => model.status(),
    };
    if text.is_empty() || !rect.is_positive() || !ui.is_rect_visible(rect) {
        *cached = None;
        return;
    }
    let size = [rect.width(), rect.height()];
    let scale = ui.ctx().pixels_per_point();
    let max_side = ui.input(|input| input.max_texture_side);
    if !cached.as_ref().is_some_and(|cache| {
        cache.text == text
            && cache.size == size
            && cache.scale == scale
            && cache.max_side == max_side
    }) {
        let text = text.to_owned();
        *cached = model
            .raster_text(&text, size, scale, message.is_none(), max_side)
            .map(|bitmap| {
                let image = egui::ColorImage::from_rgba_premultiplied(bitmap.size, &bitmap.rgba);
                let texture =
                    ui.ctx()
                        .load_texture("jsnir-text", image, egui::TextureOptions::LINEAR);
                TextTexture {
                    text,
                    size,
                    scale,
                    max_side,
                    texture,
                }
            });
    }
    if let Some(cache) = cached {
        let [width, height] = cache.texture.size();
        let bounds = Rect::from_min_size(
            rect.min,
            egui::vec2(width as f32 / scale, height as f32 / scale),
        );
        let uv = Rect::from_min_max(Pos2::ZERO, Pos2::new(1., 1.));
        ui.painter().with_clip_rect(rect).image(
            cache.texture.id(),
            bounds,
            uv,
            egui::Color32::WHITE,
        );
    }
}

impl ChatView {
    pub fn new(mode: Mode) -> Self {
        Self {
            model: Chat::new(mode, Duration::from_millis(500)),
            initial_focus: true,
            editor: EditorView::default(),
            status_texture: None,
            message_textures: HashMap::new(),
        }
    }
    pub fn show(&mut self, ui: &mut Ui) -> std::ops::Range<usize> {
        self.model.poll();
        let screen = ui.max_rect();
        ui.painter()
            .rect_filled(screen, 0, egui::Color32::from_rgb(19, 32, 38));
        // Sizes come from the Taffy layout, computed in the core for the window size.
        self.model.layout(screen.width(), screen.height());
        let area = |[x, y, w, h]: [f32; 4]| {
            Rect::from_min_size(screen.min + egui::vec2(x, y), egui::vec2(w, h))
        };
        let history = area(self.model.history_rect());
        let [text_rect, button_rect] = self.model.controls().map(area);
        if ui.rect_contains_pointer(history) {
            let delta = ui.input(|input| input.smooth_scroll_delta().y);
            if delta != 0. {
                self.model.scroll_history(ScrollEvent::Wheel(-delta));
            }
        }
        let mut list = self.model.history_view();
        if let Some(track) = list.track {
            let response = ui.interact(
                area(track),
                ui.id().with("history-scroll"),
                egui::Sense::click_and_drag(),
            );
            if response.drag_started()
                && let Some(origin) = ui.input(|input| input.pointer.press_origin())
            {
                self.model
                    .scroll_history(ScrollEvent::Grab(origin.y - screen.min.y));
            }
            if let Some(pos) = response.interact_pointer_pos() {
                let y = pos.y - screen.min.y;
                if response.dragged() {
                    self.model.scroll_history(ScrollEvent::Drag(y));
                } else if response.clicked() {
                    self.model.scroll_history(ScrollEvent::Grab(y));
                    self.model.scroll_history(ScrollEvent::Release);
                }
            }
            if response.drag_stopped() {
                self.model.scroll_history(ScrollEvent::Release);
            }
            list = self.model.history_view();
            ui.painter()
                .rect_filled(area(track), 3., egui::Color32::from_rgb(27, 43, 52));
            if let Some(thumb) = list.thumb {
                ui.painter().rect_filled(
                    area(thumb).shrink(1.),
                    3.,
                    egui::Color32::from_rgb(81, 105, 120),
                );
            }
        }
        ui.scope_builder(egui::UiBuilder::new().max_rect(history), |ui| {
            ui.set_clip_rect(ui.clip_rect().intersect(area(list.content)));
            for row in list.rows.clone() {
                let rect = area(list.row_rect(row));
                ui.interact(rect, ui.id().with(row), egui::Sense::hover())
                    .widget_info(|| {
                        egui::WidgetInfo::labeled(
                            egui::WidgetType::Label,
                            true,
                            self.model.message(row),
                        )
                    });
                let cached = self.message_textures.entry(row).or_default();
                paint_text(ui, &mut self.model, cached, rect, Some(row));
            }
        });
        let visible = list.rows;
        self.message_textures.retain(|row, _| visible.contains(row));
        let status = area(self.model.status_rect());
        paint_text(ui, &mut self.model, &mut self.status_texture, status, None);
        let input = ui.interact(text_rect, editor::id(), egui::Sense::click_and_drag());
        if self.initial_focus || input.clicked() || input.drag_started() {
            input.request_focus();
            self.initial_focus = false;
        }
        let send = ui.put(button_rect, egui::Button::new("Send")).clicked();
        self.editor
            .show(ui, &mut self.model, text_rect, &input, send);
        if send {
            input.request_focus();
        }
        if self.model.pending() {
            ui.ctx().request_repaint_after(Duration::from_millis(8));
        } else if self.model.watching() {
            // egui does not redraw an idle window: this picks up a reload even when nobody touches anything.
            ui.ctx().request_repaint_after(Duration::from_millis(100));
        }
        visible
    }
}
impl eframe::App for ChatView {
    fn ui(&mut self, ui: &mut Ui, _: &mut eframe::Frame) {
        self.show(ui);
    }
}
