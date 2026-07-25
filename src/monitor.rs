//! Silence detection for `audio-sensing`: a WASAPI loopback RMS meter feeding a hysteresis gate that fires the
//! pulse only after sustained real silence. The pure decision logic (`rms_dbfs`, `SilenceGate`) is
//! unit-tested; the loopback capture (`LoopbackMeter`) is integration-verified.

use anyhow::Result;
use std::collections::VecDeque;
use wasapi::*;

/// RMS of interleaved f32 samples in dBFS, floored at -90.
pub fn rms_dbfs(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return -90.0;
    }
    let sum_sq: f64 = samples.iter().map(|&s| (s as f64) * (s as f64)).sum();
    let rms = (sum_sq / samples.len() as f64).sqrt() as f32;
    if rms <= 1e-9 {
        -90.0
    } else {
        (20.0 * rms.log10()).max(-90.0)
    }
}

/// Hysteresis gate: fires when RMS has stayed below the silence threshold long enough that the sub
/// would otherwise be sleeping. Audio above threshold (debounced) resets the silent timer.
pub struct SilenceGate {
    threshold: f32,
    silence_debounce_s: f32,
    audio_debounce_s: f32,
    silent_for: f32,   // seconds RMS has been below threshold (post-debounce)
    below_streak: f32, // seconds continuously below threshold
    above_streak: f32, // seconds continuously above threshold
}

impl SilenceGate {
    pub fn new(threshold: f32, silence_debounce_s: f32, audio_debounce_s: f32) -> Self {
        SilenceGate {
            threshold,
            silence_debounce_s,
            audio_debounce_s,
            silent_for: 0.0,
            below_streak: 0.0,
            above_streak: 0.0,
        }
    }

    /// Feed the latest RMS over `dt_s`. Returns true (and resets the silent timer) when silence has
    /// held for `silent_hold_s`.
    pub fn update(&mut self, rms_dbfs: f32, dt_s: f32, silent_hold_s: f32) -> bool {
        if rms_dbfs > self.threshold {
            self.above_streak += dt_s;
            self.below_streak = 0.0;
            if self.above_streak >= self.audio_debounce_s {
                self.silent_for = 0.0; // real audio is holding the sub; restart the clock
            }
        } else {
            self.below_streak += dt_s;
            self.above_streak = 0.0;
            if self.below_streak >= self.silence_debounce_s {
                self.silent_for += dt_s;
            }
        }
        if self.silent_for >= silent_hold_s {
            self.silent_for = 0.0;
            return true;
        }
        false
    }
}

/// A WASAPI loopback capture on a render endpoint, read in short RMS snapshots.
/// Keeps the AudioClient alive (dropping it stops the stream).
pub struct LoopbackMeter {
    _audio_client: AudioClient,
    capture_client: AudioCaptureClient,
    bytes_per_sample: usize,
    is_float: bool,
}

fn find_render_device(name: Option<&str>) -> Result<Device> {
    let e = DeviceEnumerator::new().map_err(|x| anyhow::anyhow!("enumerator: {x:?}"))?;
    if let Some(want) = name {
        for d in &e
            .get_device_collection(&Direction::Render)
            .map_err(|x| anyhow::anyhow!("collection: {x:?}"))?
        {
            let d = d.map_err(|x| anyhow::anyhow!("device: {x:?}"))?;
            if d.get_friendlyname().map_err(|x| anyhow::anyhow!("name: {x:?}"))? == want {
                return Ok(d);
            }
        }
        anyhow::bail!("monitor device not found: {want:?}");
    }
    e.get_default_device(&Direction::Render)
        .map_err(|x| anyhow::anyhow!("default render: {x:?}"))
}

/// Open a loopback meter on `device_name` (or the default render device). Loopback = get the render
/// device's client, initialize it for Capture in shared mode (the crate sets the LOOPBACK flag).
pub fn open_loopback_meter(device_name: Option<&str>) -> Result<LoopbackMeter> {
    let _ = initialize_mta();
    let device = find_render_device(device_name)?;
    let mut audio_client = device
        .get_iaudioclient()
        .map_err(|x| anyhow::anyhow!("get_iaudioclient: {x:?}"))?;
    let format = audio_client
        .get_mixformat()
        .map_err(|x| anyhow::anyhow!("get_mixformat: {x:?}"))?;
    let (_def, min_period) = audio_client
        .get_device_period()
        .map_err(|x| anyhow::anyhow!("get_device_period: {x:?}"))?;
    let mode = StreamMode::PollingShared {
        autoconvert: true,
        buffer_duration_hns: min_period,
    };
    audio_client
        .initialize_client(&format, &Direction::Capture, &mode)
        .map_err(|x| anyhow::anyhow!("init loopback: {x:?}"))?;
    let capture_client = audio_client
        .get_audiocaptureclient()
        .map_err(|x| anyhow::anyhow!("get_audiocaptureclient: {x:?}"))?;
    audio_client
        .start_stream()
        .map_err(|x| anyhow::anyhow!("start_stream: {x:?}"))?;
    let blockalign = format.get_blockalign() as usize;
    let channels = format.get_nchannels() as usize;
    let is_float = matches!(
        format.get_subformat().unwrap_or(SampleType::Float),
        SampleType::Float
    );
    Ok(LoopbackMeter {
        _audio_client: audio_client,
        capture_client,
        bytes_per_sample: (blockalign / channels.max(1)).max(1),
        is_float,
    })
}

impl LoopbackMeter {
    /// Drain available frames and return their RMS in dBFS (−90 if nothing captured this tick).
    pub fn read_rms_dbfs(&mut self) -> f32 {
        let mut queue: VecDeque<u8> = VecDeque::new();
        if self.capture_client.read_from_device_to_deque(&mut queue).is_err() {
            return -90.0;
        }
        if queue.is_empty() {
            return -90.0;
        }
        let bytes: Vec<u8> = queue.into_iter().collect();
        let bps = self.bytes_per_sample;
        let mut samples: Vec<f32> = Vec::with_capacity(bytes.len() / bps);
        for c in bytes.chunks_exact(bps) {
            let v = if self.is_float && bps == 4 {
                f32::from_le_bytes([c[0], c[1], c[2], c[3]])
            } else if !self.is_float && bps == 2 {
                i16::from_le_bytes([c[0], c[1]]) as f32 / 32768.0
            } else if !self.is_float && bps == 4 {
                (i32::from_le_bytes([c[0], c[1], c[2], c[3]]) as f64 / 2_147_483_648.0) as f32
            } else {
                0.0
            };
            samples.push(v);
        }
        rms_dbfs(&samples)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rms_of_silence_is_floor() {
        assert!(rms_dbfs(&[0.0; 512]) <= -89.0);
    }

    #[test]
    fn rms_of_full_scale_sine_is_about_minus_3() {
        let s: Vec<f32> = (0..48_000)
            .map(|n| (2.0 * std::f32::consts::PI * 1000.0 * n as f32 / 48_000.0).sin())
            .collect();
        let db = rms_dbfs(&s);
        assert!(db > -4.0 && db < -2.0, "sine RMS dBFS was {db}");
    }

    #[test]
    fn gate_fires_after_sustained_silence() {
        let mut g = SilenceGate::new(-50.0, 2.0, 0.5);
        let mut fired_at = None;
        for i in 0..(13 * 60) {
            if g.update(-70.0, 1.0, 12.0 * 60.0) {
                fired_at = Some(i);
                break;
            }
        }
        assert!(fired_at.is_some());
        let t = fired_at.unwrap();
        assert!(t >= 720 && t <= 726, "fired at {t}s");
    }

    #[test]
    fn gate_resets_when_audio_returns() {
        let mut g = SilenceGate::new(-50.0, 2.0, 0.5);
        for _ in 0..600 {
            g.update(-70.0, 1.0, 12.0 * 60.0);
        }
        for _ in 0..5 {
            g.update(-20.0, 1.0, 12.0 * 60.0);
        }
        let mut fired = false;
        for _ in 0..(11 * 60) {
            if g.update(-70.0, 1.0, 12.0 * 60.0) {
                fired = true;
                break;
            }
        }
        assert!(!fired, "gate fired too soon after audio interrupted silence");
    }
}
