//! Asset loaders (fonts and PNG images) used for startup loading and zero-VM-restart hot reload.
use jsnir::host::Host;
use jsnir_gpu::Pusher;
use std::{
    fs::{self, File},
    io::BufReader,
    path::{Path, PathBuf},
};

/// Decodes a PNG file into straight RGBA8 pixels and `[width, height]`.
pub fn decode_png(path: &Path) -> Result<(Vec<u8>, [u32; 2]), String> {
    let file = File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut decoder = png::Decoder::new(BufReader::new(file));
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder
        .read_info()
        .map_err(|e| format!("{}: {e}", path.display()))?;
    let mut buf = vec![0; reader.output_buffer_size()];
    let info = reader
        .next_frame(&mut buf)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    let bytes = &buf[..info.buffer_size()];
    let rgba = match info.color_type {
        png::ColorType::Rgba => bytes.to_vec(),
        png::ColorType::Rgb => bytes
            .chunks_exact(3)
            .flat_map(|c| [c[0], c[1], c[2], 255])
            .collect(),
        png::ColorType::GrayscaleAlpha => bytes
            .chunks_exact(2)
            .flat_map(|c| [c[0], c[0], c[0], c[1]])
            .collect(),
        png::ColorType::Grayscale => bytes.iter().flat_map(|&g| [g, g, g, 255]).collect(),
        other => return Err(format!("{}: unsupported PNG color type {other:?}", path.display())),
    };
    Ok((rgba, [info.width, info.height]))
}

/// Loads configured fonts and images into a `Host` before opening the window.
pub fn load_into_host(
    host: &mut Host,
    fonts: &[PathBuf],
    default_family: Option<&str>,
    images_dir: Option<&Path>,
) {
    for font in fonts {
        if let Ok(bytes) = fs::read(font) {
            host.add_font(bytes);
        }
    }
    if let Some(family) = default_family {
        host.set_default_family(family.to_string());
    }
    if let Some(dir) = images_dir {
        for (source, path) in scan_pngs(dir) {
            if let Ok((rgba, size)) = decode_png(&path) {
                host.set_image(source, rgba, size);
            }
        }
    }
}

/// Pushes a single changed font file to the live window without restarting QuickJS.
pub fn push_font(pusher: &Pusher, path: &Path) -> Result<(), String> {
    let bytes = fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    pusher.font(bytes);
    Ok(())
}

/// Pushes a single changed PNG image to the live window without restarting QuickJS.
pub fn push_image(pusher: &Pusher, images_dir: &Path, path: &Path) -> Result<String, String> {
    let source = image_source_name(images_dir, path);
    let (rgba, size) = decode_png(path)?;
    pusher.image(source.clone(), rgba, size);
    Ok(source)
}

pub fn image_source_name(images_dir: &Path, path: &Path) -> String {
    let rel = path.strip_prefix(images_dir).unwrap_or(path);
    rel.to_string_lossy().replace('\\', "/")
}

fn scan_pngs(dir: &Path) -> Vec<(String, PathBuf)> {
    let mut out = Vec::new();
    collect_pngs(dir, dir, &mut out);
    out
}

fn collect_pngs(base: &Path, current: &Path, out: &mut Vec<(String, PathBuf)>) {
    let Ok(entries) = fs::read_dir(current) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_pngs(base, &path, out);
        } else if path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("png"))
        {
            out.push((image_source_name(base, &path), path));
        }
    }
}
