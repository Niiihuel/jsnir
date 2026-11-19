// The benchmark keeps a single event in flight to isolate the cost of each update.
use crate::Runtime;
use std::{
    fs::File,
    io::{BufWriter, Write},
    path::Path,
    time::{Duration, Instant},
};

pub fn replay(
    rt: &mut Runtime,
    mut draw: impl FnMut(u16),
    mut capture: impl FnMut() -> Vec<u8>,
    dir: &Path,
) {
    std::fs::create_dir_all(dir).unwrap();
    let mut csv = BufWriter::new(File::create(dir.join("samples.csv")).unwrap());
    writeln!(csv, "event,commit_ns,render_ns").unwrap();
    for i in 0..=1100 {
        let start = Instant::now();
        if i > 0 {
            assert!(rt.activate());
            while rt.pending() {
                rt.poll();
                assert!(start.elapsed() < Duration::from_secs(5));
                std::thread::yield_now()
            }
        }
        let committed = Instant::now();
        assert_eq!(rt.view.value, i % 1000);
        draw(rt.view.value);
        rt.drawn();
        let rendered = Instant::now();
        if i > 100 {
            writeln!(
                csv,
                "{},{},{}",
                i,
                (committed - start).as_nanos(),
                (rendered - committed).as_nanos()
            )
            .unwrap();
        }
        if [0, 1, 42, 999, 1000].contains(&i) {
            let bytes = capture();
            assert_eq!(bytes.len(), 320 * 240 * 3);
            let mut f = File::create(dir.join(format!("{i:04}.ppm"))).unwrap();
            f.write_all(b"P6\n320 240\n255\n").unwrap();
            f.write_all(&bytes).unwrap();
        }
    }
}
