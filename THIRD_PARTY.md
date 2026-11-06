# Componentes de terceros

La licencia MIT de Mukure cubre su código propio. Las dependencias conservan sus respectivos avisos de copyright y licencias.

## Herramientas multimedia

Mukure ejecuta FFmpeg y FFprobe como procesos separados. La compilación incluida usa FFmpeg con x264 y está disponible bajo **GPL-2.0-or-later**. No se habilitan componentes `nonfree`.

| Componente | Fuente | Licencia |
| --- | --- | --- |
| FFmpeg y FFprobe | [FFmpeg](https://ffmpeg.org/) | GPL-2.0-or-later en esta compilación |
| x264 | [VideoLAN](https://code.videolan.org/videolan/x264) | GPL-2.0-or-later |

Las revisiones y checksums están fijados en `scripts/media-versions.sh`. `scripts/build-media.sh` descarga los archivos oficiales, verifica sus SHA-256 y compila los ejecutables. Las fuentes de FFmpeg y x264 se usan sin modificaciones.

La aplicación distribuye el código fuente correspondiente completo, los scripts de compilación y las licencias en:

- `Mukure.app/Contents/Resources/third-party/media/sources/`
- `Mukure.app/Contents/Resources/third-party/media/licenses/`

FFmpeg incluye código basado en el trabajo del Independent JPEG Group. Sus archivos derivados de libjpeg no se modifican en esta compilación. Consulta también la [información de licencias de FFmpeg](https://ffmpeg.org/legal.html).

## Dependencias de la aplicación

Mukure usa Tauri, React, Lucide, librqbit y otras bibliotecas Rust y JavaScript. Los lockfiles registran sus versiones exactas. `pnpm licenses:generate` recopila las licencias y avisos incluidos en los paquetes y genera un inventario con sus repositorios y licencias declaradas.

Los avisos que no vienen en los paquetes se conservan en `third-party/licenses/`, con su procedencia y SHA-256 en `third-party/license-overrides.json`. También se incluyen las fuentes originales de las dependencias MPL.

Los avisos se incluyen en `Mukure.app/Contents/Resources/third-party/dependencies/`. Las herramientas multimedia se distribuyen con sus fuentes y licencias por separado, en la ruta indicada arriba.
