use eframe::egui::{self, Color32, Pos2, Rect, Vec2};
use jsnir::{Mode, Runtime, design};
mod chat;
mod editor;
mod measurement;
mod offscreen;
mod presentation;

fn area(r: design::Rect) -> Rect {
    Rect::from_min_size(
        Pos2::new(r.x as f32, r.y as f32),
        Vec2::new(r.w as f32, r.h as f32),
    )
}
fn paint(ui: &mut egui::Ui, value: u16) {
    design::paint(value, |r| {
        let [_, red, green, blue] = r.rgb.to_be_bytes();
        ui.painter()
            .rect_filled(area(r), 0, Color32::from_rgb(red, green, blue));
    });
}
struct Counter(Runtime);
impl Counter {
    fn show(&mut self, ui: &mut egui::Ui) {
        if ui
            .interact(
                area(design::BUTTON),
                ui.id().with(self.0.view.button),
                egui::Sense::click(),
            )
            .clicked()
        {
            self.0.activate();
        }
        self.0.poll();
        paint(ui, self.0.view.value);
        self.0.drawn();
        if self.0.pending() {
            ui.ctx().request_repaint();
        }
    }
}
impl eframe::App for Counter {
    fn ui(&mut self, ui: &mut egui::Ui, _: &mut eframe::Frame) {
        self.show(ui)
    }
}
fn exact(ctx: &egui::Context) {
    ctx.tessellation_options_mut(|o| o.feathering = false);
}
fn main() -> eframe::Result {
    let mode = Mode::from_args();
    if std::env::args().nth(2).as_deref() == Some("scroll") {
        let out = std::env::args().nth(3).expect("pass a new CSV file");
        offscreen::scroll(mode, out.as_ref());
        return Ok(());
    }
    if std::env::args().nth(2).as_deref() == Some("presentation") {
        // jsnir-egui <mode> presentation counter|chat <new folder>
        let (scene, size) = match std::env::args().nth(3).as_deref() {
            Some("counter") => (
                measurement::Scene::Counter(Counter(Runtime::new(mode))),
                [320., 240.],
            ),
            Some("chat") => (
                measurement::Scene::Chat(chat::ChatView::new(mode)),
                [640., 480.],
            ),
            _ => panic!("expected scene: counter or chat"),
        };
        let out = std::env::args().nth(4).expect("pass a new folder");
        let app = measurement::Measure::new(scene, out.into());
        let options = eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default()
                .with_inner_size(size)
                .with_resizable(false),
            // vsync is on by default in eframe 0.36: glow_options.vsync = true (SwapInterval::Wait(1)).
            ..Default::default()
        };
        return eframe::run_native(
            "JSNIR · measurement",
            options,
            Box::new(move |_| Ok(Box::new(app))),
        );
    }
    if std::env::args().nth(2).as_deref() == Some("chat") {
        let mut chat = chat::ChatView::new(mode);
        // Live reload: saving app/chat.tsx updates the window without restarting or losing the draft.
        if std::env::var_os("JSNIR_LIVE").is_some() {
            chat.model.watch();
        }
        if let Some(out) = std::env::args().nth(3) {
            offscreen::chat_replay(chat, out.as_ref());
            return Ok(());
        }
        let options = eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default()
                .with_inner_size([640., 480.])
                .with_resizable(false),
            ..Default::default()
        };
        return eframe::run_native(
            "JSNIR · Chat",
            options,
            Box::new(move |_| Ok(Box::new(chat))),
        );
    }
    let rt = Runtime::new(mode);
    if let Some(out) = std::env::args().nth(2) {
        offscreen::run(rt, out.as_ref());
        return Ok(());
    }
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([320., 240.])
            .with_resizable(false),
        dithering: false,
        ..Default::default()
    };
    eframe::run_native(
        "JSNIR",
        options,
        Box::new(move |c| {
            exact(&c.egui_ctx);
            Ok(Box::new(Counter(rt)))
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pointer_hits_only_the_button_in_both_modes() {
        for mode in [Mode::Rust, Mode::Js] {
            let ctx = egui::Context::default();
            let mut app = Counter(Runtime::new(mode));
            let frame = |app: &mut Counter, events| {
                let input = egui::RawInput {
                    screen_rect: Some(Rect::from_min_max(Pos2::ZERO, egui::pos2(320., 240.))),
                    events,
                    ..Default::default()
                };
                let _output = ctx.run_ui(input, |ui| app.show(ui));
            };
            frame(&mut app, vec![]);
            for (pos, expected) in [(egui::pos2(12., 12.), 0), (egui::pos2(160., 173.), 1)] {
                frame(&mut app, vec![egui::Event::PointerMoved(pos)]);
                for pressed in [true, false] {
                    frame(
                        &mut app,
                        vec![egui::Event::PointerButton {
                            pos,
                            button: egui::PointerButton::Primary,
                            pressed,
                            modifiers: Default::default(),
                        }],
                    );
                }
                let start = std::time::Instant::now();
                while app.0.pending() {
                    assert!(start.elapsed().as_secs() < 3);
                    frame(&mut app, vec![]);
                }
                assert_eq!(app.0.view.value, expected);
            }
        }
    }
}
