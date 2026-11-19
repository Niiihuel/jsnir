use eframe::egui;
use glow::HasContext;
use std::{
    ffi::{CString, c_char, c_void},
    path::Path,
    sync::Arc,
};
unsafe extern "C" {
    fn open_gl(width: i32, height: i32) -> i32;
    fn SDL_GL_GetProcAddress(name: *const c_char) -> *const c_void;
    fn SDL_Quit();
}
pub fn run(mut rt: jsnir::Runtime, out: &Path) {
    let (gl, mut painter) = setup(320, 240);
    let ctx = egui::Context::default();
    super::exact(&ctx);
    jsnir::measure::replay(
        &mut rt,
        |value| {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_max(
                    egui::Pos2::ZERO,
                    egui::pos2(320., 240.),
                )),
                ..Default::default()
            };
            let output = ctx.run_ui(input, |ui| super::paint(ui, value));
            paint_frame(&ctx, &mut painter, output, [320, 240]);
            unsafe { gl.finish() }
        },
        || {
            let mut rgb = vec![0; 320 * 240 * 3];
            unsafe {
                gl.read_pixels(
                    0,
                    0,
                    320,
                    240,
                    glow::RGB,
                    glow::UNSIGNED_BYTE,
                    glow::PixelPackData::Slice(Some(&mut rgb)),
                )
            }
            rgb.as_chunks::<{ 320 * 3 }>()
                .0
                .iter()
                .rev()
                .flatten()
                .copied()
                .collect()
        },
        out,
    );
    painter.destroy();
    drop(painter);
    drop(gl);
    unsafe { SDL_Quit() }
}

fn setup(width: i32, height: i32) -> (Arc<glow::Context>, egui_glow::Painter) {
    assert_ne!(unsafe { open_gl(width, height) }, 0, "SDL OpenGL");
    let gl = Arc::new(unsafe {
        glow::Context::from_loader_function(|name| {
            SDL_GL_GetProcAddress(CString::new(name).unwrap().as_ptr())
        })
    });
    eprintln!("GL {}", unsafe { gl.get_parameter_string(glow::RENDERER) });
    let painter = egui_glow::Painter::new(gl.clone(), "", None, false).unwrap();
    (gl, painter)
}
pub fn chat_replay(mut app: super::chat::ChatView, out: &Path) {
    use egui::{Event, Key, Modifiers};
    use std::{
        fs,
        io::Write,
        time::{Duration, Instant},
    };
    fs::create_dir(out).expect("use a new output folder");
    let (gl, mut painter) = setup(640, 480);
    let ctx = egui::Context::default();
    ctx.all_styles_mut(|s| s.visuals.text_cursor.blink = false);
    let mut frame = |app: &mut super::chat::ChatView, events: Vec<Event>, name: Option<&str>| {
        let input = egui::RawInput {
            time: Some(1.),
            screen_rect: Some(egui::Rect::from_min_max(
                egui::Pos2::ZERO,
                egui::pos2(640., 480.),
            )),
            events,
            ..Default::default()
        };
        let output = ctx.run_ui(input, |ui| {
            app.show(ui);
        });
        paint_frame(&ctx, &mut painter, output, [640, 480]);
        if let Some(name) = name {
            let mut rgb = vec![0; 640 * 480 * 3];
            unsafe {
                gl.read_pixels(
                    0,
                    0,
                    640,
                    480,
                    glow::RGB,
                    glow::UNSIGNED_BYTE,
                    glow::PixelPackData::Slice(Some(&mut rgb)),
                )
            }
            let mut file =
                std::io::BufWriter::new(fs::File::create(out.join(format!("{name}.ppm"))).unwrap());
            file.write_all(b"P6\n640 480\n255\n").unwrap();
            for row in rgb.as_chunks::<{ 640 * 3 }>().0.iter().rev() {
                file.write_all(row).unwrap()
            }
            file.flush().unwrap();
        }
    };
    let key = |key, shift| {
        [true, false].map(|pressed| Event::Key {
            key,
            physical_key: Some(key),
            pressed,
            repeat: false,
            modifiers: Modifiers {
                shift,
                ..Default::default()
            },
        })
    };
    frame(&mut app, vec![], None);
    frame(&mut app, vec![], Some("A"));
    frame(
        &mut app,
        vec![Event::Text("Hello, Córdoba".into())],
        Some("B"),
    );
    let start = Instant::now();
    frame(&mut app, key(Key::Enter, false).to_vec(), None);
    frame(&mut app, vec![], Some("C"));
    assert!(app.model.pending() && app.model.draft().is_empty());
    frame(&mut app, vec![Event::Text("a second draft".into())], None);
    let events = (0..8).flat_map(|_| key(Key::ArrowLeft, true)).collect();
    frame(&mut app, events, Some("D"));
    let check_editor = |app: &super::chat::ChatView| {
        assert!(ctx.memory(|m| m.has_focus(super::editor::id())));
        assert_eq!(app.model.selection(), [6, 14]);
        assert_eq!(app.model.draft(), "a second draft");
    };
    check_editor(&app);
    assert_eq!(app.model.history_len(), 0);
    let edited = start.elapsed();
    while app.model.pending() {
        assert!(start.elapsed() < Duration::from_secs(5));
        std::thread::sleep(Duration::from_millis(2));
        frame(&mut app, vec![], None);
    }
    frame(&mut app, vec![], Some("E"));
    check_editor(&app);
    assert_eq!(app.model.history_len(), 1);
    assert_eq!(app.model.message(0), "Hello, Córdoba");
    // The app read from its replica, without waiting, the draft and selection written during its delay.
    assert_eq!(app.model.status(), "draft «a second draft», selection 6–14");
    println!(
        "D={} ms E={} ms",
        edited.as_millis(),
        start.elapsed().as_millis()
    );
    painter.destroy();
    drop(painter);
    drop(gl);
    unsafe { SDL_Quit() }
}

fn paint_frame(
    ctx: &egui::Context,
    painter: &mut egui_glow::Painter,
    output: egui::FullOutput,
    size: [u32; 2],
) {
    for (id, deltas) in &output.textures_delta.set {
        for delta in deltas {
            painter.set_texture(*id, delta)
        }
    }
    let meshes = ctx.tessellate(output.shapes, output.pixels_per_point);
    painter.paint_primitives(size, 1., &meshes);
    for &id in &output.textures_delta.free {
        painter.free_texture(id)
    }
}
pub fn scroll(mode: jsnir::Mode, out: &Path) {
    use std::{
        io::Write,
        time::{Duration, Instant},
    };
    let mut app = super::chat::ChatView::new(mode);
    app.model = jsnir::chat::Chat::new(mode, Duration::ZERO);
    let deadline = Instant::now() + Duration::from_secs(30);
    for i in 0..10000 {
        app.model
            .set_draft(&format!("Message {i:05} · Córdoba"), [0, 0]);
        while !app.model.submit() {
            assert!(Instant::now() < deadline);
            app.model.poll();
            std::thread::yield_now()
        }
    }
    while app.model.pending() {
        assert!(Instant::now() < deadline);
        app.model.poll();
        std::thread::yield_now()
    }
    assert_eq!(app.model.history_len(), 10000);
    let (gl, mut painter) = setup(640, 480);
    let mut csv = std::io::BufWriter::new(std::fs::File::create_new(out).unwrap());
    writeln!(csv, "frame,first_row,end_exclusive,frame_ns").unwrap();
    let ctx = egui::Context::default();
    let mut visited = vec![false; 10000];
    for i in 0..1200 {
        let mut events = vec![egui::Event::PointerMoved(egui::pos2(140., 180.))];
        if i >= 100 {
            events.push(egui::Event::MouseWheel {
                unit: egui::MouseWheelUnit::Point,
                phase: egui::TouchPhase::Move,
                delta: egui::vec2(0., 240.),
                modifiers: Default::default(),
            });
        }
        let input = egui::RawInput {
            time: Some(i as f64 / 60.),
            screen_rect: Some(egui::Rect::from_min_max(
                egui::Pos2::ZERO,
                egui::pos2(640., 480.),
            )),
            events,
            ..Default::default()
        };
        let mut rows = 0..0;
        let start = Instant::now();
        let output = ctx.run_ui(input, |ui| {
            rows = app.show(ui);
        });
        paint_frame(&ctx, &mut painter, output, [640, 480]);
        unsafe { gl.finish() }
        let ns = start.elapsed().as_nanos();
        assert!(rows.len() <= 25, "virtualization");
        visited[rows.clone()].fill(true);
        if i >= 100 {
            writeln!(csv, "{i},{},{},{ns}", rows.start, rows.end).unwrap();
        }
    }
    assert!(
        visited.iter().all(|v| *v),
        "the scroll did not cover the whole history"
    );
    csv.flush().unwrap();
    painter.destroy();
    drop(painter);
    drop(gl);
    unsafe { SDL_Quit() }
}
