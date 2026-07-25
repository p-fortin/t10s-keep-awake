# Portability / platform boundary

The app currently targets **Windows only**, but the core logic is platform-agnostic and the
OS-specific parts are isolated to a handful of files. This doc maps that boundary so a **macOS or
Linux backend can be contributed** without touching the portable core.

> **macOS / Linux PRs welcome** — from someone who can *test on the hardware*. The pulse math is
> portable; the audio-device and UI layers must be verified on the real platform, not built blind.

## Module map

| File | Portable? | What it does / what a non-Windows backend needs |
|---|---|---|
| `config.rs` | ✅ portable | serde/toml config + clamps. No OS calls. Unit-tested. |
| `scheduler.rs` | ✅ mostly | engine loop + mode dispatch; only its calls into the platform modules below are OS-specific. |
| `monitor.rs` | ◐ split | `rms_dbfs` + `SilenceGate` are pure/portable (unit-tested). `LoopbackMeter` is **WASAPI loopback** — needs a CoreAudio (macOS) / PulseAudio-monitor (Linux) equivalent. |
| `audio.rs` | ✅ portable | shared-mode pulse render via **cpal**, which is already cross-platform (CoreAudio / ALSA). Should build and work as-is on macOS/Linux. |
| `audio_exclusive.rs` | ❌ Windows | **WASAPI exclusive** for >24 kHz (ultrasonic) output. See note below — this is *easier* on macOS. |
| `volume.rs` | ❌ Windows | pins device volume via `IAudioEndpointVolume`. macOS: CoreAudio `kAudioHardwareServiceDeviceProperty_VirtualMainVolume`; Linux: `pactl`/ALSA. Non-fatal — a stub that no-ops is acceptable. |
| `tray.rs` | ❌ Windows | system-tray + menu via **nwg**. macOS: a menu-bar (NSStatusItem) app; Linux: a StatusNotifierItem. A cross-platform tray crate (`tray-icon` + `tao`) could replace nwg for all three. |
| `settings_web.rs` | ◐ split | settings window as a local HTML page via **wry**/WebView2. `wry` and `tao` are already cross-platform (WKWebView on macOS, WebKitGTK on Linux); only `with_any_thread` is Windows-specific. The page in `assets/settings.html` is portable as-is. |
| `settings_ui.rs` | ❌ Windows | fallback settings window in native nwg controls, used only when WebView2 is missing. A non-Windows port can simply omit it. |
| `device_guard.rs` | ❌ Windows | holds the dedicated device's volume, and (shared-mode only) attempts a format repair. macOS: setting `kAudioDevicePropertyNominalSampleRate` actually *works*, unlike the Windows property write — see RESEARCH.md. |
| `health.rs` | ✅ portable | engine status shared with the UI. No OS calls. Unit-tested. |
| `autostart.rs` | ❌ Windows | HKCU `Run` key. macOS: a `LaunchAgent` plist in `~/Library/LaunchAgents`; Linux: a `.desktop` autostart entry or systemd user unit. |

## The ultrasonic pulse is *easier* on macOS
The Windows path needs WASAPI **exclusive mode** to escape the 48 kHz shared mix format and emit
>24 kHz. macOS CoreAudio has no exclusive-mode dance: you set the device's **nominal sample rate**
directly (`kAudioDevicePropertyNominalSampleRate`), then cpal can render at that rate. So a
CoreAudio hi-res output is likely simpler than `audio_exclusive.rs`.

## Suggested shape for a cross-platform version
If someone tackles this, the clean refactor is to put the OS-specific pieces behind small traits and
select the implementation with `#[cfg(target_os = "...")]`:

- `HiResOutput` — render a summed-sine pulse at an arbitrary sample rate (Windows: WASAPI exclusive; macOS: CoreAudio nominal-rate + cpal).
- `LoopbackMeter` — rolling RMS of what's playing on a render device (already an interface in `monitor.rs`).
- `VolumeControl` — get/set an endpoint's volume (may be a no-op).
- `TrayUi` + `Autostart` — the shell.

The portable core (`config`, `scheduler`, `SilenceGate`, the pulse synthesis) stays untouched; only
the backend crates change per platform. Keep the `MAX_LEVEL_DBFS = -12` safety clamp on every backend.
