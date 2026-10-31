#!/usr/bin/env bash
# Builds the Beatlumen Android APK with Qt for Android:
#   qt-cmake (multi-ABI)       -> native libraries, Rust core via cargo
#   apk target (androiddeployqt + Gradle) -> unsigned release APK
#   zipalign + apksigner       -> dist/android/beatlumen.apk
#
# Environment overrides:
#   QT_ROOT=/opt/Qt/6.11.3                 Qt (android_* kits and the gcc_64 host Qt)
#   ANDROID_SDK_ROOT=/opt/android-sdk      SDK: platforms/android-36, build-tools/36.x,
#                                          licenses/ (Qt 6.11 builds Java against API 36)
#   ANDROID_NDK_ROOT=/opt/android/android-ndk-r27d
#   ABIS="arm64-v8a x86_64"                ABIs in the APK; the first one is the primary
#   BUILD_TYPE=Release                     CMake build type (Release/RelWithDebInfo/Debug)
#   BUILD_DIR=build/android                CMake build directory
#   VERSION_CODE=111                       android:versionCode
#   KEYSTORE, KEY_ALIAS                    release key (PKCS12/JKS keystore and alias)
#   KEYSTORE_PASS, KEY_PASS                its passwords (KEY_PASS defaults to KEYSTORE_PASS)
#                                          Without KEYSTORE, build/android-debug.keystore is
#                                          used and generated when missing.
#   MAVEN_CENTRAL_MIRROR                   Maven Central mirror for Gradle, e.g.
#                                          https://maven-central.storage-download.googleapis.com/maven2/
#   PATCH_QT_QML=1                         Ship Qt's QML library built with
#                                          apps/beatlumen/android/qt-patches (no executable
#                                          memory probe, see SECURITY.md); built once and
#                                          cached in build/qt-patched. 0 = official library.
#
# SPDX-License-Identifier: AGPL-3.0-or-later

set -euo pipefail

cd "$(dirname "$0")"
ROOT="$PWD"

QT_ROOT="${QT_ROOT:-/opt/Qt/6.11.3}"
export ANDROID_SDK_ROOT="${ANDROID_SDK_ROOT:-/opt/android-sdk}"
export ANDROID_NDK_ROOT="${ANDROID_NDK_ROOT:-/opt/android/android-ndk-r27d}"
ABIS="${ABIS:-arm64-v8a}"
BUILD_TYPE="${BUILD_TYPE:-Release}"
BUILD_DIR="${BUILD_DIR:-$ROOT/build/android}"
VERSION_CODE="${VERSION_CODE:-111}"
OUT_DIR="$ROOT/dist/android"
OUT_APK="$OUT_DIR/beatlumen.apk"

die() { echo "error: $*" >&2; exit 1; }
step() { printf '\n==> %s\n' "$*"; }

qt_kit() {
    case "$1" in
        arm64-v8a) echo "$QT_ROOT/android_arm64_v8a" ;;
        armeabi-v7a) echo "$QT_ROOT/android_armv7" ;;
        x86_64) echo "$QT_ROOT/android_x86_64" ;;
        x86) echo "$QT_ROOT/android_x86" ;;
        *) die "unknown ABI $1" ;;
    esac
}
rust_target() {
    case "$1" in
        arm64-v8a) echo aarch64-linux-android ;;
        armeabi-v7a) echo armv7-linux-androideabi ;;
        x86_64) echo x86_64-linux-android ;;
        x86) echo i686-linux-android ;;
    esac
}

# ---- checks -------------------------------------------------------------------------
for tool in cmake ninja cargo java keytool; do
    command -v "$tool" >/dev/null || die "$tool not found on PATH"
done
java_major="$(java -version 2>&1 | sed -n 's/.*version "\([0-9]*\).*/\1/p' | head -n 1)"
[ "${java_major:-0}" -ge 17 ] || die "the Android Gradle plugin needs JDK 17 or newer (found ${java_major:-none})"
[ -d "$ANDROID_NDK_ROOT" ] || die "NDK not found: $ANDROID_NDK_ROOT"
ls "$ANDROID_SDK_ROOT"/platforms/android-*/android.jar >/dev/null 2>&1 || die "no SDK platform in $ANDROID_SDK_ROOT/platforms"
BUILD_TOOLS="$(ls -d "$ANDROID_SDK_ROOT"/build-tools/* 2>/dev/null | sort -V | tail -n 1)"
[ -x "$BUILD_TOOLS/apksigner" ] || die "no build-tools in $ANDROID_SDK_ROOT/build-tools"
installed_targets="$(rustup target list --installed 2>/dev/null || true)"
abi_list=""
extra_kits=()
primary=""
for abi in $ABIS; do
    kit="$(qt_kit "$abi")"
    [ -x "$kit/bin/qt-cmake" ] || die "Qt for Android ($abi) not found: $kit"
    if [ -n "$installed_targets" ] && ! grep -qx "$(rust_target "$abi")" <<<"$installed_targets"; then
        die "Rust target missing: rustup target add $(rust_target "$abi")"
    fi
    if [ -z "$primary" ]; then
        primary="$abi"
    else
        extra_kits+=("-DQT_PATH_ANDROID_ABI_$abi=$kit")
    fi
    abi_list="${abi_list:+$abi_list;}$abi"
done

# ---- 1. configure + native build (all ABIs) --------------------------------------
step "configure ($abi_list, $BUILD_TYPE)"
"$(qt_kit "$primary")/bin/qt-cmake" -Wno-dev -S "$ROOT/apps/beatlumen" -B "$BUILD_DIR" -G Ninja \
    -DCMAKE_BUILD_TYPE="$BUILD_TYPE" \
    -DANDROID_SDK_ROOT="$ANDROID_SDK_ROOT" \
    -DANDROID_NDK_ROOT="$ANDROID_NDK_ROOT" \
    -DQT_HOST_PATH="$QT_ROOT/gcc_64" \
    -DQT_ANDROID_ABIS="$abi_list" \
    "${extra_kits[@]}" \
    -DBL_ANDROID_VERSION_CODE="$VERSION_CODE"

# ---- 2. APK (androiddeployqt + Gradle) -------------------------------------------
# Gradle downloads its dependencies on the first run; retry on network errors
# (Maven Central answers 429 when rate limiting).
if [ "${PATCH_QT_QML:-1}" = 1 ]; then
    step "patched Qt QML library"
    for abi in $ABIS; do
        QT_ROOT="$QT_ROOT" "$ROOT/apps/beatlumen/android/qt-patches/build-qml.sh" "$abi"
    done
fi

step "build native libraries and package the APK"
# androiddeployqt never removes libraries from earlier builds (e.g. Qt
# plugins no longer imported) and Gradle packages everything it finds.
rm -rf "$BUILD_DIR/android-build/libs" "$BUILD_DIR/android-build/assets" "$BUILD_DIR/android-build/build" \
    "$BUILD_DIR/android-build/beatlumen.apk"
for attempt in 1 2 3; do
    if cmake --build "$BUILD_DIR" --target apk; then
        break
    fi
    [ "$attempt" -lt 3 ] || die "APK build failed"
    echo "APK build failed (attempt $attempt), retrying in 20 s..." >&2
    sleep 20
done

if [ "${PATCH_QT_QML:-1}" = 1 ]; then
    # Replace the official library deployed by androiddeployqt and package
    # again (Gradle only redoes the packaging steps).
    step "package with the patched Qt QML library"
    for abi in $ABIS; do
        cp "$ROOT/build/qt-patched/$abi/libQt6Qml_$abi.so" "$BUILD_DIR/android-build/libs/$abi/libQt6Qml_$abi.so"
    done
    gradle_task=assembleRelease
    [ "$BUILD_TYPE" = Debug ] && gradle_task=assembleDebug
    (cd "$BUILD_DIR/android-build" && ./gradlew --no-daemon -q "$gradle_task") || die "Gradle packaging failed"
fi

unsigned="$(ls -t "$BUILD_DIR"/android-build/build/outputs/apk/*/*.apk 2>/dev/null | head -n 1)"
[ -f "$unsigned" ] || die "no APK produced under $BUILD_DIR/android-build/build/outputs/apk"

# ---- 3. sign ---------------------------------------------------------------------------
if [ -n "${KEYSTORE:-}" ]; then
    [ -f "$KEYSTORE" ] || die "keystore not found: $KEYSTORE"
    [ -n "${KEY_ALIAS:-}" ] || die "KEY_ALIAS is required with KEYSTORE"
    [ -n "${KEYSTORE_PASS:-}" ] || die "KEYSTORE_PASS is required with KEYSTORE"
    export BL_KS_PASS="$KEYSTORE_PASS"
    export BL_KEY_PASS="${KEY_PASS:-$KEYSTORE_PASS}"
    step "sign with release key $KEY_ALIAS ($KEYSTORE)"
else
    KEYSTORE="$ROOT/build/android-debug.keystore"
    KEY_ALIAS="androiddebugkey"
    export BL_KS_PASS="android" BL_KEY_PASS="android"
    if [ ! -f "$KEYSTORE" ]; then
        step "generate debug keystore $KEYSTORE"
        mkdir -p "$(dirname "$KEYSTORE")"
        keytool -genkeypair -noprompt -keystore "$KEYSTORE" -storetype PKCS12 \
            -storepass android -keypass android -alias "$KEY_ALIAS" \
            -keyalg RSA -keysize 2048 -validity 10000 \
            -dname "CN=Android Debug,O=Android,C=US" 2>/dev/null
    fi
    step "sign with the debug key ($KEYSTORE)"
fi

mkdir -p "$OUT_DIR"
aligned="$BUILD_DIR/beatlumen-aligned.apk"
# 16 KiB page alignment for uncompressed native libraries.
"$BUILD_TOOLS/zipalign" -f -P 16 4 "$unsigned" "$aligned"
"$BUILD_TOOLS/apksigner" sign --ks "$KEYSTORE" --ks-key-alias "$KEY_ALIAS" \
    --ks-pass env:BL_KS_PASS --key-pass env:BL_KEY_PASS \
    --out "$OUT_APK" "$aligned"
rm -f "$aligned" "$OUT_APK.idsig"

# ---- 4. check ---------------------------------------------------------------------------
step "verify"
"$BUILD_TOOLS/apksigner" verify --print-certs "$OUT_APK" | grep -E "Signer #1 certificate (DN|SHA-256)" || true
"$BUILD_TOOLS/aapt2" dump badging "$OUT_APK" | grep -E "^(package|sdkVersion|targetSdkVersion|application-label|native-code):" || true
echo
echo "APK: $OUT_APK ($(du -h "$OUT_APK" | cut -f1))"
