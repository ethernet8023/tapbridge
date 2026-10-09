#!/bin/bash
# Builds the debug APK (and its Rust JNI library) on macOS/Linux.
# Run inside the flake dev shell: nix develop -c scripts/build-android.sh
# Extra args go to Gradle (default task: assembleDebug).
# Output: android/app/build/outputs/apk/debug/app-debug.apk
# Install: adb install -r android/app/build/outputs/apk/debug/app-debug.apk
set -euo pipefail
cd "$(dirname "$0")/../android"

for v in JAVA_HOME ANDROID_HOME; do
    if [[ -z "${!v:-}" ]]; then
        echo "error: $v unset; run inside 'nix develop'" >&2
        exit 1
    fi
done
if [[ ! -f app/google-services.json ]]; then
    echo "error: android/app/google-services.json missing (gitignored Firebase config)" >&2
    exit 1
fi

# Default to assembleDebug unless a task (a non-flag argument) was given.
has_task=0
for a in "$@"; do [[ "$a" == -* ]] || has_task=1; done
[[ $has_task == 1 ]] || set -- assembleDebug "$@"
"$JAVA_HOME/bin/java" -classpath gradle/wrapper/gradle-wrapper.jar \
    org.gradle.wrapper.GradleWrapperMain "$@"

# 16 KB page-size gate (Android 15+ devices, Play requirement): every native
# library in every built APK must have 16 KB-aligned LOAD segments and sit
# 16 KB-aligned in the zip. Third-party AARs (CameraX < 1.4) break this.
readelf=$(ls "$ANDROID_NDK_HOME"/toolchains/llvm/prebuilt/*/bin/llvm-readelf | head -1)
zipalign=$(ls "$ANDROID_HOME"/build-tools/*/zipalign | tail -1)
bad=0
while IFS= read -r apk; do
    "$zipalign" -c -P 16 4 "$apk" || { echo "error: $apk: zip entries not 16 KB aligned" >&2; bad=1; }
    libs=$(mktemp -d)
    unzip -q -o "$apk" 'lib/*' -d "$libs" 2>/dev/null || true
    while IFS= read -r so; do
        if "$readelf" -lW "$so" | awk '/LOAD/ && $NF != "0x4000" && $NF != "0x10000" {bad=1} END {exit !bad}'; then
            echo "error: ${so#"$libs"/}: LOAD segments not 16 KB aligned" >&2
            bad=1
        fi
    done < <(find "$libs" -name '*.so')
    rm -rf "$libs"
done < <(find app/build/outputs/apk -name '*.apk' -newer app/build.gradle.kts 2>/dev/null)
if [[ $bad == 1 ]]; then
    exit 1
fi
echo "16 KB page size: OK"
