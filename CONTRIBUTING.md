# Contribuir a Mukure

Usa una Mac con Apple Silicon y los requisitos de desarrollo del [README](README.md). Para informar un problema, incluye la versión de macOS, la versión de Mukure, el modo de reproducción y pasos reproducibles. Usa un video propio o una muestra pública; omite claves, rutas personales y datos privados.

## Estructura

- `src/`: interfaz React, controles y subtítulos.
- `src-tauri/src/`: motor Rust, BitTorrent, transporte de video, proveedores y Chromecast.
- `src-tauri/examples/`: herramientas para pruebas locales.
- `scripts/`: preparación de dependencias y paquetes de distribución.

## Preparar el entorno

```sh
pnpm install --frozen-lockfile
pnpm prepare:app
pnpm tauri dev
```

`pnpm prepare:app` compila las herramientas multimedia y reúne los avisos de las dependencias. Requiere acceso a Internet la primera vez y `pkg-config` disponible en `PATH` (`brew install pkg-config`). Las siguientes ejecuciones reutilizan la compilación.

`with-rust.sh` usa Rust de `PATH`, o el toolchain de `.tools/` si existe. Para reemplazar los ejecutables multimedia durante el desarrollo, configura `MUKURE_FFMPEG` y `MUKURE_FFPROBE` antes de iniciar la app.

## Comprobaciones

```sh
pnpm build
pnpm test
pnpm check:rust
pnpm test:rust
sh scripts/with-rust.sh cargo fmt --manifest-path src-tauri/Cargo.toml -- --check
```

Las pruebas Rust usan puertos de loopback, archivos temporales y FFmpeg. La prueba de servicios de subtítulos externos está excluida de la ejecución normal; para ejecutarla explícitamente:

```sh
sh scripts/with-rust.sh cargo test --locked --manifest-path src-tauri/Cargo.toml live_public_providers -- --ignored
```

Para probar BitTorrent con un video propio, inicia un peer y tracker locales:

```sh
sh scripts/with-rust.sh cargo run --locked --manifest-path src-tauri/Cargo.toml \
  --example local_fixture -- /ruta/video.mp4 /ruta/prueba.torrent
```

Mantén el proceso abierto y carga el `.torrent` o magnet que imprime. Ctrl+C cierra el peer sin borrar el video original.

## Compilar una versión

`pnpm release` construye la app y genera un ZIP con su checksum en `release/`. La app incluye FFmpeg, FFprobe, las fuentes exactas de sus componentes, los scripts de compilación y los avisos de dependencias. Los archivos generados no se versionan.

Las versiones deben coincidir en `package.json`, `src-tauri/Cargo.toml` y `src-tauri/tauri.conf.json`. Registra los cambios de usuario en `CHANGELOG.md`. Para modificar las dependencias multimedia, actualiza `scripts/media-versions.sh` con las revisiones oficiales y sus SHA-256, y ejecuta las pruebas con los binarios nuevos.

La compilación local es ad hoc. Para una distribución con Developer ID y notarización, configura las credenciales de firma mediante el [flujo de Tauri para macOS](https://v2.tauri.app/distribute/sign/macos/), sin incluir certificados ni contraseñas en el repositorio.
