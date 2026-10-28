#!/usr/bin/env bash
# Builds the Beatlumen desktop app for Linux as an AppImage (one file that
# runs on most distributions with glibc at least as new as the build
# system's): dist/linux/Beatlumen-x86_64.AppImage
#
# Uses linuxdeploy and its Qt plugin (pinned releases, checked by SHA-256)
# to collect Qt's libraries, plugins and QML modules.
#
# Environment overrides:
#   QT_ROOT=/opt/Qt/6.11.3/gcc_64     desktop Qt with Qt Multimedia
#   BUILD_DIR=build/appimage          CMake build directory
#   TOOLS_DIR=build/tools             downloaded tools (cached)
#
# SPDX-License-Identifier: AGPL-3.0-or-later

set -euo pipefail

cd "$(dirname "$0")"
ROOT="$PWD"
QT_ROOT="${QT_ROOT:-/opt/Qt/6.11.3/gcc_64}"
BUILD_DIR="${BUILD_DIR:-$ROOT/build/appimage}"
TOOLS_DIR="${TOOLS_DIR:-$ROOT/build/tools}"
OUT_DIR="$ROOT/dist/linux"
APPDIR="$BUILD_DIR/AppDir"
APP_ID=io.github.randomnam3.beatlumen

LINUXDEPLOY_URL=https://github.com/linuxdeploy/linuxdeploy/releases/download/1-alpha-20250213-2/linuxdeploy-x86_64.AppImage
LINUXDEPLOY_SHA256=4648f278ab3ef31f819e67c30d50f462640e5365a77637d7e6f2ad9fd0b4522a
PLUGIN_QT_URL=https://github.com/linuxdeploy/linuxdeploy-plugin-qt/releases/download/1-alpha-20250213-1/linuxdeploy-plugin-qt-x86_64.AppImage
PLUGIN_QT_SHA256=15106be885c1c48a021198e7e1e9a48ce9d02a86dd0a1848f00bdbf3c1c92724

die() { echo "error: $*" >&2; exit 1; }
step() { printf '\n==> %s\n' "$*"; }

fetch() { # url sha256 file
    if [ ! -f "$3" ] || ! echo "$2  $3" | sha256sum -c - >/dev/null 2>&1; then
        curl -fL --retry 3 -o "$3.part" "$1"
        mv "$3.part" "$3"
    fi
    echo "$2  $3" | sha256sum -c - >/dev/null || die "checksum mismatch: $3"
    chmod +x "$3"
}

[ -x "$QT_ROOT/bin/qmake" ] || die "desktop Qt not found: $QT_ROOT"
for tool in cmake ninja cargo curl; do
    command -v "$tool" >/dev/null || die "$tool not found on PATH"
done

step "tools"
mkdir -p "$TOOLS_DIR"
fetch "$LINUXDEPLOY_URL" "$LINUXDEPLOY_SHA256" "$TOOLS_DIR/linuxdeploy-x86_64.AppImage"
fetch "$PLUGIN_QT_URL" "$PLUGIN_QT_SHA256" "$TOOLS_DIR/linuxdeploy-plugin-qt-x86_64.AppImage"

step "build"
cmake -S apps/beatlumen -B "$BUILD_DIR" -G Ninja -DCMAKE_BUILD_TYPE=Release \
    -DCMAKE_PREFIX_PATH="$QT_ROOT" -DCMAKE_INSTALL_PREFIX=/usr
cmake --build "$BUILD_DIR"

step "AppDir"
rm -rf "$APPDIR"
install -Dm755 "$BUILD_DIR/beatlumen" "$APPDIR/usr/bin/beatlumen"
install -Dm644 apps/beatlumen/assets/icon.png "$APPDIR/usr/share/icons/hicolor/512x512/apps/$APP_ID.png"
install -d "$APPDIR/usr/share/applications"
cat > "$APPDIR/usr/share/applications/$APP_ID.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=Beatlumen
GenericName=Music player
Comment=Music player with beat-synchronized visuals
Exec=beatlumen %F
Icon=$APP_ID
Categories=AudioVideo;Audio;Player;
MimeType=audio/mpeg;audio/flac;audio/ogg;audio/x-vorbis+ogg;audio/opus;audio/mp4;audio/aac;audio/x-wav;audio/x-matroska;
Terminal=false
EOF
install -Dm644 LICENSE "$APPDIR/usr/share/doc/beatlumen/LICENSE"
install -Dm644 THIRD_PARTY_NOTICES.md "$APPDIR/usr/share/doc/beatlumen/THIRD_PARTY_NOTICES.md"

step "deploy Qt and package"
mkdir -p "$OUT_DIR"
rm -f "$OUT_DIR"/Beatlumen-*.AppImage
cd "$BUILD_DIR"
# The tools are AppImages themselves; extract-and-run avoids needing FUSE.
export APPIMAGE_EXTRACT_AND_RUN=1
export QMAKE="$QT_ROOT/bin/qmake"
export LD_LIBRARY_PATH="$QT_ROOT/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
export QML_SOURCES_PATHS="$ROOT/apps/beatlumen/qml:$BUILD_DIR/qml"
# X11 and Wayland sessions.
export EXTRA_PLATFORM_PLUGINS="libqwayland.so"
export EXTRA_QT_MODULES="waylandcompositor"
export LDAI_OUTPUT="$OUT_DIR/Beatlumen-x86_64.AppImage"
export LINUXDEPLOY_OUTPUT_VERSION="$(sed -n 's/^project(.*VERSION \([0-9.]*\).*/\1/p' "$ROOT/apps/beatlumen/CMakeLists.txt")"
"$TOOLS_DIR/linuxdeploy-x86_64.AppImage" --appdir "$APPDIR" \
    --desktop-file "$APPDIR/usr/share/applications/$APP_ID.desktop" \
    --icon-file "$APPDIR/usr/share/icons/hicolor/512x512/apps/$APP_ID.png" \
    --plugin qt --output appimage

echo "==> $LDAI_OUTPUT ($(du -h "$LDAI_OUTPUT" | cut -f1))"
