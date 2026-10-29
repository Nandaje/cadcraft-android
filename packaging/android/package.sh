#!/usr/bin/env bash
# Build the Android native libraries (arm64-v8a) and assemble the Android APK.
#
# Usage: packaging/android/package.sh [--skip-gradle]
#
# Needs: cargo-ndk (`cargo install cargo-ndk --locked`), Android NDK (`ANDROID_NDK_HOME`),
# and the `aarch64-linux-android` Rust target.
set -euo pipefail
# shellcheck source=../env.sh
. "$(dirname "${BASH_SOURCE[0]}")/../env.sh"
HERE="$ROOT/packaging/android"
JNI_LIBS="$HERE/app/src/main/jniLibs"

mkdir -p "$JNI_LIBS/arm64-v8a"
for pair in \
  "mipmap-mdpi:$ROOT/assets/app-icon/hicolor/48x48/apps/ai.storyteller.cadcraft.png" \
  "mipmap-hdpi:$ROOT/assets/app-icon/cadcraft-64.png" \
  "mipmap-xhdpi:$ROOT/assets/app-icon/hicolor/128x128/apps/ai.storyteller.cadcraft.png" \
  "mipmap-xxhdpi:$ROOT/assets/app-icon/cadcraft-256.png" \
  "mipmap-xxxhdpi:$ROOT/assets/app-icon/hicolor/512x512/apps/ai.storyteller.cadcraft.png"; do
  dir="${pair%%:*}"
  src="${pair#*:}"
  mkdir -p "$HERE/app/src/main/res/$dir"
  cp "$src" "$HERE/app/src/main/res/$dir/ic_launcher.png"
done
(
  cd "$ROOT"
  if command -v cargo-ndk >/dev/null 2>&1; then
    cargo ndk \
      -t arm64-v8a \
      -o "$JNI_LIBS" \
      build --release --locked -p cadcraft-android
  else
    cargo build --release --locked --target aarch64-linux-android -p cadcraft-android
    cp "$CARGO_TARGET_DIR/aarch64-linux-android/release/libcadcraft_android.so" "$JNI_LIBS/arm64-v8a/libcadcraft_android.so"
  fi
)

if [ "${1:-}" != "--skip-gradle" ]; then
  if command -v gradle >/dev/null; then
    (cd "$HERE" && gradle assembleRelease)
    APK="$HERE/app/build/outputs/apk/release/app-release.apk"
    if [ -f "$APK" ]; then
      cp "$APK" "$DIST/cadcraft-$VERSION-android.apk"
      echo "packaged: $DIST/cadcraft-$VERSION-android.apk"
    fi
  else
    echo "note: gradle not on PATH; native libraries staged in $JNI_LIBS"
  fi
fi
