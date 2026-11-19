# Preparar Iced, Masonry y Slint en CachyOS

Comprobado en el equipo de Jurui el 18 de noviembre de 2026. Los tres adaptadores ya tienen sus dependencias declaradas. El primer paso en una terminal con Internet es dejar que Cargo las descargue y compile. Se usa la cuenta normal del usuario.

## 1. Compilar los tres contadores

```bash
cd ~/proyectos/personal/Jurui
for backend in iced masonry slint; do
    cargo build --release --target-dir target \
        --manifest-path "backends/$backend/Cargo.toml" || break
done
```

| Adaptador | Dependencias directas fijadas | Ejecutable esperado |
| --- | --- | --- |
| Iced | iced 0.13.1 con canvas y tokio | `target/release/jurui-iced` |
| Masonry | masonry 0.4.0 y masonry_winit 0.4.0 | `target/release/jurui-masonry` |
| Slint | slint 1.13.1 | `target/release/jurui-slint` |

La primera resolución crea un `Cargo.lock` junto al manifiesto de cada adaptador. Conservá esos archivos con los resultados. En corridas posteriores usá `cargo build --locked` con las mismas opciones. Si solo querés precargar las dependencias, podés ejecutar `cargo fetch --manifest-path backends/iced/Cargo.toml` y repetirlo para masonry y slint. [Referencia de Cargo](https://doc.rust-lang.org/cargo/commands/cargo-fetch.html)

Para abrir el contador desde tu sesión gráfica, ejecutá cada modo por separado y cerrá la ventana antes de abrir el siguiente. Sustituí `iced` por `masonry` o `slint` para probar los otros dos.

```bash
./target/release/jurui-iced rust
./target/release/jurui-iced js
```

## 2. Instalar las herramientas de comparación visual

En este equipo ya están instalados xdotool 4.20260303.1 y xorg-server-xvfb 21.1.26, además de Python, ImageMagick y xauth. Para reproducir la instalación en CachyOS:

```bash
sudo pacman -S --needed xdotool xorg-server-xvfb
command -v xdotool Xvfb xvfb-run magick
```

xdotool genera los clics. Xvfb crea un escritorio virtual aislado y el paquete incluye el comando xvfb-run. [Arch Linux, contenido de xorg-server-xvfb](https://archlinux.org/packages/extra/x86_64/xorg-server-xvfb/files/)

## 3. Comprobar el mismo diseño en ambos modos

```bash
cd ~/proyectos/personal/Jurui
serie=$(mktemp -d "$PWD/metrics/local-ventanas-XXXXXX")
for backend in iced masonry slint; do
    xvfb-run -a -s "-screen 0 800x600x24" \
        python scripts/window_pair.py "$backend" "$serie/$backend" || break
done
```

Cada backend escribe cinco PNG y un `paridad.json` por modo. El criterio es cero píxeles diferentes frente a cada imagen de referencia. Un error al abrir el dispositivo gráfico bajo Xvfb requiere revisar el renderer y la disponibilidad de GPU en esa sesión. Primero se puede comprobar la apertura del binario en el escritorio habitual. Documentá por separado las corridas por hardware y por software.

## Diagnóstico de este equipo

| Verificación | Resultado |
| --- | --- |
| Toolchain y host | rustc 1.99.1, Cargo, GCC, CMake, pkgconf, Node 26 y Duktape 2.7 presentes |
| Ventanas y gráficos | Wayland, X11, libxkbcommon, Mesa, cargador Vulkan y driver Radeon instalados |
| Dependencias de escritorio | fontconfig, FreeType, Qt 6 base y declarativo presentes |
| Capturas | Python, ImageMagick, xdotool y Xvfb instalados |
| Descarga desde la sesión de trabajo | El primer build falló al resolver index.crates.io. Ejecutar `cargo fetch` por separado para cada manifiesto descargó las tres colecciones de dependencias y generó sus archivos Cargo.lock |
| Estado de los adaptadores | Iced, Masonry y Slint compilados en release. Se repitió el bucle completo con éxito y ldd resolvió las bibliotecas de los tres binarios. Paridad visual pendiente |

La prueba de arranque con `xvfb-run -a xdotool getdisplaygeometry` falló en esta sesión al crear el socket X11. Xvfb informó `Cannot establish any listening sockets`. No se obtuvieron capturas para estos tres backends.

Si falla el primer bloque, conservá el primer error de Cargo. Un fallo al resolver `index.crates.io` ocurre durante la descarga. Un error de Rust con archivo y número de línea ocurre al compilar el adaptador. Cada caso requiere una corrección distinta.

Después del contador, la [prueba de chat local](prueba-02-chat.md) conserva la comparación Rust puro / JS + Rust para cada backend.

La terminal del usuario confirmó una pantalla Xvfb de 800 × 600. Para la verificación posterior se usaron también los renderers fuera de pantalla de Iced, Slint y Masonry. Los comandos y los resultados de los dos modos se encuentran en el [cuaderno de investigación](investigacion.md#verificación-adicional-del-contador-y-decisión-para-el-chat).
