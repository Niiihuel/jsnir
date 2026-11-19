//! Latency up to the real presentation. Each event is injected at the start of a frame (raw_input_hook) and
//! measured until the first presented frame whose content shows it. The kernel → compositor → app path
//! is the same in every mode, so the comparison covers what the bridge changes.
use crate::presentation::{Presentation, now_ns};
use eframe::egui;
use raw_window_handle::{HasDisplayHandle, HasWindowHandle, RawDisplayHandle, RawWindowHandle};
use std::{collections::HashMap, fmt::Write as _, path::PathBuf};

const WARMUP: u64 = 90;
const INTERVAL: u64 = 15;
const EVENTS: usize = 120;

pub enum Scene {
    Counter(super::Counter),
    Chat(super::chat::ChatView),
}
impl Scene {
    // What the frame shows: the counter value or the draft length.
    fn observed(&self) -> u64 {
        match self {
            Scene::Counter(c) => c.0.view.value as u64,
            Scene::Chat(c) => c.model.draft().chars().count() as u64,
        }
    }
    fn show(&mut self, ui: &mut egui::Ui) {
        match self {
            Scene::Counter(c) => c.show(ui),
            Scene::Chat(c) => drop(c.show(ui)),
        }
    }
    fn expected(&self) -> u64 {
        match self {
            Scene::Counter(_) => (self.observed() + 1) % 1000,
            Scene::Chat(_) => self.observed() + 1,
        }
    }
    // Events for step `step` of the gesture. The counter click completes on release, in the second frame.
    fn gesture(&self, step: u64) -> (Vec<egui::Event>, bool) {
        let pos = egui::pos2(160., 173.);
        let button = |pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: Default::default(),
        };
        match (self, step) {
            (Scene::Counter(_), 0) => (vec![egui::Event::PointerMoved(pos), button(true)], false),
            (Scene::Counter(_), 1) => (vec![button(false)], true),
            (Scene::Chat(_), 0) => (vec![egui::Event::Text("x".into())], true),
            _ => (vec![], false),
        }
    }
}
pub struct Measure {
    scene: Scene,
    out: PathBuf,
    presentation: Option<Presentation>,
    clock: u32,
    frame: u64,
    frame_start: u64,
    // (event, injection frame, ns, expected value)
    events: Vec<(usize, u64, u64, u64)>,
    // (frame, start in ns, shown value)
    frames: Vec<(u64, u64, u64)>,
    pending_expected: Option<u64>,
    // Real system events dropped: the person's pointer or keyboard must not mix with the injection.
    discarded_input: usize,
}
impl Measure {
    pub fn new(scene: Scene, out: PathBuf) -> Self {
        std::fs::create_dir(&out).expect("use a new output folder");
        Self {
            scene,
            out,
            presentation: None,
            clock: 1,
            frame: 0,
            frame_start: 0,
            events: Vec::new(),
            frames: Vec::new(),
            pending_expected: None,
            discarded_input: 0,
        }
    }
    fn write(&mut self) {
        let p = self.presentation.as_mut().unwrap();
        p.poll();
        let presented: HashMap<u64, (u64, u32, u32)> = p
            .feedback
            .presented
            .iter()
            .map(|&(f, ns, r, fl)| (f, (ns, r, fl)))
            .collect();
        let mut csv = String::from("frame,start_ns,observed,presented_ns,refresh_ns,flags\n");
        for &(f, start, obs) in &self.frames {
            let (ns, r, fl) = presented.get(&f).map_or(
                (String::new(), String::new(), String::new()),
                |(a, b, c)| (a.to_string(), b.to_string(), c.to_string()),
            );
            writeln!(csv, "{f},{start},{obs},{ns},{r},{fl}").unwrap();
        }
        std::fs::write(self.out.join("frames.csv"), csv).unwrap();
        let mut csv = String::from("event,frame,inject_ns,expected\n");
        for (e, f, ns, exp) in &self.events {
            writeln!(csv, "{e},{f},{ns},{exp}").unwrap();
        }
        std::fs::write(self.out.join("events.csv"), csv).unwrap();
        let meta = format!(
            "{{\"clock\":{},\"presented\":{},\"discarded\":{},\"frames\":{},\"discarded_real_input\":{}}}\n",
            self.clock,
            presented.len(),
            p.feedback.discarded.len(),
            self.frames.len(),
            self.discarded_input
        );
        std::fs::write(self.out.join("meta.json"), meta).unwrap();
    }
}
impl eframe::App for Measure {
    fn raw_input_hook(&mut self, _: &egui::Context, raw: &mut egui::RawInput) {
        self.frame_start = now_ns(self.clock);
        let before = raw.events.len();
        raw.events.retain(|e| {
            !matches!(
                e,
                egui::Event::PointerMoved(_)
                    | egui::Event::MouseMoved(_)
                    | egui::Event::PointerButton { .. }
                    | egui::Event::PointerGone
                    | egui::Event::Text(_)
                    | egui::Event::Key { .. }
                    | egui::Event::MouseWheel { .. }
            )
        });
        self.discarded_input += before - raw.events.len();
        if self.presentation.is_none() || self.frame < WARMUP || self.events.len() >= EVENTS {
            return;
        }
        let step = (self.frame - WARMUP) % INTERVAL;
        if step == 0 {
            self.pending_expected = Some(self.scene.expected());
        }
        let (events, completes) = self.scene.gesture(step);
        raw.events.extend(events);
        if completes {
            let expected = self.pending_expected.take().unwrap();
            self.events
                .push((self.events.len(), self.frame, self.frame_start, expected));
        }
    }
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        if self.presentation.is_none() {
            let (Ok(w), Ok(d)) = (frame.window_handle(), frame.display_handle()) else {
                panic!("no window handles")
            };
            let (RawWindowHandle::Wayland(w), RawDisplayHandle::Wayland(d)) =
                (w.as_raw(), d.as_raw())
            else {
                panic!("presentation measurement needs a Wayland window")
            };
            // SAFETY: winit keeps the display and surface alive while the window exists.
            let p = unsafe { Presentation::new(d.display.as_ptr(), w.surface.as_ptr()) }
                .unwrap_or_else(|e| panic!("{e}"));
            self.clock = p
                .feedback
                .clock
                .expect("the compositor did not announce its clock");
            eprintln!("wp_presentation ready, clock {}", self.clock);
            self.presentation = Some(p);
        }
        self.scene.show(ui);
        self.frames
            .push((self.frame, self.frame_start, self.scene.observed()));
        let p = self.presentation.as_mut().unwrap();
        p.request(self.frame);
        p.poll();
        self.frame += 1;
        if self.frame.is_multiple_of(300) {
            let p = self.presentation.as_ref().unwrap();
            eprintln!(
                "frame {}: {} events, {} presented, {} discarded",
                self.frame,
                self.events.len(),
                p.feedback.presented.len(),
                p.feedback.discarded.len()
            );
        }
        let last = WARMUP + INTERVAL * EVENTS as u64;
        if self.frame == last + 120 {
            self.write();
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
        }
        ui.ctx().request_repaint();
    }
}
