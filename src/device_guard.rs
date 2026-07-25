//! Keep the dedicated keep-awake device configured the way the app needs it.
//!
//! The keep-awake output is meant to be a **dedicated** path — nothing else plays through it — so
//! the app is entitled to enforce its settings rather than politely give up when something else
//! changes them. Two things drift in practice:
//!
//! * **Volume.** Anyone (or any app) can move the endpoint slider. Previously the pin only ran at
//!   launch, so a mid-session change stuck until restart.
//! * **Shared-mode format.** Windows can reset the endpoint to 44.1/48 kHz — driver update,
//!   "enhancement" toggling, a helpful utility. A shared-mode 30 kHz pulse then sits above Nyquist,
//!   so `audio::check_nyquist` refuses to emit rather than blast an audible alias.
//!
//!   Rewriting `PKEY_AudioEngine_DeviceFormat` to repair that **does not work** from user space:
//!   SetValue/Commit both succeed and Windows ignores the value until the endpoint is
//!   re-initialised (which is why format-changer tools disable and re-enable the device). Measured
//!   2026-07-25: the write reported success on every 20 s cycle while the device stayed at
//!   44.1 kHz. The real fix is not to depend on the shared format at all — the pulse runs in
//!   **exclusive mode**, negotiating its rate with the driver directly. The repair below is kept
//!   only for the shared-mode fallback, and now verifies instead of assuming it worked.
//!
//! `enforce()` repairs both before a pulse. Everything here is best-effort: a failure is reported,
//! never fatal, because a degraded pulse attempt is still better than no attempt.

use anyhow::{Result, anyhow};
use windows::Win32::Media::Audio::{
    PKEY_AudioEngine_DeviceFormat, WAVEFORMATEX, WAVEFORMATEXTENSIBLE, WAVEFORMATEXTENSIBLE_0,
};
use windows::Win32::Media::Multimedia::KSDATAFORMAT_SUBTYPE_IEEE_FLOAT;
use windows::Win32::System::Com::{STGM_WRITE, StructuredStorage::PROPVARIANT};
use windows::Win32::UI::Shell::PropertiesSystem::IPropertyStore;

use crate::volume;

/// Minimum stream rate needed to emit `freq_hz` without aliasing, plus headroom.
///
/// Nyquist alone (2x) is the hard wall; the extra margin keeps us off formats that only *just*
/// clear it, where a reconstruction filter would attenuate the tone into uselessness.
pub fn required_rate(freq_hz: f32) -> u32 {
    ((freq_hz * 2.2).ceil() as u32).max(48_000)
}

/// Is `rate` usable for a pulse at `freq_hz`?
pub fn rate_is_usable(rate: u32, freq_hz: f32) -> bool {
    rate as f32 > freq_hz * 2.0
}

/// How a device measures up for the configured pulse — drives the settings-window readout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Capability {
    /// Runs fast enough for a silent ultrasonic pulse.
    Ready { rate: u32 },
    /// Too slow: the pulse would alias into the audible band, so the app will refuse to emit.
    TooSlow { rate: u32, needed: u32 },
    /// Couldn't be queried (unplugged, renamed, in use).
    Unknown { why: String },
}

impl Capability {
    /// One-line description shown under the device picker.
    pub fn describe(&self) -> String {
        match self {
            Capability::Ready { rate } => {
                format!("{} kHz \u{b7} Ultrasonic Ready", rate / 1000)
            }
            Capability::TooSlow { rate, needed } => format!(
                "{} kHz \u{b7} Too Slow \u{2014} needs {} kHz or higher (a hi-res DAC)",
                rate / 1000,
                needed / 1000
            ),
            Capability::Unknown { why } => format!("Device Unavailable \u{2014} {why}"),
        }
    }

    pub fn is_ready(&self) -> bool {
        matches!(self, Capability::Ready { .. })
    }
}

/// Classify a device for the configured pulse frequency.
pub fn capability(device_name: &str, freq_hz: f32) -> Capability {
    match crate::audio::device_sample_rate_info(Some(device_name)) {
        Ok((default_rate, max_rate)) => {
            // The shared stream runs at the *default* rate; the max is what a repair could reach.
            if rate_is_usable(default_rate, freq_hz) {
                Capability::Ready { rate: default_rate }
            } else {
                Capability::TooSlow {
                    rate: default_rate,
                    needed: required_rate(freq_hz).min(max_rate.max(required_rate(freq_hz))),
                }
            }
        }
        Err(e) => Capability::Unknown {
            why: format!("{e:#}"),
        },
    }
}

/// What `enforce()` actually did, so the caller can log/surface it.
#[derive(Debug, Default)]
pub struct Report {
    pub volume_repinned: bool,
    pub format_repaired: bool,
    pub problems: Vec<String>,
}

impl Report {
    pub fn summary(&self) -> Option<String> {
        if self.problems.is_empty() {
            return None;
        }
        Some(self.problems.join("; "))
    }
}

/// Re-pin volume and, if the endpoint's shared format is too slow for `freq_hz`, rewrite it.
///
/// Called before each pulse. Never returns Err: problems land in `Report::problems` so one bad
/// step can't stop the pulse attempt that follows.
pub fn enforce(device_name: &str, volume_pct: u8, freq_hz: f32, exclusive: bool) -> Report {
    let mut report = Report::default();
    if device_name.trim().is_empty() {
        return report;
    }

    match volume::set_endpoint_volume_percent(device_name, volume_pct) {
        Ok(()) => report.volume_repinned = true,
        Err(e) => report.problems.push(format!("volume pin failed: {e:#}")),
    }

    // In exclusive mode the pulse negotiates its rate straight with the driver, so the shared
    // "Default Format" is irrelevant — and trying to rewrite it is pointless: Windows accepts the
    // property write and ignores it unless the endpoint is re-initialised (which needs the device
    // to be disabled and re-enabled). Don't retry something that provably doesn't take.
    if exclusive {
        return report;
    }

    match crate::audio::device_sample_rate_info(Some(device_name)) {
        Ok((current, max)) if !rate_is_usable(current, freq_hz) => {
            let want = pick_repair_rate(max, freq_hz);
            match want {
                Some(rate) => match set_shared_format_rate(device_name, rate) {
                    // SetValue/Commit returning Ok does NOT mean Windows adopted the format, so
                    // read the endpoint back before claiming anything. Reporting an unverified
                    // "repaired" is worse than reporting a failure: it hides a dead keep-alive.
                    Ok(()) => match crate::audio::device_sample_rate_info(Some(device_name)) {
                        Ok((now, _)) if rate_is_usable(now, freq_hz) => {
                            report.format_repaired = true;
                            tracing::info!(
                                device = device_name,
                                from = current,
                                to = now,
                                "repaired endpoint shared format"
                            );
                        }
                        Ok((now, _)) => report.problems.push(format!(
                            "format repair did not take: asked for {rate} Hz, device still reports \
                             {now} Hz - set it manually in Windows Sound settings"
                        )),
                        Err(e) => report
                            .problems
                            .push(format!("could not verify format repair: {e:#}")),
                    },
                    Err(e) => report
                        .problems
                        .push(format!("format repair to {rate} Hz failed: {e:#}")),
                },
                None => report.problems.push(format!(
                    "device tops out at {max} Hz, too slow for a {freq_hz:.0} Hz silent pulse"
                )),
            }
        }
        Ok(_) => {}
        Err(e) => report.problems.push(format!("rate query failed: {e:#}")),
    }
    report
}

/// Highest sensible rate to repair to: the device's max, provided it clears Nyquist for `freq_hz`.
pub fn pick_repair_rate(max_rate: u32, freq_hz: f32) -> Option<u32> {
    if rate_is_usable(max_rate, freq_hz) {
        Some(max_rate)
    } else {
        None
    }
}

/// Rewrite the endpoint's shared-mode format (what the Sound control panel calls "Default Format")
/// to 2-channel 32-bit float at `rate`.
///
/// This is the `PKEY_AudioEngine_DeviceFormat` property that the audio engine reads when it opens
/// the shared stream. The value is a VT_BLOB holding a `WAVEFORMATEX`.
fn set_shared_format_rate(device_name: &str, rate: u32) -> Result<()> {
    let dev = volume::immdevice_by_name(device_name)?;
    let channels: u16 = 2;
    let bits: u16 = 32;
    let block_align = channels * bits / 8;
    // WAVEFORMATEXTENSIBLE, not a bare WAVEFORMATEX: the audio engine stores the endpoint's
    // "Default Format" in extensible form, and silently ignores a plain 18-byte float header —
    // which is exactly what happened before, with SetValue/Commit both returning success while
    // the device stayed at 44.1 kHz.
    let wfx = WAVEFORMATEXTENSIBLE {
        Format: WAVEFORMATEX {
            wFormatTag: 0xFFFE, // WAVE_FORMAT_EXTENSIBLE
            nChannels: channels,
            nSamplesPerSec: rate,
            nAvgBytesPerSec: rate * block_align as u32,
            nBlockAlign: block_align,
            wBitsPerSample: bits,
            cbSize: 22,
        },
        Samples: WAVEFORMATEXTENSIBLE_0 {
            wValidBitsPerSample: bits,
        },
        dwChannelMask: 0x3, // FRONT_LEFT | FRONT_RIGHT
        SubFormat: KSDATAFORMAT_SUBTYPE_IEEE_FLOAT,
    };
    let bytes = unsafe {
        std::slice::from_raw_parts(
            (&wfx as *const WAVEFORMATEXTENSIBLE) as *const u8,
            std::mem::size_of::<WAVEFORMATEXTENSIBLE>(),
        )
    };

    unsafe {
        let store: IPropertyStore = dev
            .OpenPropertyStore(STGM_WRITE)
            .map_err(|e| anyhow!("OpenPropertyStore(WRITE): {e:?}"))?;
        let pv = blob_propvariant(bytes)?;
        store
            .SetValue(&PKEY_AudioEngine_DeviceFormat, &pv)
            .map_err(|e| anyhow!("SetValue(DeviceFormat): {e:?}"))?;
        store
            .Commit()
            .map_err(|e| anyhow!("PropertyStore::Commit: {e:?}"))?;
    }
    Ok(())
}

/// Build a VT_BLOB `PROPVARIANT` owning a CoTaskMem copy of `bytes`.
///
/// The blob must be CoTaskMem-allocated: `PropVariantClear` frees it with `CoTaskMemFree`, so
/// handing it stack or Rust-heap memory would corrupt the heap when the variant is cleared.
/// Written through the crate's real union arms rather than a hand-rolled layout mirror, so the
/// compiler checks the field offsets instead of us hoping we got them right.
unsafe fn blob_propvariant(bytes: &[u8]) -> Result<PROPVARIANT> {
    use windows::Win32::System::Com::{BLOB, CoTaskMemAlloc};
    use windows::Win32::System::Variant::VT_BLOB;

    let buf = unsafe { CoTaskMemAlloc(bytes.len()) } as *mut u8;
    if buf.is_null() {
        return Err(anyhow!("CoTaskMemAlloc({}) failed", bytes.len()));
    }
    unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), buf, bytes.len()) };

    let mut pv = PROPVARIANT::default();
    unsafe {
        let inner = &mut *pv.Anonymous.Anonymous;
        inner.vt = VT_BLOB;
        inner.Anonymous.blob = BLOB {
            cbSize: bytes.len() as u32,
            pBlobData: buf,
        };
    }
    Ok(pv)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nyquist_decides_usability() {
        // The locked 30 kHz pulse: fine at hi-res, impossible at 48 kHz.
        assert!(rate_is_usable(384_000, 30_000.0));
        assert!(rate_is_usable(96_000, 30_000.0));
        assert!(!rate_is_usable(48_000, 30_000.0));
        // Exactly 2x is NOT usable — a tone sitting on Nyquist is not reproducible.
        assert!(!rate_is_usable(60_000, 30_000.0));
    }

    #[test]
    fn required_rate_has_headroom_over_bare_nyquist() {
        assert!(required_rate(30_000.0) > 60_000);
        // Never proposes something below the universal 48 kHz baseline for low frequencies.
        assert_eq!(required_rate(50.0), 48_000);
    }

    #[test]
    fn repair_target_is_the_device_max_when_it_clears_nyquist() {
        assert_eq!(pick_repair_rate(384_000, 30_000.0), Some(384_000));
        assert_eq!(pick_repair_rate(96_000, 30_000.0), Some(96_000));
        // A device that simply cannot do it yields None rather than a bogus repair attempt.
        assert_eq!(pick_repair_rate(48_000, 30_000.0), None);
    }

    #[test]
    fn capability_descriptions_are_human_readable() {
        assert_eq!(
            Capability::Ready { rate: 384_000 }.describe(),
            "384 kHz \u{b7} Ultrasonic Ready"
        );
        let slow = Capability::TooSlow {
            rate: 48_000,
            needed: 96_000,
        };
        assert!(slow.describe().contains("Too Slow"));
        assert!(!slow.is_ready());
    }
}
