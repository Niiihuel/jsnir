// Cost of Chat::layout per frame: steady (nothing changed, Taffy uses its cache) and resizing (recomputes and publishes).
use jsnir::{Mode, chat::Chat};
use std::time::{Duration, Instant};
fn stats(mut v: Vec<u128>) -> String {
    v.sort_unstable();
    let q = |p: f64| v[((v.len() - 1) as f64 * p) as usize] as f64 / 1000.;
    format!("p50 {:.3} µs, p99 {:.3} µs", q(0.5), q(0.99))
}
fn main() {
    for mode in [Mode::Rust, Mode::Js] {
        let mut chat = Chat::new(mode, Duration::ZERO);
        // A large history: its rows do not go through Taffy.
        for i in 0..10_000 {
            chat.set_draft(&format!("message {i}"), [0, 0]);
            while !chat.submit() {
                chat.poll();
            }
        }
        while chat.pending() {
            chat.poll();
        }
        chat.layout(640., 480.);
        let measure = |chat: &mut Chat, resize: bool| {
            (0..11_000)
                .map(|i| {
                    let w = if resize { 640. + (i % 2) as f32 } else { 640. };
                    let t = Instant::now();
                    chat.layout(w, 480.);
                    t.elapsed().as_nanos()
                })
                .skip(1000)
                .collect::<Vec<_>>()
        };
        println!(
            "{mode:?} with {} messages · steady: {}",
            chat.history_len(),
            stats(measure(&mut chat, false))
        );
        println!(
            "{mode:?} with {} messages · resizing: {}",
            chat.history_len(),
            stats(measure(&mut chat, true))
        );
    }
}
