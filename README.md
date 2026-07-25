# T10s_Keep_Awake

**Adam Audio confirms the T10S subwoofer's auto-standby cannot be disabled** — no menu option, no
switch. After ~16 min of "no signal" it sleeps, and since it houses the crossover, it drags the
T-series monitors down with it (waking takes seconds and pops). Their only advice is "run it hot."

This is a tiny Windows system-tray utility that keeps it awake instead. The T10S takes both
**balanced (XLR) and unbalanced (RCA) input and sums them** — so if your listening path runs into
the balanced inputs, the unbalanced pair is free. The utility fires a brief, configurable keep-alive
pulse into it on a **dedicated USB audio device of its own**, at a fixed, knob-independent level.
The sub sees the pulse; your listening path is never touched.

To make the pulse as unobtrusive as possible I reverse-engineered the sub's auto-sense detector.
**That research — the interesting part — is written up with charts in [`docs/RESEARCH.md`](docs/RESEARCH.md):**

![Detector frequency response](docs/charts/detector_frequency_response.png)

> _Not affiliated with Adam Audio. Personal project — use at your own risk. Pulses are hard-capped at
> −12 dBFS with the dongle pinned to 25% volume, so the sub is never driven hard._

## Signal chain

![Signal path](docs/charts/signal_path.svg)

The pulse device carries only the pulse (never music), so its level is fixed and knob-independent —
calibrate once, valid at any listening volume. The **galvanic USB isolator is not optional**: without
it the unbalanced run puts a ground loop between PC and sub, audible as static on the monitors. Use a
**high-speed (480 Mbps)** isolator, not full-speed — high speed passes a hi-res DAC unthrottled. Do
not substitute an inline RCA/transformer isolator; those roll off below ~50 Hz and above ~20 kHz,
killing exactly the pulses that work.

## Download
Grab `t10s_keep_awake.exe` from [Releases](../../releases) and run it — no installer, nothing to
unpack. The C runtime is linked statically, so it needs no VC++ redistributable. The settings window
uses the **Edge WebView2 runtime** (present by default on Windows 11); without it the app falls back
to a native settings form, so it works either way.

Windows SmartScreen will warn about an unsigned executable from an unknown publisher — the binary
isn't code-signed. Build it yourself if you'd rather not take that on trust.

## Build
```
cargo build --release
```
Release binaries link the CRT statically, which is not the cargo default:
```
set RUSTFLAGS=-C target-feature=+crt-static
cargo build --release
```

## Run
```
t10s_keep_awake.exe run     # tray app: keep-awake engine + system-tray icon (or just double-click)
t10s_keep_awake.exe         # same (run is the default)
```
Right-click the tray icon for **Enabled / Disabled** (a checkmark shows which is active),
**Send Test Pulse**, **Configure…** and **Quit**. The icon turns amber and raises a notification if
a pulse ever fails, so a broken keep-alive can't go unnoticed. A pulse fires immediately on start
and whenever you re-enable it, then every `interval-min` minutes.

Other subcommands:
```
t10s_keep_awake.exe list-devices                         # list output devices
t10s_keep_awake.exe device-info --device "<name>"        # sample-rate + exclusive support + current volume
t10s_keep_awake.exe settings                             # open the settings window without the tray
t10s_keep_awake.exe pulse --device "<name>" --freqs 50,250,500 --db=-38 --dur 0.5   # fire one pulse (test tool)
```

## Configure
Tray → **Configure…** opens the settings window (a local HTML page rendered by WebView2). It
deliberately exposes only what you should change:

| Setting | Meaning |
|---|---|
| **Keep the sub awake** | master switch; applies immediately |
| **Output device** | the dedicated dongle. The window checks it can carry the pulse and refuses to save one that can't |
| **Interval** | 10–14 minutes between pulses (12 recommended; the sub sleeps after ~16 min) |
| **Start with Windows** | autostart via the `HKCU\...\Run` key |

Everything else stays in `%APPDATA%\T10sKeepAwake\config.toml` at its locked value — `freq-hz`,
`level-dbfs`, `duration-s`, `fade-ms`, `freqs`/`decay-s` (the audible chord/chime fallbacks),
`exclusive`/`sample-rate`, `device-volume-pct`, `mode`/`monitor-device`/`silence-threshold-dbfs`.
Edits to that file are picked up within ~2 s, no restart needed. All values are clamped on load,
and `level-dbfs` can never exceed −12 dBFS.

## Safety
`level-dbfs` is hard-clamped to **−12 dBFS** in code (`MAX_LEVEL_DBFS`) and the dongle volume is
held at 25% while the app runs — no config or typo can drive the sub harder than that.

## The pulse
**Locked: 30 000 Hz · −12 dBFS · 5 s · every 12 min** — an ultrasonic pulse that the sub's auto-sense
detector responds to but the T-series tweeters (25 kHz ceiling) cannot reproduce, so it is **silent
by physics**, not merely quiet — and silent to pets too, which a merely above-human 20 kHz tone
would not be. Running in production since, holding the sub awake indefinitely; see
[`docs/RESEARCH.md`](docs/RESEARCH.md), which is the full account of how these numbers were
arrived at. Raw measurements are in [`test-and-learn/`](test-and-learn) and every chart regenerates
from them via `scripts/make_charts.py`.

Requires a DAC that natively runs ≥ 96 kHz — check yours with `device-info`. Without one, the app
is fully configurable to the audible fallbacks characterised in
[`docs/RESEARCH.md`](docs/RESEARCH.md) (chords at −38 dBFS per tone, soft chimes at −32) by setting
`freqs`/`decay-s` in `config.toml`.

The pulse is emitted in **WASAPI exclusive mode**, which negotiates the sample rate directly with the
driver. That matters for robustness: if Windows changes the device's shared "Default Format" — a
driver update, or someone clicking about in Sound settings — a shared-mode pulse would sit above
Nyquist and be refused, silently ending the keep-alive. Exclusive mode is immune to that setting.
Taking exclusive control is acceptable precisely *because the device is dedicated*: nothing else
plays through it. If exclusive is refused (another app holds the device), the app falls back to
shared mode rather than skipping the pulse.

> The app refuses to emit any tone at or above Nyquist for the stream it actually gets — at a 48 kHz
> endpoint a 30 kHz pulse would fold to an audible 18 kHz. It never guesses.

## Autostart
Toggle in the settings window (or set `autostart = true`). Managed via the `HKCU\...\Run` key.

## Platform support
Windows only for now, but the pulse math and scheduler are platform-agnostic — only the audio-device,
tray, and volume layers are Windows-specific. **macOS / Linux backends are welcome** from anyone who
can test on the hardware; [`docs/PORTABILITY.md`](docs/PORTABILITY.md) maps exactly which files are
portable vs. platform-specific and where a new backend plugs in. (Note: the ultrasonic hi-res output
is actually *simpler* on macOS CoreAudio than on Windows WASAPI.)

The keep-alive approach applies to any **Adam Audio T-series** monitor paired with the T10S — the T5V/T7V/
T8V share the same U-ART tweeter and all cap at 25 kHz, so a >25 kHz pulse is silent on all of them.
