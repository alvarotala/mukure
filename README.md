# Mukure

Prototipo de reproductor de escritorio con **Tauri 2, React y Rust**. Abre magnet links, archivos `.torrent` y videos locales; permite elegir un video del torrent y reproducirlo mientras se descarga. El objetivo inicial es **macOS con Apple Silicon**.

## Probar en desarrollo

Requisitos: Node.js compatible con Vite 8, pnpm, Rust estable y Xcode Command Line Tools. Consulta los [requisitos oficiales de Tauri](https://v2.tauri.app/start/prerequisites/#macos). La primera compilación descarga dependencias y puede tardar varios minutos.

```sh
pnpm install
pnpm tauri dev
```

`scripts/with-rust.sh` usa el toolchain del proyecto en `.tools/` si existe; en otro caso usa el Rust disponible en `PATH`. `.tools/` no forma parte del código versionado.

Para generar una aplicación local:

```sh
pnpm tauri build --bundles app
```

El resultado queda en `src-tauri/target/release/bundle/macos/Mukure.app`. Este comando no prepara firma ni notarización para distribución.

Comprobaciones disponibles:

```sh
pnpm build
pnpm test
pnpm check:rust
pnpm test:rust
```

`pnpm dev` por sí solo muestra una vista previa en el navegador; la reproducción y el motor de torrents requieren la aplicación Tauri.

Verificado en esta Mac: compilación de la interfaz y del `.app`, pruebas de HTTP Range y CORS, transferencias BitTorrent reales con `.torrent` y magnet entre dos clientes locales, lectura antes de completar la descarga, seek y limpieza. En la app nativa se reprodujeron un MP4 local, un magnet y un `.torrent` de prueba en modo directo y/o FFmpeg, y un MKV con pista de audio mediante FFmpeg. Son archivos sintéticos propios; no se probó disponibilidad o rendimiento de torrents públicos ni películas largas.

Subtítulos: 24 pruebas Rust y 4 de frontend pasan, incluyendo lectura de SRT desde un peer real, cancelación sin peers, extracción integrada y conservación de tiempos al adelantar. La revisión nativa verificó SRT externo y MP4 timed text integrado en reproducción directa y FFmpeg, mostrar/ocultar, ajuste y conservación de la selección al cambiar de modo.

## Uso

1. Pega un magnet o abre un archivo `.torrent`.
2. Espera sus metadatos y elige un archivo de video en la lista real del torrent.
3. Usa **Directo** si tu sistema admite el formato; prueba **FFmpeg** si no lo reproduce.
4. Detén la sesión para desconectar el torrent y borrar sus datos descargados.

También puedes abrir un video del disco para probar el reproductor sin depender de la disponibilidad de peers. Sólo se mantiene un torrent activo por vez.

## Subtítulos

Con un video abierto, el panel **Subtítulos** permite:

- Abrir un `.srt` o `.vtt` local, o elegir uno incluido en el torrent sin descargar todos sus archivos.
- Pulsar **Buscar pistas integradas** para detectar los idiomas y elegir una pista de texto dentro del video. FFprobe obtiene la lista y FFmpeg entrega los subtítulos progresivamente, tanto en reproducción directa como en el modo FFmpeg.
- Mostrar u ocultar la pista y ajustar su sincronización hasta ±60 segundos. Un valor positivo retrasa el texto.

La selección se conserva al cambiar el modo de reproducción y se limpia al cambiar de video. En reproducción directa, los saltos reinician la extracción integrada cerca de la nueva posición, conservando sus marcas de tiempo. La disponibilidad de piezas y el índice del contenedor pueden demorar las pistas de un torrent.

Los archivos externos admiten UTF-8, UTF-16 con BOM y Windows-1252, hasta 5 MiB. Las pistas integradas de texto incluyen SRT, ASS/SSA y MP4 timed text; la conversión conserva el texto y formato básico, no los estilos y posiciones avanzadas de ASS/WebVTT. Los subtítulos de imagen (PGS/DVD/DVB) se identifican, pero todavía no se reproducen.

## Qué ocurre en tu equipo

- **Interfaz:** React dentro del WebView del sistema; los comandos Tauri conectan la interfaz con Rust.
- **BitTorrent:** `librqbit 9.0.1` embebido. Sus lectores priorizan las piezas cercanas al cursor y esperan datos verificados antes de entregarlos. Un salto temporal solicita los rangos correspondientes.
- **Transporte del video:** un listener HTTP interno en `127.0.0.1`, con puerto aleatorio y token por ejecución. Sólo entrega el video seleccionado; no requiere iniciar un servicio aparte ni expone una API de control en la red local.
- **Decodificación directa:** el elemento de video del WebView usa los codecs disponibles en el sistema. El transporte admite peticiones HTTP Range; los saltos temporales dependen también del formato y del WebView.
- **Compatibilidad FFmpeg:** un proceso local convierte a H.264 y AAC dentro de MP4 fragmentado. La interfaz lo incorpora mediante MediaSource, con un buffer de aproximadamente 30 segundos por delante y 30 por detrás; la descarga HTTP se cancela al cerrar el video. La conversión empieza desde el inicio y **este prototipo no implementa seeking arbitrario en ese modo**. Puede consumir bastante más CPU.

No hay login, cuentas ni backend remoto de Mukure. No se agregó telemetría a la aplicación. **BitTorrent sí usa Internet:** conecta con peers, trackers y DHT, y puede subir piezas a otros peers; local no significa anónimo ni desconectado. Reproducir archivos locales y sus subtítulos no requiere esas conexiones.

Los datos del torrent se guardan bajo el directorio de caché de la aplicación, en `sessions/session-*`; la ruta exacta aparece en la ayuda. La caché puede crecer hasta el tamaño del video. Detener el torrent borra sus datos y un cierre limpio elimina la caché de esa ejecución. Un cierre forzado o un crash puede dejar archivos residuales. El video local original no se borra.

## FFmpeg

Para facilitar la prueba en esta Mac, el proyecto usa los binarios de desarrollo `@ffmpeg-installer/darwin-arm64` y `@ffprobe-installer/darwin-arm64`; la configuración actual también los incluye como recursos del `.app` local. No hace falta instalar un servidor ni ejecutarlos manualmente. FFprobe se usa para descubrir las pistas integradas.

Se puede elegir otro ejecutable con `MUKURE_FFMPEG` o usar una instalación disponible en `PATH`. Reinicia la aplicación después de cambiarlo:

```sh
MUKURE_FFMPEG=/ruta/al/ffmpeg pnpm tauri dev
```

Para sustituir FFprobe, usa `MUKURE_FFPROBE=/ruta/al/ffprobe`.

**Distribución:** `librqbit 9.0.1` declara [Apache-2.0 en su manifiesto oficial](https://github.com/ikatson/rqbit/blob/v9.0.1/crates/librqbit/Cargo.toml), con su [licencia correspondiente](https://github.com/ikatson/rqbit/blob/v9.0.1/LICENSE). La licencia de FFmpeg depende de cómo se compiló. El binario npm actual informa `--enable-gpl --enable-version3 --enable-nonfree` y su propia salida `ffmpeg -L` indica que no es redistribuible. **Antes de distribuir un `.app`, reemplaza o excluye ese binario** y cumple los avisos y obligaciones del build elegido; la [licencia oficial de FFmpeg](https://github.com/FFmpeg/FFmpeg/blob/master/LICENSE.md) explica GPL y `nonfree`.

## Alcance del prototipo

La reproducción torrent necesita peers con las piezas solicitadas; un enlace sin fuentes puede quedar esperando o fallar al obtener metadatos. No hay reanudación entre ejecuciones ni biblioteca persistente. Aún no se implementaron selección de pistas de audio, subtítulos de imagen ni seeking arbitrario de la conversión FFmpeg. Windows, Linux y macOS Intel requieren adaptar el empaquetado y validar su WebView y codecs.

Tauri usa el [WebView del sistema](https://v2.tauri.app/concept/architecture/), mientras que [Electron integra Chromium y Node.js](https://www.electronjs.org/docs/latest/tutorial/process-model). Eso cambia tamaño, dependencias y compatibilidad; no garantiza que un reproductor Tauri consuma menos recursos o reproduzca mejor. Para este caso conviene medir memoria, CPU, inicio y reproducción con los mismos videos, especialmente cuando FFmpeg está convirtiendo.

Más información sobre el [streaming de rqbit y su prioridad de piezas](https://github.com/ikatson/rqbit#streaming-support).

## Prueba reproducible con un peer local

El ejemplo de desarrollo genera un `.torrent` de un video propio y mantiene un seeder y un tracker accesibles sólo en loopback:

```sh
sh scripts/with-rust.sh cargo run --manifest-path src-tauri/Cargo.toml \
  --example local_fixture -- /ruta/video.mp4 /ruta/prueba.torrent
```

Deja esa terminal abierta y usa el `.torrent` o el magnet que imprime en Mukure. Ctrl+C detiene el seeder sin borrar el video original. El motor de la app sigue configurado para DHT y trackers normales; el contenido de este ejemplo procede del peer local.
