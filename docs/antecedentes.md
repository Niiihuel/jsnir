# Antecedentes · NSH y la interfaz en vivo en Rust · 19 de noviembre de 2026

## NSH (Native Shell, Longbridge)

Framework de UI nativa para interfaces que se distribuyen en tiempo de ejecución: apps TSX firmadas que corren sobre un runtime Rust con QuickJS y Taffy. Licencia Apache 2.0. Se leyó el código del commit `fc4d7a8` (19 de noviembre de 2026) sin compilarlo ni ejecutarlo. **[V]**: visto en el código. **[D]**: tomado de su documentación interna, sin contrastar línea por línea con el código.

### Cómo cruzan de JS a Rust

| Tema | NSH | Jurui |
| --- | --- | --- |
| Hilo de JS | Propio (`nsh-engine`); la entrada llega por un canal sin límite **[V]** | Propio; eventos por cola acotada y registro sin límite |
| Motor | QuickJS-NG 0.16.2 a través de `quickjs-jit` 0.14.2, el fork de rquickjs de Longbridge (`Cargo.toml:241`). **No usan el JIT**: el JIT es otro crate (`quickjs-jit-runtime`), que no está en su `Cargo.lock` **[V]** | QuickJS-NG 0.16.2 a través de `rquickjs` 0.14.0 |
| Escrituras | Una función nativa por operación, decodificada a un `Op` tipado, sin JSON **[V]** | Igual (`jurui.set`, `ponerTexto`); la estructura va por lote JSON |
| Señales | Grafo reactivo en Rust (planificación de Solid 1.9); **cada lectura y escritura es una llamada nativa** en el mismo hilo **[V]** | Señales en JS; leer es bytecode, sin cruzar |
| Lecturas síncronas del estado nativo | **No hay.** `measure()` devuelve una promesa. El valor y la selección de un campo llegan como eventos, con un contador de ediciones para que un valor viejo de JS no pise lo que se está escribiendo **[D]** | **Sí**: columnas compartidas (≈ 4 ns por lectura) y réplica por registro. JS no puede escribir el campo |
| Layout y texto | Se calculan en el hilo de JS, en el `commit`; el hilo principal solo recibe un `MountBatch` con lo que cambió **[V]** | El layout se calcula en el hilo de la interfaz y vuelve a JS por la réplica |
| Lotes con errores | Se salta la operación inválida y el árbol queda consistente; algunos errores detienen la VM **[V]** | El lote entero se rechaza de forma atómica |
| Contrato | Declaraciones TS, especificaciones y códigos generados desde Rust, verificados por tests **[D]** | Igual (`contrato.rs`, `jurui.d.ts`) |
| Listas virtualizadas | Altos variables con sumas prefijas (árbol de Fenwick), celdas recicladas, a lo sumo 150 operaciones o 2 ms de enlace por turno **[V]** | Filas de alto fijo dibujadas por egui |
| Rendimiento | Contador +1: 10 operaciones, 7 ítems, ≈ 261–269 µs por commit en build de depuración **[D]** | Contador +1 en release: 1 operación, ≈ 3–5 µs ida y vuelta |

### Interfaz en vivo

- **Compilación** **[V]**: Oxc 0.153 compila TSX y JSX en el CLI, rolldown empaqueta, y opcionalmente se compila a bytecode de QuickJS. Solo se usa el bytecode si corresponde a la versión exacta del motor; si no, se carga el fuente.
- **Paquetes firmados** **[V]**:
  - Formato `.nshpkg`: `NSHP`, cabecera JSON, archivos y firma.
  - Ed25519 sobre `contexto \0 SHA-256(mensaje)`, con un contexto por tipo (paquete, certificado, revocaciones).
  - El almacén de confianza guarda el build más alto aceptado por app y rechaza uno anterior (protección contra rollback).
  - **[D]** Una clave raíz fuera de línea certifica las claves de publicación, por patrón de app y ventana de validez. Las revocaciones llevan número de secuencia. La verificación corre fuera del hilo de la interfaz, y el build nuevo se acepta cuando el host lo declara sano.
- **Carga**: un paquete reemplaza la app entera de una superficie, y cada superficie tiene una VM **[V]/[D]**.
- **Sandbox** **[D]**:
  - capacidades declaradas en `nsh.json` y recortables por la app anfitriona (red, archivos, procesos, portapapeles);
  - sin `eval` ni `Function`, con los prototipos congelados;
  - 256 MB de heap y 2 s por llamada por defecto.
- **Recarga en desarrollo** **[V]**:
  - `notify` vigila los archivos y los cambios llegan por WebSocket como un parche por módulo;
  - los componentes de primer nivel se envuelven en proxies estables;
  - las señales de primer nivel conservan su valor si su inicializador no cambió;
  - un parche tarda ≈ 0,2–0,5 ms **[D]**; si no se puede aplicar, la app se recarga.

### Qué tomar para Jurui, con su fundamento

1. ~~Probar `quickjs-jit`~~. Probado en la Entrega 14 de [investigacion.md](investigacion.md): acelera 35× el JS puro, pero no compila las funciones que llaman a funciones nativas ni las que leen typed arrays, agrega 6,5 MB y no corre en iOS ni Android. No se adoptó.
2. **Límites de la VM.** Memoria máxima y tiempo por llamada (rquickjs tiene límite de memoria y manejador de interrupciones). Es la deuda de «inyección de fallas» anotada al principio: hoy un callback colgado deja a la app esperando.
3. **Paquetes firmados para la interfaz en vivo.** Ed25519 con contexto por tipo, protección contra rollback, claves en dos niveles y aceptación después de un chequeo de salud. Es un diseño probado que no conviene reinventar.
4. **Bytecode precompilado**, ligado a la versión exacta del motor, con el fuente como respaldo. Mejora el arranque.
5. **Oxc en lugar de `tsc`/Node para compilar TSX.** Es un crate de Rust: el build dejaría de depender de Node y la compilación podría hacerse en proceso, por ejemplo para recargar en desarrollo. `tsc` se mantendría para la verificación de tipos.
6. **Listas con alto variable** con sumas prefijas, para cuando los mensajes ocupen más de una línea (después de la medición de texto).

### Dónde se diferencia Jurui

- **Lectura síncrona sin cruzar**: NSH no la tiene (sus lecturas son promesas o eventos).
- **Un dueño por nodo** en lugar de contadores de edición: JS no puede escribir el estado de la interfaz.
- **Lotes atómicos** y **latencia medida hasta la luz**, igual a Rust nativo.
- Las señales de NSH viven en Rust, así que cada lectura cruza. En Jurui viven en JS. Es un compromiso: el grafo en Rust permite que los componentes Rust participen de la reactividad; el nuestro ahorra la llamada nativa por lectura.

## La interfaz en vivo en el ecosistema Rust

| Herramienta | Qué ofrece | Límite |
| --- | --- | --- |
| Dioxus + Subsecond | Parcheo del código Rust en caliente durante el desarrollo (`dx serve --hotpatch`), desviando llamadas por una tabla de saltos | Experimental en 0.7. La documentación de Subsecond y de Freya dice que el estado de los hooks se reinicia en cada parche, aunque el anuncio de Dioxus 0.7 dice que se conserva. No es para producción ([docs de subsecond](https://docs.rs/subsecond/0.7.10/), [Freya](https://docs.rs/crate/freya/0.4.0-rc.21/source/src/_docs/hot_reload.rs), [herramientas de Dioxus](https://www.mintlify.com/DioxusLabs/dioxus/advanced/devtools)) |
| Slint | Vista previa en vivo con un intérprete de `.slint`; `slint-interpreter` puede cargar un `.slint` en ejecución | La vista previa es solo para desarrollo. El intérprete actualiza la interfaz declarativa, pero la lógica sigue compilada en Rust ([vista previa](https://docs.slint.dev/latest/docs/slint/guide/tooling/live-preview/), [intérprete](https://docs.rs/crate/slint-interpreter/1.16.0/source/lib.rs)) |
| Makepad | DSL de diseño recargable sin recompilar; Splash (2.0) como lenguaje de UI en ejecución | Los cambios en Rust necesitan recompilar. Los datos de 2.0 vienen de fuentes de la comunidad, sin verificar ([documentación](https://publish.obsidian.md/makepad-docs)) |

**Lectura.** En Rust existe la recarga en caliente durante el desarrollo, limitada y experimental. Lo que no existe es actualizar **interfaz y lógica** en una app ya instalada, que es lo que en React Native resuelven las actualizaciones por aire (OTA): Expo Updates y los reemplazos de CodePush, retirado el 31 de marzo de 2025, varios con firma de paquetes ([Expo](https://expo.dev/blog/what-to-do-without-codepush), [comparativa 2026](https://blog.codemagic.io/react-native-ota-tools-in-2026/), [Revopush](https://revopush.org/revopush-vs-expo-updates)). Jurui puede ofrecer las dos cosas, y su arquitectura tiene una ventaja propia: **el estado de la interfaz (borrador, selección, scroll, layout) vive en Rust y sobrevive por construcción a la recarga de JS**. La persona no pierde lo que está escribiendo.

**Restricción de las tiendas (pendiente de verificar en la fuente).** Según fuentes de terceros, la licencia de Apple (3.3.1(B)) permite descargar código interpretado si no cambia el propósito principal de la app y no evita la firma, el sandbox ni otras protecciones del sistema ([Capgo](https://capgo.app/blog/apple-remote-code-execution-capgo-live-updates/), [WebF](https://openwebf.com/docs/add-webf-to-flutter/store-guidelines)). El texto exacto tiene que leerse en los documentos de Apple antes de decidir el modelo de distribución.
