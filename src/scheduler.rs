//! The keep-awake engine: on a background thread, fires the configured pulse while enabled.
//!
//! `always-on` fires on a fixed interval and is what ships. `audio-sensing` instead waits for
//! sustained silence (see `monitor.rs`), which is only useful with an audible fallback pulse.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use crate::audio;
use crate::audio_exclusive;
use crate::config::{Config, MAX_LEVEL_DBFS, Mode};
use crate::device_guard;
use crate::health;
use crate::monitor;

/// Sleep for `dur`, waking early if `stop` is set. Returns true if stopped.
fn sleep_interruptible(dur: Duration, stop: &AtomicBool) -> bool {
    let step = Duration::from_millis(250);
    let mut left = dur;
    while left > Duration::ZERO {
        if stop.load(Ordering::Relaxed) {
            return true;
        }
        let chunk = left.min(step);
        std::thread::sleep(chunk);
        left = left.saturating_sub(chunk);
    }
    stop.load(Ordering::Relaxed)
}

/// Fire one pulse from the current config snapshot. Logs success/failure; never panics.
/// Also records the outcome in `health` so a failing pulse reaches the tray, not just the log.
fn fire(cfg: &Config, health: &health::Shared) {
    let device = if cfg.output_device.trim().is_empty() {
        None
    } else {
        Some(cfg.output_device.as_str())
    };
    // Defense in depth: never exceed the safety ceiling even if config somehow slipped past.
    let level = cfg.level_dbfs.min(MAX_LEVEL_DBFS);
    let freqs = cfg.effective_freqs();

    // The keep-awake output is a dedicated device, so re-assert our settings on it before every
    // pulse: a moved volume slider or a Windows-reset sample rate would otherwise quietly defeat
    // the pulse. Best-effort — we still attempt the pulse if this couldn't fix everything.
    if let Some(dev) = device {
        let top_freq = freqs.iter().cloned().fold(0.0f32, f32::max);
        let report = device_guard::enforce(dev, cfg.device_volume_pct, top_freq, cfg.exclusive);
        if let Some(problems) = report.summary() {
            tracing::warn!(device = dev, %problems, "device enforcement incomplete");
        }
    }
    let result = if cfg.exclusive {
        match device {
            Some(dev) => {
                let excl = audio_exclusive::play_pulse_exclusive(
                    dev, &freqs, level, cfg.duration_s, cfg.sample_rate, cfg.fade_ms,
                );
                // Exclusive can be refused (another app holds the device, or "allow exclusive
                // control" is off). Shared still works whenever Windows has the endpoint at a
                // hi-res format, so try it rather than skipping the pulse outright.
                match excl {
                    Ok(()) => Ok(()),
                    Err(e) => {
                        tracing::warn!(error = %format!("{e:#}"), "exclusive pulse refused; trying shared");
                        audio::play_pulse(
                            device, &freqs, level, cfg.duration_s, cfg.fade_ms, cfg.decay_s, false,
                        )
                    }
                }
            }
            None => Err(anyhow::anyhow!("exclusive mode requires an output device")),
        }
    } else {
        audio::play_pulse(device, &freqs, level, cfg.duration_s, cfg.fade_ms, cfg.decay_s, false)
    };
    match result {
        Ok(()) => {
            tracing::info!(
                ?freqs,
                dbfs = level,
                dur = cfg.duration_s,
                exclusive = cfg.exclusive,
                "pulse fired"
            );
            health.record_ok();
        }
        Err(e) => {
            let msg = format!("{e:#}");
            tracing::warn!(error = %msg, "pulse failed");
            health.record_err(msg);
        }
    }
}

/// Fire one pulse right now, off the engine's schedule — the tray's "Send test pulse" and the
/// settings window's Test button. Blocks for the pulse duration, so call it on a worker thread.
pub fn fire_once(cfg: &Config, health: &health::Shared) {
    fire(cfg, health);
}

/// Adopt `config.toml` if another process changed it.
///
/// The in-process settings window edits the shared `Config` directly, but the standalone
/// `settings` subcommand is a separate process and can only write the file — without this, its
/// edits would be invisible until the tray app restarted.
fn reload_config_from_disk(cfg: &Arc<Mutex<Config>>) {
    let Ok(path) = Config::path() else { return };
    let Ok(text) = std::fs::read_to_string(&path) else {
        return;
    };
    let Ok(mut on_disk) = toml::from_str::<Config>(&text) else {
        return;
    };
    on_disk.sanitize();
    let mut cur = cfg.lock().unwrap();
    // Compare the fields a user can actually change; everything else is derived or locked.
    let changed = cur.enabled != on_disk.enabled
        || cur.interval_min != on_disk.interval_min
        || cur.output_device != on_disk.output_device
        || cur.autostart != on_disk.autostart;
    if changed {
        tracing::info!(
            enabled = on_disk.enabled,
            interval = on_disk.interval_min,
            device = %on_disk.output_device,
            "config.toml changed on disk; adopted"
        );
        *cur = on_disk;
    }
}

/// Run the engine loop until `stop` is set. Reads a fresh config snapshot each cycle so live
/// edits from the settings window take effect on the next pulse.
pub fn run(cfg: Arc<Mutex<Config>>, stop: Arc<AtomicBool>, health: health::Shared) {
    tracing::info!("engine started");
    // Tracks the enabled state we last acted on. Starting at `false` means the first enabled pass
    // through the loop fires immediately — which covers BOTH app start and being switched back on
    // from the tray or settings window. Without this, enabling only scheduled a pulse one full
    // interval away, leaving an already-sleeping sub asleep for another 12 minutes.
    let mut was_enabled = false;
    loop {
        let snapshot = cfg.lock().unwrap().clone();
        if !snapshot.enabled {
            if was_enabled {
                tracing::info!("keep-awake disabled");
                health.clear_next_due();
                was_enabled = false;
            }
            if sleep_interruptible(Duration::from_secs(2), &stop) {
                break;
            }
            continue;
        }
        if !was_enabled {
            tracing::info!("keep-awake enabled; firing immediately");
            fire(&snapshot, &health);
            was_enabled = true;
        }
        let stopped = match snapshot.mode {
            Mode::AlwaysOn => run_mode_b(&snapshot, &cfg, &stop, &health),
            Mode::AudioSensing => run_mode_a(&snapshot, &cfg, &stop, &health),
        };
        if stopped {
            break;
        }
    }
    tracing::info!("engine stopped");
}

/// One `always-on` cycle: wait out the interval, then fire. Returns true if stopped.
///
/// The wait is polled rather than one long sleep so a config change lands immediately: editing the
/// interval in the settings window re-targets the deadline (measured from the LAST pulse, not from
/// the edit), and disabling breaks out at once instead of after the remaining minutes.
fn run_mode_b(
    _snapshot: &Config,
    cfg: &Arc<Mutex<Config>>,
    stop: &AtomicBool,
    health: &health::Shared,
) -> bool {
    let last_pulse = std::time::Instant::now();
    let mut shown_interval = f32::NAN;
    let mut last_guard = std::time::Instant::now();
    let mut last_reload = std::time::Instant::now();
    loop {
        // Pick up edits made to config.toml by another process (the standalone `settings` window
        // writes the file, not this process's memory) so they don't wait for a restart.
        if last_reload.elapsed() >= Duration::from_secs(2) {
            last_reload = std::time::Instant::now();
            reload_config_from_disk(cfg);
        }

        let s = cfg.lock().unwrap().clone();
        if !s.enabled {
            return false; // outer loop notices and clears the schedule
        }

        // Re-assert the device between pulses too. Enforcing only at pulse time meant a format or
        // volume change made in Windows sat un-repaired for up to a whole interval.
        if last_guard.elapsed() >= Duration::from_secs(20) && !s.output_device.trim().is_empty() {
            last_guard = std::time::Instant::now();
            let top = s.effective_freqs().iter().cloned().fold(0.0f32, f32::max);
            let report =
                device_guard::enforce(&s.output_device, s.device_volume_pct, top, s.exclusive);
            if report.format_repaired {
                tracing::info!(device = %s.output_device, "device format drifted; repaired");
            }
            if let Some(problems) = report.summary() {
                tracing::warn!(%problems, "device enforcement incomplete");
            }
        }
        let interval = Duration::from_secs_f32((s.interval_min * 60.0).max(1.0));
        // Only rewrite the deadline when the interval actually changed, so the countdown isn't
        // nudged by a fraction every poll.
        if s.interval_min != shown_interval {
            if shown_interval.is_finite() {
                tracing::info!(
                    from = shown_interval,
                    to = s.interval_min,
                    "interval changed; next pulse re-targeted"
                );
            }
            shown_interval = s.interval_min;
            health.set_next_due_at(last_pulse + interval);
        }
        if last_pulse.elapsed() >= interval {
            fire(&s, health);
            return false;
        }
        if sleep_interruptible(Duration::from_millis(250), stop) {
            return true;
        }
    }
}

/// `audio-sensing`: poll the loopback RMS meter and fire only after sustained silence. Falls back
/// to interval timing if the meter can't open, so the sub is never left to sleep.
fn run_mode_a(snapshot: &Config, cfg: &Arc<Mutex<Config>>, stop: &AtomicBool, health: &health::Shared) -> bool {
    let mon_dev = snapshot.monitor_device.as_deref();
    if mon_dev == Some(snapshot.output_device.as_str()) {
        tracing::warn!("monitor device == output device; would self-capture. Using interval timing.");
        return run_mode_b(snapshot, cfg, stop, health);
    }
    let mut meter = match monitor::open_loopback_meter(mon_dev) {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!(error = %format!("{e:#}"), "loopback meter failed; using interval timing");
            return run_mode_b(snapshot, cfg, stop, health);
        }
    };
    let mut gate = monitor::SilenceGate::new(snapshot.silence_threshold_dbfs, 2.0, 0.5);
    let dt = 0.25_f32;
    loop {
        if stop.load(Ordering::Relaxed) {
            return true;
        }
        let s = cfg.lock().unwrap().clone();
        if !s.enabled || s.mode != Mode::AudioSensing {
            return false; // re-dispatch on enable/mode change
        }
        let rms = meter.read_rms_dbfs();
        let hold = (s.interval_min * 60.0).max(1.0);
        if gate.update(rms, dt, hold) {
            fire(&s, health);
        }
        std::thread::sleep(Duration::from_secs_f32(dt));
    }
}
