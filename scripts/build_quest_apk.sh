#!/usr/bin/env bash
set -euo pipefail

usage() {
    cat <<'EOF'
Usage: scripts/build_quest_apk.sh [OPTIONS]

Build and package the Meta Quest 3 OpenXR client (vdesk-quest) APK.

Options:
  --release      Build release APK (default)
  --debug        Build debug APK
  --debug-sign   Sign a release APK with ~/.android/debug.keystore (for local sideload testing only)
  --install      Install the resulting APK to a connected Quest via `adb install -r`
  -h, --help     Show this help message

Release Signing:
  For public/release distribution, set:
    CARGO_APK_RELEASE_KEYSTORE=/path/to/release.keystore
    CARGO_APK_RELEASE_KEYSTORE_PASSWORD=...
EOF
}

PROFILE="release"
DEBUG_SIGN=false
INSTALL=false

while [[ $# -gt 0 ]]; do
    case "$1" in
        --release)
            PROFILE="release"
            shift
            ;;
        --debug)
            PROFILE="debug"
            shift
            ;;
        --debug-sign)
            DEBUG_SIGN=true
            shift
            ;;
        --install)
            INSTALL=true
            shift
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            echo "Unknown option: $1" >&2
            usage >&2
            exit 1
            ;;
    esac
done

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd -- "$SCRIPT_DIR/.." && pwd)"
cd "$REPO_ROOT"

# Auto-detect Android SDK
if [[ -z "${ANDROID_SDK_ROOT:-}" ]]; then
    for candidate in /opt/android-sdk "$HOME/Android/Sdk" "$HOME/android-sdk"; do
        if [[ -d "$candidate" ]]; then
            export ANDROID_SDK_ROOT="$candidate"
            break
        fi
    done
fi
if [[ -z "${ANDROID_SDK_ROOT:-}" || ! -d "$ANDROID_SDK_ROOT" ]]; then
    echo "Error: ANDROID_SDK_ROOT is not set and no Android SDK was found in default locations." >&2
    exit 1
fi

# Auto-detect Android NDK
if [[ -z "${ANDROID_NDK_ROOT:-}" ]]; then
    if [[ -d /opt/android-ndk ]]; then
        export ANDROID_NDK_ROOT="/opt/android-ndk"
    elif [[ -d "$ANDROID_SDK_ROOT/ndk" ]]; then
        newest_ndk="$(find "$ANDROID_SDK_ROOT/ndk" -mindepth 1 -maxdepth 1 \( -type d -o -type l \) | sort -V | tail -n 1 || true)"
        if [[ -n "$newest_ndk" ]]; then
            export ANDROID_NDK_ROOT="$newest_ndk"
        fi
    fi
fi
if [[ -z "${ANDROID_NDK_ROOT:-}" || ! -d "$ANDROID_NDK_ROOT" ]]; then
    echo "Error: ANDROID_NDK_ROOT is not set and no Android NDK was found." >&2
    exit 1
fi

if ! command -v cargo-apk >/dev/null 2>&1; then
    echo "Error: cargo-apk not found. Install with: cargo install cargo-apk --version 0.10.0" >&2
    exit 1
fi

if ! rustup target list --installed | grep -qx "aarch64-linux-android"; then
    echo "Error: Rust target 'aarch64-linux-android' is not installed." >&2
    echo "Install with: rustup target add aarch64-linux-android" >&2
    exit 1
fi

if [[ "$PROFILE" == "release" ]]; then
    if [[ -n "${CARGO_APK_RELEASE_KEYSTORE:-}" && -n "${CARGO_APK_RELEASE_KEYSTORE_PASSWORD:-}" ]]; then
        if [[ ! -f "$CARGO_APK_RELEASE_KEYSTORE" ]]; then
            echo "Error: CARGO_APK_RELEASE_KEYSTORE does not exist: $CARGO_APK_RELEASE_KEYSTORE" >&2
            exit 1
        fi
        echo "Using release keystore: $CARGO_APK_RELEASE_KEYSTORE"
    elif [[ "$DEBUG_SIGN" == "true" ]]; then
        if ! command -v keytool >/dev/null 2>&1; then
            echo "Error: keytool not found in PATH (required to ensure ~/.android/debug.keystore)." >&2
            exit 1
        fi
        debug_keystore="$HOME/.android/debug.keystore"
        if [[ ! -f "$debug_keystore" ]]; then
            mkdir -p "$HOME/.android"
            keytool -genkeypair -v \
                -keystore "$debug_keystore" \
                -storepass android \
                -alias androiddebugkey \
                -keypass android \
                -dname "CN=Android Debug,O=Android,C=US" \
                -keyalg RSA \
                -keysize 2048 \
                -validity 10000
        fi
        export CARGO_APK_RELEASE_KEYSTORE="$debug_keystore"
        export CARGO_APK_RELEASE_KEYSTORE_PASSWORD="android"
        echo "Signing release APK with local debug keystore ($debug_keystore) for sideload testing."
    else
        cat >&2 <<'EOF'
Error: Release APK builds require a signing keystore.

For public release builds, set:
  export CARGO_APK_RELEASE_KEYSTORE=/path/to/your-release.keystore
  export CARGO_APK_RELEASE_KEYSTORE_PASSWORD="your-password"

For local sideload testing with ~/.android/debug.keystore, pass --debug-sign:
  ./scripts/build_quest_apk.sh --debug-sign
EOF
        exit 1
    fi
fi

echo "ANDROID_SDK_ROOT=$ANDROID_SDK_ROOT"
echo "ANDROID_NDK_ROOT=$ANDROID_NDK_ROOT"

if [[ "$PROFILE" == "release" ]]; then
    cargo apk build --release -p vdesk-quest
    SRC_APK="target/release/apk/vdesk-quest.apk"
    OUT_NAME="VirtualRustTop-quest3.apk"
else
    cargo apk build -p vdesk-quest
    SRC_APK="target/debug/apk/vdesk-quest.apk"
    OUT_NAME="VirtualRustTop-quest3-debug.apk"
fi

if [[ ! -f "$SRC_APK" ]]; then
    echo "Error: Expected APK not found at $SRC_APK" >&2
    exit 1
fi

mkdir -p dist
OUT_APK="dist/$OUT_NAME"
cp -f "$SRC_APK" "$OUT_APK"

APKSIGNER_BIN="$(command -v apksigner || true)"
if [[ -z "$APKSIGNER_BIN" ]]; then
    APKSIGNER_BIN="$(find "$ANDROID_SDK_ROOT/build-tools" -maxdepth 2 -name apksigner -type f 2>/dev/null | sort -V | tail -n 1 || true)"
fi
if [[ -n "$APKSIGNER_BIN" ]]; then
    "$APKSIGNER_BIN" verify "$OUT_APK"
    echo "Verified APK signature with $APKSIGNER_BIN"
else
    echo "Warning: apksigner not found; skipping signature verification."
fi

(
    cd dist
    sha256sum "$OUT_NAME" > "${OUT_NAME}.sha256"
)

echo "Built APK: $OUT_APK ($(du -h "$OUT_APK" | cut -f1))"
echo "SHA-256:   $(cat "dist/${OUT_NAME}.sha256")"

if [[ "$INSTALL" == "true" ]]; then
    ADB_BIN="$(command -v adb || true)"
    if [[ -z "$ADB_BIN" && -x "$ANDROID_SDK_ROOT/platform-tools/adb" ]]; then
        ADB_BIN="$ANDROID_SDK_ROOT/platform-tools/adb"
    fi
    if [[ -z "$ADB_BIN" ]]; then
        echo "Error: adb not found for --install" >&2
        exit 1
    fi
    "$ADB_BIN" install -r "$OUT_APK"
fi
