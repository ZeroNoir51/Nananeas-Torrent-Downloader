# Local BitTorrent engine changes / Cambios locales del motor

Upstream: https://github.com/ikatson/rqbit — librqbit 9.0.1, Apache-2.0.
Keep the upstream LICENSE and any NOTICE files in vendor/librqbit-9.0.1.
Conservar LICENSE y cualquier NOTICE original dentro de vendor/librqbit-9.0.1.

## English

Cargo.toml must retain:

```toml
[patch.crates-io]
librqbit = { path = "vendor/librqbit-9.0.1" }
```

Local modifications:
- src/read_buf.rs: fixed 256 KiB receive ring buffer, heap-backed storage and temporary contiguous buffer.
- src/error.rs: receive-buffer error sizes and positions use usize instead of truncating to u16.
- src/peer_info_reader/mod.rs: eight initial metadata requests per peer, replenished on received blocks; up to three retries of rejected blocks with increasing delays; SHA-1 verification retained.
- Optional metadata diagnostics log sizes, block numbers and responses without block contents.

The 256 KiB limit is fixed, not adaptive. Larger messages may still exceed it. Each connection allocates that receive buffer; reorganizing it temporarily requires a second buffer. Metadata resolution also allocates its own metadata storage per peer. Limits for outgoing messages were not changed or validated by this receive-side fix.

Arch runtime evidence: the previously failing magnets 560fbbd1…, 068a5666… and 1edfd986… indexed successfully after the relevant fixes; the user confirmed downloads work. The 068a5666… eight-request trial completed in about 26 seconds with no metadata rejects or retries. This is not a guarantee for every swarm or platform. Windows has not been validated.

Run engine tests separately: cargo test in the application does not run dependency unit tests.

## Español

Conservar la sección [patch.crates-io] anterior: selecciona la copia local del motor.

Cambios locales:
- src/read_buf.rs: búfer circular de recepción fijo de 256 KiB, con almacenamiento y reserva temporal en el heap.
- src/error.rs: tamaños y posiciones de los errores usan usize; se eliminó el truncamiento a u16.
- src/peer_info_reader/mod.rs: ocho solicitudes iniciales de metadatos por peer, con reposición al recibir bloques; hasta tres reintentos por bloque rechazado, con esperas crecientes. Se conserva la verificación SHA-1.
- Diagnóstico opcional de tamaños, números de bloque y respuestas, sin contenido de los bloques.

El límite de 256 KiB es fijo. Un mensaje mayor todavía puede excederlo. Cada conexión reserva su búfer, y la reorganización necesita temporalmente otro. La resolución de metadatos usa además su propia memoria por peer. Este ajuste de recepción no modifica ni valida los límites de mensajes de salida.

Pruebas en Arch: los magnets antes fallidos 560fbbd1…, 068a5666… y 1edfd986… lograron indexarse con los ajustes correspondientes; el usuario confirmó las descargas. La prueba de 068a5666… con ocho solicitudes terminó en unos 26 segundos sin rechazos ni reintentos. Esto no garantiza todos los torrents ni todas las plataformas. Windows sigue pendiente de validación.

Ejecutar las pruebas del motor por separado: cargo test de la aplicación no ejecuta las pruebas unitarias de sus dependencias.
