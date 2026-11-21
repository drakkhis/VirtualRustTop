# Release Checklist (`v0.1.0`)

Owner checklist for tagging and publishing a public release of `VirtualRustTop`.

---

## 1. Clean-Room Verification

Clone the repository into a fresh temporary directory and follow [`README.md`](../README.md) verbatim to confirm nothing depends on local machine configuration:

```bash
git clone https://github.com/drakkhis/VirtualRustTop.git /tmp/VirtualRustTop-clean
cd /tmp/VirtualRustTop-clean
cargo test --workspace
cargo build --release -p vdesk-host
./target/release/vdesk-host --help
```

Verify that:
- `vdesk-host` launches on Wayland, initializes Portal + PipeWire + NVENC, and streams smoothly to the Quest 3.
- LAN auto-discovery connects without `VDESK_HOST_ADDR` or `host.txt` overrides.

---

## 2. Build the Signed Release APK & Checksums

Build a release-signed APK using your private release keystore (stored **outside** the repo):

```bash
export CARGO_APK_RELEASE_KEYSTORE="$HOME/.config/vdesk-signing/vdesk-release.keystore"
export CARGO_APK_RELEASE_KEYSTORE_PASSWORD="..."
./scripts/build_quest_apk.sh
```

Verify the generated artifacts in `dist/`:

```bash
ls -lh dist/
cat dist/VirtualRustTop-quest3.apk.sha256
"$ANDROID_SDK_ROOT"/build-tools/*/apksigner verify dist/VirtualRustTop-quest3.apk
```

---

## 3. Record Demo Clip & Update `README.md`

- Capture a ~20-second Meta Quest 3 recording showing:
  - Crisp text/code readability in mixed-reality passthrough
  - Smooth 60+ FPS video playback over Wi-Fi
  - Brief overlay or note on end-to-end latency and host VRAM usage
- Replace the `[Demo Clip Placeholder]` line at the top of `README.md` with the clip/GIF link before tagging.

---

## 4. Tag & Publish GitHub Release (`v0.1.0`)

```bash
git tag -a v0.1.0 -m "VirtualRustTop v0.1.0"
git push origin v0.1.0
```

Attach to the GitHub Release:
- `dist/VirtualRustTop-quest3.apk`
- `dist/VirtualRustTop-quest3.apk.sha256`

### Release Notes Template

```markdown
## VirtualRustTop v0.1.0

Initial public release of **VirtualRustTop** — a native Linux Wayland-to-Quest 3 virtual desktop streamer written in Rust.

### Tested Stack
- Linux Wayland (KDE Plasma / KWin)
- NVIDIA RTX 40-series+ (NVENC AV1 via zero-copy PipeWire DMA-BUF -> EGL -> CUDA)
- Meta Quest 3 (OpenXR + Vulkan + hardware `AMediaCodec` AV1)

> Released as-is with no support guarantees. PRs for AMD/Intel/X11/other compositors and headsets are welcome.

### Installation
1. Sideload `VirtualRustTop-quest3.apk` onto your Quest 3 (`adb install -r VirtualRustTop-quest3.apk`).
2. Follow [README.md](README.md) to set up `/dev/uinput` permissions and build/run `vdesk-host`.

### Checksums
See `VirtualRustTop-quest3.apk.sha256`.
```

---

## 5. Community Sharing

1. **Linux VR Adventures (LVRA) Discord / Matrix** — best audience for technical Wayland/OpenXR feedback and potential contributors.
2. **r/linux_gaming** & **r/virtualreality** (or **r/OculusQuest**) — lead directly with the ~20-second headset demo clip.
3. **r/rust** — highlight the engineering pipeline (`PipeWire DMA-BUF` → `EGL/CUDA` → `NVENC AV1` → `quinn` QUIC + ABR → `OpenXR` + `Vulkan` + `AMediaCodec`).
