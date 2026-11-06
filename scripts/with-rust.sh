#!/bin/sh
set -eu
PROJECT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
if [ -x "$PROJECT_DIR/.tools/cargo/bin/cargo" ]; then
  export CARGO_HOME="$PROJECT_DIR/.tools/cargo"
  export RUSTUP_HOME="$PROJECT_DIR/.tools/rustup"
  export PATH="$CARGO_HOME/bin:$PATH"
fi
if [ -z "${MUKURE_FFMPEG:-}" ] && [ -x "$PROJECT_DIR/src-tauri/resources/media/bin/ffmpeg" ]; then
  export MUKURE_FFMPEG="$PROJECT_DIR/src-tauri/resources/media/bin/ffmpeg"
fi
if [ -z "${MUKURE_FFPROBE:-}" ] && [ -x "$PROJECT_DIR/src-tauri/resources/media/bin/ffprobe" ]; then
  export MUKURE_FFPROBE="$PROJECT_DIR/src-tauri/resources/media/bin/ffprobe"
fi
exec "$@"
