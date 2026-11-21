# Building the Quest 3 Client (`vdesk-quest`) from Source

Most users should download the pre-built `VirtualRustTop-quest3.apk` from [GitHub Releases](https://github.com/drakkhis/VirtualRustTop/releases). Follow this guide if you want to modify or compile the OpenXR + Vulkan Quest client yourself.

---

## Tested Toolchain Versions

- **Rust:** Stable toolchain (`rustc` / `cargo`) with Android target:
  ```bash
  rustup target add aarch64-linux-android
  ```
- **Packaging tool:** [`cargo-apk`](https://crates.io/crates/cargo-apk) `0.10.0`
  ```bash
  cargo install cargo-apk --version 0.10.0
  ```
- **Android SDK:**
  - Platform: **API 32** (`platforms/android-32`, matching `target_sdk_version = 32` in `crates/vdesk-quest/Cargo.toml`)
  - Build-Tools: **37.0.0** (provides `aapt2`, `zipalign`, `apksigner`)
  - Platform-Tools: `adb`
- **Android NDK:** **r30** (`30.0.16248370`)
- **JDK:** `keytool` (for generating a signing keystore)

> **Bundled OpenXR Loader:** `crates/vdesk-quest/libs/arm64-v8a/libopenxr_loader.so` is already checked into the repository (Khronos OpenXR Loader for Android, Apache-2.0 — see [THIRD_PARTY_NOTICES.md](../THIRD_PARTY_NOTICES.md)). You do not need to download the Meta OpenXR Mobile SDK separately.

---

## Environment Setup

Export your SDK and NDK paths (adjust to match your machine):

```bash
export ANDROID_SDK_ROOT=/opt/android-sdk
export ANDROID_NDK_ROOT=/opt/android-sdk/ndk/30.0.16248370
```

`scripts/build_quest_apk.sh` also checks `/opt/android-sdk`, `$HOME/Android/Sdk`, `/opt/android-ndk`, and the newest directory under `$ANDROID_SDK_ROOT/ndk/` automatically.

---

## Building & Signing APKs

### Option A: Local Sideload Testing (`--debug-sign`)

For local testing over USB/ADB, sign the release APK using your local Android debug keystore (`~/.android/debug.keystore`, generated automatically via `keytool` if missing):

```bash
./scripts/build_quest_apk.sh --debug-sign
```

To build and immediately install onto a connected Quest 3:

```bash
./scripts/build_quest_apk.sh --debug-sign --install
```

### Option B: Signed Release Build (Public Distribution)

Never commit `.keystore` or `.jks` files to git. Generate a release keystore **outside** the repository:

```bash
mkdir -p ~/.config/vdesk-signing
keytool -genkeypair -v \
  -keystore ~/.config/vdesk-signing/vdesk-release.keystore \
  -alias vdesk \
  -keyalg RSA -keysize 2048 -validity 10000
```

Pass the keystore and password via environment variables when running the build script:

```bash
export CARGO_APK_RELEASE_KEYSTORE="$HOME/.config/vdesk-signing/vdesk-release.keystore"
export CARGO_APK_RELEASE_KEYSTORE_PASSWORD="your-keystore-password"
./scripts/build_quest_apk.sh
```

> **Note:** `cargo-apk` signs the release APK using `CARGO_APK_RELEASE_KEYSTORE` and `CARGO_APK_RELEASE_KEYSTORE_PASSWORD` (and assumes the key password matches the keystore password, or that the keystore uses PKCS12).

### Build Output & Verification

`scripts/build_quest_apk.sh` writes:
- `dist/VirtualRustTop-quest3.apk` (or `dist/VirtualRustTop-quest3-debug.apk` with `--debug`)
- `dist/VirtualRustTop-quest3.apk.sha256`

To verify the APK signature manually:

```bash
"$ANDROID_SDK_ROOT"/build-tools/*/apksigner verify --verbose dist/VirtualRustTop-quest3.apk
```

---

## Debug Build & Logcat

For faster incremental debug iterations:

```bash
./scripts/build_quest_apk.sh --debug --install
adb logcat -s vdesk-quest:* RustStdoutStderr:*
```
