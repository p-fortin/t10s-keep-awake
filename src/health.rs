//! Shared engine health, so a failing pulse becomes *visible* instead of only reaching the log.
//!
//! The engine can fail to emit for reasons the user cannot otherwise see: the output device
//! disappeared, or — the case this was built for — Windows moved the endpoint to a 48 kHz shared
//! format, at which point the Nyquist guard refuses the 30 kHz ultrasonic pulse rather than emit an
//! audible 18 kHz alias. That is the right call, but it silently stops protecting the sub.
//!
//! The engine thread writes here and pokes `on_change`; the tray thread reads it and reflects the
//! state in the icon and tooltip.

use std::sync::{Arc, Mutex};
use std::time::Instant;

/// Result of the most recent pulse attempt.
#[derive(Debug, Clone, Default)]
pub struct Health {
    /// `None` = last pulse succeeded. `Some(msg)` = it failed, with the reason.
    pub last_error: Option<String>,
    /// How many pulses have failed back-to-back (0 when healthy).
    pub consecutive_failures: u32,
    /// When the last pulse was attempted. `Instant`, and reported as "3 min ago" rather than a
    /// clock time — elapsed time needs no timezone conversion and reads better anyway.
    pub last_pulse: Option<Instant>,
    /// When the next pulse is due. `Instant` (not `SystemTime`) so a clock change can't make the
    /// countdown nonsense.
    pub next_due: Option<Instant>,
}

impl Health {
    pub fn is_failing(&self) -> bool {
        self.last_error.is_some()
    }

    /// `M:SS` until the next pulse. Seconds matter here: a display that only changes once a minute
    /// reads as frozen, which is indistinguishable from "the app has hung".
    pub fn countdown_to_next(&self) -> Option<String> {
        let due = self.next_due?;
        let left = due.checked_duration_since(Instant::now())?.as_secs();
        Some(format!("{}:{:02}", left / 60, left % 60))
    }

    /// How long ago the last pulse fired, phrased for display.
    pub fn last_pulse_ago(&self) -> Option<String> {
        let secs = self.last_pulse?.elapsed().as_secs();
        Some(match secs {
            0..=59 => "Just Now".to_string(),
            60..=119 => "1 Min Ago".to_string(),
            _ => format!("{} Min Ago", secs / 60),
        })
    }

    /// Headline + detail for the settings window's status band, e.g.
    /// `("Active", "last pulse 2 min ago  \u{b7}  next pulse in 11:47")`.
    pub fn status_parts(&self) -> (String, String) {
        if let Some(e) = &self.last_error {
            let brief: String = e.chars().take(110).collect();
            return ("Not Pulsing".to_string(), brief);
        }
        let mut detail = Vec::new();
        if let Some(ago) = self.last_pulse_ago() {
            detail.push(format!("Last Pulse {ago}"));
        }
        match self.countdown_to_next() {
            Some(c) => detail.push(format!("Next Pulse in {c}")),
            None if self.last_pulse.is_some() => detail.push("Next Pulse Due".to_string()),
            None => {}
        }
        ("Active".to_string(), detail.join("  \u{b7}  "))
    }

    /// One-line tray tooltip. Windows truncates tips around 127 chars, so keep it short.
    pub fn tooltip(&self) -> String {
        match &self.last_error {
            None => "T10S Keep-Awake \u{2014} Active".to_string(),
            Some(e) => {
                let brief: String = e.chars().take(80).collect();
                format!(
                    "T10S Keep-Awake \u{2014} NOT PULSING ({} failed): {brief}",
                    self.consecutive_failures
                )
            }
        }
    }
}

/// Health shared between the engine thread and the tray, plus a callback to wake the GUI thread.
#[derive(Clone)]
pub struct Shared {
    inner: Arc<Mutex<Health>>,
    on_change: Arc<dyn Fn() + Send + Sync>,
}

impl Shared {
    pub fn new(on_change: Arc<dyn Fn() + Send + Sync>) -> Self {
        Shared {
            inner: Arc::new(Mutex::new(Health::default())),
            on_change,
        }
    }

    pub fn snapshot(&self) -> Health {
        self.inner.lock().unwrap().clone()
    }

    /// Record a successful pulse. Only notifies on an actual state change, so a healthy engine
    /// never spams the GUI thread.
    pub fn record_ok(&self) {
        let changed = {
            let mut h = self.inner.lock().unwrap();
            let was_failing = h.is_failing();
            h.last_error = None;
            h.consecutive_failures = 0;
            h.last_pulse = Some(Instant::now());
            was_failing
        };
        if changed {
            (self.on_change)();
        }
    }

    /// Record a failed pulse. Always notifies: the failure count is part of the surfaced state.
    pub fn record_err(&self, msg: impl Into<String>) {
        {
            let mut h = self.inner.lock().unwrap();
            h.last_error = Some(msg.into());
            h.consecutive_failures = h.consecutive_failures.saturating_add(1);
            h.last_pulse = Some(Instant::now());
        }
        (self.on_change)();
    }

    /// Point the countdown at an absolute deadline. Used when the interval changes mid-wait: the
    /// next pulse is due `interval` after the LAST pulse, not after the edit.
    pub fn set_next_due_at(&self, at: Instant) {
        self.inner.lock().unwrap().next_due = Some(at);
    }

    /// Forget any scheduled pulse (the engine was disabled), so a stale countdown can't linger.
    pub fn clear_next_due(&self) {
        self.inner.lock().unwrap().next_due = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    fn shared() -> (Shared, Arc<AtomicUsize>) {
        let n = Arc::new(AtomicUsize::new(0));
        let n2 = n.clone();
        (
            Shared::new(Arc::new(move || {
                n2.fetch_add(1, Ordering::Relaxed);
            })),
            n,
        )
    }

    #[test]
    fn healthy_engine_does_not_repeatedly_wake_the_gui() {
        let (s, notifies) = shared();
        s.record_ok();
        s.record_ok();
        s.record_ok();
        assert_eq!(notifies.load(Ordering::Relaxed), 0, "no change => no notify");
        assert!(!s.snapshot().is_failing());
    }

    #[test]
    fn failures_accumulate_and_recovery_clears_them() {
        let (s, notifies) = shared();
        s.record_err("device format moved to 48 kHz");
        s.record_err("device format moved to 48 kHz");
        let h = s.snapshot();
        assert!(h.is_failing());
        assert_eq!(h.consecutive_failures, 2);
        assert!(h.tooltip().contains("NOT PULSING"));

        s.record_ok();
        let h = s.snapshot();
        assert!(!h.is_failing());
        assert_eq!(h.consecutive_failures, 0);
        assert!(h.tooltip().contains("Active"));
        // 2 failures + 1 recovery transition
        assert_eq!(notifies.load(Ordering::Relaxed), 3);
    }

    #[test]
    fn status_line_reads_as_plain_english() {
        let (s, _) = shared();
        s.set_next_due_at(Instant::now() + Duration::from_secs(7 * 60));
        s.record_ok();
        let (head, detail) = s.snapshot().status_parts();
        assert_eq!(head, "Active");
        assert!(detail.contains("Last Pulse Just Now"), "{detail}");
        // Seconds are shown, so the countdown visibly moves instead of sitting on whole minutes.
        assert!(detail.contains("Next Pulse in 6:5"), "{detail}");

        s.record_err("output device not found");
        let (head, detail) = s.snapshot().status_parts();
        assert_eq!(head, "Not Pulsing");
        assert!(detail.contains("output device not found"), "{detail}");
    }

    #[test]
    fn countdown_shows_seconds_and_stops_once_overdue() {
        let (s, _) = shared();
        s.set_next_due_at(Instant::now() + Duration::from_secs(90));
        let c = s.snapshot().countdown_to_next().unwrap();
        assert!(c.starts_with("1:2"), "90s should read as 1:2x, got {c}");
        // An elapsed deadline reports None rather than underflowing into a huge number.
        let h = Health {
            next_due: Instant::now().checked_sub(Duration::from_secs(30)),
            ..Default::default()
        };
        assert_eq!(h.countdown_to_next(), None);
    }
}
