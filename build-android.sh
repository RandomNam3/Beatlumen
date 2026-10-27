#!/usr/bin/env bash
# Builds the Beatlumen Android APK without Gradle:
#   cargo (NDK clang as linker) -> libbeatlumen.so per ABI
#   gen_icons example           -> launcher icon resources
#   javac -> d8                 -> classes.dex
#   aapt2 compile/link          -> base APK (manifest + resources)
#   zip (dex + libs) -> zipalign -> apksigner (debug keystore)
# Output: dist/android/beatlumen.apk
#
# Environment overrides:
#   ANDROID_NDK, ANDROID_BUILD_TOOLS, ANDROID_PLATFORM_JAR  tool locations
#   ABIS="arm64-v8a x86_64 armeabi-v7a"                     ABIs to build
#   PROFILE=release                                          cargo profile
#   VERSION_CODE=1                                           android:versionCode
#   KEYSTORE, KEYSTORE_PASS, KEY_ALIAS                       signing key
#   CARGO_TARGET_DIR, CARGO_BUILD_JOBS                       usual cargo vars
#
# SPDX-License-Identifier: GPL-3.0-or-later

set -euo pipefail

cd "$(dirname "$0")"
ROOT="$PWD"

# ---- project layout (adjust here when crates/folders are renamed) -------------
CRATE="openchroma-android"
CRATE_DIR="apps/openchroma-android"
LIB_NAME="beatlumen"
APK_NAME="beatlumen.apk"
MIN_SDK=26
TARGET_SDK=34

# ---- tools ---------------------------------------------------------------------------
ANDROID_NDK="${ANDROID_NDK:-/opt/android/android-ndk-r27d}"
ANDROID_BUILD_TOOLS="${ANDROID_BUILD_TOOLS:-/opt/android/android-14}"
ANDROID_PLATFORM_JAR="${ANDROID_PLATFORM_JAR:-/opt/android/android-34-ext12/android.jar}"
NDK_BIN="$ANDROID_NDK/toolchains/llvm/prebuilt/linux-x86_64/bin"
ABIS="${ABIS:-arm64-v8a x86_64 armeabi-v7a}"
PROFILE="${PROFILE:-release}"
VERSION_CODE="${VERSION_CODE:-1}"
VERSION_NAME="$(sed -n 's/^version *= *"\(.*\)"/\1/p' Cargo.toml | head -n 1)"
TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/target}"
WORK="$TARGET_DIR/android-apk"
OUT_DIR="$ROOT/dist/android"
KEYSTORE="${KEYSTORE:-$TARGET_DIR/android-debug.keystore}"
KEYSTORE_PASS="${KEYSTORE_PASS:-android}"
KEY_ALIAS="${KEY_ALIAS:-androiddebugkey}"

for tool in "$NDK_BIN/llvm-strip" "$ANDROID_BUILD_TOOLS/aapt2" "$ANDROID_BUILD_TOOLS/d8" \
    "$ANDROID_BUILD_TOOLS/zipalign" "$ANDROID_BUILD_TOOLS/apksigner" "$ANDROID_PLATFORM_JAR"; do
    [ -e "$tool" ] || { echo "error: missing $tool" >&2; exit 1; }
done
for tool in cargo javac keytool zip; do
    command -v "$tool" >/dev/null || { echo "error: $tool not found on PATH" >&2; exit 1; }
done

step() { printf '\n==> %s\n' "$*"; }

rm -rf "$WORK"
mkdir -p "$WORK/stage/lib" "$WORK/res" "$WORK/classes" "$WORK/dex" "$OUT_DIR"

# ---- 1. native library -------------------------------------------------------------
# abi -> rust target, clang prefix
abi_target() {
    case "$1" in
        arm64-v8a) echo "aarch64-linux-android aarch64-linux-android" ;;
        armeabi-v7a) echo "armv7-linux-androideabi armv7a-linux-androideabi" ;;
        x86_64) echo "x86_64-linux-android x86_64-linux-android" ;;
        x86) echo "i686-linux-android i686-linux-android" ;;
        *) echo "error: unknown ABI $1" >&2; exit 1 ;;
    esac
}

BUILT_ABIS=""
for abi in $ABIS; do
    read -r target clang_prefix <<<"$(abi_target "$abi")"
    step "cargo build ($abi, $target, profile $PROFILE)"
    env_target="$(echo "$target" | tr 'a-z-' 'A-Z_')"
    linker="$NDK_BIN/${clang_prefix}${MIN_SDK}-clang"
    # 16 KiB page alignment keeps the library loadable on 16K-page devices.
    flags="-C link-arg=-Wl,-z,max-page-size=16384 -C link-arg=-Wl,-soname,lib${LIB_NAME}.so"
    if ! env "CARGO_TARGET_${env_target}_LINKER=$linker" \
            "CARGO_TARGET_${env_target}_RUSTFLAGS=$flags" \
            "CC_${target//-/_}=$linker" \
            "AR_${target//-/_}=$NDK_BIN/llvm-ar" \
            cargo build -p "$CRATE" --lib --target "$target" --profile "$PROFILE"; then
        if [ "$abi" = "armeabi-v7a" ]; then
            echo "warning: armeabi-v7a build failed; skipping this ABI" >&2
            continue
        fi
        exit 1
    fi
    profile_dir="$PROFILE"
    [ "$PROFILE" = "dev" ] && profile_dir="debug"
    so="$TARGET_DIR/$target/$profile_dir/lib${LIB_NAME}.so"
    mkdir -p "$WORK/stage/lib/$abi"
    "$NDK_BIN/llvm-strip" --strip-unneeded -o "$WORK/stage/lib/$abi/lib${LIB_NAME}.so" "$so"
    BUILT_ABIS="$BUILT_ABIS $abi"
done
[ -n "$BUILT_ABIS" ] || { echo "error: no ABI built" >&2; exit 1; }

# ---- 2. resources (launcher icons) --------------------------------------------------
step "generating launcher icons"
cargo run -q -p "$CRATE" --example gen_icons -- "$WORK/res"

# ---- 3. Java helper -> classes.dex ----------------------------------------------------
step "javac + d8"
mapfile -t java_sources < <(find "$CRATE_DIR/java" -name '*.java' | sort)
# --release 8: java.* API from the JDK's Java 8 profile, android.* from
# android.jar; d8 desugars the lambdas.
javac -encoding UTF-8 --release 8 -Xlint:-options -Xlint:deprecation \
    -classpath "$ANDROID_PLATFORM_JAR" -d "$WORK/classes" "${java_sources[@]}"
mapfile -t class_files < <(find "$WORK/classes" -name '*.class' | sort)
"$ANDROID_BUILD_TOOLS/d8" --release --min-api "$MIN_SDK" --lib "$ANDROID_PLATFORM_JAR" \
    --output "$WORK/dex" "${class_files[@]}"
cp "$WORK/dex/classes.dex" "$WORK/stage/classes.dex"

# ---- 4. manifest + resources -> base APK ---------------------------------------------
step "aapt2 compile + link"
"$ANDROID_BUILD_TOOLS/aapt2" compile --dir "$WORK/res" -o "$WORK/res.zip"
"$ANDROID_BUILD_TOOLS/aapt2" link -o "$WORK/unaligned.apk" \
    -I "$ANDROID_PLATFORM_JAR" \
    --manifest "$CRATE_DIR/AndroidManifest.xml" \
    --min-sdk-version "$MIN_SDK" --target-sdk-version "$TARGET_SDK" \
    --version-code "$VERSION_CODE" --version-name "$VERSION_NAME" \
    "$WORK/res.zip"

# ---- 5. add dex + native libs, align, sign ---------------------------------------------
step "packaging (ABIs:$BUILT_ABIS)"
(cd "$WORK/stage" && zip -q -r -D -X "$WORK/unaligned.apk" classes.dex lib)
"$ANDROID_BUILD_TOOLS/zipalign" -f -p 4 "$WORK/unaligned.apk" "$WORK/aligned.apk"

if [ ! -f "$KEYSTORE" ]; then
    step "creating debug keystore $KEYSTORE"
    mkdir -p "$(dirname "$KEYSTORE")"
    keytool -genkeypair -noprompt -keystore "$KEYSTORE" -storepass "$KEYSTORE_PASS" -keypass "$KEYSTORE_PASS" \
        -alias "$KEY_ALIAS" -keyalg RSA -keysize 2048 -validity 10000 -dname "CN=Android Debug,O=Android,C=US"
fi

step "apksigner"
"$ANDROID_BUILD_TOOLS/apksigner" sign --ks "$KEYSTORE" --ks-pass "pass:$KEYSTORE_PASS" \
    --key-pass "pass:$KEYSTORE_PASS" --ks-key-alias "$KEY_ALIAS" --v4-signing-enabled false \
    --out "$OUT_DIR/$APK_NAME" "$WORK/aligned.apk"
"$ANDROID_BUILD_TOOLS/apksigner" verify "$OUT_DIR/$APK_NAME"

step "done: $OUT_DIR/$APK_NAME ($(du -h "$OUT_DIR/$APK_NAME" | cut -f1))"
