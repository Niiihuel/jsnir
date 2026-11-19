# Prueba 02 · Escribir mientras el chat procesa un envío

**Estado:** implementada y medida en egui. Los demás adaptadores conservan el guion como especificación. La primera serie del contador sigue en el [informe de investigación](investigacion.md).

## Pregunta

¿Puede una persona seguir escribiendo y seleccionando texto mientras la lógica de la aplicación tarda 500 ms en aceptar el mensaje anterior? Se compararán el mismo chat en Rust puro y con el puente JS de Jurui para egui, GPUI, Iced, Masonry, Slint y Jurui propio.

## Pantalla común propuesta

Área cliente de 640 × 480 píxeles a escala 1. Fondo y colores del contador. Historial arriba, borrador de una línea debajo y botón «Enviar». Usar el mismo archivo de fuente empaquetado en los seis adaptadores, fijar su SHA-256, tamaño y altura de línea antes de generar las referencias. Fijar también las cajas de cada control, el espaciado, el estilo de selección y el sentido del scroll. Las capturas deben controlar la fase del cursor intermitente. Cada backend dibuja el texto y los controles a partir del mismo estado.

## Guion de edición

| Punto | Acción | Estado que hay que comprobar |
| --- | --- | --- |
| A | Abrir el chat vacío y enfocar el campo | Historial vacío, borrador vacío y foco en el campo |
| B | Escribir «Hola, Córdoba» | Acento visible, cursor al final |
| C | Pulsar Enter | Rust captura el texto una sola vez, encola el envío y limpia el campo |
| D | Mientras el envío está retenido, escribir «segundo borrador» y seleccionar «borrador» | Texto visible y selección activa antes de liberar el envío |
| E | Liberar la respuesta | Aparece «Hola, Córdoba» exactamente una vez. El borrador, el foco y la selección conservan el estado de D |

Para las capturas, un punto de sincronización explícito retendrá la respuesta hasta obtener D. Así la velocidad de la máquina no cambia el orden del guion. Para la medición temporal, usar una demora de 500 ms en ambos modos fuera del hilo de interfaz. Registrar la hora de cada entrada y la llegada del commit. En la variante JS la demora afecta al worker que procesa el envío.

## Qué registrar

- **Imagen.** Comparar cada punto con su PNG de referencia y comparar ambos modos de cada backend. Guardar las dos capturas, cantidad de píxeles distintos y una imagen de diferencias por cada fallo. Criterio de aprobación: cero píxeles diferentes. Una discrepancia de glifos se conserva como resultado.
- **Comportamiento.** Registrar los mensajes con sus identificadores, texto del borrador, foco y rango de selección. Verificar que cada carácter escrito durante la demora se refleje antes del commit.
- **Tiempo.** Medir entrada → actualización visible del borrador y commit → primera imagen con el mensaje. Indicar si el punto final corresponde a lectura de framebuffer o a presentación. Informar p50, p95, p99, tamaño de muestra y si la captura está incluida en el tiempo.
- **Carga adicional.** En una corrida separada, recorrer el mismo historial de 10.000 mensajes con altura y virtualización equivalentes. Guardar tiempos de frame, filas visibles y memoria residente. Mantener separados el guion de edición y el de scroll.
- **Trazabilidad.** Anotar versión del código y de dependencias, sistema, fuente, renderer, GPU o driver software, escala y frecuencia. Adjuntar tiempos crudos, capturas y resumen Markdown. Marcar como pendiente cualquier backend que no haya ejecutado el guion.

## Resultado del primer prototipo · egui

Se implementó el camino completo en `src/chat.rs`, `app/chat.ts` y el adaptador egui. El hilo de la interfaz posee el borrador, la selección y el foco. Al enviar, transfiere una copia lógica independiente del mensaje a una cola acotada. El worker espera 500 ms, ejecuta el callback y devuelve una operación que añade el mensaje al historial. La respuesta carece de un campo que pueda sobrescribir el borrador. Si la cola está llena, Rust restituye el texto al editor.

El ejecutor inyectó texto, Enter y ocho pulsaciones de Mayús + flecha izquierda en el editor real. En cada proceso comprobó que «borrador» quedaba seleccionado (caracteres 8 a 16), que el campo conservaba el foco y que «Hola, Córdoba» se agregaba una sola vez. Para este primer experimento se usó la demora real y se comprobó que D se alcanzó antes del commit. El cursor intermitente se desactivó en la captura.

Cinco procesos por modo, cinco estados por proceso, 50 imágenes de 640 × 480 leídas del framebuffer OpenGL con llvmpipe. Las 50 coincidieron con el estado correspondiente de la primera ejecución Rust. A y C coincidieron también entre sí, como corresponde al editor vacío y enfocado. Esta referencia permite comparar ambos modos del mismo adaptador. La prueba de texto entre backends sigue abierta.

| Desde el envío hasta completar la captura | Rust, mín.–máx. | JS + Rust, mín.–máx. |
| --- | ---: | ---: |
| D, segundo borrador seleccionado | 2–3 ms | 2–3 ms |
| E, primer mensaje agregado y selección conservada | 539–545 ms | 541–543 ms |

Estas mediciones incluyen lectura de píxeles y escritura de la captura. El escenario incluye la demora intencional y usa una ventana fuera de pantalla. Las cinco observaciones por modo sirven para verificar el orden temporal. [Tiempos crudos](../metrics/chat-02/tiempos.csv) · [Hashes por estado](../metrics/chat-02/paridad.json)

### Historial de 10.000 mensajes

En otra corrida se precargó el mismo historial por la cola del modo correspondiente, sin demora artificial. Se aplicaron eventos de rueda al historial. Se comprobó que la unión de los rangos dibujados cubrió las 10.000 filas. Durante el recorrido se dibujaron entre 17 y 18 filas por frame. Por modo se descartaron 100 frames de calentamiento y se midieron 1.100, desde el inicio del pase egui hasta que acabó el dibujo de OpenGL. La simulación adelantó el reloj de la UI en pasos de 1/60 s y ejecutó los frames sin esperar al monitor.

| Modo | Frame p50 | p95 | p99 |
| --- | ---: | ---: | ---: |
| Rust | 0,414 ms | 0,465 ms | 0,546 ms |
| JS + Rust | 0,406 ms | 0,465 ms | 0,661 ms |

Una corrida por modo en esta máquina. Durante el scroll, el historial ya estaba en Rust. La prueba de memoria residente y la latencia hasta presentación física siguen pendientes. [Frames Rust](../metrics/chat-02/scroll-rust.csv) · [Frames JS](../metrics/chat-02/scroll-js.csv)

### Reproducir desde la raíz de Jurui

```bash
python scripts/chat_replay.py metrics/local-chat
SDL_VIDEODRIVER=offscreen target/release/jurui-egui rust scroll metrics/local-chat/scroll-rust.csv
SDL_VIDEODRIVER=offscreen target/release/jurui-egui js scroll metrics/local-chat/scroll-js.csv
```

Para usar la ventana en el escritorio, ejecutar `target/release/jurui-egui js chat` o `target/release/jurui-egui rust chat`.
