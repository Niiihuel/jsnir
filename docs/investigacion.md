# Contador v1 · Cuaderno de investigación

> **Nota (19 de noviembre de 2026).** El proyecto pasó a llamarse **JSNIR** y el código quedó en inglés. Este cuaderno se conserva en español como registro histórico y nombra a Jurui con su nombre de entonces. Los experimentos cerrados (protocolos JSON, directo y buffer, `src/lectura.rs`, `examples/estructuras.rs`, `scripts/curva.py` y `app/counter.ts`) se retiraron del código; sus resultados siguen acá y en `metrics/`.

## Pregunta

Si Rust conserva el estado que se dibuja y un clic puede hacer un viaje de ida y vuelta a JavaScript, ¿se conserva exactamente la imagen? ¿Qué coste tiene ese viaje en esta carga?

## Primera serie · 18 de noviembre de 2026

Linux 7.3.4 CachyOS, AMD Ryzen 9 270, rustc 1.99.1, perfil release. egui usó OpenGL offscreen con llvmpipe (LLVM 23.1.4). Jurui dibujó sobre un buffer RGB en Rust y actualizó una superficie SDL2 con controlador dummy. Ambos renderers se ejecutaron por software. La Radeon integrada no intervino en esta serie.

Tres procesos nuevos por combinación, 100 activaciones de calentamiento y 1.000 medidas por proceso. El orden de los cuatro casos se invirtió en la segunda repetición. Un solo evento en vuelo, sin sincronización con el monitor. Cada fila muestra la mediana de los tres percentiles por proceso. Todos los tiempos están en **microsegundos**.

| Backend | Modo | Cambio aplicado p50 / p95 / p99 | Dibujo p50 / p95 / p99 |
| --- | --- | ---: | ---: |
| egui | Rust | 0,030 / 0,071 / 0,170 | 113,670 / 128,447 / 146,911 |
| egui | JS | 8,896 / 15,448 / 19,235 | 115,863 / 134,909 / 297,128 |
| Jurui propio | Rust | 0,020 / 0,021 / 0,040 | 53,418 / 57,666 / 71,191 |
| Jurui propio | JS | 6,763 / 8,045 / 34,955 | 53,959 / 57,115 / 81,921 |

El primer intervalo comienza antes de despachar la activación y termina al aplicar la nueva versión. En modo JS incluye dos colas, planificación de hilos, callback, JSON y espera activa con cesión del hilo en el banco. El camino Rust puro es tan corto que el coste del reloj influye en sus cifras. El segundo intervalo incluye preparar y completar el dibujo, con `glFinish` en egui y actualización de la superficie en SDL. La lectura de píxeles y la escritura del CSV quedan fuera de los intervalos.

**Paridad medida.** Cinco capturas por proceso, doce procesos, sesenta capturas. Todas coincidieron con la referencia correspondiente en los 76.800 píxeles. El estado de arranque y el retorno a cero tuvieron el mismo hash. Las capturas proceden de la salida de cada renderer. Los CSV y las imágenes están junto al [resumen de la serie](../metrics/primera-serie/summary.json).

**Interpretación.** Para este contador, el camino por JavaScript añadió unos 7–9 µs a la mediana del cambio aplicado y conservó la imagen final. El p99 de dibujo varió más que la mediana en egui. Tres repeticiones en una máquina y con estos drivers dejan abierta la causa de esa variación. Estas mediciones terminan antes de la composición en pantalla. La latencia de entrada a presentación, la GPU física, el RSS, el arranque en frío, el consumo en reposo y el scroll con texto siguen pendientes.

## Observaciones por backend

- **egui.** La UI interactiva usa un identificador estable para el botón. En el banco se ejecuta un pase de egui, se tesela y se lee el framebuffer de egui_glow. Se desactivaron el difuminado de bordes y el dithering para respetar los colores exactos. [API de Painter](https://docs.rs/egui/0.36.2/egui/struct.Painter.html)
- **GPUI.** La entidad retiene el runtime. El canvas emite quads en coordenadas de la ventana y un elemento con identidad recibe el clic. La compilación funcionó. El intento de abrir la ventana devolvió `NoCompositor` y X11 tampoco fue accesible desde esta sesión. Queda por verificar la imagen. [Arquitectura de GPUI](https://docs.rs/gpui/0.2.2/gpui/)
- **Iced.** El adaptador traduce el puntero en un mensaje de activación. La suscripción temporal comprueba la respuesta del worker mientras hay cambios en vuelo. Medir esa espera real requerirá capturar la ventana. [Guía de Iced](https://docs.rs/iced/0.13.1/iced/)
- **Masonry.** Se implementó un widget hoja con tamaño fijo. Recibe eventos y emite rectángulos en su pase de pintura. El repintado tras el commit se solicita explícitamente. [Contrato de Widget](https://docs.rs/masonry/0.4.0/masonry/core/trait.Widget.html)
- **Slint.** Cada rectángulo se expresa como un elemento declarativo y el botón recibe eventos mediante TouchArea. El adaptador reconstruye el modelo de rectángulos tras un cambio. El coste de esa reconstrucción pertenece al adaptador y merece instrumentación antes de optimizarlo. [Integración de Slint con Rust](https://docs.rs/slint.dev/latest/docs/rust/slint/)
- **Jurui propio.** Rust decide geometría, rasterización, área activa y redibujado. SDL abre la ventana y entrega los eventos. El primer renderer usa CPU para disponer de una salida de píxeles reproducible. WGPU y Taffy quedan como alternativas cuando haya una carga que necesite aceleración o layout dinámico. [SDL](https://www.libsdl.org/)

Las dependencias de Iced, Masonry y Slint faltaban en la caché local y Cargo no pudo resolver crates.io desde esta sesión. En una ejecución posterior, `cargo fetch` por manifiesto completó las descargas y los tres adaptadores compilaron en release. La verificación visual en ventana sigue pendiente por el fallo de apertura del socket X11 de Xvfb. El alcance comprobado del backend propio es el contador. La edición con IME, el foco por teclado, la accesibilidad, la virtualización y la portabilidad de plataforma requieren pruebas específicas.

## Decisiones del prototipo

La primera VM es Duktape 2.x, disponible en este equipo. El paso de build elimina anotaciones de tipos. El ejemplo usa el subconjunto de JavaScript compatible con esta VM. El callback y la lógica de negocio están en TypeScript. El contrato de montaje declara las identidades de display, botón y callback, y el contrato de actualización permite cambiar el número del display con una versión esperada. La geometría de este ejemplo reside en el diseño Rust compartido. El árbol genérico, TSX y las mutaciones estructurales son trabajo posterior. [Duktape](https://duktape.org/)

Para esta primera comparación, el modo Rust evita la VM y usa el mismo ejecutable, estado visual y renderer que el modo JS. Esto controla diferencias de compilación entre las dos rutas. Medir tamaño instalado o coste de enlazado requiere además un binario construido sin la dependencia de Duktape.

Cada cola admite hasta 64 eventos o respuestas. El host limita el total en vuelo y comunica saturación mediante el retorno de `activate`. El banco verifica una activación aceptada antes de continuar. La interfaz de demostración omite activaciones cuando la cola está llena. El hilo de pintura sondea respuestas y conserva el último estado aplicado mientras espera. El test de ráfaga verifica orden, saturación y retorno a cero.

## Siguiente experimento capaz de cambiar la decisión

El contador elimina el texto y los layouts variables para poder atribuir un píxel diferente a un error concreto. Un chat con la misma fuente empaquetada, historial fijo y guion de edición pondría a prueba justamente lo que aquí queda abierto. Conviene conservar este contador como regresión y añadir una segunda especificación visual para el chat, incluidos borrador, selección, foco y scroll. La decisión sobre el renderer de producción necesita esa segunda carga y una medición en ventana con hardware.

## Verificación adicional del contador y decisión para el chat

| Backend | Ejecución adicional | Resultado RGB |
| --- | --- | --- |
| Iced | El mismo Canvas de la ventana dibujado con tiny-skia, cinco estados en Rust y cinco en JS | 10/10 exactos |
| Slint | El mismo componente declarativo y su modelo en MinimalSoftwareWindow, ambos modos | 10/10 exactos |
| Masonry | TestHarness inyectó 1.000 clics por modo con pulsación y liberación. Vello renderizó los cinco puntos de control con Vulkan por software (SwiftShader) | 10/10 exactos, también el canal alfa |
| GPUI | El adaptador compiló. El TestWindow de la versión 0.2.2 omite el dibujo de la escena | Paridad de píxeles pendiente |

En la ejecución de Iced bajo Xvfb iniciada en el escritorio se verificaron además las imágenes Rust de 0, 1 y 42 clics, todas con cero diferencias. Las advertencias DRI3 coexistieron con imágenes correctas. En el directorio de esa serie solo estaban disponibles esas tres capturas al revisar. El test sin servidor X usa el callback de activación y el Canvas real, mientras que la serie de ventana usa el puntero de X11.

### Reproducción de las tres pruebas adicionales

Ejecutar en Bash desde Jurui, con un directorio nuevo. Slint usa el estilo compilado fluent para evitar inicializar el tema Qt del escritorio durante el render fuera de pantalla. El test de Masonry puede usar un dispositivo Vulkan disponible. Se indica el ICD de SwiftShader utilizado en este equipo.

```bash
serie=$(mktemp -d "$PWD/metrics/local-software-XXXXXX")
for b in iced slint; do
    SLINT_STYLE=fluent cargo build --locked --release --target-dir target --manifest-path "backends/$b/Cargo.toml" || exit 1
    for modo in rust js; do
        "target/release/jurui-$b" "$modo" "$serie/$b/$modo" && python scripts/visual.py "$serie/$b/$modo" || exit 1
    done
done
VK_DRIVER_FILES=/usr/lib/chromium/vk_swiftshader_icd.json JURUI_MASONRY_CAPTURES="$serie/masonry" cargo test --release --locked --target-dir target --manifest-path backends/masonry/Cargo.toml counter_pixels
```

**Decisión provisional.** Conservar el editor y el scroll en el toolkit Rust. Enviar a JavaScript un mensaje inmutable por envío y aplicar su respuesta solo al historial. El contador prueba portabilidad del dibujo simple en cinco adaptadores. El chat prueba el comportamiento del editor en egui. La elección de toolkit para producción requiere repetir la escena de texto en otro adaptador y evaluar IME, accesibilidad y presentación. [Resultados y límites del chat](prueba-02-chat.md)

## Entrega 1 · Árbol de nodos, lotes atómicos y traza · 19 de noviembre de 2026

El contrato fijo `{display, button, callback}` se reemplazó por un árbol con identidades estables ([tree.rs](../src/tree.rs)). Tanto el montaje como cada actualización son un `Batch { base, ops }`. Rust valida el lote completo antes de tocar el árbol. Si una operación es inválida (base vieja, nodo inexistente o de otro tipo, padre que no es columna, id duplicado, valor fuera de rango), el lote se rechaza y el árbol y la versión quedan intactos. Un test cubre los siete casos. El modo Rust construye el mismo lote que `montar()` en [counter.ts](../app/counter.ts), así que ambos modos pasan por la misma validación. La versión 1 corresponde al montaje.

**Traza.** Con `JURUI_TRACE=archivo.csv` (debe ser un archivo nuevo), el runtime escribe una fila por evento: nodo que recibió el evento, callback, base y cantidad de operaciones del lote, versión resultante, estado (`ok` o el motivo del rechazo), frames dibujados durante la espera y primer frame dibujado después del lote, con marcas en µs. El adaptador numera los frames con `Runtime::drawn()`. Está conectado en egui, en Jurui propio y en el banco. Ese frame es el fin del pase de dibujo del adaptador, no la presentación física.

**Demora.** `JURUI_DELAY_MS` retiene cada evento en el hilo de la VM antes del callback. En modo Rust no hay VM y la variable se ignora.

### Resultados

| Prueba | Resultado |
| --- | --- |
| Banco offscreen egui, 1.100 eventos por modo, con traza | 1.100 lotes `ok` por modo. 0, 1, 42, 999 y 1.000 coinciden con la referencia en los 76.800 píxeles |
| Ventana egui en Xvfb, modo JS, 500 ms de demora, dos clics por puntero X11 | Ambos lotes `ok`. Durante la espera del primero se dibujaron 1.686 frames. El segundo clic se encoló y esperó 796 ms |
| Test `slow_callback_keeps_drawing_and_traces_each_click` | Con 150 ms de demora se siguen dibujando frames, un segundo clic se acepta durante la espera y la traza conserva el orden |

Traza de la ventana ([CSV](../metrics/traza-01/ventana-js-500ms.csv)):

| Evento | Nodo | Lote base → versión | Evento → lote | Frames en espera | Frame |
| ---: | ---: | --- | ---: | ---: | ---: |
| 1 | 3 | 1 → 2 | 500,2 ms | 1.686 | 1.692 |
| 2 | 3 | 2 → 3 | 796,3 ms | 2.748 | 3.440 |

Tiempos del banco de esta corrida. Un solo proceso por modo y en µs. Son indicativos, no siguen el protocolo de tres procesos de la primera serie.

| Modo | Cambio aplicado p50 / p95 / p99 | Dibujo p50 / p95 / p99 |
| --- | ---: | ---: |
| Rust | 0,331 / 0,591 / 2,225 | 60,633 / 68,328 / 156,423 |
| JS | 6,723 / 8,806 / 12,394 | 23,404 / 24,997 / 42,309 |

**Lectura.** El camino Rust pasó de 0,030 a 0,331 µs. El aumento corresponde a construir y validar el lote. Sigue unas veinte veces por debajo del viaje a JS. Esta vez OpenGL eligió la **NVIDIA RTX 5060 Laptop (driver 620.70.06)**, mientras que en la primera serie usó llvmpipe, así que los dibujos no son comparables con esa serie. Que el dibujo en modo Rust fuera más lento que en JS dentro de un solo proceso necesita la repetición con orden alternado antes de interpretarse. Puede deberse a los estados de reloj de la GPU.

**Hallazgos que cambian el plan.**
- egui pide un repintado continuo mientras hay un lote en vuelo: 1.686 frames en 500 ms (≈3.400 fps sin vsync en Xvfb). Con un compositor real el vsync lo limita, pero conviene usar `request_repaint_after` para no gastar CPU durante la espera.
- La VM procesa los eventos en serie. Un segundo clic espera la demora del primero más la suya (bloqueo en cabeza de cola). Para el chat es correcto porque conserva el orden de los envíos. Si el SDK quiere callbacks independientes, la cola tiene que dejar de ser una sola.
- Con un lote rechazado, los siguientes del mismo script también se rechazan por base vieja, porque JS lleva su propio contador. Falta definir la reconciliación (reenviar la versión actual a JS o volver a montar) antes de las mutaciones estructurales.

```bash
d=metrics/traza-local; mkdir "$d"
for m in rust js; do
    SDL_VIDEODRIVER=offscreen JURUI_TRACE=$d/traza-$m.csv target/release/jurui-egui $m $d/egui-$m && python scripts/visual.py $d/egui-$m
done
JURUI_DELAY_MS=500 JURUI_TRACE=$d/ventana.csv target/release/jurui-egui js   # hacer clic y cerrar
```

## Entrega 2 · Curva por tamaño de lote y cambio a QuickJS · 19 de noviembre de 2026

Los umbrales se fijaron antes de esta serie, en la [propuesta](Propuesta_de_runtime_TS_Rust_MVP.md#criterios-de-decisión). La referencia es el modo Rust nativo del mismo ejecutable.

**Método.** `examples/curva.rs` mide la ida y vuelta desde el evento hasta el lote aplicado, sin dibujo, con un evento en vuelo. El callback devuelve un lote de 1, 10, 100 o 1.000 operaciones `set` (`Runtime::activate_ops`). Por tamaño: 100 de calentamiento y 1.000 medidas. [curva.py](../scripts/curva.py) abre tres procesos por modo, alterna el orden en la segunda repetición e informa la mediana de los percentiles por proceso. La misma curva se midió con Duktape 2.7 (antes del cambio) y con QuickJS-ng 0.16.2 (`rquickjs` 0.14.0). La memoria es el pico residente (VmHWM) del proceso sin ventana.

| Operaciones | Rust p50 / p99 | Duktape p50 / p99 | QuickJS p50 / p99 | Umbral p99 |
| ---: | ---: | ---: | ---: | ---: |
| 1 | 0,05 / 0,06 | 5,19 / 8,70 | 4,54 / 6,56 | 1.000 |
| 10 | 0,10 / 0,11 | 15,51 / 28,52 | 12,23 / 21,44 | 1.000 |
| 100 | 0,43 / 0,44 | 112,39 / 128,38 | 87,07 / 98,01 | 1.000 |
| 1.000 | 4,00 / 4,69 | 1.045,21 / 1.111,89 | 843,75 / 1.242,76 | 4.000 (informativo) |

Tiempos en µs. [Duktape](../metrics/curva-duktape/summary.json) · [QuickJS](../metrics/curva-quickjs/summary.json)

| Recurso | Rust | QuickJS | Diferencia | Umbral |
| --- | ---: | ---: | ---: | ---: |
| Montaje inicial | 6 µs | 635 µs | +0,6 ms | +100 ms |
| Memoria residente máxima, sin ventana | 3.200 kB | 4.636 kB | +1,4 MB | +20 MB |
| Ejecutable del banco | — | 2,33 MB (Duktape: 0,84 MB enlazado dinámicamente) | | |
| Ejecutable egui | — | 15,4 MB (con Duktape dinámico: 13,2 MB) | | +5 MB |

**Lectura.** El costo crece de forma lineal y no aparece un codo hasta 1.000 operaciones. Hay un costo fijo de unos 4,5 µs (dos colas y el despertar del hilo) y unos 0,84 µs por operación con QuickJS (1,05 µs con Duktape), dominados por `JSON.stringify` y la decodificación en Rust. Por operación, JS cuesta unas 200 veces más que Rust. En valor absoluto, un lote de 1.000 cambios cabe 13 veces en un frame de 60 Hz. QuickJS mejora la mediana entre un 13 % y un 23 % en todos los tamaños. Con 1.000 operaciones su p99 queda por encima del de Duktape. Probablemente se deba al GC, pero falta comprobarlo. El tamaño agregado de la VM todavía no tiene una referencia justa: Duktape se enlazaba como biblioteca del sistema, y falta un binario sin VM.

Con QuickJS se repitieron los tests del núcleo y de egui, los cinco estados del contador en cada modo (0 píxeles distintos) y el guion del chat (50 capturas verificadas). El escape `\uXXXX` de `app/chat.ts` era necesario por las cadenas CESU-8 de Duktape. Con QuickJS sobra, pero no molesta.

### Scroll de 10.000 mensajes con tres procesos por modo

| Modo | Frame p50 | p95 | p99 | p99 de cada proceso |
| --- | ---: | ---: | ---: | --- |
| Rust | 0,112 ms | 0,242 ms | 0,265 ms | 0,262 · 0,372 · 0,265 |
| JS + Rust | 0,112 ms | 0,243 ms | 0,317 ms | 0,318 · 0,317 · 0,296 |

NVIDIA RTX 5060 Laptop, OpenGL offscreen, 1.100 frames medidos por proceso. [CSV](../metrics/scroll-quickjs/)

**No cumple el umbral.** El p99 de JS es un 20 % mayor (el umbral es +10 %), aunque p50 y p95 coinciden. Son 0,05 ms en un frame de 16,7 ms. Durante el scroll el historial ya está en Rust y el hilo de la VM está bloqueado esperando eventos, así que la diferencia no viene de llamadas a JS. Hipótesis para investigar: en modo JS las 10.000 cadenas se crean en el hilo de trabajo y quedan en otra arena del asignador, con peor localidad al dibujar. Prueba de la hipótesis: copiar el historial en el hilo de interfaz después de la precarga y repetir. Según la regla de la propuesta, corresponde una corrección acotada antes de reformular. Si el umbral relativo se considera demasiado estricto para frames por debajo de 1 ms, el cambio debe anotarse en la propuesta con este dato como motivo.

### Corrección del scroll y control A/A

Se probó la hipótesis de la arena del asignador con cinco procesos por modo y orden alternado. Mediana del p99 de frame, mismo equipo.

| Condición | Rust p99 | JS p99 | JS / Rust |
| --- | ---: | ---: | ---: |
| A · binario actual | 0,253 ms | 0,282 ms | +11,6 % |
| A2 · el mismo binario, otra vez | 0,272 ms | 0,278 ms | +2,3 % |
| B · `MALLOC_ARENA_MAX=1` | 0,247 ms | 0,262 ms | +6,2 % |
| C · historial copiado en el hilo de interfaz | 0,253 ms | 0,254 ms | +0,1 % |

[CSV por condición](../metrics/scroll-fix/)

**Lectura.** A y A2 son el mismo ejecutable, pero su diferencia varió de +11,6 % a +2,3 %. El p99 de cada proceso oscila entre 0,24 y 0,35 ms en ambos modos. El ruido entre procesos del p99 tiene el mismo tamaño que el umbral, así que el +20 % de la serie anterior no es evidencia de un costo de JS. C tampoco prueba que la copia ayude. La copia se retiró y el código quedó como en A. Conclusión: con este método no se detecta diferencia de scroll entre los modos. p50 y p95 coinciden en todas las condiciones.

**Cambio de método recomendado.** Cada comparación contra un umbral relativo debe medirse junto a un control A/A del mismo binario. Una diferencia cuenta solo si supera la variación A/A. Para el p99 de frame conviene más muestra por proceso (10.000 frames en lugar de 1.100) o un intervalo bootstrap de la diferencia.

## Entrega 3 · Protocolo directo, al estilo JSI · 19 de noviembre de 2026

**Pregunta.** ¿Cuánto del costo de JS se debe a JSON y cuánto al salto entre hilos? React Native dejó atrás su puente JSON asíncrono con JSI, que hace llamadas directas de JS a funciones nativas. Este experimento repite esa idea en Jurui.

**Implementación.** Al crear la VM, Rust registra el objeto global `jurui` con funciones nativas. `jurui.set(id, valor)` escribe un `Op::Set` en un buffer de Rust que vive en el hilo de la VM, sin texto ni JSON de por medio. Al terminar el callback, el hilo de la VM toma el buffer y envía un `Batch` ya tipado. El árbol lo valida igual que antes: si es inválido, se rechaza sin tocar nada. `rquickjs` rechaza los números fuera del rango de `u16`. El modo es `directo` y convive con `js` (JSON) y `rust`. Para separar el costo del salto, `local-json` y `local-directo` ejecutan el mismo callback en el hilo que mide, sin colas.

Tiempos en µs, mediana de tres procesos con el orden alternado ([resumen](../metrics/curva-directo/summary.json)):

| Ops | Rust | Directo | JSON | Local directo | Local JSON | JSON / directo |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 1 | 0,05 / 0,05 | 2,43 / 3,87 | 5,20 / 8,18 | 0,42 / 0,52 | 2,11 / 2,26 | 2,1× |
| 10 | 0,10 / 0,11 | 3,49 / 4,85 | 12,62 / 20,08 | 1,12 / 1,19 | 10,05 / 13,25 | 3,6× |
| 100 | 0,64 / 0,64 | 13,25 / 18,28 | 91,14 / 102,48 | 9,05 / 10,76 | 86,73 / 97,37 | 6,9× |
| 1.000 | 3,95 / 6,45 | 103,11 / 152,62 | 888,83 / 1.270,63 | 86,25 / 90,21 | 876,18 / 1.301,47 | 8,6× |

Cada celda es p50 / p99. Los cinco estados del contador en modo directo coincidieron con la referencia en todos los píxeles. La traza registró 1.100 lotes `ok`.

**Descomposición con una operación.** Ida y vuelta directa: 2,43 µs. Salto entre hilos (dos colas, despertar el hilo y cederlo): ≈ 1,96 µs. Llamada a JS más la función nativa: 0,42 µs. Aplicar en Rust: 0,05 µs.

**Costo por operación.** Directo: ≈ 0,10 µs. JSON: ≈ 0,88 µs. Rust: ≈ 0,004 µs.

**Lectura.** JSON era el 85 % del costo por operación. Con llamadas directas, un lote de 1.000 cambios cuesta 0,1 ms, el 0,6 % de un frame a 60 Hz. En lotes chicos domina el salto entre hilos, que es el precio de que JS nunca bloquee la interfaz. Lo que queda por operación (≈ 86 ns) es el intérprete de QuickJS ejecutando el bucle más la llamada nativa a través de `rquickjs`. Sigue siendo unas 20 veces el costo de Rust por operación, pero ya no es la parte importante del total.

### Próximos pasos del protocolo

1. **Buffer de comandos compartido.** JS escribe las operaciones en un `ArrayBuffer` o `Int32Array` que Rust lee al cerrar el lote. Así desaparece el costo de una llamada nativa por operación. Comparar contra `jurui.set` con 1.000 operaciones.
2. **Lectura síncrona del árbol.** Lo que JSI permite y un puente asíncrono no: que JS consulte estado nativo (medidas de texto, layout, selección) sin esperar un mensaje. Propuesta: Rust publica una instantánea inmutable y versionada del árbol (`Arc`) después de cada lote. JS la lee sin bloqueo desde su hilo, y las escrituras siguen yendo por lotes validados.
3. **Montaje y mutaciones estructurales por la misma vía.** Agregar `jurui.create` y `jurui.remove`, retirar el JSON del montaje y definir cómo se reconcilia la versión cuando se rechaza un lote.
4. **Salto entre hilos.** Una cola sin bloqueos con espera activa breve antes de dormir. Medir cuánto de los 1,96 µs se recupera y cuánta CPU cuesta en reposo.
5. **El chat por el protocolo directo**, para medir texto y Unicode sin JSON.

## Entrega 4 · Buffer de comandos · 19 de noviembre de 2026

**Pregunta.** Con llamadas directas, el costo por operación (≈ 86 ns) ¿se debe a cruzar la frontera una vez por operación? Si es así, un buffer binario cruzado una sola vez por lote debería bajarlo.

**Fuentes consultadas.** React Native 0.88 permite que un método nativo síncrono tome prestada la memoria de un `ArrayBuffer` sin copiarla. Esos bytes valen hasta que el método retorna y, si hay que retenerlos, se copian durante la llamada ([blog de React Native 0.88](https://github.com/react/react-native-website/blob/main/website/blog/2026-10-19-react-native-0.88.mdx)). En rquickjs 0.14, `TypedArray::as_raw()` devuelve `None` si el buffer está desacoplado, y el puntero vale solo hasta que vuelva a correr JS. Es la misma regla de préstamo.

**Implementación.** Modo `buffer`. JS escribe `[1, id, valor]` por operación en un `Int32Array` que reutiliza y duplica cuando no alcanza. Al final del callback llama una vez a `jurui.commit(buffer, largo)`. Rust toma prestados los bytes, decodifica y valida el lote completo durante la llamada y lo copia a `Vec<Op>`. Ante un error lanza una `TypeError` y no deja operaciones parciales. Un test cubre siete casos de rechazo: código desconocido, id o valor fuera de rango, número negativo, longitud mayor que el buffer o que no es múltiplo de 3, buffer desacoplado con `transfer()`, y `Float64Array` en lugar de `Int32Array`. También cubre una vista con desplazamiento (`subarray`). En egui, los cinco estados del contador coincidieron con la referencia en todos los píxeles. La traza registró 1.100 lotes `ok`.

Tiempos en µs, p50 / p99, mediana de tres procesos. La parte local se mide en el hilo de la VM, sin colas.

| Ops | Directo | Buffer | Local directo | Local buffer con `emitir()` | Local buffer en línea | Local en línea con variables locales |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 1 | 3,18 / 4,26 | 2,77 / 5,25 | 0,29 / 0,31 | 0,58 / 0,63 | 0,54 / 0,60 | 0,53 / 0,59 |
| 10 | 3,82 / 7,49 | 4,98 / 6,64 | 1,13 / 1,23 | 2,60 / 2,73 | 2,07 / 2,18 | 1,69 / 1,75 |
| 100 | 13,82 / 20,89 | 24,79 / 35,37 | 9,07 / 11,70 | 21,44 / 24,35 | 15,88 / 18,25 | 12,37 / 15,11 |
| 1.000 | 104,21 / 112,95 | 217,38 / 232,93 | 87,82 / 97,61 | 208,89 / 229,34 | 153,22 / 159,62 | 118,09 / 125,13 |

[Directo y buffer](../metrics/curva-buffer/summary.json) · [En línea](../metrics/curva-buffer-en-linea/summary.json) · [Con variables locales](../metrics/curva-buffer-locales/summary.json)

**Resultado negativo.** En QuickJS el buffer es más lento que las llamadas directas en todos los tamaños mayores que uno. Con 1.000 operaciones: 118 µs en la mejor variante del buffer contra 88 µs de las llamadas directas. Quitar la función auxiliar ahorra un 27 %. Pasar las globales a variables locales ahorra otro 23 %. Aun así, el buffer no llega. Según la [documentación de QuickJS-ng](https://quickjs-ng.github.io/quickjs/developer-guide/internals), el motor es un intérprete de bytecode, sin JIT, con llamadas a función rápidas y accesos a typed arrays optimizados. Una llamada nativa son pocas instrucciones. Tres escrituras indexadas son varias veces más. En este motor, el costo por operación lo pone la cantidad de bytecode, no cruzar la frontera.

**Decisión.** El protocolo de mutaciones sigue siendo de llamadas directas. `buffer` queda disponible y probado para datos binarios masivos (imágenes, filas precalculadas), que es el uso que React Native le da a `ArrayBuffer`. `activarBufferEnLinea` existe solo para el banco.

**Lecciones para el SDK.**
- Generar código que lea globales una sola vez por callback.
- Medir antes de agregar funciones auxiliares en la ruta caliente.

Otras dos ideas que salieron de la documentación, sin medir todavía:
- Guardar `jurui.set` en una variable local para evitar buscar la propiedad en cada operación.
- Precompilar la app a bytecode con `qjsc` (`js_std_eval_binary`), que según QuickJS-ng evita compilar al arrancar y permite omitir el compilador del binario.

## Entrega 5 · Lectura síncrona del árbol desde JS · 19 de noviembre de 2026

**Pregunta.** JS corre en su hilo y Rust aplica los lotes en el hilo de interfaz. ¿Cómo puede JS leer el estado en el momento, sin esperar un mensaje y sin ver datos a medio escribir?

**Estrategias** ([lectura.rs](../src/lectura.rs)):

| Estrategia | Lectura desde JS | Publicación en el hilo de interfaz | Consistencia dentro del callback |
| --- | --- | --- | --- |
| pedido | Pide el valor al hilo de interfaz y se bloquea hasta la respuesta, como el método síncrono de un puente | Aplica en su árbol | Cada lectura puede ver una versión distinta |
| mutex | Candado por lectura sobre el árbol compartido | Toma el candado y aplica en el lugar | Cada lectura puede ver una versión distinta |
| foto | Toma un `Arc` del árbol al empezar el callback. Las lecturas no usan candado | `Arc::make_mut`: clona el árbol solo si JS tiene una foto abierta | Una sola versión |
| réplica | Lee una copia propia del hilo de JS, sin sincronización | Aplica el lote y lo envía al hilo de JS. Ese hilo aplica los pendientes al empezar el callback | Una sola versión |

**Escenario.** Árbol de 10.000 etiquetas. El hilo de interfaz publica un lote de una operación cada 100 µs. El callback JS lee k valores (`jurui.leer` guardado en una variable local). Tres procesos por estrategia, con el orden alternado. 100 callbacks de calentamiento y 1.000 medidas por k. El hilo de interfaz del banco atiende pedidos en un bucle continuo, el mejor caso para `pedido`: una interfaz real los atendería entre frames.

Tiempo del callback en µs, p50 / p99 ([resumen](../metrics/lectura-01/summary.json)):

| Lecturas | pedido | mutex | foto | réplica |
| ---: | ---: | ---: | ---: | ---: |
| 1 | 2,37 / 3,53 | 0,27 / 0,30 | 0,28 / 0,34 | 0,28 / 0,30 |
| 10 | 19,29 / 25,80 | 1,32 / 1,50 | 1,32 / 71,78 | 1,31 / 1,48 |
| 100 | 185,14 / 198,47 | 12,15 / 14,31 | 80,24 / 100,30 | 12,03 / 14,06 |
| 1.000 | 1.956,23 / 2.073,24 | 120,51 / 130,56 | 218,00 / 309,74 | 118,68 / 191,59 |

| Costo de publicar un lote | pedido | mutex | foto | réplica |
| --- | ---: | ---: | ---: | ---: |
| p50 | 0,08 µs | 0,11 µs | **74,1 µs** | 0,19 µs |
| p99 | 0,22 µs | 0,25 µs | **135,9 µs** | 0,57 µs |
| máximo | 2,1 µs | 2,3 µs | **458,9 µs** | 3,3 µs |

**Lectura.**
- **pedido** cuesta ≈ 1,95 µs por lectura: el salto entre hilos en cada valor. Mil lecturas ocupan 2 ms en el mejor caso. Si la interfaz atiende una vez por frame, cada lectura puede esperar hasta 16,7 ms. Queda descartado.
- **mutex** y **réplica** cuestan lo mismo por lectura (≈ 0,12 µs): la llamada nativa más la búsqueda en el `BTreeMap`. El candado sin contención no se nota. La diferencia está en la consistencia: con `mutex`, un callback que lee dos valores puede verlos de versiones distintas. Con `réplica` no.
- **foto** es consistente, pero con un `BTreeMap` la copia-al-escribir es O(n). Como el banco tiene a JS siempre ocupado, casi cada publicación encontró una foto abierta y clonó 10.000 nodos: 74 µs en el hilo de interfaz. Además, liberar la versión vieja cae dentro del callback de JS, lo que explica los 80 µs con 100 lecturas. Sería viable con una estructura persistente, con copia de caminos en O(log n), o con una app cuyo JS esté casi siempre inactivo.
- **réplica** gana en las tres dimensiones: lectura más barata, publicación de 0,19 µs y una sola versión por callback. Su p99 con 1.000 lecturas (191 µs) incluye ponerse al día con los lotes acumulados al empezar el callback. El precio es tener el árbol dos veces en memoria. A 10.000 etiquetas no se distingue en el pico residente (6,8 MB contra 6,8 MB del mutex).

**Diseño que surge: réplica por registro ordenado.** El hilo de interfaz sigue siendo la única fuente de verdad. Cada lote aceptado, venga de JS o de la propia interfaz (borrador, selección, scroll, medidas), entra en un registro ordenado. El hilo de JS aplica ese mismo registro a su réplica al empezar cada callback. JS lee siempre una versión completa y conocida (`replica.version`), sin candados ni saltos de hilo. Queda una cuestión abierta: JS no ve sus propios cambios hasta que la interfaz los acepta y los devuelve. Se puede resolver con una capa optimista sobre la réplica, descartada si el lote se rechaza.

**Próximos experimentos.**
1. Réplica con almacenamiento indexado por id (`Vec`) en lugar de `BTreeMap`, para bajar el piso de 0,12 µs por lectura.
2. Foto con una estructura persistente, para comparar memoria y costo de publicación contra la réplica.
3. Llevar la réplica al runtime: lotes aceptados devueltos al hilo de JS, `jurui.leer` y `jurui.version` en el SDK, y la capa optimista para leer los cambios propios.

## Entrega 6 · Réplica en el runtime y estructuras de datos · 19 de noviembre de 2026

### Réplica optimista en el hilo de JS

`tree::Replica` mantiene en el hilo de JS tres cosas: el árbol confirmado por la interfaz, los lotes propios todavía sin respuesta (cada uno recuerda si se aplicó) y la **vista**, que es lo confirmado más esos pendientes. JS lee la vista de forma síncrona con `jurui.leer(id)` y `jurui.version()`. Así ve sus propios cambios aunque estén en vuelo. La interfaz responde cada lote con un booleano por un canal sin límite, para no bloquearse nunca. No reenvía el lote: la réplica ya lo tiene. Al recibir una aceptación, el lote pasa a lo confirmado. Si la interfaz rechaza un lote que la vista ya contaba, la vista se rehace con lo confirmado y los pendientes que sigan siendo válidos.

Cada lote se valida primero en el hilo de JS. Si es inválido ahí, la versión local no avanza y el lote siguiente sale con la base correcta. Esto resuelve el rechazo en cascada señalado en la Entrega 1.

`app/counter.ts` ya no guarda estado propio: el valor siguiente sale de `jurui.leer(2)` y la base de `jurui.version()`. Rust es la única fuente de verdad.

| Verificación | Resultado |
| --- | --- |
| Test de la réplica | Lee sus propios cambios. Un lote inválido no avanza la versión. La vista vuelve a lo confirmado ante un rechazo. Al final, vista y confirmado coinciden con la interfaz |
| Ráfaga de 64 eventos en vuelo, modos js, directo y buffer | Versión 1025, valor 24. Cada callback calculó su valor leyendo la vista optimista |
| egui offscreen, rust, js, directo y buffer | 5 de 5 estados idénticos a la referencia en cada modo. 1.100 lotes `ok` por modo |
| Guion del chat | 50 capturas verificadas. El chat todavía no usa el árbol, así que la réplica no interviene en él |
| Test de puntero de egui | Pasa |

Ida y vuelta con la réplica, en µs, p50 / p99 ([resumen](../metrics/replica-01/curva/summary.json)): con 1 operación, 2,86 / 4,95 en directo y 5,20 / 11,33 con JSON. Con 1.000 operaciones, 108,4 / 156,6 en directo, contra 104,2 / 113,0 antes de la réplica. El costo agregado queda dentro de la variación entre series. Esta vez `activarDirecto` guarda `jurui.set` en una variable local.

### Estructuras para la lectura y la publicación

[estructuras.rs](../examples/estructuras.rs), con 10.000 nodos. Tres ejecuciones con resultados similares, se muestra la primera ([salidas](../metrics/estructuras-01/)). `imbl` 7.0.2 es solo dependencia de desarrollo. Según su documentación, `OrdMap::clone` es O(1) porque comparte la raíz, e `insert` / `get_mut` copian en O(log n) solo los nodos compartidos.

| Lectura de un valor | ns |
| --- | ---: |
| `BTreeMap` (la actual) | 35–37 |
| `imbl::OrdMap` | 26–27 |
| `Vec` indexado por id | 1,1 |

| Publicar con una foto abierta (µs) | p50 | p99 | máx | Soltar la foto, p50 |
| --- | ---: | ---: | ---: | ---: |
| Foto con `BTreeMap` | 20,50 | 28,38 | 153,89 | 31,61 |
| Foto con `imbl::OrdMap` | **0,31** | 1,44 | 5,49 | **0,08** |
| Réplica con `BTreeMap`, dos copias | 0,12 | 0,47 | 1,31 | — |
| Réplica con `Vec`, dos copias | **0,02** | 0,19 | 0,35 | — |

**Lectura.**
- De los ≈ 120 ns que cuesta una lectura desde JS, ≈ 36 son la búsqueda en el `BTreeMap`. El resto es la llamada nativa de QuickJS a través de rquickjs. Un `Vec` indexado por id bajaría la lectura a unos 85 ns. Es una estimación: falta medirlo desde JS.
- Con un mapa persistente, la foto deja de ser cara: publicar cuesta 0,31 µs y soltarla 0,08 µs, frente a 20,5 y 31,6 µs con `BTreeMap`.

**Las dos opciones viables:**

| | Réplica con `Vec` | Foto con `imbl::OrdMap` |
| --- | --- | --- |
| Lectura | ≈ 1 ns más la llamada nativa | ≈ 26 ns más la llamada nativa |
| Publicación | Aplicar dos veces, ≈ 0,02 µs | Copia de caminos, ≈ 0,31 µs |
| Memoria | El árbol dos veces | Una vez, más los nodos compartidos de las fotos abiertas |
| Lectores | Un hilo de JS por réplica | Cualquier cantidad de hilos con la misma foto |
| Requiere | Registro ordenado de respuestas | Una dependencia y la capa optimista para los cambios propios |

Con un solo hilo de JS, la réplica con `Vec` es la más rápida. Si Jurui va a tener varios workers de JS que leen la misma interfaz, la foto persistente escala mejor.

## Entrega 7 · El chat en el árbol, con dos escritores · 19 de noviembre de 2026

**Pregunta.** ¿Puede JS leer de forma síncrona estado que escribe la interfaz (el borrador y la selección) mientras la interfaz sigue siendo dueña de ese estado?

**Diseño: árbol particionado por escritor.** El árbol tiene dos escritores y cada uno es dueño de sus nodos.

| Nodos | Escritor | Operaciones |
| --- | --- | --- |
| Columnas, etiquetas, textos, botones | La app (JS, o la lógica Rust del modo de referencia) | `create`, `set`, `settext` |
| `Input`: borrador y selección | La interfaz | `setinput` |

La app declara el campo al montar, pero después no puede escribirlo: un lote de la app con `setinput` se rechaza (`nodo_de_la_interfaz`). La interfaz solo puede emitir `setinput`. Como ningún nodo tiene dos escritores, los lotes de uno y otro **conmutan**: el orden no cambia el resultado y no hacen falta candados ni rebase. Un test lo verifica aplicando los mismos dos lotes en ambos órdenes. La versión global cuenta los lotes de los dos escritores. La base de un lote de la app se compara solo con `app_seq`, la cantidad de lotes de la app aplicados, así que escribir en el campo no invalida un lote de JS en vuelo. Esa base la completa el hilo de la app; el script ya no la envía.

**Registro.** La interfaz agrega al registro de la réplica dos tipos de entrada: respuestas (`Answer`) y sus propios lotes (`Ui`). Al empezar cada callback, el hilo de la app aplica las respuestas a lo confirmado y los lotes de la interfaz a lo confirmado y a la vista. No hace falta rehacer la vista, porque conmutan.

**Hilo de app genérico.** `spawn_app` concentra el montaje, la réplica, el registro y el bucle de eventos. Lo usan el contador JS (tres protocolos), el chat JS y el chat Rust. Los eventos llevan `Event { callback, arg, text }`.

**Chat.** El historial es una columna de nodos `Text`. El borrador y la selección son el `Input` 3. El adaptador egui publica en cada frame el borrador y la selección de `TextEdit` como un lote de la interfaz, solo si cambiaron. Al enviar, Rust captura el texto, limpia el campo y escribe ese campo limpio en el registro antes de encolar el evento. `app/chat.ts` agrega el mensaje con `jurui.crearTexto` y escribe la etiqueta de estado con lo que lee en ese momento: `jurui.leerTexto(3)` y `jurui.seleccion(3)`. El escape `\uXXXX` de Duktape desapareció junto con el JSON del mensaje. Los nodos pasan a un `Vec` indexado por id.

| Verificación | Resultado |
| --- | --- |
| 9 tests del núcleo | Pasan. Particiones, conmutatividad, réplica, chat en los dos modos |
| Test del chat, 80 ms de demora | En Rust y en JS el estado dice «borrador «en edición», selección 3–10»: la app leyó lo escrito mientras estaba ocupada |
| Guion A–E, cinco repeticiones por modo | 50 capturas iguales. Los cinco estados tienen **el mismo hash que antes del cambio** ([paridad](../metrics/chat-arbol-01/chat/paridad.json)). En E, el estado es «borrador «segundo borrador», selección 8–16» |
| Envío → E | 501–504 ms en ambos modos, con 500 ms de demora intencional ([tiempos](../metrics/chat-arbol-01/chat/tiempos.csv)) |
| Contador egui, cuatro modos | 5 de 5 estados exactos y 1.100 lotes `ok` por modo |

**Scroll de 10.000 mensajes desde el árbol** (cinco procesos por modo, con control A/A, [CSV](../metrics/chat-arbol-01/)):

| Serie | Rust p99 | JS p99 | JS / Rust |
| --- | ---: | ---: | ---: |
| A | 0,360 ms | 0,336 ms | −6,7 % |
| A2, el mismo binario | 0,317 ms | 0,325 ms | +2,7 % |

La diferencia cambia de signo entre series del mismo binario: no hay diferencia detectable. **Cumple el umbral.**

**Lectura desde JS con `Vec`** (estrategia réplica, 1.000 lecturas, p50): 112,4 µs, frente a 118,7 µs con `BTreeMap`. Mejora un 5 %, no el 30 % estimado en la Entrega 6. En el banco aislado la búsqueda en el `BTreeMap` costaba 36 ns, pero dentro de la llamada desde JS apenas pesa. El costo real es la llamada nativa: ≈ 110 ns por lectura.

**Ida y vuelta** ([resumen](../metrics/chat-arbol-01/curva/summary.json)): con 1.000 operaciones, Rust 7,26 µs (antes 3,95), directo 117,1 µs (antes 104–108) y JSON 818,7 µs. `Op` y `Node` crecieron con las variantes de texto, y copiar y recorrer lotes grandes cuesta unos 3 ns más por operación en Rust y unos 10 ns más en directo. Con una sola operación no cambia nada.

**Lo que demuestra.** JS lee estado de la interfaz al instante, sin pedirlo y sin esperar. Ve una versión completa por callback. No puede escribir ese estado. Y no pierde lo que la persona escribe mientras la app está ocupada.

**Pendiente.**
- El scroll y las medidas de texto como estado de la interfaz.
- Reducir el tamaño de `Op` (por ejemplo, poner el texto en un `Box<str>`) si los lotes grandes lo justifican.
- El chat por los otros adaptadores.

## Entrega 8 · Memoria compartida: JS lee el heap de Rust sin traducir · 19 de noviembre de 2026

**Idea.** Los dos lenguajes representan los mismos datos de forma distinta. En Rust, un `u16` está en una posición fija. En JS, un `JSValue` etiquetado vive en el heap del recolector. Cruzar significa traducir, y eso es lo que cuestan JSON (todo el lote) y las llamadas nativas (cada argumento). El stack no se puede compartir, porque los frames de cada lenguaje mueren con su llamada. El heap sí: si los datos numéricos se guardan en columnas planas, Rust los lee como slice y JS como `Int32Array`. **La misma memoria, dos lentes, nada se copia ni se traduce.**

**Implementación** ([lectura.rs](../src/lectura.rs)). La columna es un `Box<[i32]>` en el heap de Rust, con un valor por id de nodo. Se expone con `ArrayBuffer::from_source_immutable`: según la documentación de rquickjs, JS no recibe permiso de escritura y Rust puede conservar la memoria. `Persistent` impide que el recolector libere el buffer. Antes de cada callback, Rust aplica los lotes de la interfaz escribiendo con el puntero que devuelve `as_raw()` en ese momento. Esto ocurre en el hilo de JS y sin JS en ejecución, la condición que la documentación pone para que el puntero sea válido. Si el buffer fuera desacoplado, `as_raw()` devuelve `None` y Rust se detiene en lugar de escribir memoria liberada. JS lee `jurui.valores[id]`. Un test comprueba tres cosas:
- JS ve la suma correcta;
- un cambio escrito por Rust se ve en JS sin copia;
- JS no puede modificar la columna.

**Hallazgo.** En QuickJS-ng 0.16.2, escribir en un typed array inmutable no lanza una excepción, ni siquiera en modo estricto: la escritura se ignora en silencio (`quickjs.c`, rama `ta_immutable`). La propiedad de un solo escritor se mantiene, pero el SDK debería avisar en desarrollo.

**Medición.** El mismo escenario de la Entrega 5: 10.000 nodos, la interfaz publica cada 100 µs y hay tres procesos por estrategia. `vacio` ejecuta el mismo bucle y el mismo cálculo de índices sin leer nada. Es el piso del intérprete. Tiempo del callback en µs, p50 / p99 ([resumen](../metrics/memoria-01/summary.json)):

| Lecturas | vacio (piso) | memoria | réplica (llamada nativa) | mutex |
| ---: | ---: | ---: | ---: | ---: |
| 1 | 0,21 / 0,23 | 0,27 / 0,29 | 0,28 / 0,30 | 0,28 / 0,31 |
| 10 | 0,81 / 0,83 | 0,90 / 0,99 | 1,26 / 1,58 | 1,29 / 1,54 |
| 100 | 6,85 / 7,82 | 7,27 / 9,51 | 11,14 / 14,14 | 11,32 / 14,49 |
| 1.000 | 66,72 / 72,28 | 70,74 / 76,40 | 109,61 / 137,00 | 111,63 / 130,71 |

| Costo por lectura, descontado el piso | ns |
| --- | ---: |
| Memoria compartida | **≈ 4** |
| Llamada nativa (`jurui.leer`) | ≈ 43 |
| El bucle de JS (índice, suma, vuelta) | ≈ 67 por iteración |

**Lectura.**
- La memoria compartida baja el costo de leer unas 10 veces frente a la llamada nativa. Queda cerca del costo de leer un `Array` propio de JS.
- **El puente deja de pesar: lo que queda es el JS que escribe el desarrollador.** Con 1.000 lecturas, el 94 % del callback es el bucle del intérprete.
- Esto corrige la Entrega 7: la llamada nativa no costaba ≈ 110 ns por lectura, sino ≈ 43. El resto era el bucle.
- Publicar en la columna cuesta lo mismo que en la réplica (p50 151 ns).

**Consecuencias para el diseño.**
1. **Lecturas numéricas por columnas compartidas**: valores, selección, scroll, rectángulos de layout. JS las lee sin cruzar.
2. **Texto y estructuras variables por llamada**, por id. Cruza una vez y solo cuando se necesita.
3. **Escrituras por lotes validados**, con dueño por nodo, como hasta ahora.
4. La disposición de las columnas se define una sola vez, y de ahí se generan los accesos en Rust y el `.d.ts` de TypeScript.
5. Con el puente fuera del camino, **la próxima palanca de rendimiento es el código JS**: lo que hace el SDK por cada evento y cuánta lógica pesada se delega a primitivas Rust.

## Entrega 9 · Contrato único y columnas compartidas en el runtime · 19 de noviembre de 2026

**Regla de trabajo.** Cada pieza del puente se justifica en cómo funcionan los dos motores y en una medición, no en agregar por agregar.

| Decisión | Fundamento en el motor | Medición |
| --- | --- | --- |
| Números por columnas compartidas | En QuickJS, indexar un typed array es una instrucción del intérprete. Una llamada nativa pasa por un trampolín que convierte cada `JSValue` | ≈ 4 ns contra ≈ 43 ns por lectura (Entrega 8) |
| Texto por llamada y por id | JS guarda cadenas de 8 o 16 bits por carácter y Rust UTF-8: cruzar texto siempre transcodifica | — |
| Rust es el único escritor de las columnas, JS solo lee | El préstamo de Rust (`&` compartido / `&mut` exclusivo) llevado entre los dos lenguajes. Rust escribe solo sin JS en ejecución y en el mismo hilo | Test: JS no puede modificar la columna |
| Una declaración genera las dos lentes | Los tipos de Rust se conocen al compilar: una macro puede derivar el tipo TypeScript | Test: `app/jsnir.d.ts` se genera y se compara |

**Contrato** ([contrato.rs](../src/contract.rs)):
- `columnas!` declara cada columna una sola vez: nombre en JS, nodo y campo de origen, y documentación. Genera `project` (el valor de cada columna para un nodo) y su parte del `.d.ts`. Columnas actuales: `valor` (etiqueta), `hijos` (columna), `seleccionDesde` y `seleccionHasta` (campo).
- `def!` registra cada función nativa y deriva su firma TypeScript de los tipos Rust del closure, mediante el trait `Ts`. El parámetro `Ctx` no aparece en la firma.
- `app/jsnir.d.ts` se genera desde ahí. Un test falla si el archivo quedó desactualizado (`JURUI_ACTUALIZAR=1 cargo test contrato` lo regenera). Los scripts ya no declaran `jurui` a mano.

**Funciones que quedan, una por razón:**
- escribir: `set`, `commit`, `crearTexto`, `ponerTexto`;
- leer texto: `leerTexto`.

Se retiraron `leer`, `version`, `seleccion` y `hijos`: eran números leídos por llamada y ahora salen de las columnas.

**Columnas en el runtime.** Cada VM instala `jurui.col` con un `Int32Array` inmutable por columna, respaldado por un `Box<[i32]>` en el heap de Rust e indexado por id de nodo. El hilo de la app las mantiene al día en tres momentos, siempre sin JS en ejecución:
- al ponerse al día con el registro, con los nodos que tocaron los lotes de la interfaz;
- después de producir cada lote propio (la vista optimista);
- entera, si un rechazo obliga a rehacer la vista.

Cuando aparece un id mayor que la longitud, las columnas se recrean al doble de tamaño. Por eso el `.d.ts` advierte que no se guarden entre callbacks. `counter.ts` lee `jurui.col.valor[2]`. `chat.ts` lee `col.hijos`, `col.seleccionDesde` y `col.seleccionHasta`, y usa `leerTexto` solo para el borrador.

| Verificación | Resultado |
| --- | --- |
| 11 tests del núcleo | Pasan. Incluyen la ráfaga de 64 eventos en vuelo leyendo la columna optimista en tres protocolos, el chat leyendo la selección por columnas y el `.d.ts` generado |
| Guion del chat | 50 capturas, **mismo hash que antes del cambio** en los cinco estados |
| Contador egui, cuatro modos | 5 de 5 exactos en cada uno |
| Scroll de 10.000 mensajes en modo JS | Completo. Las columnas crecieron hasta cubrir los 10.100 ids |
| Masonry, seis adaptadores | Píxeles exactos. Todos compilan |

**Costo de mantener las columnas** ([resumen](../metrics/contrato-01/curva/summary.json)): en modo directo, 3,43 µs con 1 operación (3,20 en la Entrega 7) y 120,8 µs con 1.000 (117,1). Sincronizar cada lote, que restaura los cuatro buffers y escribe los ids tocados, cuesta unas décimas de microsegundo. Está en el borde del ruido entre series.

**Pendiente.**
- Medir cuánto de esas décimas es restaurar cuatro `Persistent` por lote. Si pesa, convendría guardar los punteros en una sola estructura y validar solo que el buffer siga acoplado.
- Llevar a columnas el scroll y los rectángulos de layout cuando existan como estado de la interfaz.
- Generar también los nombres de nodos y operaciones del montaje (hoy JSON escrito a mano en los scripts).

## Entrega 10 · Tipos verificados de punta a punta · 19 de noviembre de 2026

**Objetivo.** La forma de los datos que cruzan la frontera existe en un solo lugar (Rust), y el compilador de TypeScript la hace cumplir en las apps.

**TypeScript.** `typescript` 7.0.2 se instaló como dependencia de desarrollo con versión exacta (`package.json`; `node_modules/` queda fuera del control de versiones). Cada app se verifica por separado contra `jurui.d.ts`, porque las dos son scripts globales y definen las mismas funciones (`app/tsconfig.counter.json` y `app/tsconfig.chat.json`, que extienden `tsconfig.base.json`). Las opciones salen del motor:
- `lib: ["es2020"]` sin DOM, porque QuickJS no tiene `window` ni `document`;
- `erasableSyntaxOnly`, que admite solo sintaxis que `stripTypeScriptTypes` borra sin transformar, que es lo que hace `build.rs`;
- `strict`, y sin variables ni parámetros sin uso.

**Enums del contrato.** `Node` y `Op` se declaran con `contrato_enum!`, que genera el enum Rust con serde y su unión TypeScript desde la misma declaración. El tag y el nombre de cada variante salen de `rename_all = "lowercase"` en los dos lados. Marcas:
- `(skip)`: Rust deriva el campo y JS no lo envía (los hijos de una columna);
- `(default)`: el campo es opcional en JS (el texto y la selección iniciales de un campo);
- `(ui)`: la variante es exclusiva de la interfaz y queda fuera del tipo de JS (`SetInput`).

El `.d.ts` incluye `Nodo`, `Operacion` y `Lote`. Los montajes y los lotes JSON de `counter.ts` y `chat.ts` se declaran como `Lote`.

**Defensa en dos capas.** Que JS intente escribir el borrador ya no compila: `"setinput"` no es asignable a `Operacion`. Si llegara igual, por ejemplo desde código sin verificar, Rust lo rechaza (`nodo_de_la_interfaz`).

| Verificación | Resultado |
| --- | --- |
| `tsc` sobre `counter.ts` y `chat.ts` | Sin errores |
| `tsc` sobre usos incorrectos | Detecta la columna inexistente, el argumento de tipo equivocado, el `string \| undefined` asignado a `string`, el `setinput` desde JS y la etiqueta sin `value` |
| Test `contrato_typescript_apps_typecheck` | Corre `tsc` dentro de `cargo test`. Falla al cambiar `valor` por `valorr` en `counter.ts` y vuelve a pasar al restaurarlo |
| 12 tests del núcleo, egui, Masonry | Pasan |
| Chat, contador en cuatro modos, seis adaptadores | 50 capturas con el mismo hash que antes. 5 de 5 exactos por modo. Compilan |

**Lo que queda escrito en dos lugares.** `Lote` es una línea literal en `contrato::dts`, porque `Batch` no pasa por la macro (su `base` la completa Rust y JS no la envía). Las apps Rust de referencia (`ChatRust`, `rust_mount`) duplican a propósito la lógica de las apps JS, porque son la línea base de las mediciones.

## Entrega 11 · Latencia hasta la luz en pantalla, y la espera acotada · 19 de noviembre de 2026

**Pregunta.** Hasta que la persona lo ve en pantalla, ¿un evento que pasa por JS llega igual de rápido que uno resuelto en Rust?

**Fuente de la marca de tiempo.** `wp_presentation` (protocolo Wayland estable *presentation-time*). Según su especificación, `presented` indica cuándo el contenido «se convirtió en luz por primera vez» en la salida principal, aproximado por el compositor a partir del *flip* del framebuffer. Se interpreta con el reloj de `clock_id`; niri anunció 1 (`CLOCK_MONOTONIC`). `feedback` corresponde al próximo envío de contenido de la superficie, así que se pide durante el frame, antes de que eframe haga el swap.

**Implementación** ([presentacion.rs](../backends/egui/src/presentation.rs), [medicion.rs](../backends/egui/src/measurement.rs)). Se usa la misma conexión de winit: su `wl_display` con `Backend::from_foreign_display` y su `wl_surface` con `ObjectId::from_ptr`, más una cola de eventos propia. `wayland-client` 0.31.15, `wayland-backend` 0.3.17 y `wayland-protocols` 0.32.13 ya estaban en el árbol vía winit, con las mismas features: no se compiló ningún crate nuevo.

**Error encontrado en el camino.** La primera versión leía el socket de Wayland por su cuenta (`prepare_read` + `read`). Así dejaba en la cola de winit eventos que winit no sabía que tenía que despachar, y la ventana se trababa al tercer frame. Ahora solo se despacha lo que winit ya repartió a nuestra cola.

**Método.** `raw_input_hook` inyecta cada evento al empezar un frame y toma la marca con el reloj del compositor. Para el contador es un clic (pulsar en un frame, soltar en el siguiente; cuenta desde que se suelta). Para el chat es una letra. Cada frame registra lo que muestra (el valor o el largo del borrador) y pide su aviso. La latencia de un evento va desde su inyección hasta el primer frame **presentado** que muestra el valor esperado. El camino kernel → compositor → app es el mismo en todos los modos, así que la comparación aísla lo que cambia el puente. 90 frames de calentamiento, 120 eventos cada 15 frames y dos corridas por configuración. Ventana real en niri, vsync activo y monitor a 120 Hz (8,333 ms). [Datos](../metrics/presentacion-02/) · [análisis](../scripts/presentation.py)

**Primer hallazgo: un frame de más en JS.** En modo JS, los 120 eventos aparecieron exactamente un frame más tarde que en Rust (32,9 contra 24,6 ms). `Counter::show` activa y hace `poll` en el mismo pase. La respuesta de JS (≈ 3 µs) todavía no llegó cuando se pinta, y el frame no la espera.

**Mejora: espera acotada en `Runtime::poll`.** Si hay eventos activados hace menos de `budget`, `poll` se bloquea en el canal con `recv_timeout`, sin consumir CPU, hasta ese plazo contado desde la activación.
- **Fundamento:** la ida y vuelta medida es de ≈ 3 µs y un frame a 120 Hz dura 8.333 µs.
- **Plazo por defecto:** 1 ms, el 12 % del frame. egui dibuja el contador en ≈ 0,4 ms.
- **Si el callback es lento:** el plazo vence y los frames siguientes no esperan nada.
- **Alcance:** está en `Runtime`, así que lo reciben los seis adaptadores sin cambios. Se configura con `JURUI_ESPERA_US`; 0 lo desactiva.
- **Test:** 100 respuestas seguidas entran en el mismo `poll`. Con un callback de 200 ms, `poll` vuelve dentro del plazo, y el siguiente no espera.

| Configuración | p50 | p95 | p99 | Frames de retraso (240 eventos) |
| --- | ---: | ---: | ---: | --- |
| Contador, Rust nativo | 24,57–24,60 ms | 24,66–24,67 | 24,67–24,68 | 0 en todos |
| Contador, JS sin espera | 32,91–32,92 ms | 32,99–33,02 | 33,00–33,03 | **1 en todos** |
| **Contador, JS con espera** | **24,55–24,57 ms** | 24,66–24,67 | 24,68–24,69 | **0 en todos** |
| **Contador, directo con espera** | **24,55–24,57 ms** | 24,65–24,66 | 24,66–24,68 | **0 en todos** |
| Chat escribiendo, Rust | 24,57–24,61 ms | 24,67 | 24,68–24,69 | 0 en todos |
| Chat escribiendo, JS | 24,55–24,58 ms | 24,67 | 24,67–24,68 | 0 en todos |

Ningún frame se descartó y los 2.880 eventos se midieron. Las diferencias entre modos (≈ 0,03 ms) son menores que las que hay entre las dos corridas de un mismo modo. Los ≈ 24,6 ms son el pipeline de este equipo: frame de egui, swap, composición de niri y escaneo, unos tres períodos de refresco. Con la espera acotada, el contador sigue exacto en los cuatro modos. Los seis adaptadores compilan y pasan los tests de egui y Masonry.

### Estado de los umbrales de la propuesta

| Umbral | Resultado | Entrega |
| --- | --- | --- |
| Tecla → borrador visible: sin diferencia (±5 % o ±0,1 ms) | ✅ Diferencia ≈ 0,03 ms, dentro de la variación A/A | 11 |
| Frame p99 con 10.000 mensajes: hasta +10 % | ✅ Sin diferencia detectable con control A/A (−6,7 % y +2,7 %) | 7 |
| Evento → lote, de 1 a 100 operaciones: p99 ≤ 1 ms | ✅ Directo: 4,9–23,9 µs | 9 |
| Evento → lote, 1.000 operaciones: p99 ≤ 4 ms (informativo) | ✅ Directo: 127 µs | 9 |
| Evento → primer frame: hasta un frame extra | ✅ **Cero frames extra** con la espera acotada | 11 |
| Memoria residente: hasta +20 MB | ✅ +1,4 MB, medido sin ventana | 2 |
| Arranque hasta el primer frame: hasta +100 ms | ✅ +0,6 ms de montaje, medido sin ventana | 2 |
| Tamaño del ejecutable: hasta +5 MB | ✅ ≈ +2,2 MB en egui (QuickJS enlazado estáticamente) | 2 |

**Límites de la serie.**
- Un solo equipo, un compositor y un monitor a 120 Hz.
- La entrada se inyecta en la app, así que el tramo de hardware → compositor no está medido (es igual en todos los modos).
- La memoria y el arranque se midieron sin ventana. Medirlos con ventana y en frío sigue pendiente.
- Falta la comparación del beneficio frente a una WebView (Tauri o Electron) con la misma pantalla.

## Entrega 12 · SDK en TSX con señales · 19 de noviembre de 2026

**Objetivo.** Que escribir una interfaz sea cómodo (TSX, componentes, estado reactivo) sin perder lo ganado en el puente.

**Decisiones y su fundamento.**

| Decisión | Fundamento | Medición o fuente |
| --- | --- | --- |
| Señales de grano fino, sin DOM virtual. Cada componente corre una sola vez | Comparar árboles en JS es bytecode en el intérprete. Una señal emite exactamente la operación de su nodo | Bucle JS ≈ 67 ns por vuelta (Entrega 8) |
| Cambios por llamada directa (`set`, `ponerTexto`) | Es la vía más barata de escritura | ≈ 0,1 µs por operación (Entrega 3) |
| Estructura por lote JSON: un subárbol, una llamada (`jurui.lote`) | Crear nodos es poco frecuente y el JSON se amortiza en el subárbol | Entrega 2 |
| Listas con mutaciones explícitas (`items.push`) | Un ítem nuevo es un `create`, O(1), sin comparar la lista | — |
| Efectos agrupados por evento | Varias escrituras en un callback producen un solo cambio | Test |
| Lo que escribe la interfaz se lee de Rust (`TextInput` → `campo.texto()`, `campo.seleccion()`) | Un solo dueño por dato (Entregas 7 a 9) | — |
| Tipos en dos capas: el contrato generado (cable) y el SDK escrito en TS, verificado contra él | Si el SDK emite algo que Rust no acepta, no compila | Entrega 10 |
| JSX compilado por `tsc` (`jsx: react`, `jsxFactory: h`) | JSX no es sintaxis borrable y `stripTypeScriptTypes` no lo transforma. TypeScript 7 retiró `outFile`, así que `build.rs` emite SDK y app por separado y los concatena | — |

**El SDK** ([sdk.ts](../app/sdk.ts)): `signal`, `list`, los componentes `Column`, `List`, `Text`, `Label`, `Button` y `TextInput`, y `mount`. Implementa el protocolo con Rust con dos funciones: `montar()`, que emite un lote con el árbol, y `evento(callback, texto)`, que corre el manejador y vacía los efectos.

**Las apps** quedan así:
- [chat.tsx](../app/chat.tsx) es el boceto de la propuesta: `<List items={mensajes}>`, `<TextInput onSubmit>` y `<Text>{estado}</Text>`.
- [contador.tsx](../app/counter.tsx) se ejecuta con `jurui-egui sdk`.

`counter.ts` queda como banco de protocolos (`js`, `directo`, `buffer`).

**Cambios en el contrato.**
- Nodo `List`: una lista con desplazamiento, distinta de `Column`, que es solo disposición.
- `Input.submit`: el callback del envío.
- `lote(json)` reemplaza a `crearTexto`.

El chat de Rust ya no usa ids fijos: después del montaje encuentra la lista, el campo con envío y el estado por su tipo. En el primer build, el SDK se compiló contra el `.d.ts` anterior y `tsc` señaló exactamente los tres cambios todavía no publicados. Los errores de tipos no frenan el build, para poder regenerar `jurui.d.ts`, pero sí hacen fallar `cargo test`.

| Verificación | Resultado |
| --- | --- |
| 16 tests del núcleo | Pasan. Incluyen la ráfaga de 64 eventos en modo `sdk` y tres tests nuevos del SDK |
| Contador TSX | Monta **el mismo árbol** que la referencia en Rust y emite **un solo `set` por clic**, sin tocar el botón (la prueba que pedía la propuesta) |
| Chat TSX | Cada envío emite **exactamente** un `create` y un `settext`. El estado lee el borrador y la selección vivos |
| Agrupamiento | Tres escrituras en un evento producen un solo `settext` con el valor final |
| `lote` inválido | Lanza en JS y no deja operaciones |
| Chat TSX en egui, 50 capturas | **Mismo hash que antes** en los cinco estados |
| Contador TSX en egui | 5 de 5 estados exactos |
| Scroll con el chat TSX | 10.000 mensajes completos (un lote por mensaje) |
| `tsc` sobre las tres apps | Sin errores |

**Costo.** Ida y vuelta con 1 operación: SDK 5,59 µs frente a 4,66 µs del protocolo directo escrito a mano en la misma serie ([resumen](../metrics/sdk-01/curva/summary.json)). El SDK agrega ≈ 0,9 µs por evento (señal, efecto, agrupamiento), un 0,01 % de un frame a 120 Hz.

**Latencia hasta la pantalla real** (niri, 120 Hz, [datos](../metrics/sdk-01/)):

| App | Eventos | p50 | Frames de retraso |
| --- | ---: | ---: | --- |
| Chat TSX escribiendo | 120 | 24,56 ms | 0 en todos |
| Chat Rust escribiendo | 120 | 24,58 ms | 0 en todos |
| Contador TSX | 960 en 8 corridas | 24,53–24,59 ms | 0 en 957, 15 en 3 |
| Contador Rust y directo | 1.560 entre ambas series | 24,54–24,60 ms | 0 en todos |

**Clics perdidos (pendiente).** Tres clics del contador TSX (en 960 eventos) no produjeron cambio y el valor llegó con el clic siguiente, de ahí los 15 frames. En la corrida que tenía traza hubo 119 activaciones para 120 clics, y cada activación produjo su operación: **el puente no perdió nada, egui no reconoció el clic**. La causa no está identificada. Se probó la hipótesis de que la entrada real del sistema se mezclara con la inyectada: el arnés ahora la descarta y la cuenta, pero en las corridas siguientes no hubo ningún evento real que descartar. Así que la hipótesis no queda confirmada, aunque las 600 mediciones posteriores salieron sin pérdidas. La diferencia contra los demás modos (3 de 960 contra 0 de 1.560) no es concluyente.

## Entrega 13 · Layout con Taffy · 19 de noviembre de 2026

**Objetivo.** Que el layout lo calcule Rust a partir de estilos declarados en TSX, y que JS pueda leer las medidas sin cruzar la frontera.

**Fuentes.** La documentación de Taffy (Context7, `/dioxuslabs/taffy`) y su código en la versión 0.15.1:
- según su benchmark, calcular cuesta ≈ 0,25–0,47 µs por nodo (10.000 nodos ≈ 2,7–4,8 ms);
- cachea por nodo y marca como sucio lo que cambia;
- `location` es relativa al padre;
- el redondeo viene activado (`use_rounding`);
- una hoja sin función de medición mide 0×0.

**Decisiones.**

| Decisión | Fundamento |
| --- | --- |
| El layout se calcula en Rust, en el hilo de la interfaz | Depende del tamaño de la ventana y, más adelante, de la medición de texto: es estado derivado de la interfaz |
| Llega a JS como lote de la interfaz (`setrect`) solo con lo que cambió, y de ahí a las columnas `x`, `y`, `ancho` y `alto` | El mismo camino que el borrador: un solo dueño y lectura en ≈ 4 ns (Entrega 8) |
| Los ítems de una `List` no pasan por Taffy: la lista es una hoja | 10.000 filas costarían 2,7–4,8 ms, más de medio frame a 120 Hz. El adaptador ya dibuja solo las filas visibles |
| Solo la feature `flexbox` de Taffy | El chat no necesita grid, block ni float, y el ejecutable tiene un umbral de tamaño |
| `Estilo` (`width`, `height`, `grow`, `padding`, `gap`) declarado con `contrato_struct!` | Una sola definición para Rust y TS. Serde rechaza campos desconocidos y el árbol rechaza valores negativos o no finitos (`estilo_invalido`) |
| La raíz toma el tamaño de la ventana | Taffy trata el espacio disponible de la raíz como una restricción blanda |

**Implementación.**
- [layout.rs](../src/layout.rs) mantiene un `TaffyTree` paralelo al árbol. Cada nodo creado se suma, salvo los descendientes de una lista. `compute(ancho, alto)` fija el tamaño de la raíz solo si cambió, deja que Taffy use su caché, acumula las posiciones relativas desde la raíz y devuelve solo los rectángulos que cambiaron.
- `Chat::layout` los publica como lote de la interfaz: entran al árbol y al registro de la réplica, y desde ahí a las columnas.
- En el SDK, `Column`, `List`, `Text` y `TextInput` reciben `style?: Estilo`, que pasa tal cual al nodo, así que no hay que repetir la lista de campos. `campo.rect()` lee el rectángulo desde las columnas.
- [chat.tsx](../app/chat.tsx) declara el layout que egui tenía escrito a mano: una columna con `padding: 16`, `gap: 16` y `grow: 1`, una lista con `grow: 1` y un campo con `height: 34`. La etiqueta de estado queda fuera de esa columna y, sin medición de texto, mide 0 (paso 3).
- El adaptador egui dejó sus constantes de posición y dibuja con los rectángulos del núcleo. El botón «Enviar» sigue siendo parte del widget del campo (92 px y 12 de separación).

| Verificación | Resultado |
| --- | --- |
| 19 tests del núcleo | Pasan |
| Taffy frente al layout escrito a mano, Rust y JS | **Idéntico**: lista 16,16 de 608×398 y campo 16,430 de 608×34. Con 800×600, 16,16 de 768×518 y 16,550 de 768×34. Recalcular sin cambios no publica nada |
| JS lee el layout | `jurui.col.x/y/ancho/alto` devuelven 16, 430, 608 y 34 para el campo, y 398 de alto para la lista |
| Estilos inválidos | `paddng` no pasa serde y `padding: -1` no pasa el árbol |
| Chat en egui dibujado con Taffy, 50 capturas | **Mismo hash que antes** en los cinco estados |
| Scroll de 10.000 mensajes, egui, Masonry, seis adaptadores, `tsc` | Bien |

**Costo** ([medición](../metrics/layout-01/costo.txt), tres ejecuciones, 10.000 mensajes en el historial):

| `Chat::layout` por frame | p50 | p99 |
| --- | ---: | ---: |
| Estable (sin cambios, con caché de Taffy) | 0,22–0,23 µs | 0,24–0,48 µs |
| Redimensionando en cada llamada (recalcula y publica) | 1,14–1,19 µs | 2,6–3,5 µs |

El tamaño del historial no influye porque sus filas no pasan por Taffy. Hasta la pantalla real, con el layout calculado en cada frame, el chat da JS 24,57 ms y Rust 24,60 ms (p50), con 0 frames de retraso en 120 de 120 eventos. El ejecutable de egui pasó de 15,38 a 15,92 MB.

**Pendiente.**
- Medición de texto (paso 3): que `Text` tenga tamaño propio, y con eso la etiqueta de estado se vuelva visible.
- Estilos reactivos: hoy son fijos al crear el nodo.
- Contenedor horizontal (`Row`).
- Alineación.
- Llevar el layout al contador, que conserva su diseño fijo como referencia de píxeles para los seis adaptadores.

### Antecedente: NSH (Native Shell)

Al consultar Taffy apareció [NSH](https://deepwiki.com/longbridge/nsh/1-overview), de Longbridge. Según su documentación en DeepWiki (no revisamos el código):
- es un framework de UI nativa para interfaces generadas por IA y distribuidas en tiempo de ejecución, que carga módulos TSX firmados;
- ejecuta JS con QuickJS y calcula el layout con Taffy;
- su grafo reactivo (`signal`, `computed`, `effect`) vive en Rust, y los componentes corren una sola vez, sin DOM virtual;
- envía las mutaciones a backends nativos por plataforma (y HTML en la web) como lotes con diferencias (`MountBatch`).

Coincide con decisiones a las que Jurui llegó midiendo: componentes de una pasada, señales de grano fino, QuickJS y Taffy. Según esa misma documentación, las diferencias están en el foco. NSH apunta a UI distribuida y firmada sobre widgets nativos de cada plataforma. Jurui apunta a igualar el rendimiento de Rust nativo, con memoria compartida para leer, réplica por registro, árbol particionado por dueño y medición hasta la luz en pantalla. Convendría leer su código antes de decidir el paso 4 (renderer propio), para comparar dónde resuelve cada uno el cruce JS → Rust.

## Entrega 14 · JIT para QuickJS (`quickjs-jit`): medido y descartado · 19 de noviembre de 2026

**Pregunta.** Lo que queda del costo de Jurui es el bytecode de JS (≈ 67 ns por vuelta de bucle, Entrega 8). ¿Lo baja un JIT?

**El crate.** `quickjs-jit` 0.14.2 es el fork de rquickjs de Longbridge (`github.com/longbridge/rquickjs`, licencia MIT). En crates.io tiene un solo dueño, existe desde el 1 de septiembre de 2026 y suma ≈ 50.000 descargas. Mantiene la API de rquickjs y QuickJS-NG `0fdea21` (0.16.2+), la misma base que ya usamos. El fork por sí solo **no trae el JIT**: expone una ABI (`jit-abi`) donde se engancha otro crate, `quickjs-jit-runtime`, un JIT por niveles con Cranelift (feature `compiler`). Ejecución nativa solo en macOS, Windows y Linux (x86_64 y aarch64): **no hay iOS ni Android**, que de todos modos prohíben generar código ejecutable. Al revisar el `Cargo.lock` de NSH apareció un error del análisis anterior: NSH usa el fork, pero **no el JIT**.

**Método.** Mismo ejecutable con y sin `JURUI_JIT=1`, y `Jit::attach` con la configuración por defecto (política automática, compila después de 32 llamadas o 56 vueltas de bucle, un hilo compilador). Se tomó una línea base con `rquickjs` 0.14.0 en la misma sesión. Las métricas del JIT (`native_entries`, `compile_failures`, `tier2_entries`) se leyeron al cerrar cada VM para confirmar qué se ejecutó en código nativo. [Datos](../metrics/jit-01/)

**Corrección.** La suite completa pasó con el JIT en debug. En release, con los tests en paralelo, falló una vez el test que exige que una respuesta rápida entre en el mismo `poll` (menos de 1 ms); aislado pasó 3 de 3, y sin JIT siempre pasa. Indicio, sin confirmar, de una pausa al subir de nivel.

| Banco, 1.000 vueltas u operaciones (p50, µs) | rquickjs 0.14.0 | Fork, intérprete | Fork + JIT |
| --- | ---: | ---: | ---: |
| `vacio`: bucle de JS puro | 67,30 | 64,63 | **1,84** |
| `memoria`: lectura de typed array | 71,26 | 69,39 | 70,16 |
| `replica`: llamada nativa | 113,88 | 111,98 | 113,12 |
| `local-directo`: callback con `jurui.set` | — | 82,97 | 83,84 |
| `directo`, ida y vuelta | 150,83 | 152,42 | 138,13 |
| Con 1 vuelta u operación: `vacio` / `local-directo` | 0,21 / — | 0,22 / 0,36 | 0,36 / 0,56 |

| Métricas del JIT | Fallos de compilación | Entradas nativas | Nivel optimizado |
| --- | ---: | ---: | ---: |
| `vacio` | 0 | 1.873 | 1.862 |
| `memoria` | 4 | 121 | 0 |
| `replica` | 4 | 66 | 0 |
| `local-directo` | 1 | 0 | 0 |

**Lectura.**
- El JIT acelera **35×** un bucle de JS puro.
- **No compila** las funciones que llaman a funciones nativas ni las que leen typed arrays, que son las primitivas del puente. Esas siguen interpretadas y pagan el costo de contabilidad del JIT (+0,14–0,20 µs por llamada antes de compilar).
- El fork sin JIT rinde igual que rquickjs.
- Ejecutable de egui: **22,39 MB con Cranelift**, contra 15,92 MB. Son +6,47 MB aunque el JIT esté apagado: el umbral de +5 MB queda superado.

**Decisión.** Se volvió a `rquickjs` 0.14.0. El JIT no mejora el puente, rompe el umbral de tamaño, no existe en móviles, viene de un crate joven con un solo dueño y dejó un indicio de pausas al compilar. Sirve como referencia para una app con cálculo pesado en JS puro: cuando el JIT compile llamadas nativas y typed arrays, conviene repetir la medición. Para reproducir:

```toml
rquickjs = { package = "quickjs-jit", version = "=0.14.2" }
rquickjs-jit = { package = "quickjs-jit-runtime", version = "=0.14.2", features = ["compiler"] }
```

con `rquickjs_jit::Jit::attach(&rt, rquickjs_jit::JitConfig::builder().build()?)` al crear la VM, guardando el `Jit` mientras viva el runtime.

### ¿Se puede quedar solo con lo útil del JIT?

Se midió sin símbolos (`strip`), que es lo que pesa al distribuir. Cada variante se compiló en un directorio aparte y el proyecto se restauró después.

| Ejecutable egui, sin símbolos | MB | Diferencia con el intérprete |
| --- | ---: | ---: |
| Intérprete (`rquickjs` 0.14.0) | 11,97 | — |
| JIT tal como se publica (Cranelift para x86 y arm64) | 17,04 | +5,07 |
| JIT con Cranelift solo para x86 (copia parcheada del crate) | 16,33 | +4,36 |
| Lo anterior más Cranelift compilado con `opt-level = "s"` | 15,99 | **+4,02** |

- **Dos recortes posibles:** compilar Cranelift solo para la arquitectura de destino ahorra 0,71 MB. Optimizar por tamaño el propio compilador ahorra 0,34 MB más. Esa opción cambia la velocidad con que el JIT compila (en un hilo de fondo), no la calidad del código que genera. El rendimiento de esta variante no se midió.
- **El piso:** queda en ≈ 4 MB, que es el núcleo de Cranelift. Bajar de ahí exigiría otro tipo de JIT, por ejemplo uno de plantillas (*copy-and-patch*, como el de CPython 3.13). Eso es otro proyecto, no un recorte.
- **El costo de mantenerlo:** las features de Cargo se suman, así que el primer recorte necesita mantener una copia parcheada de `quickjs-jit-runtime`. Lo sostenible es pedir aguas arriba una feature por arquitectura.
- **Conclusión:** solo convendría como feature opcional de escritorio para apps con cálculo pesado en JS puro. Para el puente no cambia nada.

## Entrega 15 · Interfaz en vivo en desarrollo · 19 de noviembre de 2026

**Objetivo.** Editar el TSX y ver el cambio en la ventana abierta, sin reiniciar y sin perder lo que la persona está escribiendo.

**Decisiones.**

| Pieza | Decisión | Fundamento |
| --- | --- | --- |
| Reemplazar la app | Operación `remove` (subárbol). La recarga es **un lote atómico**: `[remove(raíz vieja), create(…app nueva)]` | Nunca hay un frame con la app a medio reemplazar. Validar todo o nada (Entrega 1) |
| Estado de la interfaz | Borrador, selección y scroll viven en Rust (`Chat` y egui). Al remontar, Rust vuelve a publicar el borrador en el campo nuevo | Un dueño por dato: la recarga de JS no puede tocar lo que es de la interfaz |
| Estado de JS | La VM vieja entrega un snapshot (`estadoDeRecarga()`) y la nueva lo lee en `__juruiEstadoPrevio` antes de correr. Cada `signal` o `list` recupera su valor si coinciden su posición en el orden de creación y su valor inicial | Si cambia el inicializador, el valor guardado deja de ser confiable (la misma regla que NSH) |
| Código con errores | Si no compila o tiene errores de tipos, no se recarga. Si el script no carga, la VM vieja sigue. **Una excepción en un callback ya no tumba la app**: ese evento no produce cambios y las operaciones que alcanzó a emitir se descartan | En desarrollo, guardar un archivo a medio escribir es lo normal. Cierra el pendiente de inyección de fallas |
| Vigilar y compilar | [vivo.rs](../src/live.rs) revisa las fechas de modificación de `app/chat.tsx` y `app/sdk.ts` cada 250 ms y compila con el mismo `tsc` del build. Se activa con `JURUI_VIVO=1` | Sin dependencias nuevas. Para desarrollo alcanza |

**El árbol.**
- **Raíz explícita.** `Tree.root` reemplaza a la deducción por «árbol vacío», que dejaba de valer al poder eliminar nodos.
- **Padre de cada nodo**, para desprender un nodo eliminado en O(1).
- **`remove` solo para la app** y solo sobre nodos que ya existían antes del lote.
- **Validación con un registro superpuesto**: lo creado o eliminado en el lote manda sobre el árbol, así que un lote puede eliminar la raíz y volver a crear los mismos ids.
- **El layout se reconstruye entero** (`Layout::from_tree`) cuando cambia la estructura, y las columnas se resincronizan completas.

**Dos errores encontrados en el camino.**
1. **Orden del registro.** Al aplicar el remontaje, la interfaz publicaba el borrador en el campo nuevo **antes** de confirmar el lote, y la réplica recibía un `setinput` para un nodo que en su árbol confirmado todavía no existía. Regla: la respuesta a un lote va antes que cualquier lote de la interfaz que dependa de él. Ahora se responde apenas se aplica.
2. **Validación O(n²).** El registro superpuesto era una lista y cada `create` la recorría entera: montar 10.000 nodos tardaba ≈ 775 ms en debug. Recargar un chat con 10.000 mensajes guardados es exactamente ese caso, y se valida tres veces (vista, confirmado, interfaz). Con un `HashMap` baja a **9,7 ms en debug y ≈ 1–1,7 ms en release**. Un lote de una sola operación no reserva memoria (`HashMap::new()` es perezoso): el modo directo sigue en 4,96 µs con 1 operación.

Un tercer detalle, de egui: con la ventana quieta no redibuja, y la recarga se recoge en un frame. Con el modo vivo activo, el chat pide un frame cada 100 ms.

| Verificación | Resultado |
| --- | --- |
| Test de `remove` | Desprende, elimina subárboles, rechaza lo inexistente y lo que viene de la interfaz, y reemplaza la raíz reutilizando ids |
| Test de recarga en vivo (modo JS) | Después de recargar con código cambiado: **los 2 mensajes siguen** (señal de JS), **el borrador «en edición» sigue en el campo nuevo** (interfaz), el layout se rehízo (16,430 de 608×34) y el estado sale con el formato nuevo. Un script roto se rechaza y la app sigue. Un callback que lanza no produce cambios y la app sigue |
| Ventana real (niri, `JURUI_VIVO=1`) | 7 guardados: **compila en 41–45 ms y aplica en 1–8 ms**. El guardado con un error de sintaxis se rechazó con los errores de `tsc` y la app siguió |
| 21 tests, egui, Masonry, chat (50 capturas con el mismo hash), scroll, seis adaptadores, `tsc` | Bien |

De guardar a ver el cambio pasan hasta ≈ 250 ms de espera del vigilante, más ≈ 45 ms de compilación y unos pocos ms de aplicación.

**Límites.**
- Las señales se emparejan por orden de creación. Si se reordenan señales con el mismo valor inicial, sus valores se cruzan.
- Las señales creadas dinámicamente (dentro de ítems) se reinician.
- Los valores deben poder pasar por JSON.
- Recargar remonta la app entera; todavía no hay parches por módulo como en NSH.
- Solo el chat tiene recarga; el contador no.
- Producción (paquetes firmados, límites de la VM) queda pendiente.

## Entrega 16 · Medición y rasterización de texto en Rust

Jurui usa cosmic-text 0.14.2 con las fuentes del sistema para componer y rasterizar los mensajes y el estado del chat. Taffy consulta ese mismo motor para medir los nodos `Text` del layout. Un `SetText` invalida la medida del nodo. Los rectángulos calculados siguen llegando a JS por las columnas compartidas. El núcleo conserva sus dependencias libres de frameworks de UI. [Motor de texto](../src/text.rs) · [Medición en Taffy](../src/layout.rs)

El estado vacío mide cero. Cuando aparece texto, ocupa su espacio en la ventana y se ajusta al ancho disponible. El historial conserva filas virtualizadas de una línea. El adaptador egui sube las imágenes RGBA como texturas y reutiliza las de las filas que siguen visibles. El editor, su IME, el portapapeles, el botón y el desplazamiento siguen a cargo del adaptador. [Adaptador egui](../backends/egui/src/chat.rs)

En release, con OpenGL offscreen y llvmpipe, el guion de 10.000 mensajes recorrió todas las filas, con un máximo de 18 por frame. La tabla muestra la mediana de tres procesos por caso, con 1.100 frames medidos en cada proceso y orden alternado.

| Ejecutable | Rust p50 / p99 | JS p50 / p99 | Tamaño sin símbolos |
| --- | ---: | ---: | ---: |
| Entrega 15 | 0,457 / 0,897 ms | 0,453 / 1,246 ms | 12,05 MB |
| Texto de Jurui | 0,725 / 1,258 ms | 0,713 / 1,150 ms | 13,73 MB |

El dibujo suma alrededor de 0,27 ms a la mediana con este adaptador. El ejecutable suma 1,68 MB sin símbolos. El test del núcleo verifica que actualizar el estado provoca una nueva medición con la misma ventana, que reducir el ancho hace saltar de línea y que ambas líneas se rasterizan a escala 2×. Las 50 capturas del guion del chat conservan la paridad entre Rust y JS. [Banco de scroll](../backends/egui/src/offscreen.rs) · [Prueba de medición](../src/chat.rs)

## Entrega 17 · Editor de una línea en Rust

El editor comparte las fuentes y la rasterización de la Entrega 16. Cosmic-text conserva el búfer, forma los glifos y traduce posiciones del puntero a posiciones del texto. Jurui agrega la edición por grafemas, el desplazamiento horizontal al cursor, el historial de deshacer y rehacer y la geometría de selección. Los índices del editor son bytes de UTF-8. El SDK sigue recibiendo rangos en caracteres. [Editor](../src/editor.rs) · [Publicación del borrador](../src/chat.rs)

La composición IME se dibuja como una vista provisional del borrador. `Commit` aplica el texto al documento y una cancelación conserva el documento y la selección previos. En una entrega de eventos con un `Commit`, Enter queda reservado al IME en ambos órdenes. Un Enter posterior envía el mensaje. La recarga de JS conserva el editor en Rust, incluida una composición activa. [Eventos de egui-winit](https://docs.rs/egui-winit/0.36.2/src/egui_winit/lib.rs.html#717) · [Pruebas del editor](../src/chat.rs)

El adaptador enruta el foco y los eventos del sistema, presenta las texturas, entrega el rectángulo del cursor al IME y envía las solicitudes del portapapeles a eframe. Se probaron esos intercambios con eventos sintéticos. Esta sesión recibió un error de permisos al conectar al socket Wayland, así que quedan pendientes el IME y el portapapeles del sistema en una ventana real. El historial vertical sigue usando el scroll virtualizado del adaptador. [Adaptador del editor](../backends/egui/src/editor.rs)

Pasaron los 27 tests del núcleo y las tres pruebas de egui. Las 50 capturas del guion conservaron la paridad de Rust y JS. El banco de 10.000 mensajes siguió recorriendo todo el historial con un máximo de 18 filas por frame. Las cifras son la mediana de tres procesos por caso, con 1.100 frames por proceso en OpenGL offscreen y llvmpipe.

| Ejecutable | Rust p50 / p99 | JS p50 / p99 | Tamaño sin símbolos |
| --- | ---: | ---: | ---: |
| Entrega 16 | 0,685 / 1,134 ms | 0,684 / 1,028 ms | 13,73 MB |
| Editor de Jurui | 0,671 / 0,974 ms | 0,677 / 0,989 ms | 13,90 MB |

Estas corridas miden el desplazamiento con el editor en reposo. Falta medir la latencia desde la entrada hasta la presentación durante la edición. [Banco de scroll](../backends/egui/src/offscreen.rs)

## Entrega 18 · Historial compartido y backend propio con winit y wgpu

Rust conserva la posición del historial, calcula el rango visible por división y actualiza la barra de scroll. Al llegar un mensaje, sigue el final si el usuario estaba abajo y conserva el desplazamiento si estaba leyendo más arriba. La recarga de JS conserva ese estado de interfaz. egui y el nuevo backend usan el mismo historial. [Historial en Rust](../src/history.rs)

`jurui-gpu` tiene su propio manifiesto. winit lleva la ventana y la entrada, arboard el portapapeles, y wgpu ejecuta un shader propio que compone rectángulos y texturas de glifos con recorte y alfa premultiplicado. El layout, el editor, la virtualización y el rasterizado tipográfico vienen de Jurui. El árbol de dependencias del nuevo binario está libre de frameworks de UI. El contador SDL2 queda como referencia de rasterizado en CPU. [Ventana](../backends/gpu/src/window.rs) · [Renderer](../backends/gpu/src/renderer.rs) · [Presentación de superficies en wgpu](https://wgpu.rs/doc/wgpu/struct.Surface.html)

La captura wgpu fuera de pantalla coincide en sus 307.200 píxeles entre Rust y JS. Incluye un mensaje enviado, el editor con selección y el texto de estado. Se ejecutó sobre OpenGL con llvmpipe. El ejecutable `jurui-gpu` sin símbolos mide 13,64 MB en esta máquina.

```sh
cargo build --release --locked --manifest-path backends/gpu/Cargo.toml --target-dir target
salida=$(mktemp -d)
for modo in rust js; do
  env -u DISPLAY -u WAYLAND_DISPLAY EGL_PLATFORM=surfaceless LIBGL_ALWAYS_SOFTWARE=1 WGPU_BACKEND=gl \
    target/release/jurui-gpu "$modo" --offscreen "$salida/$modo.ppm"
done
cmp "$salida/rust.ppm" "$salida/js.ppm"
```

Pasaron los 29 tests del núcleo, las 50 capturas del chat en egui y la prueba de conversión de rangos UTF-8 del IME de winit. El banco egui recorrió 10.000 mensajes en cada modo con un máximo de 17 filas por frame. En una corrida de 1.100 frames por modo, el p50/p99 fue 0,671/0,931 ms en Rust y 0,670/1,068 ms en JS. El banco cuenta todo el recorrido para la cobertura, incluido el calentamiento en el que se muestran las últimas filas. Los tiempos se toman después del calentamiento. [Banco de scroll](../backends/egui/src/offscreen.rs)

La apertura en ventana real devolvió `NoCompositor` en esta sesión. Quedan por validar la presentación en una GPU física, el IME y el portapapeles del sistema en Wayland o X11. La API IME de winit 0.30 proporciona preedición y confirmación. Operaciones de protocolo adicionales, como borrar texto circundante, requieren integración de plataforma en este backend. [Eventos de IME en winit](https://docs.rs/winit/latest/winit/event/enum.Ime.html)

## Entrega 19 · Formulario de React Bits portado a JSNIR y a GPUI · 19 de noviembre de 2026

Se portó el Stepper de React Bits (demo "onboarding": cuatro pasos, campo de texto, indicador deslizante y transiciones con resorte) dos veces bajo la misma [especificación](../comparison/stepper/SPEC.md): en TSX sobre JSNIR y en Rust sobre GPUI 0.2.2. La referencia es el componente original sin cambios en Chromium. Las tres versiones usan Inter 4.1 y la misma ventana de 640 × 560. [Comparación completa](../comparison/stepper/README.md)

**Qué hubo que agregar a JSNIR.** El árbol ganó un contenedor general (`box`) con filas, alineación, márgenes, posición absoluta, recorte y nodos que siguen el rectángulo de otro (`follow`); íconos de trazo; y una apariencia (`paint`) con degradé, aro interior, brillo superior, sombras, opacidad, desplazamiento, escala, brillo y variantes de hover, press y foco. Las transiciones las declara JS y las corre Rust en cada cuadro ([motion.rs](../src/motion.rs)), incluida la curva `linear()` del resorte y animaciones de layout (altura, posición y ancho). Un host genérico ([host.rs](../src/host.rs)) corre cualquier app TSX y entrega una lista de dibujo; el renderer de wgpu agrega sombras desenfocadas, sombras interiores y capas con opacidad de grupo, y mezcla en el espacio codificado, como el navegador.

**Fidelidad.** Píxeles que difieren de Chromium en más de 6 %: 0,08–0,14 % en JSNIR y 0,18–0,36 % en GPUI, en los seis estados. Lo que queda es antialiasing de glifos. Las capturas de GPUI salen de una ventana a 2× reducida, lo que suma error de remuestreo.

**Hallazgo.** cosmic-text 0.14 ignora el alfa del color al rasterizar glifos (`TODO: blend base alpha?` en `swash.rs`): el texto atenuado salía opaco. Se aplica el alfa en JSNIR; la diferencia con Chromium bajó a la mitad.

**Costo.** Pantalla: 389 líneas de TSX en JSNIR, contra 1.162 de Rust más 574 de un campo de texto adaptado en GPUI, que no trae uno. Binario sin símbolos: 15,0 MB contra 19,6 MB. Memoria residente máxima: 121 MB contra 109 MB, después de evitar que wgpu iniciara también el driver OpenGL de Mesa (costaba unos 29 MB). GPUI no tiene transformaciones ni espaciado entre letras: su port no escala el botón al presionar ni ajusta el título.

**Pendiente.** Comparar las animaciones en ventanas reales cuadro a cuadro contra el navegador; medir arranque y latencia de entrada en el host genérico; llevar el chat al host genérico para que haya una sola ventana.
