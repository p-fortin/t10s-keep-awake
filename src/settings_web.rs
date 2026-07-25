//! The settings window, as a local HTML page rendered by Edge WebView2 (via `wry`).
//!
//! The native Win32 form this replaces couldn't be made to look like anything but a native Win32
//! form — no rounded controls, no accent colours, no coloured text, no animation. The page in
//! `assets/settings.html` gets all of that for free, and the Rust side here is reduced to a small
//! JSON bridge.
//!
//! Threading: the window runs its own `tao` event loop on a worker thread, which needs
//! `with_any_thread(true)` on Windows, because the main thread already belongs to the tray's nwg
//! message loop. The event loop owns the `WebView`; the IPC handler can't (it is built first), so
//! requests are forwarded to the loop as user events and answered there with `evaluate_script`.
//!
//! Anything that fails here falls back to the native window in `settings_ui`, so a machine without
//! the WebView2 runtime still has working settings.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tao::event::{Event, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tao::platform::run_return::EventLoopExtRunReturn;
use tao::platform::windows::EventLoopBuilderExtWindows;
use tao::window::WindowBuilder;
use wry::WebViewBuilder;

use crate::config::Config;
use crate::{audio, autostart, device_guard, health, volume};

/// Intervals offered in the picker. The measured standby timer is ~16 min, so the list stops at 14.
const INTERVALS: [u32; 5] = [10, 11, 12, 13, 14];

/// Only one settings window at a time — a second event loop on another thread would work, but two
/// windows editing the same config is just a way to lose edits.
static OPEN: AtomicBool = AtomicBool::new(false);

enum UserEvent {
    /// A JSON message from the page.
    Ipc(String),
    /// The 500 ms status tick.
    Tick,
}

/// Open the settings window. Returns `false` if the WebView couldn't be created (no WebView2
/// runtime, most likely), so the caller can fall back to the native window.
pub fn open(cfg: Arc<Mutex<Config>>, health: health::Shared, live: bool) -> bool {
    if OPEN.swap(true, Ordering::SeqCst) {
        return true; // already showing; nothing to do
    }
    let ok = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run(cfg, health, live)));
    OPEN.store(false, Ordering::SeqCst);
    match ok {
        Ok(Ok(())) => true,
        Ok(Err(e)) => {
            tracing::warn!(error = %format!("{e:#}"), "webview settings window failed");
            false
        }
        Err(_) => {
            tracing::warn!("webview settings window panicked");
            false
        }
    }
}

fn run(cfg: Arc<Mutex<Config>>, health: health::Shared, live: bool) -> anyhow::Result<()> {
    let mut builder = EventLoopBuilder::<UserEvent>::with_user_event();
    // The tray owns the main thread's message loop, so this one must be allowed to live elsewhere.
    builder.with_any_thread(true);
    let mut event_loop = builder.build();
    let proxy = event_loop.create_proxy();

    // Taskbar / title-bar icon. tao wants raw RGBA, so `assets/tray_64.rgba` is emitted by
    // `assets/make_icon.py` alongside the .ico files rather than decoding our own ICO at runtime.
    let icon = tao::window::Icon::from_rgba(
        include_bytes!("../assets/tray_64.rgba").to_vec(),
        64,
        64,
    )
    .ok();

    let window = WindowBuilder::new()
        .with_title("T10S Keep-Awake")
        .with_window_icon(icon)
        .with_inner_size(tao::dpi::LogicalSize::new(560.0, 720.0))
        .with_min_inner_size(tao::dpi::LogicalSize::new(500.0, 620.0))
        .with_theme(None) // follow the Windows setting
        .build(&event_loop)?;

    let ipc_proxy = proxy.clone();
    let webview = WebViewBuilder::new()
        .with_html(include_str!("../assets/settings.html"))
        // Matches the page's dark background so there's no white flash while it loads.
        .with_background_color((22, 24, 29, 255))
        .with_ipc_handler(move |req| {
            let _ = ipc_proxy.send_event(UserEvent::Ipc(req.into_body()));
        })
        .build(&window)?;

    // Drives the countdown; also picks up enable/disable made from the tray.
    let tick_proxy = proxy.clone();
    std::thread::spawn(move || {
        while tick_proxy.send_event(UserEvent::Tick).is_ok() {
            std::thread::sleep(Duration::from_millis(500));
        }
    });

    let testing = Arc::new(AtomicBool::new(false));
    // Populated on "ready" — devices come and go, so the list is refreshed when the page loads
    // rather than captured once at build time.
    let default_device = audio::default_output_device_name().ok();

    // run_return, NOT run: tao's `run` calls process::exit when the loop ends, which would take
    // the tray and the keep-awake engine down with the settings window. This one returns.
    event_loop.run_return(move |event, _target, control_flow| {
        *control_flow = ControlFlow::Wait;
        match event {
            Event::WindowEvent {
                event: WindowEvent::CloseRequested,
                ..
            } => *control_flow = ControlFlow::Exit,

            Event::UserEvent(UserEvent::Tick) => {
                let js = format!("window.__status({});", status_json(&cfg, &health, live, &testing));
                let _ = webview.evaluate_script(&js);
            }

            Event::UserEvent(UserEvent::Ipc(body)) => {
                let msg: Value = match serde_json::from_str(&body) {
                    Ok(v) => v,
                    Err(_) => return,
                };
                match msg["cmd"].as_str().unwrap_or_default() {
                    "ready" => {
                        // Re-enumerate on open: devices come and go, and a stale list is how you
                        // end up pointing the pulse at a dongle that isn't plugged in any more.
                        let devices = audio::list_output_devices().unwrap_or_default();
                        let c = cfg.lock().unwrap().clone();
                        let init = json!({
                            "devices": devices,
                            "device": c.output_device,
                            "interval": c.interval_min.round() as u32,
                            "autostart": c.autostart,
                            "enabled": c.enabled,
                            "intervals": INTERVALS,
                        });
                        let _ = webview.evaluate_script(&format!("window.__init({init});"));
                        let _ = webview.evaluate_script(&format!(
                            "window.__capability({});",
                            capability_json(&c.output_device, c.freq_hz, &default_device)
                        ));
                        let _ = webview.evaluate_script(&format!(
                            "window.__status({});",
                            status_json(&cfg, &health, live, &testing)
                        ));
                    }

                    "select_device" => {
                        let name = msg["name"].as_str().unwrap_or_default().to_string();
                        let freq = cfg.lock().unwrap().freq_hz;
                        let _ = webview.evaluate_script(&format!(
                            "window.__capability({});",
                            capability_json(&name, freq, &default_device)
                        ));
                    }

                    "set_enabled" => {
                        let on = msg["value"].as_bool().unwrap_or(true);
                        {
                            let mut c = cfg.lock().unwrap();
                            c.enabled = on;
                            let _ = c.save();
                        }
                        tracing::info!(enabled = on, "keep-awake toggled from settings");
                        let _ = webview.evaluate_script(&format!(
                            "window.__status({});",
                            status_json(&cfg, &health, live, &testing)
                        ));
                    }

                    "set_interval" => {
                        if let Some(v) = msg["value"].as_u64() {
                            {
                                let mut c = cfg.lock().unwrap();
                                c.interval_min = v as f32;
                                c.sanitize();
                                let _ = c.save();
                            }
                            tracing::info!(interval = v, "interval changed from settings");
                            let _ = webview.evaluate_script(&format!(
                                "window.__status({});",
                                status_json(&cfg, &health, live, &testing)
                            ));
                        }
                    }

                    "test" => {
                        if !testing.swap(true, Ordering::SeqCst) {
                            let mut probe = cfg.lock().unwrap().clone();
                            if let Some(d) = msg["device"].as_str() {
                                if !d.trim().is_empty() {
                                    probe.output_device = d.to_string();
                                }
                            }
                            probe.sanitize();
                            let h = health.clone();
                            let flag = testing.clone();
                            // Blocks for the pulse duration, so never on this thread.
                            std::thread::spawn(move || {
                                crate::scheduler::fire_once(&probe, &h);
                                flag.store(false, Ordering::SeqCst);
                            });
                        }
                    }

                    "save" => {
                        let device = msg["device"].as_str().unwrap_or_default().to_string();
                        let freq = cfg.lock().unwrap().freq_hz;
                        if device.trim().is_empty() {
                            let _ = webview.evaluate_script(
                                "window.__error('Choose an output device first.');",
                            );
                            return;
                        }
                        let cap = device_guard::capability(&device, freq);
                        if !cap.is_ready() {
                            let _ = webview.evaluate_script(&format!(
                                "window.__error({});",
                                json!(format!("Can't use that device \u{2014} {}", cap.describe()))
                            ));
                            return;
                        }
                        let interval = msg["interval"].as_u64().unwrap_or(12) as f32;
                        let auto = msg["autostart"].as_bool().unwrap_or(false);
                        let (dev, pct) = {
                            let mut c = cfg.lock().unwrap();
                            c.output_device = device;
                            c.interval_min = interval;
                            c.autostart = auto;
                            c.sanitize();
                            let _ = c.save();
                            (c.output_device.clone(), c.device_volume_pct)
                        };
                        let _ = volume::set_endpoint_volume_percent(&dev, pct);
                        let _ = autostart::set_enabled(auto);
                        tracing::info!(device = %dev, interval, autostart = auto, "settings saved");
                        *control_flow = ControlFlow::Exit;
                    }

                    // The page measures itself and asks for a window that fits it exactly, so
                    // there's never a scrollbar or a strip of dead space — and it stays correct
                    // across DPI and Windows text-scaling changes that a fixed height can't track.
                    "resize" => {
                        if let Some(css_h) = msg["height"].as_f64() {
                            // CSS px and the window's logical px are different units whenever
                            // Windows text scaling is active. Convert through PHYSICAL pixels using
                            // the page's own devicePixelRatio: mixing the two made each resize
                            // enlarge the viewport, which measured taller, which resized again.
                            let dpr = msg["dpr"].as_f64().unwrap_or(1.0).max(0.1);
                            let want_phys = (css_h * dpr).round().clamp(360.0, 2400.0);
                            let cur = window.inner_size();
                            if (cur.height as f64 - want_phys).abs() > 4.0 {
                                window.set_inner_size(tao::dpi::PhysicalSize::new(
                                    cur.width as f64,
                                    want_phys,
                                ));
                            }
                        }
                    }

                    "close" => *control_flow = ControlFlow::Exit,
                    _ => {}
                }
            }
            _ => {}
        }
    });
    Ok(())
}

/// Capability + dedicated-device warning for `device`, as JSON for the page.
fn capability_json(device: &str, freq: f32, default_device: &Option<String>) -> String {
    if device.trim().is_empty() {
        return json!({"ready": false, "text": "No Device Selected", "is_default": false}).to_string();
    }
    let cap = device_guard::capability(device, freq);
    let is_default = default_device
        .as_deref()
        .is_some_and(|d| d.eq_ignore_ascii_case(device.trim()));
    json!({
        "ready": cap.is_ready(),
        "text": cap.describe(),
        "is_default": is_default,
    })
    .to_string()
}

/// Current status for the page: which colour, the two lines of text, and the countdown ring.
fn status_json(
    cfg: &Arc<Mutex<Config>>,
    health: &health::Shared,
    live: bool,
    testing: &Arc<AtomicBool>,
) -> String {
    let (enabled, interval_min) = {
        let c = cfg.lock().unwrap();
        (c.enabled, c.interval_min)
    };
    let h = health.snapshot();

    let (kind, head, detail) = if !enabled {
        (
            "bad",
            "Disabled".to_string(),
            "The sub will fall asleep on its own in about 16 minutes".to_string(),
        )
    } else if h.is_failing() {
        let (a, b) = h.status_parts();
        ("warn", a, b)
    } else if !live && h.last_pulse.is_none() {
        (
            "idle",
            "Settings Only".to_string(),
            "The tray app runs the keep-awake engine".to_string(),
        )
    } else {
        let (a, b) = h.status_parts();
        ("ok", a, b)
    };

    // Ring: fraction of the interval still to run, so it empties as the next pulse approaches.
    let (clock, frac) = match (enabled, h.next_due) {
        (true, Some(due)) => match due.checked_duration_since(Instant::now()) {
            Some(left) => {
                let total = (interval_min * 60.0).max(1.0);
                let f = (left.as_secs_f32() / total).clamp(0.0, 1.0);
                (
                    format!("{}:{:02}", left.as_secs() / 60, left.as_secs() % 60),
                    f,
                )
            }
            None => ("now".to_string(), 0.0),
        },
        _ => ("--:--".to_string(), -1.0),
    };

    json!({
        "kind": kind,
        "head": head,
        "detail": detail,
        "clock": clock,
        "frac": frac,
        "enabled": enabled,
        "testing": testing.load(Ordering::Relaxed),
    })
    .to_string()
}
