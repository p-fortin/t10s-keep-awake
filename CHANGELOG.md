# Changelog

All notable changes to T10s_Keep_Awake are recorded here.

## [Unreleased]

## [0.1.0] — 2026-07-25
First public release. A prebuilt `t10s_keep_awake.exe` is attached to the
[v0.1.0 release](https://github.com/p-fortin/t10s-keep-awake/releases/tag/v0.1.0); the C runtime is
linked statically, so it needs no VC++ redistributable. The binary is unsigned, so SmartScreen warns
about an unknown publisher.

### Added — the app grew up (2026-07-25)
**Settings window rewritten as a local HTML page** rendered by Edge WebView2 (`wry` + `tao`),
replacing the native Win32 form. Dark/light following the Windows theme, a status card with a live
countdown ring, real toggle switches, and a segmented interval picker. The native form is kept as
`settings_ui.rs` and used automatically when the WebView2 runtime is missing, so settings are never
unreachable. The window sizes itself to its content, so there is no scrollbar at any DPI.

The window exposes **four settings** — on/off, output device, interval (10–14 min), start with
Windows. Frequency, level, duration, fade, mode, monitor device, sample rate and device volume are
locked values that stay in `config.toml`; they are read on every cycle, so editing the file needs no
restart. "Sound Detect" mode is gone from the UI (an inaudible pulse has nothing to gain from
silence-gating) but remains available as `mode = "audio-sensing"`.

- **Device capability check.** The picker reports `384 kHz · Ultrasonic Ready` or `Too Slow`, and
  refuses to save a device that cannot carry the pulse — previously the app would simply never pulse
  and never say why. It also warns when the chosen device is your default playback device, which the
  app is not entitled to manage.
- **`health.rs`** — engine status shared with the UI. The tray icon switches to an amber variant and
  raises a notification when a pulse fails, and returns to green on recovery.
- **Tray**: checkmarks show which of Enabled/Disabled is current; new **Send Test Pulse**.
- **`device_guard.rs`** — holds the dedicated device's volume while the app runs (it used to be
  pinned only at launch), and classifies devices for the settings UI.

### Fixed — three real defects (2026-07-25)
- **A 5 s pulse was audible.** Phase was computed from the absolute sample index in f32; by ~1.92 M
  samples (384 kHz × 5 s) an ulp of the phase argument is ~0.06 rad against a 0.49 rad step, smearing
  the carrier into **−56 dB of audible-band hash** — the "faint modem sound". Phase is now computed
  in f64 (`audio::sine_sum`), which puts the artifact below −160 dB. Regression test included; it was
  verified to fail against the old maths before the fix landed.
- **Closing the settings window killed the app.** `tao`'s `event_loop.run()` calls `process::exit`,
  which took the tray and engine with it. Now uses `run_return`.
- **Enabling didn't pulse, and interval edits didn't apply.** The engine only fired at startup and
  slept the whole interval in one call. It now fires on any disabled→enabled transition and polls at
  250 ms, so an interval change re-targets the current countdown (from the last pulse, not the edit)
  and disabling takes effect at once.

### Changed — exclusive mode, because the shared format can't be defended (2026-07-25)
The pulse is now emitted in **WASAPI exclusive mode** by default. Changing the endpoint's shared
format in Windows Sound settings (e.g. to 16-bit 44.1 kHz) put 30 kHz above Nyquist, and the app
correctly refused to emit — silently ending the keep-alive.

Repairing it by writing `PKEY_AudioEngine_DeviceFormat` **does not work**: `SetValue`/`Commit` return
success and Windows ignores the value until the endpoint is re-initialised. The first implementation
believed the return value and logged "repaired" on every cycle while the device sat at 44.1 kHz —
an unverified success that hid a dead keep-alive. Repairs now read the device back and report
honestly. Exclusive mode sidesteps the problem entirely by negotiating the rate with the driver;
taking exclusive control is acceptable precisely because the device is dedicated. Falls back to
shared mode if exclusive is refused. Verified inaudible by ear before being made the default.

- **Nyquist guard** (`audio::check_nyquist`): refuses to emit any tone at or above half the stream's
  rate rather than aliasing into the audible band. Covers both output paths.
- Taskbar/title-bar icon for the settings window; new help and warning icons.

### Changed — repository trimmed for publication (2026-07-25)
`CLAUDE.md`, `test-and-learn/FINDINGS.md` and `test-and-learn/CAMPAIGN.md` are no longer tracked
(kept locally, like `HANDOFF.md`). They were working notes carrying superseded conclusions and
rig-specific values. **`docs/RESEARCH.md` is now the single published account of the method and
findings**; raw CSVs and `led_watcher.py` remain tracked so every chart is reproducible.

### Changed — RESEARCH.md corrected on exclusive mode (2026-07-25)
It stated that exclusive mode "proved unnecessary". True when written, overturned by the above. New
section **"The sample rate is not yours to keep"** documents the drift, the property-write that
reports success and does nothing, and why an unverified success is worse than a failure.

### Changed — public docs rewritten for readability (2026-07-25)
`docs/RESEARCH.md` reworked from a lab-notebook register into a readable writeup, same data and
findings throughout. The substantive changes:
- **Framing:** the sub's dual inputs lead the document. The T10S sums balanced and unbalanced input,
  so a listening path on XLR leaves the RCA pair free — that is the opportunity the project is built
  on, not an implementation footnote. The RCA input is no longer described as "spare".
- **The rig is now staged before the findings:** the generic USB 2.0 dongle and its 48 kHz native
  ceiling (including the `supported_output_configs` 384 kHz trap — a shared-engine resampling
  figure, not native), the ground-loop static, and why the isolator had to be high-speed.
- **The audibility argument is stated properly:** above human hearing (~20 kHz) is the wrong bar,
  since dogs hear to ~45 kHz. The real threshold is the speakers' 25 kHz reproduction ceiling.
- Standby figure reconciled to **~16 min** in README (it still read ~17.5 in two places).
- Keep-alive described as holding the sub awake **indefinitely in production**; the endurance run is
  cited as validation rather than as the claim.

### Fixed — signal-path diagram and chord chart (2026-07-25)
- New **`docs/charts/signal_path.svg`** — a drawn, colour-coded signal-path diagram replacing the
  ASCII chain in both README and RESEARCH.md. Hand-authored (not generated from data), so it needs
  updating by hand if the hardware chain changes.
- **`chart_chord_vs_single()`** in `scripts/make_charts.py` had two rendering defects: the chord
  annotation was offset below the axis floor and collided with the x tick label, and the four
  per-bar value labels were drawn just *below* each bar in white — invisible against the white
  background. Both now sit inside the bars. Only this chart changed visually.

### Added — PHASE 1 LOCKED: the silent ultrasonic pulse (2026-07-24)
The keep-alive pulse is now **inaudible and verified**, closing the core tension the whole project
existed to resolve. Locked parameters (`test-and-learn/FINDINGS.md`):
**30 000 Hz · −12 dBFS · 5 s · every 12 min**, via the FiiO KA11 in shared mode @ 384 kHz.
- **Detected:** woke the sub from standby in 3.0 s → 30 kHz unambiguously trips the auto-sense
  detector (which latches past 22 kHz with no measured cutoff).
- **Silent:** at −12 dBFS — the `MAX_LEVEL_DBFS` clamp maximum — nothing heard or felt, confirmed
  by ear. Works because the T-series U-ART tweeters all cap at 25 kHz.
- **Re-arms the timer** (coast method, 12 min coast): 2 s → 16.3 min hold, 5 s → 16.8 min.
  A candidate that did nothing would have held ~4–5 min, so the outcome is unambiguous.
  2 s already saturates; 5 s taken as free margin since length costs nothing when inaudible.
- **1-hour endurance run PASSED** — 4 consecutive 12-min cycles in the production regime, never
  slept, in a genuinely silent room. Raw data: `ultrasonic.csv`, `ultrasonic_endurance.csv`.
- Every audible candidate (chords −38, chimes −32, 40 Hz −24) is now a **fallback**, not the plan.

### Changed — standby timer corrected to ~16 min (2026-07-24)
- Three LED-timed measurements clocked from the **last signal** cluster at **15.85 / 16.3 / 16.8 min**.
  The long-standing "~17.5 min" figure was anchored from the *wake* event and silently included wake
  lag. Consequence: the locked **15 min** interval had ~1 min of real margin, not the assumed 2.5 —
  so the interval is revised to **12 min** (~4 min margin), which is free now that cadence is no
  longer constrained by audibility.

### Changed — Hardware state (2026-07-24)
- **FiiO KA11 installed** — the fully-silent **>25 kHz ultrasonic** pulse is no longer blocked by
  the old 48 kHz dongle. `device-info` reports a **384 kHz shared-mode default** and exclusive
  support at every rate 44.1k–384k, so **exclusive mode is not needed**: shared mode inherits the
  384 kHz device rate (Nyquist 192 kHz) and never blocks other apps. Runtime config repointed to
  `"Headphones (FIIO KA11)"` with the 30 kHz / −12 dBFS / 1.0 s candidate; not yet fired.
- Known gaps surfaced by that probe: no Nyquist guard in the shared-mode path (a 30 kHz pulse would
  fold to an audible 18 kHz if Windows dropped the device to 48 kHz), and `freq-hz` is clamped to
  30 kHz in `config.rs`, which caps testing above that.
- **Webcam moved → ROI re-aimed to `751,133,100,100`** (from `387,198,100,100`, which the move had
  left pointing at the cabinet instead of the LED). Validated against the standby LED: RED, ~160 px,
  hue 174, sat 239.
- **`led_watcher.py autoroi`** — new subcommand that locates the LED as the largest saturated
  red/green blob and prints a ready-to-paste `--roi` plus an annotated frame. A stale ROI fails
  silently (every trial reads as a failed wake), and the camera has now drifted twice, so finding it
  is no longer a manual pixel-eyeballing job.

### Added — Public release prep (2026-07-18)
- **`docs/RESEARCH.md`** — writeup of the detector reverse-engineering: the webcam/LED measurement
  rig, the coast-then-pulse methodology trap (keep-alive threshold ≪ wake threshold), the detector
  frequency response, and the chord-vs-single-tone and hold-vs-level results.
- **`docs/charts/`** — four charts (PNG + SVG): detector frequency response, hold-vs-level cliff,
  chord vs. single tone, wake/hold erraticness — regenerated from the raw CSVs by
  **`scripts/make_charts.py`** (matplotlib/numpy).
- **Raw campaign data committed** — `test-and-learn/{phase1_widefreq,phase2_floor,phase4_chord,
  phase6_highfreq,detector_floor,search,definitive,endurance}.csv`, so every chart is reproducible.
- **MIT `LICENSE`**.
- **`docs/PORTABILITY.md`** — per-module platform-boundary map (portable core vs. Windows-only
  audio/tray/volume layers) with a "macOS/Linux backends welcome, test on hardware" note; README
  gained a matching **Platform support** section.
- **`README.md`** — problem statement, signal-chain diagram, build/run, subcommands, full config
  table, and the −12 dBFS / 25 %-volume safety note.

### Changed — Public release prep (2026-07-18)
- Repo trimmed for publication: `.gitignore` now excludes webcam frames (`*.jpg`), churning logs
  (`*.log`), `led_holds_buggy.csv`, `__pycache__/`, and keeps AI/session docs local-only
  (`HANDOFF.md`, `docs/superpowers/`, and later `PROMOTION.md`) — those files still exist on disk,
  they are simply no longer tracked.
- `test-and-learn/led_watcher.py` — minor cleanup as part of the same pass.

### Added — Phase 2 production app (2026-07-15)
- **Generalized pulse config**: `freqs` (chord), `decay-s` (chime), `exclusive` + `sample-rate`
  (ultrasonic hi-res) with an `effective_freqs()` accessor and widened clamps; keeps the −12 dBFS cap.
- **`scheduler::fire()`** routes shared vs exclusive output from config (tone/chord/chime/ultrasonic).
- **`monitor.rs`** — Mode-A WASAPI-loopback RMS meter + hysteresis `SilenceGate` (unit-tested);
  engine now gates firing on real silence in Mode A, falling back to Mode-B timing on capture failure.
- **`volume.rs`** — pins the output device's Windows volume to `device-volume-pct` on launch
  (IAudioEndpointVolume); round-trip verified. `device-info` now reports current volume.
- **`settings_ui.rs`** — native settings window (all config fields) replacing "Configure → Notepad";
  runs on its own message-loop thread. New `settings` subcommand opens it directly.
- Chord/chime/exclusive synthesis in the `pulse` CLI (`--freqs`, `--decay`, `--exclusive --rate`);
  `device-info` exclusive-rate probe. Default mode flipped to **Always-On** (silent-ultrasonic winner).
- Fixed a latent tray-icon crash (needs nwg `image-decoder`). 10 unit tests; release build verified.
- Design + plan under `docs/superpowers/{specs,plans}/2026-07-15-*`.

### Added
- Initial project scaffold and `CLAUDE.md` documenting the problem, two-phase scope
  (Test & Learn → Production Build), Voicemeeter signal chain, and technical approach.
- `test-and-learn/keepawake_test.py` — tone generator/player with single-shot and `--hold`/`--gui`
  keep-alive loop; GUI indicator now lit for the full pulse duration.
- `test-and-learn/FINDINGS.md` — experiment log (audibility ladder, integration-time insight,
  the analog-knob constraint, XLR/RCA summing) and the settled Track B design.
- `HANDOFF.md` — resume document for picking the project back up once the USB dongle arrives.

### Changed
- **Pivoted from software Track A to hardware Track B.** Testing showed the iD4 volume knob
  (analog, post-DAC, position not reported to the PC) makes any fixed digital tone level
  unreliable across listening volumes. New approach: a dedicated USB dongle feeds an inaudible
  fixed-level pulse into the sub's spare RCA input, bypassing the knob entirely.
- Project **paused** pending hardware (USB dongle + 3.5mm→RCA cable).
