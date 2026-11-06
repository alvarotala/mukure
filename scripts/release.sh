#!/bin/sh
set -eu
ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$ROOT"
VERSION=$(node -p "JSON.parse(require('fs').readFileSync('package.json', 'utf8')).version")
node --input-type=module -e '
import fs from "node:fs";
const version = JSON.parse(fs.readFileSync("package.json")).version;
const tauri = JSON.parse(fs.readFileSync("src-tauri/tauri.conf.json")).version;
const rust = fs.readFileSync("src-tauri/Cargo.toml", "utf8").match(/^version = "([^"]+)"/m)?.[1];
if (version !== tauri || version !== rust) throw new Error("Release versions do not match");
'
pnpm tauri build --bundles app
APP="$ROOT/src-tauri/target/release/bundle/macos/Mukure.app"
# A local build has no Developer ID. Seal the complete bundle ad hoc,
# including the standalone media executables, before archiving it.
if [ -z "${APPLE_SIGNING_IDENTITY:-}" ]; then
  for tool in ffmpeg ffprobe; do
    codesign --force --sign - "$APP/Contents/Resources/bin/$tool"
  done
  codesign --force --sign - "$APP"
fi
codesign --verify --deep --strict "$APP"
mkdir -p "$ROOT/release"
ARCHIVE="Mukure-$VERSION-macos-arm64.zip"
ditto -c -k --sequesterRsrc --keepParent "$APP" "$ROOT/release/$ARCHIVE"
cp LICENSE THIRD_PARTY.md CHANGELOG.md "$ROOT/release/"
(cd "$ROOT/release" && shasum -a 256 "$ARCHIVE" > SHA256SUMS)
printf 'Release ready: %s/release/%s\n' "$ROOT" "$ARCHIVE"
