<div align="center">
  <img src="src-tauri/app-icon.svg" width="88" alt="Mukure" />
  <h1>Mukure</h1>
  <p>Tu video, tus subtítulos, tu pantalla.</p>
  <p><strong>v0.1.1 · macOS Apple Silicon · MIT</strong></p>
</div>

Mukure es un reproductor de escritorio para videos locales, archivos `.torrent` y enlaces magnet. Reproduce mientras descarga, busca subtítulos automáticamente y permite enviar el video a un Chromecast. Su nombre viene de la zarigüeya paraguaya en guaraní.

## Funciones

- Videos locales y streaming BitTorrent, con selección de archivos dentro del torrent.
- Reproducción directa o conversión con FFmpeg y FFprobe incluidos.
- Subtítulos externos SRT/WebVTT, pistas de texto integradas y búsqueda online en múltiples fuentes.
- Ventana redimensionable, controles de reproducción y pantalla completa con ocultamiento automático de barras y cursor.
- Envío a Chromecast, con búsqueda de dispositivos y conexión por IP local.

## Instalación

El paquete `Mukure-0.1.1-macos-arm64.zip` contiene `Mukure.app`. Descomprímelo y mueve la aplicación a **Aplicaciones**. FFmpeg y FFprobe vienen incluidos. Para crear el paquete desde el código fuente, sigue los pasos de desarrollo de abajo.

## Uso

Abre un video desde **Abrir**, selecciona un `.torrent` o pega un magnet. El modo **Directo** usa los formatos compatibles con macOS; **FFmpeg** convierte los demás formatos durante la reproducción.

En **CC** puedes elegir una pista, cambiar el idioma, activar o desactivar la búsqueda online y ajustar la sincronización. Los subtítulos se buscan en OpenSubtitles —mediante el addon público de Stremio— y SubtitleDB sin cuenta. En **Fuentes de subtítulos** puedes conectar SubDL y Wyzie; la app incluye enlaces para obtener sus claves de API. La disponibilidad y los cupos dependen de cada proveedor.

Para usar Chromecast, conecta ambos equipos a la misma red y pulsa **Enviar a Chromecast**. Selecciona los subtítulos externos antes de transmitir. Si el receptor no admite el formato original, activa **Convertir para TV**. Mantén Mukure abierto durante la transmisión.

| Atajo | Acción |
| --- | --- |
| Espacio | Reproducir o pausar |
| ← / → | Retroceder o adelantar 10 segundos |
| M | Silenciar o activar sonido |
| F | Entrar o salir de pantalla completa |

En pantalla completa, los controles se ocultan tras 3 segundos de inactividad. Mueve el mouse o usa el teclado para mostrarlos.

## Compatibilidad

La versión 0.1 está dirigida a **macOS 11 o posterior en Apple Silicon**.

- La conversión FFmpeg comienza desde el inicio y no permite saltos de posición.
- Chromecast está en fase experimental; la compatibilidad de formatos depende del receptor. La conversión para TV usa H.264/AAC hasta 1080p.
- Chromecast recibe una copia de los subtítulos externos seleccionados. Las pistas integradas se muestran en la Mac.
- Se admiten subtítulos de texto; no se admiten PGS/DVD/DVB ni estilos avanzados de ASS.
- Se mantiene un torrent activo por vez. No hay biblioteca persistente ni reanudación entre ejecuciones.

## Privacidad y datos

Mukure no requiere una cuenta propia ni incluye telemetría. Las claves de proveedores se guardan en el Llavero de macOS.

La búsqueda online envía el título o nombre del video y el idioma a servicios de subtítulos. Puedes desactivarla en **CC**; los archivos de video no se suben a esos servicios. BitTorrent se conecta a peers, trackers y DHT, y también comparte piezas descargadas. Chromecast recibe el video por la red local.

Los datos del torrent se guardan en la caché de la app y se eliminan al cerrar la sesión o salir normalmente. Un cierre forzado puede dejar archivos de caché. Los videos locales originales no se modifican ni se borran.

## Desarrollo

Requisitos: macOS Apple Silicon, Xcode Command Line Tools, Node.js 22.12 o posterior, pnpm 11, Rust estable y `pkg-config`. Consulta los [requisitos de Tauri para macOS](https://v2.tauri.app/start/prerequisites/#macos).

```sh
git clone https://github.com/alvarotala/mukure.git
cd mukure
pnpm install --frozen-lockfile
pnpm prepare:app
pnpm tauri dev
```

`media:build` compila FFmpeg y x264 desde fuentes fijadas y verificadas. La primera ejecución puede tardar varios minutos. `pnpm dev` abre sólo la interfaz web; la reproducción requiere la aplicación Tauri.

Para generar la aplicación:

```sh
pnpm release
```

La compilación produce `src-tauri/target/release/bundle/macos/Mukure.app`. Los paquetes de distribución se guardan en `release/`. La compilación local usa firma ad hoc; la firma Developer ID y la notarización requieren credenciales de Apple.

Para ejecutar las comprobaciones:

```sh
pnpm build
pnpm test
pnpm check:rust
pnpm test:rust
```

Consulta [CONTRIBUTING.md](CONTRIBUTING.md) para la estructura del proyecto, pruebas y compilación de dependencias, y [CHANGELOG.md](CHANGELOG.md) para las novedades de cada versión.

## Licencias

El código de Mukure está disponible bajo la [licencia MIT](LICENSE). FFmpeg, x264 y las demás dependencias conservan sus propias licencias; consulta [THIRD_PARTY.md](THIRD_PARTY.md).
