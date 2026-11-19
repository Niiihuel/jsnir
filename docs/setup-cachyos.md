# Historical setup on CachyOS

These instructions record how Iced, Masonry and Slint were built before JSNIR adopted the GPU backend and kept egui as its measurement baseline. The commands use the historical source snapshot, including the `jurui-*` binaries. [Current JSNIR setup](../README.md#run-the-stepper)

Checked on the project machine on 18 November 2026. The three backends already had their dependencies declared. The first step, in a terminal with Internet access, is to let Cargo download and build them. The user's normal account is used.

## 1. Build the three counters

Run from the root of the historical snapshot.

```bash
for backend in iced masonry slint; do
    cargo build --release --target-dir target \
        --manifest-path "backends/$backend/Cargo.toml" || break
done
```

| Backend | Pinned direct dependencies | Expected executable |
| --- | --- | --- |
| Iced | iced 0.13.1 with canvas and tokio | `target/release/jurui-iced` |
| Masonry | masonry 0.4.0 and masonry_winit 0.4.0 | `target/release/jurui-masonry` |
| Slint | slint 1.13.1 | `target/release/jurui-slint` |

The first resolution creates a `Cargo.lock` next to each backend's manifest. Keep those files with the results. In later runs, use `cargo build --locked` with the same options. To only prefetch the dependencies, run `cargo fetch --manifest-path backends/iced/Cargo.toml` and repeat it for masonry and slint. [Cargo reference](https://doc.rust-lang.org/cargo/commands/cargo-fetch.html)

To open the counter from your graphical session, run each mode separately and close the window before opening the next one. Replace `iced` with `masonry` or `slint` to try the other two.

```bash
./target/release/jurui-iced rust
./target/release/jurui-iced js
```

## 2. Install the visual comparison tools

This machine already has xdotool 4.20260303.1 and xorg-server-xvfb 21.1.26, plus Python, ImageMagick and xauth. To reproduce the installation on CachyOS:

```bash
sudo pacman -S --needed xdotool xorg-server-xvfb
command -v xdotool Xvfb xvfb-run magick
```

xdotool generates the clicks. Xvfb creates an isolated virtual desktop, and the package includes the xvfb-run command. [Arch Linux, contents of xorg-server-xvfb](https://archlinux.org/packages/extra/x86_64/xorg-server-xvfb/files/)

## 3. Check the same design in both modes

```bash
series=$(mktemp -d "$PWD/metrics/local-windows-XXXXXX")
for backend in iced masonry slint; do
    xvfb-run -a -s "-screen 0 800x600x24" \
        python scripts/window_pair.py "$backend" "$series/$backend" || break
done
```

The script was configured to write five PNGs and a `paridad.json` per mode. The criterion was zero differing pixels against each reference image. The current `scripts/window_pair.py` runs the egui counter and writes `parity.json`. [Current capture script](../scripts/window_pair.py)

## Diagnosis of this machine

| Check | Result |
| --- | --- |
| Toolchain and host | rustc 1.99.1, Cargo, GCC, CMake, pkgconf, Node 26 and Duktape 2.7 present |
| Windows and graphics | Wayland, X11, libxkbcommon, Mesa, Vulkan loader and Radeon driver installed |
| Desktop dependencies | fontconfig, FreeType, Qt 6 base and declarative present |
| Captures | Python, ImageMagick, xdotool and Xvfb installed |
| Download from the work session | The first build failed to resolve index.crates.io. Running `cargo fetch` separately for each manifest downloaded the three dependency sets and generated their Cargo.lock files |
| Backend status | Iced, Masonry and Slint built in release. The full loop was repeated successfully and ldd resolved the libraries of the three binaries. Visual parity pending |

The startup test with `xvfb-run -a xdotool getdisplaygeometry` failed in this session when creating the X11 socket. Xvfb reported `Cannot establish any listening sockets`. No captures were obtained for these three backends.

If the first block fails, keep the first Cargo error. A failure resolving `index.crates.io` happens during download. A Rust error with a file and line number happens while building the backend. Each case needs a different fix.

The follow-up chat test defined a shared editing script and measured it in egui. [Local chat test](chat-test-02.md)

The user's terminal confirmed an 800 × 600 Xvfb screen. The later check also used the offscreen renderers of Iced, Slint and Masonry. [Additional counter results](research.md#additional-counter-verification-and-the-chat-decision)
