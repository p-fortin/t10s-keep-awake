//! Persistent configuration, stored as TOML in `%APPDATA%\T10sKeepAwake\config.toml`.
//!
//! Defaults are the locked parameters from the measurement campaign (see `docs/RESEARCH.md`):
//! a 30 kHz ultrasonic pulse on the isolated dongle path — detected by the sub's auto-sense but
//! above the T-series 25 kHz reproduction ceiling, so it is inaudible in the room.

use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// Hard safety ceiling on pulse level. The dongle drives the sub hard (at 100% device volume,
/// -20 dBFS "shook the house"); this clamp means no config/typo can ever emit above it.
pub const MAX_LEVEL_DBFS: f32 = -12.0;

/// App folder name under %APPDATA%.
const APP_DIR: &str = "T10sKeepAwake";
const CONFIG_FILE: &str = "config.toml";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Mode {
    /// Fire on a fixed interval regardless of what's playing. What ships: the pulse is
    /// inaudible, so there is nothing to gain by avoiding overlap with music.
    AlwaysOn,
    /// Only fire after sustained silence, letting real audio hold the sub awake instead. Kept for
    /// anyone running an *audible* fallback pulse; not exposed in the settings window.
    AudioSensing,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct Config {
    /// Whether the keep-awake pulsing is currently enabled (tray toggle).
    pub enabled: bool,
    /// Operating mode.
    pub mode: Mode,
    /// Device the pulse plays to (WASAPI name), e.g. "Speakers (USB Audio Device)".
    pub output_device: String,
    /// Device `audio-sensing` listens to via loopback. None = default output.
    pub monitor_device: Option<String>,
    /// Minutes between pulses (clamped to 5..=16; measured ~16 min standby timer).
    pub interval_min: f32,
    /// Pulse frequency in Hz (single tone; used when `freqs` is empty).
    pub freq_hz: f32,
    /// Chord: frequencies summed into one pulse. Empty = single tone at `freq_hz`.
    pub freqs: Vec<f32>,
    /// Exponential decay time constant (s). 0 = flat sustain; >0 = chime/bell.
    pub decay_s: f32,
    /// Emit in WASAPI exclusive mode, negotiating the rate with the driver so the pulse is
    /// unaffected by whatever Windows sets the endpoint's shared format to.
    pub exclusive: bool,
    /// Exclusive-mode sample rate (Hz): 48000 / 96000 / 192000 / 384000.
    pub sample_rate: u32,
    /// Pulse level in dBFS (<= 0, clamped to MAX_LEVEL_DBFS).
    pub level_dbfs: f32,
    /// Pulse duration in seconds (short "blip").
    pub duration_s: f32,
    /// Raised-cosine fade in/out, milliseconds (anti-click).
    pub fade_ms: f32,
    /// Windows volume (%) the app holds the output device at; re-applied while running.
    pub device_volume_pct: u8,
    /// `audio-sensing`: RMS level (dBFS) below which the monitor path counts as "silent".
    pub silence_threshold_dbfs: f32,
    /// Start with Windows (HKCU Run).
    pub autostart: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            enabled: true,
            mode: Mode::AlwaysOn, // silent ultrasonic winner needs no silence-gating; simplest + correct
            output_device: "Headphones (FIIO KA11)".to_string(),
            monitor_device: None,
            // Measured standby timer is ~16 min (not the long-assumed 17.5), so 12 leaves ~4 min.
            interval_min: 12.0,
            freq_hz: 30_000.0, // ultrasonic: detected by the sub, above the T-series 25 kHz ceiling
            freqs: vec![],
            decay_s: 0.0,
            // Exclusive mode: the pulse negotiates its rate straight with the driver, so it is
            // immune to Windows changing the endpoint's shared "Default Format" (which cannot be
            // reliably changed back from user space — the property write is accepted and ignored).
            // Blocking the device is fine here precisely because it is dedicated to the pulse.
            exclusive: true,
            sample_rate: 384_000,
            level_dbfs: MAX_LEVEL_DBFS, // -12: inaudible even at the safety-clamp maximum
            // 2 s already saturates the timer; 5 s is free margin because nothing is audible.
            duration_s: 5.0,
            fade_ms: 15.0,
            device_volume_pct: 25,
            silence_threshold_dbfs: -50.0,
            autostart: false,
        }
    }
}

impl Config {
    /// `%APPDATA%\T10sKeepAwake\`.
    pub fn dir() -> Result<PathBuf> {
        let base = dirs::config_dir().context("no %APPDATA% / config dir")?;
        Ok(base.join(APP_DIR))
    }

    pub fn path() -> Result<PathBuf> {
        Ok(Self::dir()?.join(CONFIG_FILE))
    }

    /// Load config, or write and return defaults if none exists.
    pub fn load_or_default() -> Result<Config> {
        let path = Self::path()?;
        if path.exists() {
            let text = fs::read_to_string(&path)
                .with_context(|| format!("reading {}", path.display()))?;
            let mut cfg: Config = toml::from_str(&text)
                .with_context(|| format!("parsing {}", path.display()))?;
            cfg.sanitize();
            Ok(cfg)
        } else {
            let cfg = Config::default();
            cfg.save()?;
            Ok(cfg)
        }
    }

    pub fn save(&self) -> Result<()> {
        let dir = Self::dir()?;
        fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
        let text = toml::to_string_pretty(self).context("serializing config")?;
        fs::write(Self::path()?, text).context("writing config")?;
        Ok(())
    }

    /// The frequencies to sum for one pulse: `freqs` if set, else the single `freq_hz`.
    pub fn effective_freqs(&self) -> Vec<f32> {
        if self.freqs.is_empty() {
            vec![self.freq_hz]
        } else {
            self.freqs.clone()
        }
    }

    /// Clamp all values into safe/valid ranges. Always call after loading or editing.
    pub fn sanitize(&mut self) {
        self.interval_min = self.interval_min.clamp(5.0, 16.0);
        self.freq_hz = self.freq_hz.clamp(20.0, 30_000.0);
        for f in self.freqs.iter_mut() {
            *f = f.clamp(20.0, 30_000.0);
        }
        self.level_dbfs = self.level_dbfs.clamp(-90.0, MAX_LEVEL_DBFS); // never hotter than the clamp
        self.duration_s = self.duration_s.clamp(0.03, 8.0);
        self.fade_ms = self.fade_ms.clamp(2.0, 200.0);
        self.decay_s = self.decay_s.clamp(0.0, 5.0);
        self.sample_rate = match self.sample_rate {
            r if r >= 288_000 => 384_000, // the KA11 supports 384k; don't silently snap it down
            r if r >= 144_000 => 192_000,
            r if r >= 72_000 => 96_000,
            r if r >= 36_000 => 48_000,
            _ => 96_000, // too-low/garbage -> hi-res default (sample_rate only matters for exclusive)
        };
        self.device_volume_pct = self.device_volume_pct.min(100);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn effective_freqs_falls_back_to_freq_hz_when_empty() {
        let mut c = Config::default();
        c.freqs = vec![];
        c.freq_hz = 40.0;
        assert_eq!(c.effective_freqs(), vec![40.0]);
    }

    #[test]
    fn effective_freqs_uses_chord_when_present() {
        let mut c = Config::default();
        c.freqs = vec![50.0, 250.0, 500.0];
        assert_eq!(c.effective_freqs(), vec![50.0, 250.0, 500.0]);
    }

    #[test]
    fn sanitize_clamps_level_to_safety_ceiling() {
        let mut c = Config::default();
        c.level_dbfs = 0.0; // hotter than -12
        c.sanitize();
        assert_eq!(c.level_dbfs, MAX_LEVEL_DBFS);
    }

    #[test]
    fn sanitize_clamps_new_fields() {
        let mut c = Config::default();
        c.freqs = vec![10.0, 40000.0]; // below 20, above 30k
        c.decay_s = 99.0;
        c.sample_rate = 12345; // not a supported rate
        c.sanitize();
        assert_eq!(c.freqs, vec![20.0, 30000.0]);
        assert!((c.decay_s - 5.0).abs() < 1e-6);
        assert_eq!(c.sample_rate, 96000); // snapped to nearest supported
    }

    #[test]
    fn old_config_without_freqs_migrates_via_effective_freqs() {
        let toml_text = "enabled = true\nfreq-hz = 55.0\n";
        let mut c: Config = toml::from_str(toml_text).unwrap();
        c.sanitize();
        assert_eq!(c.effective_freqs(), vec![55.0]);
    }

    #[test]
    fn default_mode_is_always_on() {
        assert_eq!(Config::default().mode, Mode::AlwaysOn);
    }
}
