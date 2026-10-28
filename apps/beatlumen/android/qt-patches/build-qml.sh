#!/usr/bin/env bash
# Builds Qt's QML library for Android with the patches in this folder
# (qml-no-exec-probe.patch: no probe for executable memory when the QML
# interpreter is forced, which GrapheneOS reports as dynamic code loading).
#
# The library is binary compatible with the official Qt build of the same
# version (only a function body changes) and replaces it in the APK; see
# build-android.sh (BL_QT_PATCHED_QML).
#
#   apps/beatlumen/android/qt-patches/build-qml.sh [ABI]
#
# Environment: QT_ROOT (default /opt/Qt/6.11.3), ANDROID_SDK_ROOT,
# ANDROID_NDK_ROOT, WORK_DIR (default build/qt-patched).
# Output: $WORK_DIR/<abi>/libQt6Qml_<abi>.so

set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$HERE/../../../.." && pwd)"
ABI="${1:-arm64-v8a}"
QT_VERSION=6.11.3
QT_ROOT="${QT_ROOT:-/opt/Qt/$QT_VERSION}"
WORK_DIR="${WORK_DIR:-$ROOT/build/qt-patched}"
export ANDROID_SDK_ROOT="${ANDROID_SDK_ROOT:-/opt/android-sdk}"
export ANDROID_NDK_ROOT="${ANDROID_NDK_ROOT:-/opt/android/android-ndk-r27d}"

SRC_NAME="qtdeclarative-everywhere-src-$QT_VERSION"
SRC_URL="https://download.qt.io/official_releases/qt/${QT_VERSION%.*}/$QT_VERSION/submodules/$SRC_NAME.tar.xz"
# Pinned checksum of the official source archive.
SRC_SHA256=6d6db328a02ae3bb0b1e6944aeef8dfe44cd6b55ab59b1708bbf0400f5759cb5

case "$ABI" in
    arm64-v8a) KIT="$QT_ROOT/android_arm64_v8a" ;;
    armeabi-v7a) KIT="$QT_ROOT/android_armv7" ;;
    x86_64) KIT="$QT_ROOT/android_x86_64" ;;
    x86) KIT="$QT_ROOT/android_x86" ;;
    *) echo "unknown ABI $ABI" >&2; exit 1 ;;
esac
[ -x "$KIT/bin/qt-cmake" ] || { echo "Qt for Android ($ABI) not found: $KIT" >&2; exit 1; }

OUT="$WORK_DIR/$ABI/libQt6Qml_$ABI.so"
STAMP="$(cat "$HERE"/*.patch | sha256sum | cut -c1-16)"
if [ -f "$OUT" ] && [ "$(cat "$OUT.stamp" 2>/dev/null)" = "$STAMP" ]; then
    echo "patched QML library up to date: $OUT"
    exit 0
fi

mkdir -p "$WORK_DIR"
cd "$WORK_DIR"
if [ ! -f "$SRC_NAME.tar.xz" ]; then
    echo "==> download $SRC_URL"
    curl -fL --retry 3 -o "$SRC_NAME.tar.xz.part" "$SRC_URL"
    mv "$SRC_NAME.tar.xz.part" "$SRC_NAME.tar.xz"
fi
echo "$SRC_SHA256  $SRC_NAME.tar.xz" | sha256sum -c -

rm -rf "$SRC_NAME"
tar xf "$SRC_NAME.tar.xz"
for p in "$HERE"/*.patch; do
    echo "==> apply $(basename "$p")"
    patch -d "$SRC_NAME" -p1 --forward < "$p"
done

echo "==> configure ($ABI)"
"$KIT/bin/qt-cmake" -Wno-dev -S "$SRC_NAME" -B "build-$ABI" -G Ninja \
    -DCMAKE_BUILD_TYPE=Release \
    -DQT_HOST_PATH="$QT_ROOT/gcc_64" \
    -DANDROID_SDK_ROOT="$ANDROID_SDK_ROOT" \
    -DANDROID_NDK_ROOT="$ANDROID_NDK_ROOT" \
    -DQT_BUILD_EXAMPLES=OFF -DQT_BUILD_TESTS=OFF >/dev/null

echo "==> build the QML library"
ninja -C "build-$ABI" Qml

# Must be a drop-in replacement: same exported symbols as the official one.
NM="$(ls "$ANDROID_NDK_ROOT"/toolchains/llvm/prebuilt/*/bin/llvm-nm | head -n 1)"
LIB="build-$ABI/lib/libQt6Qml_$ABI.so"
if ! diff <("$NM" -D --defined-only "$KIT/lib/libQt6Qml_$ABI.so" | awk '{print $2, $3}' | sort) \
          <("$NM" -D --defined-only "$LIB" | awk '{print $2, $3}' | sort) >/dev/null; then
    echo "patched library exports differ from the official Qt build" >&2
    exit 1
fi
mkdir -p "$(dirname "$OUT")"
cp "$LIB" "$OUT"
echo "$STAMP" > "$OUT.stamp"
echo "patched QML library: $OUT"
