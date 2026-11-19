# Interfaces de escritorio en TypeScript, con interacción nativa en Rust

## La experiencia que queremos ofrecer

Una persona escribe una aplicación de escritorio en TypeScript. Describe dónde va un campo de texto y qué debe pasar al pulsar «Enviar». Cuando alguien usa esa aplicación, Rust dibuja la ventana, recibe el teclado y conserva el borrador, la selección y el foco. JavaScript se ejecuta cuando hay trabajo de la aplicación. El ejecutable distribuido incluye su propia VM y abre la interfaz sin WebView.

La primera demostración será un chat local. En una sola ventana se puede escribir, enviar un mensaje y recorrer el historial mientras la lógica de la aplicación está ocupada. Eso permite ver y medir el beneficio que justificaría construir el runtime.

## Cómo se sentiría programarlo

Boceto de la API pública en TSX. Los nombres son provisionales.

```tsx
function Chat() {
  const mensajes = signal<Mensaje[]>([])

  return (
    <Column gap={8}>
      <List items={mensajes} keyBy={m => m.id}>
        {m => <Text>{m.texto}</Text>}
      </List>
      <TextInput
        placeholder="Escribí un mensaje"
        onSubmit={({ texto }) => {
          mensajes.update(ms => [...ms, crearMensaje(texto)])
        }}
        clearOnSubmit
      />
    </Column>
  )
}
```

La lista recibe una señal y usa la clave de cada mensaje para conservar su identidad. El campo entrega el texto confirmado al callback. Propongo que, con capacidad disponible para encolar el envío, Rust capture el texto y limpie el campo en la misma operación local. Si la cola está llena, conserva el borrador e indica que el envío está pendiente de reintento. La respuesta tardía de JavaScript solo agrega el mensaje a la lista. Así, si la persona ya empezó a escribir otro mensaje, su nuevo borrador sigue intacto. Durante una composición IME, confirmar una sugerencia debe conservar el comportamiento del editor y evitar un envío accidental.

## Corte de implementación

| Pieza | Primera responsabilidad | Prueba observable |
| --- | --- | --- |
| Núcleo Rust | Mantener nodos con identidad estable y aplicar lotes con una versión esperada | Un lote con una referencia inválida deja intactos árbol y versión |
| Host egui | Dibujar el árbol, recibir entrada y administrar el estado local del campo | Cambiar un texto vecino conserva foco, cursor y selección |
| Hilo JavaScript con QuickJS | Ejecutar el script empaquetado, recibir eventos y producir lotes | Un callback lento retrasa su resultado y el usuario puede seguir escribiendo |
| SDK TypeScript | Asociar señales y callbacks con nodos concretos | Cambiar un contador emite una actualización de texto y conserva el botón |

## Orden de trabajo

1. **Contador de extremo a extremo.** Un script monta una columna, una etiqueta y un botón. Pulsar el botón genera un evento. El callback incrementa el contador y envía un lote que cambia la etiqueta. Registrar el evento, el lote y la versión aplicada. Una llamada explícita de montaje alcanza para esta etapa.
2. **Chat local.** Agregar el campo, el historial con claves estables y el manejo de envíos. Introducir una demora deliberada de 500 ms en el callback. Escribir un segundo borrador durante esa demora y comprobar que se conserva, junto con el foco y el orden de los envíos.
3. **Comparación reproducible.** Construir la misma pantalla en egui puro, con idénticas fuentes, tamaño de ventana, historial y virtualización. En el mismo equipo y en release, repetir un recorrido de 10.000 mensajes. Registrar percentiles p50/p95/p99 del tiempo de frame, latencia desde la entrada hasta su presentación, espera de cada lote y memoria residente. Publicar la diferencia respecto de la referencia y registrar el equipo y la frecuencia de pantalla.
4. **Decisión de arquitectura.** Si el costo aparece en la espera de los lotes, revisar el despacho y las colas. Si aparece al recorrer filas visibles, investigar el adaptador. Probar la misma pantalla con un segundo backend cuando haya que evaluar una limitación concreta o demostrar portabilidad del contrato.

## Criterios de decisión

La referencia es la misma pantalla resuelta en Rust nativo, en el mismo equipo, adaptador y perfil release, con el renderer anotado. La pregunta es si pasar por JavaScript cuesta poco frente a esa referencia. Los umbrales se fijan antes de medir y solo cambian con una nota que explique el motivo.

| Métrica | Umbral para JS + Rust frente a Rust nativo | Motivo |
| --- | --- | --- |
| Tecla → borrador visible | Sin diferencia: p99 dentro de ±5 % o ±0,1 ms | El editor no pasa por JS. Si esto cambia, falla la premisa |
| Frame p99 recorriendo 10.000 mensajes | Hasta +10 % | El historial ya está en Rust |
| Evento → lote aplicado, de 1 a 100 operaciones | p99 ≤ 1 ms | Menos del 6 % de un frame a 60 Hz |
| Evento → lote aplicado, 1.000 operaciones | p99 ≤ 4 ms | Un cuarto de frame. Informativo: ubica el codo de la curva |
| Evento → primer frame con el cambio | Hasta un frame extra a 60 Hz en p99 | La respuesta se ve en el frame siguiente |
| Memoria residente | Hasta +20 MB | La VM debe pesar mucho menos que una WebView |
| Arranque hasta el primer frame | Hasta +100 ms | |
| Tamaño del ejecutable | Hasta +5 MB | |

Salvo la fila informativa, todos son obligatorios. Si alguno falla, la traza indica la etapa responsable y se intenta una corrección acotada. Si después de esa corrección sigue fallando, se reformula el reparto entre Rust y JS o se abandona la idea. El beneficio frente a una WebView (Tauri o Electron) se mide aparte con la misma pantalla.

## Primera entrega aceptable

Un ejecutable de escritorio, el código de la vista del contador y una traza de un clic. La traza debe mostrar qué nodo recibió el evento, qué lote actualizó la etiqueta y en qué frame se presentó el valor. Una segunda ejecución con el callback demorado debe permitir redibujar la ventana durante la espera. Esta entrega demuestra el recorrido completo. El chat y su referencia en Rust sirven después para cuantificar el costo.
