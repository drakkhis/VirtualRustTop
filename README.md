# VirtualRustTop

Low-latency Linux Wayland desktop streaming to **Meta Quest 3** in mixed-reality passthrough, written in Rust.

`VirtualRustTop` captures your Wayland monitors via PipeWire DMA-BUF, encodes them with hardware NVENC AV1, and streams video, stereo audio, and input over QUIC to a native OpenXR + Vulkan Quest 3 client.

---

## Features

- **Zero-Copy Wayland Capture & Hardware AV1**: Uses XDG Desktop Portal (`ScreenCast` + `RemoteDesktop`) and PipeWire DMA-BUF feeding NVIDIA NVENC (`Av1YUV420`).
- **Native Quest 3 OpenXR + Vulkan Client**: Renders curved cylindrical monitors in passthrough XR (`com.vdesk.quest`) with hardware AV1 decoding via Android `MediaCodec`.
- **QUIC Transport & LAN Auto-Discovery**: Multiplexed video, 48 kHz stereo PipeWire audio, and reliable input over QUIC (`quinn` + `rustls`, UDP `9443`), with automatic UDP broadcast discovery on port `9444`.
- **Kernel-Level Input Injection**: Injects mouse motion, clicks, scrolling, and keyboard events on the Linux host via `/dev/uinput` (`evdev`).
- **Dual-Hand Virtual Keyboard & Pointer Stabilization**: Floating virtual keyboard and toolbar quad with raycast hit-testing, haptic feedback, and pointer stabilization.
- **System Tray Integration**: Optional `--tray` mode (KDE / Freedesktop StatusNotifierItem via `ksni`) to start, stop, and restart `vdesk-host.service`.

---

## Workspace Structure

| Crate | Description |
| :--- | :--- |
| [`crates/vdesk-protocol`](crates/vdesk-protocol) | Shared `postcard`-serialized control, input, video frame, and audio packet protocol (`ALPN: vdesk-xr/1`). |
| [`crates/vdesk-host`](crates/vdesk-host) | Linux Wayland host streamer, NVENC AV1 encoder, PipeWire audio router, `/dev/uinput` injector, and tray service. |
| [`crates/vdesk-quest`](crates/vdesk-quest) | Meta Quest 3 native Android OpenXR + Vulkan client (`aarch64-linux-android`). |

---

## Prerequisites

### Linux Host (`vdesk-host`)
- **Wayland** desktop environment with `xdg-desktop-portal` (`ScreenCast` and `RemoteDesktop` portals) and **PipeWire**.
- **NVIDIA GPU** with hardware AV1 NVENC support (RTX 40-series or newer) and proprietary NVIDIA drivers.
- **Rust** toolchain (`cargo`, `rustc`) and system development libraries (`pipewire`, `libspa`, `pkg-config`, `clang`).
- `/dev/uinput` access for virtual mouse/keyboard injection.

### Meta Quest 3 Client (`vdesk-quest`)
- **Meta Quest 3** with Developer Mode enabled.
- Rust Android target:
  ```bash
  rustup target add aarch64-linux-android
  ```
- [`cargo-apk`](https://crates.io/crates/cargo-apk), **Android SDK**, **Android NDK**, and `adb`.
  *(Note: `crates/vdesk-quest/libs/arm64-v8a/libopenxr_loader.so` is already bundled in the repository.)*

---

## Setup & Installation

### 1. Configure `/dev/uinput` Permissions (Host)

Install the included udev rule so `vdesk-host` can create a virtual input device without running as root:

```bash
sudo cp udev/99-vdesk-uinput.rules /etc/udev/rules.d/
sudo udevadm control --reload-rules
sudo udevadm trigger
```

If your user is not already in the `input` group, add it and re-login:

```bash
sudo usermod -aG input "$USER"
```

### 2. Build & Run the Linux Host

```bash
cargo build --release -p vdesk-host
./target/release/vdesk-host
```

- On first launch, approve the XDG Desktop Portal screen-sharing & remote-desktop permission dialog.
- If you run a firewall, allow **UDP `9443`** (QUIC stream) and **UDP `9444`** (LAN auto-discovery) on your local network.

#### Useful Host Flags
- `--tray` — Run the KDE / Freedesktop system tray controller for `vdesk-host.service`.
- `--test-uinput` — Verify `/dev/uinput` virtual device registration and pointer injection.
- `--test-portal` — Verify XDG Desktop Portal initialization and pointer motion.
- `--dump-frames <N>` — Dump `N` encoded AV1 frames for debugging.
- `--wiggle-uinput <N>` — Inject `N` test pointer motion events via `/dev/uinput`.

### 3. Build & Sideload the Quest 3 APK

Set your Android SDK and NDK paths, then build and install to your Quest 3 over USB/ADB:

```bash
export ANDROID_SDK_ROOT=/opt/android-sdk
export ANDROID_NDK_ROOT=/opt/android-sdk/ndk/<your-ndk-version>

cargo apk build --release -p vdesk-quest
adb install -r target/release/apk/vdesk-quest.apk
```

Or build and launch in one step:

```bash
cargo apk run --release -p vdesk-quest
```

> **Network Discovery:** `vdesk-quest` automatically broadcasts `VDESK_PING` on UDP port `9444` to locate `vdesk-host` on your LAN. If your network blocks UDP broadcasts, you can override the fallback address via `VDESK_HOST_ADDR` (defaults to `192.168.1.16:9443`).

---

## Quest 3 Controls

| Input | Action |
| :--- | :--- |
| **Right Trigger** or **A** | Left click on monitor / press key on virtual keyboard |
| **Left Trigger** | Right click on monitor / press key on virtual keyboard (left hand) |
| **B** or **Right Thumbstick Click** | Right click |
| **Right Thumbstick** | 2D scroll (when pointing at a monitor) |
| **Hold Right Grip** | Grab and reposition a monitor or the virtual keyboard |
| **Right Grip + Thumbstick X** | Move grabbed monitor or keyboard closer / farther |
| **Right Grip + Thumbstick Y** | Resize grabbed monitor curvature/arc |
| **X** (Left Controller) | Toggle virtual keyboard visibility |
| **Y** (Left Controller) | Recenter unpinned monitors and keyboard |
