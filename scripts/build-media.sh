#!/bin/sh
# Build standalone FFmpeg/FFprobe with static x264 and no nonfree components.
set -eu
ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
. "$ROOT/scripts/media-versions.sh"
if [ "$(uname -s)" != Darwin ] || [ "$(uname -m)" != arm64 ]; then
  echo 'Media builds currently require macOS on Apple Silicon.' >&2
  exit 1
fi
for tool in clang make pkg-config curl tar shasum; do
  command -v "$tool" >/dev/null || { echo "Missing build tool: $tool" >&2; exit 1; }
done
BUILD_ID=$(cat "$ROOT/scripts/media-versions.sh" "$ROOT/scripts/build-media.sh" | shasum -a 256 | cut -d ' ' -f 1)
OUTPUT="$ROOT/src-tauri/resources/media"
if [ -x "$OUTPUT/bin/ffmpeg" ] && [ -x "$OUTPUT/bin/ffprobe" ] && [ "$(cat "$OUTPUT/.build-id" 2>/dev/null || true)" = "$BUILD_ID" ]; then
  echo 'FFmpeg and FFprobe are already built.'
  exit 0
fi
CACHE="$ROOT/.tools/media/downloads"
BUILD="$ROOT/.tools/media/build-$BUILD_ID"
STAGE="$BUILD/stage"
JOBS=${MUKURE_BUILD_JOBS:-8}
case "$JOBS" in ''|0|*[!0-9]*) echo 'MUKURE_BUILD_JOBS must be a positive integer.' >&2; exit 1;; esac
mkdir -p "$CACHE" "$BUILD" "$STAGE" "$OUTPUT/bin" "$OUTPUT/licenses" "$OUTPUT/sources"
fetch() {
  file=$1 url=$2 sha=$3
  if [ ! -f "$CACHE/$file" ]; then
    curl --fail --location --retry 3 --connect-timeout 20 --max-time 300 "$url" -o "$CACHE/$file.part"
    mv "$CACHE/$file.part" "$CACHE/$file"
  fi
  printf '%s  %s\n' "$sha" "$CACHE/$file" | shasum -a 256 -c -
}
fetch "ffmpeg-$FFMPEG_VERSION.tar.xz" "https://ffmpeg.org/releases/ffmpeg-$FFMPEG_VERSION.tar.xz" "$FFMPEG_SHA256"
fetch "x264-$X264_REVISION.tar.gz" "https://code.videolan.org/videolan/x264/-/archive/$X264_REVISION/x264-$X264_REVISION.tar.gz" "$X264_SHA256"
[ -d "$BUILD/ffmpeg-$FFMPEG_VERSION" ] || tar -xf "$CACHE/ffmpeg-$FFMPEG_VERSION.tar.xz" -C "$BUILD"
[ -d "$BUILD/x264-$X264_REVISION" ] || tar -xf "$CACHE/x264-$X264_REVISION.tar.gz" -C "$BUILD"
export MACOSX_DEPLOYMENT_TARGET=11.0
export CFLAGS='-mmacosx-version-min=11.0'
export LDFLAGS='-mmacosx-version-min=11.0'
(
  cd "$BUILD/x264-$X264_REVISION"
  ./configure --prefix=/usr/local --enable-static --enable-pic --disable-cli --disable-opencl --disable-avs --disable-swscale --disable-lavf --disable-ffms --disable-gpac --disable-lsmash
  make -j "$JOBS"
  make DESTDIR="$STAGE" install-lib-static
) > "$BUILD/x264.log" 2>&1 || { tail -60 "$BUILD/x264.log" >&2; exit 1; }
echo 'x264 built; building FFmpeg and FFprobe…'
(
  cd "$BUILD/ffmpeg-$FFMPEG_VERSION"
  export PKG_CONFIG_LIBDIR="$STAGE/usr/local/lib/pkgconfig"
  export PKG_CONFIG_SYSROOT_DIR="$STAGE"
  ./configure --prefix=/usr/local --arch=aarch64 --target-os=darwin \
    --enable-gpl --enable-libx264 --enable-static --disable-shared \
    --disable-nonfree --disable-version3 --disable-autodetect \
    --enable-zlib --enable-videotoolbox --enable-audiotoolbox \
    --disable-doc --disable-debug --disable-ffplay --disable-indevs --enable-indev=lavfi --disable-outdevs \
    --pkg-config-flags=--static
  make -j "$JOBS"
) > "$BUILD/ffmpeg.log" 2>&1 || { tail -60 "$BUILD/ffmpeg.log" >&2; exit 1; }
for tool in ffmpeg ffprobe; do
  install -m 755 "$BUILD/ffmpeg-$FFMPEG_VERSION/$tool" "$OUTPUT/bin/$tool"
  # Only Apple system libraries may remain dynamically linked.
  otool -L "$OUTPUT/bin/$tool" | tail -n +2 | while IFS= read -r library; do
    case "$library" in *'/usr/lib/'*|*'/System/Library/'*) ;; *) echo "Unexpected dependency: $library" >&2; exit 1;; esac
  done
  "$OUTPUT/bin/$tool" -L > "$OUTPUT/licenses/$tool-license.txt" 2>&1
  if ! grep -q 'GNU General Public License' "$OUTPUT/licenses/$tool-license.txt"; then
    echo "Unexpected license in $tool" >&2; exit 1
  fi
done
cp "$BUILD/ffmpeg-$FFMPEG_VERSION/COPYING.GPLv2" "$OUTPUT/licenses/FFmpeg-GPL-2.0.txt"
cp "$BUILD/ffmpeg-$FFMPEG_VERSION/LICENSE.md" "$OUTPUT/licenses/FFmpeg-LICENSE.md"
cp "$BUILD/x264-$X264_REVISION/COPYING" "$OUTPUT/licenses/x264-GPL-2.0.txt"
cp "$CACHE/ffmpeg-$FFMPEG_VERSION.tar.xz" "$CACHE/x264-$X264_REVISION.tar.gz" "$OUTPUT/sources/"
cp "$ROOT/scripts/build-media.sh" "$ROOT/scripts/media-versions.sh" "$OUTPUT/sources/"
printf '%s\n' "$BUILD_ID" > "$OUTPUT/.build-id"
echo "Media tools, licenses and corresponding sources: $OUTPUT"
