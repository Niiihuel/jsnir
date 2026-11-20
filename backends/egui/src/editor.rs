//! Adapts the JSNIR editor to this window's focus, input and surface.
use eframe::egui::{
    self, Color32, Event, ImeEvent, Key, Modifiers, PointerButton, Pos2, Rect, Response, Ui, Vec2,
};
use jsnir::{
    chat::Chat,
    editor::{Direction, EditorBitmapKey, EditorEvent, PointSelection},
};
use std::time::Duration;

pub fn id() -> egui::Id {
    egui::Id::new("draft")
}

#[derive(Default)]
pub struct EditorView {
    bitmap_key: Option<EditorBitmapKey>,
    texture: Option<egui::TextureHandle>,
    last_input: f64,
}

impl EditorView {
    pub fn show(&mut self, ui: &Ui, model: &mut Chat, area: Rect, input: &Response, send: bool) {
        let content = area.shrink2(egui::vec2(
            6.0_f32.min(area.width() / 2.),
            4.0_f32.min(area.height() / 2.),
        ));
        let focused = input.has_focus();
        let filter = egui::EventFilter {
            horizontal_arrows: true,
            escape: true,
            ..Default::default()
        };
        let mut events = Vec::new();
        if focused {
            ui.memory_mut(|memory| memory.set_focus_lock_filter(id(), filter));
            pointer_events(ui, input, content, &mut events);
            ui.input(|state| events.extend(state.events.iter().filter_map(keyboard_event)));
        } else if model.composing() {
            events.push(EditorEvent::Cancel);
        }
        if send {
            events.push(EditorEvent::Enter);
        }
        let output = model.edit(&events);
        if let Some(text) = output.clipboard {
            ui.ctx().copy_text(text);
        }
        if output.interrupt_ime {
            ui.memory_mut(|memory| memory.interrupt_ime());
        }
        if input.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Text);
        }
        input.widget_info(|| {
            egui::WidgetInfo::text_edit(true, model.draft(), model.draft(), "Message")
        });

        let now = ui.input(|state| state.time);
        if !events.is_empty() || input.gained_focus() {
            self.last_input = now;
        }
        let border = if focused {
            Color32::from_rgb(138, 191, 215)
        } else {
            Color32::from_rgb(88, 107, 117)
        };
        let painter = ui.painter();
        painter.rect(
            area,
            3.,
            Color32::from_rgb(12, 20, 24),
            egui::Stroke::new(1., border),
            egui::StrokeKind::Inside,
        );

        let scale = ui.ctx().pixels_per_point();
        let max_side = ui.input(|state| state.max_texture_side);
        let frame = model.editor_frame(
            [content.width().max(0.), content.height().max(0.)],
            scale,
            max_side,
            self.bitmap_key,
        );
        if Some(frame.bitmap_key) != self.bitmap_key {
            self.bitmap_key = Some(frame.bitmap_key);
            match (frame.bitmap, self.texture.as_mut()) {
                (Some(bitmap), current) => {
                    let image =
                        egui::ColorImage::from_rgba_premultiplied(bitmap.size, &bitmap.rgba);
                    if let Some(texture) = current {
                        texture.set(image, egui::TextureOptions::LINEAR);
                    } else {
                        self.texture = Some(ui.ctx().load_texture(
                            "jsnir-editor",
                            image,
                            egui::TextureOptions::LINEAR,
                        ));
                    }
                }
                (None, _) => self.texture = None,
            }
        }

        let origin = Pos2::new(content.min.x, content.center().y - frame.line_height / 2.);
        let painter = painter.with_clip_rect(content);
        for [x, y, w, h] in frame.selection {
            painter.rect_filled(
                Rect::from_min_size(origin + Vec2::new(x, y), Vec2::new(w, h)),
                0.,
                Color32::from_rgb(36, 77, 104),
            );
        }
        if let Some(texture) = &self.texture {
            let [w, h] = texture.size();
            painter.image(
                texture.id(),
                Rect::from_min_size(origin, Vec2::new(w as f32 / scale, h as f32 / scale)),
                Rect::from_min_max(Pos2::ZERO, Pos2::new(1., 1.)),
                Color32::WHITE,
            );
        }
        for [x, y, w, _] in frame.composition {
            painter.hline(
                origin.x + x..=origin.x + x + w,
                origin.y + y,
                egui::Stroke::new(1., border),
            );
        }

        if focused && area.is_positive() {
            let cursor = Rect::from_min_size(
                origin + Vec2::new(frame.caret_x, 0.),
                Vec2::new(1., frame.line_height),
            );
            let blink = ui.visuals().text_cursor.blink && !model.composing();
            let phase = (now - self.last_input).max(0.).rem_euclid(1.0);
            if frame.caret_visible && (!blink || phase < 0.5) {
                painter.rect_filled(cursor, 0., Color32::from_rgb(215, 225, 231));
            }
            if blink {
                ui.ctx()
                    .request_repaint_after(Duration::from_secs_f64(if phase < 0.5 {
                        0.5 - phase
                    } else {
                        1.0 - phase
                    }));
            }
            let transform = ui
                .ctx()
                .layer_transform_to_global(ui.layer_id())
                .unwrap_or_default();
            ui.output_mut(|out| {
                out.ime = Some(egui::output::IMEOutput {
                    purpose: egui::IMEPurpose::Normal,
                    rect: transform * content,
                    cursor_rect: transform * cursor,
                    should_interrupt_composition: output.interrupt_ime,
                });
            });
        }
    }
}

fn pointer_events(ui: &Ui, response: &Response, content: Rect, events: &mut Vec<EditorEvent>) {
    let point = |pos: Pos2, select| EditorEvent::Point {
        x: pos.x - content.min.x,
        y: pos.y - content.min.y,
        select,
    };
    let primary = PointerButton::Primary;
    let (origin, shift) = ui.input(|input| (input.pointer.press_origin(), input.modifiers.shift));
    if response.drag_started_by(primary)
        && !shift
        && let Some(origin) = origin
    {
        events.push(point(origin, PointSelection::Place));
    }
    if let Some(pos) = response.interact_pointer_pos() {
        let selection = if response.triple_clicked_by(primary) {
            PointSelection::All
        } else if response.double_clicked_by(primary) {
            PointSelection::Word
        } else if response.dragged_by(primary) || (response.clicked_by(primary) && shift) {
            PointSelection::Extend
        } else if response.clicked_by(primary) {
            PointSelection::Place
        } else {
            return;
        };
        events.push(point(pos, selection));
    }
}

fn keyboard_event(event: &Event) -> Option<EditorEvent> {
    match event {
        Event::Text(text) | Event::Paste(text) => Some(EditorEvent::Insert(text.clone())),
        Event::Copy => Some(EditorEvent::Copy),
        Event::Cut => Some(EditorEvent::Cut),
        Event::Ime(ImeEvent::Preedit {
            text,
            active_range_chars,
        }) => Some(EditorEvent::Preedit {
            text: text.clone(),
            active: active_range_chars.clone(),
        }),
        Event::Ime(ImeEvent::Commit(text)) => Some(EditorEvent::Commit(text.clone())),
        Event::Ime(ImeEvent::DeleteSurrounding {
            before_chars,
            after_chars,
        }) => Some(EditorEvent::DeleteSurrounding {
            before: *before_chars,
            after: *after_chars,
        }),
        Event::Key {
            key,
            pressed: true,
            modifiers,
            repeat,
            ..
        } => key_event(*key, *modifiers, *repeat),
        _ => None,
    }
}

fn key_event(key: Key, modifiers: Modifiers, repeat: bool) -> Option<EditorEvent> {
    let word = modifiers.ctrl || modifiers.alt;
    let direction = match key {
        Key::ArrowLeft if modifiers.mac_cmd => Some(Direction::Start),
        Key::ArrowRight if modifiers.mac_cmd => Some(Direction::End),
        Key::ArrowLeft if word => Some(Direction::WordLeft),
        Key::ArrowRight if word => Some(Direction::WordRight),
        Key::ArrowLeft => Some(Direction::Left),
        Key::ArrowRight => Some(Direction::Right),
        Key::Home => Some(Direction::Start),
        Key::End => Some(Direction::End),
        _ => None,
    };
    if let Some(direction) = direction {
        return Some(EditorEvent::Move {
            direction,
            extend: modifiers.shift,
        });
    }
    match key {
        Key::A if modifiers.command => Some(EditorEvent::SelectAll),
        Key::Z if modifiers.command && modifiers.shift => Some(EditorEvent::Redo),
        Key::Z if modifiers.command => Some(EditorEvent::Undo),
        Key::Y if modifiers.command => Some(EditorEvent::Redo),
        Key::Backspace | Key::Delete => Some(EditorEvent::Delete {
            backward: key == Key::Backspace,
            word,
        }),
        Key::Escape => Some(EditorEvent::Cancel),
        Key::Enter if modifiers.is_none() && !repeat => Some(EditorEvent::Enter),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::ChatView;
    use jsnir::Mode;

    fn frame(ctx: &egui::Context, chat: &mut ChatView, events: Vec<Event>) -> egui::PlatformOutput {
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(640., 480.))),
            events,
            ..Default::default()
        };
        ctx.run_ui(input, |ui| {
            chat.show(ui);
        })
        .platform_output
    }

    fn key(key: Key, modifiers: Modifiers) -> Event {
        Event::Key {
            key,
            physical_key: Some(key),
            pressed: true,
            repeat: false,
            modifiers,
        }
    }

    #[test]
    fn routes_clipboard_and_ime_between_the_window_and_jsnir() {
        let ctx = egui::Context::default();
        let mut chat = ChatView::new(Mode::Js);
        let output = frame(&ctx, &mut chat, vec![Event::Paste("té".into())]);
        let initial_caret = output.ime.expect("editor IME area").cursor_rect;
        let output = frame(
            &ctx,
            &mut chat,
            vec![key(Key::A, Modifiers::COMMAND), Event::Copy],
        );
        assert!(
            output
                .commands
                .contains(&egui::OutputCommand::CopyText("té".into()))
        );
        assert_eq!(chat.model.selection(), [0, 2]);

        let output = frame(
            &ctx,
            &mut chat,
            vec![Event::Ime(ImeEvent::Preedit {
                text: "candidate".into(),
                active_range_chars: Some(0..9),
            })],
        );
        let candidate_caret = output.ime.expect("IME area during composition").cursor_rect;
        assert!(candidate_caret.left() > initial_caret.left());
        assert_eq!(chat.model.draft(), "té");

        frame(
            &ctx,
            &mut chat,
            vec![
                Event::Ime(ImeEvent::Commit("茶".into())),
                key(Key::Enter, Modifiers::NONE),
            ],
        );
        assert_eq!(chat.model.draft(), "茶");
        assert!(!chat.model.pending());
        let mut release = key(Key::Enter, Modifiers::NONE);
        if let Event::Key { pressed, .. } = &mut release {
            *pressed = false;
        }
        frame(&ctx, &mut chat, vec![release]);
        frame(&ctx, &mut chat, vec![key(Key::Enter, Modifiers::NONE)]);
        assert!(chat.model.draft().is_empty() && chat.model.pending());
        assert!(ctx.memory(|memory| memory.has_focus(id())));
    }

    #[test]
    fn pointer_drag_selects_text_in_the_editable_field() {
        let ctx = egui::Context::default();
        let mut chat = ChatView::new(Mode::Rust);
        let output = frame(&ctx, &mut chat, vec![Event::Text("one phrase!".into())]);
        let area = output.ime.expect("editor area").rect;
        let start = Pos2::new(area.left(), area.center().y);
        let end = Pos2::new(area.right() - 2., start.y);
        let pointer = |pos, pressed| Event::PointerButton {
            pos,
            button: PointerButton::Primary,
            pressed,
            modifiers: Modifiers::NONE,
        };
        frame(
            &ctx,
            &mut chat,
            vec![Event::PointerMoved(start), pointer(start, true)],
        );
        frame(&ctx, &mut chat, vec![Event::PointerMoved(end)]);
        let output = frame(&ctx, &mut chat, vec![pointer(end, false), Event::Copy]);
        assert_eq!(chat.model.selection(), [0, 11]);
        assert!(
            output
                .commands
                .contains(&egui::OutputCommand::CopyText("one phrase!".into()))
        );
    }
}
