//! Fallback settings window, in native Win32 controls.
//!
//! The primary settings UI is `settings_web` (WebView2); this exists for machines without the
//! WebView2 runtime, so settings are never unreachable. Runs on its own thread with its own nwg
//! message loop, so its dispatch/stop never interferes with the tray's loop on the main thread.
//!
//! Deliberately small: **on/off, output device, interval, autostart**. Everything else (frequency,
//! level, duration, fade, mode, monitor device, sample rate…) was a knob for the test-and-learn
//! campaign and is now locked — those fields still live in `config.toml` for anyone who needs them,
//! but putting them on screen would only invite someone to break a working setup.
//!
//! Coordinates here are *logical* pixels: the nwg `high-dpi` feature converts them to physical, so
//! the window is the right size on a 4K display instead of a postage stamp.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::config::Config;
use crate::{audio, autostart, device_guard, health, volume};

/// Intervals offered, in minutes. The measured standby timer is ~16 min, so the list stops at 14 —
/// anything closer leaves no margin for a hold that comes in short.
const INTERVALS: [u32; 5] = [10, 11, 12, 13, 14];
const DEFAULT_INTERVAL: u32 = 12;

// Layout grid, in logical px. Everything lines up on these columns so the window reads as a form
// rather than a pile of controls: labels in the left column, controls in the right, help icons in
// a fixed gutter at the far right.
const W: i32 = 700;
const H: i32 = 604;
const PAD: i32 = 28; // outer margin
const COL_LABEL: i32 = 34; // left column: field names
const COL_CTRL: i32 = 210; // right column: the controls themselves
const CTRL_W: i32 = 396;
const COL_HELP: i32 = 638; // help-icon gutter
const BAND_H: i32 = 76; // status band

// Status band fills. Light enough that near-black text sits comfortably on them.
const GREEN: [u8; 3] = [223, 245, 228];
const AMBER: [u8; 3] = [255, 237, 205];
const RED: [u8; 3] = [253, 222, 222];
const GREY: [u8; 3] = [235, 238, 242];
/// Hairline rules under section headers.
const RULE: [u8; 3] = [214, 218, 224];
/// Tint behind the "this device is managed" note.
const INFO: [u8; 3] = [234, 240, 249];

const HELP_ENABLED: &str = "Master switch for the keep-alive pulse.\r\n\r\n\
When off, nothing is sent and the sub falls asleep on its own after about 16 minutes \u{2014} which \
mutes the monitors too, and pops when it next wakes.";

const HELP_DEVICE: &str = "The audio device the silent keep-alive pulse plays through.\r\n\r\n\
This must be a DEDICATED device wired to the sub's spare input \u{2014} not the one you listen \
through. The app pins its volume and sample rate, so anything else using it would be disrupted.\r\n\r\n\
It also needs to run at 96 kHz or higher: the pulse is above hearing, and a slower device cannot \
carry it.";

const HELP_INTERVAL: &str = "How often the pulse fires.\r\n\r\n\
The sub falls asleep after about 16 minutes with no signal, and every pulse restarts that clock. \
12 minutes leaves a comfortable ~4 minute margin. Shorter is safer and never harmful \u{2014} the \
pulse is inaudible either way.";

const HELP_AUTOSTART: &str = "Start T10S Keep-Awake automatically when you sign in to Windows.\r\n\r\n\
Recommended: if the app isn't running, the sub sleeps after ~16 minutes and pops when it wakes.";

const MANAGED_NOTE: &str = "\u{24d8}   This device is reserved for the keep-alive. Its volume is held at the \
safe level, and each pulse takes exclusive control of it for a few seconds \u{2014} so anything else \
playing through it would be interrupted. Use a dedicated dongle, not your listening device.";

/// Open the settings window on the CURRENT thread, blocking until it closes.
///
/// `live` says whether an engine is actually running behind this window. The standalone `settings`
/// subcommand has none, so its status strip must not claim the app is pulsing.
pub fn open_blocking(cfg: Arc<Mutex<Config>>, health: health::Shared, live: bool) {
    if nwg::init().is_err() {
        return;
    }
    let _ = nwg::Font::set_global_family("Segoe UI");
    if let Err(e) = build_and_run(cfg, health, live) {
        tracing::warn!(error = %format!("{e:#}"), "settings window failed");
    }
}

struct Ui {
    window: nwg::Window,
    /// One (headline, detail) pair per state, stacked in the same place and switched by
    /// visibility: nwg fixes a Label's background colour AND font at build time, so a single band
    /// could show neither the right colour nor two type sizes.
    /// Order: 0 = active, 1 = failing, 2 = disabled, 3 = no engine.
    bands: Vec<(nwg::Label, nwg::Label)>,
    enabled: nwg::CheckBox,
    output: nwg::ComboBox<String>,
    capability: nwg::Label,
    dedicated_warn: nwg::Label,
    interval: nwg::ComboBox<String>,
    autostart: nwg::CheckBox,
    test: nwg::Button,
    _save: nwg::Button,
    _cancel: nwg::Button,
    _tooltip: nwg::Tooltip,
    _timer: nwg::AnimationTimer,
    _help_icon: nwg::Icon,
    _help_frames: Vec<nwg::ImageFrame>,
    _fonts: Vec<Rc<nwg::Font>>,
    _labels: Vec<nwg::Label>,
    device_names: Vec<String>,
    health: health::Shared,
    cfg: Arc<Mutex<Config>>,
    live: bool,
    /// Windows' current default playback device — used to warn when the keep-awake output is
    /// pointed at the device the user actually listens through.
    default_device: Option<String>,
    test_running: Arc<AtomicBool>,
    painted_busy: RefCell<bool>,
}

fn font(size: u32, weight: u32) -> Rc<nwg::Font> {
    let mut f = nwg::Font::default();
    let _ = nwg::Font::builder()
        .family("Segoe UI")
        .size(size)
        .weight(weight)
        .build(&mut f);
    Rc::new(f)
}

fn build_and_run(cfg: Arc<Mutex<Config>>, health: health::Shared, live: bool) -> anyhow::Result<()> {
    let snap = cfg.lock().unwrap().clone();
    let devices = audio::list_output_devices().unwrap_or_default();
    let default_device = audio::default_output_device_name().ok();

    let mut window = nwg::Window::default();
    nwg::Window::builder()
        .size((W, H))
        .title("T10S Keep-Awake \u{2014} Settings")
        .flags(nwg::WindowFlags::WINDOW | nwg::WindowFlags::VISIBLE)
        .build(&mut window)?;

    // Hierarchy carries the design here: nwg can't colour label text, so size and weight do the
    // work of separating headline / section / field / hint.
    let f_status = font(26, 600); // band headline
    let f_detail = font(17, 400); // band second line
    let f_section = font(14, 700); // SECTION HEADERS
    let f_body = font(19, 400); // field labels and controls
    let f_small = font(16, 400); // hints under the device picker
    let fonts = vec![
        f_status.clone(),
        f_detail.clone(),
        f_section.clone(),
        f_body.clone(),
        f_small.clone(),
    ];

    // ---- status band: a headline + detail pair per state, only one pair visible -----------------
    // Two labels rather than one because nwg fixes a label's font as well as its background, and
    // the band needs a big headline over smaller detail to read as a status rather than a sentence.
    let band = |bg: [u8; 3]| -> anyhow::Result<(nwg::Label, nwg::Label)> {
        let mut head = nwg::Label::default();
        nwg::Label::builder()
            .text("")
            .parent(&window)
            .position((0, 0))
            .size((W, 44))
            .background_color(Some(bg))
            .build(&mut head)?;
        head.set_font(Some(&f_status));
        head.set_visible(false);

        let mut detail = nwg::Label::default();
        nwg::Label::builder()
            .text("")
            .parent(&window)
            .position((0, 44))
            .size((W, BAND_H - 44))
            .background_color(Some(bg))
            .build(&mut detail)?;
        detail.set_font(Some(&f_detail));
        detail.set_visible(false);
        Ok((head, detail))
    };
    let bands = vec![band(GREEN)?, band(AMBER)?, band(RED)?, band(GREY)?];

    let mut labels: Vec<nwg::Label> = Vec::new();
    let mk_label = |text: &str, x: i32, y: i32, w: i32, h: i32, fnt: &nwg::Font| -> nwg::Label {
        let mut l = nwg::Label::default();
        nwg::Label::builder()
            .text(text)
            .parent(&window)
            .position((x, y))
            .size((w, h))
            .build(&mut l)
            .unwrap();
        l.set_font(Some(fnt));
        l
    };
    // Hairline under a section header: a 1px label filled with the rule colour.
    let rule = |y: i32| -> nwg::Label {
        let mut l = nwg::Label::default();
        nwg::Label::builder()
            .text("")
            .parent(&window)
            .position((COL_LABEL, y))
            .size((W - COL_LABEL - PAD, 1))
            .background_color(Some(RULE))
            .build(&mut l)
            .unwrap();
        l
    };

    // ---- section: keep-awake ------------------------------------------------------------------
    labels.push(mk_label("KEEP-AWAKE", COL_LABEL, 98, 300, 22, &f_section));
    labels.push(rule(124));

    let mut enabled = nwg::CheckBox::default();
    nwg::CheckBox::builder()
        .text("Keep the Sub Awake")
        .parent(&window)
        .position((COL_CTRL, 140))
        .size((380, 30))
        .check_state(if snap.enabled {
            nwg::CheckBoxState::Checked
        } else {
            nwg::CheckBoxState::Unchecked
        })
        .build(&mut enabled)?;
    enabled.set_font(Some(&f_body));

    labels.push(mk_label("Output Device", COL_LABEL, 190, 170, 26, &f_body));
    let mut output = nwg::ComboBox::default();
    let out_idx = devices.iter().position(|d| *d == snap.output_device);
    nwg::ComboBox::builder()
        .parent(&window)
        .position((COL_CTRL, 186))
        .size((CTRL_W, 30))
        .collection(devices.clone())
        .selected_index(out_idx)
        .build(&mut output)?;

    let mut capability = nwg::Label::default();
    nwg::Label::builder()
        .text("")
        .parent(&window)
        .position((COL_CTRL, 222))
        .size((440, 24))
        .build(&mut capability)?;
    capability.set_font(Some(&f_small));

    let mut dedicated_warn = nwg::Label::default();
    nwg::Label::builder()
        .text("")
        .parent(&window)
        .position((COL_CTRL, 246))
        .size((452, 44))
        .build(&mut dedicated_warn)?;
    dedicated_warn.set_font(Some(&f_small));

    labels.push(mk_label("Interval", COL_LABEL, 304, 170, 26, &f_body));
    let mut interval = nwg::ComboBox::default();
    let interval_items: Vec<String> = INTERVALS
        .iter()
        .map(|m| {
            if *m == DEFAULT_INTERVAL {
                format!("{m} minutes   (recommended)")
            } else {
                format!("{m} minutes")
            }
        })
        .collect();
    let cur = snap.interval_min.round() as u32;
    let int_idx = INTERVALS
        .iter()
        .position(|m| *m == cur)
        .or_else(|| INTERVALS.iter().position(|m| *m == DEFAULT_INTERVAL));
    nwg::ComboBox::builder()
        .parent(&window)
        .position((COL_CTRL, 300))
        .size((280, 30))
        .collection(interval_items)
        .selected_index(int_idx)
        .build(&mut interval)?;

    // ---- section: startup ---------------------------------------------------------------------
    labels.push(mk_label("STARTUP", COL_LABEL, 356, 300, 22, &f_section));
    labels.push(rule(382));
    let mut autostart_cb = nwg::CheckBox::default();
    nwg::CheckBox::builder()
        .text("Start with Windows")
        .parent(&window)
        .position((COL_CTRL, 398))
        .size((380, 30))
        .check_state(if snap.autostart {
            nwg::CheckBoxState::Checked
        } else {
            nwg::CheckBoxState::Unchecked
        })
        .build(&mut autostart_cb)?;
    autostart_cb.set_font(Some(&f_body));

    // ---- help icons ----------------------------------------------------------------------------
    let mut help_icon = nwg::Icon::default();
    nwg::Icon::builder()
        .source_bin(Some(include_bytes!("../assets/help.ico")))
        .size(Some((20, 20)))
        .build(&mut help_icon)?;

    let mut help_frames = Vec::new();
    for y in [144, 190, 304, 402] {
        let mut fr = nwg::ImageFrame::default();
        nwg::ImageFrame::builder()
            .parent(&window)
            .position((COL_HELP, y))
            .size((22, 22))
            .icon(Some(&help_icon))
            .build(&mut fr)?;
        help_frames.push(fr);
    }

    // ---- managed-device note: full-width tinted band --------------------------------------------
    // Full width and containing nothing but its own text, so its tint can't clash with a control
    // painting itself on the window's default grey.
    let mut note = nwg::Label::default();
    nwg::Label::builder()
        .text(MANAGED_NOTE)
        .parent(&window)
        .position((0, 448))
        .size((W, 74))
        .background_color(Some(INFO))
        .build(&mut note)?;
    note.set_font(Some(&f_small));
    labels.push(note);

    // ---- footer: rule, then actions right-aligned ------------------------------------------------
    let mut footer_rule = nwg::Label::default();
    nwg::Label::builder()
        .text("")
        .parent(&window)
        .position((0, 534))
        .size((W, 1))
        .background_color(Some(RULE))
        .build(&mut footer_rule)?;
    labels.push(footer_rule);

    let mk_button = |text: &str, x: i32, w: i32| -> anyhow::Result<nwg::Button> {
        let mut b = nwg::Button::default();
        nwg::Button::builder()
            .text(text)
            .parent(&window)
            .position((x, 550))
            .size((w, 38))
            .build(&mut b)?;
        b.set_font(Some(&f_body));
        Ok(b)
    };
    let test = mk_button("Send Test Pulse", PAD, 190)?;
    let cancel = mk_button("Cancel", W - PAD - 270, 128)?;
    let save = mk_button("Save", W - PAD - 134, 134)?;

    // ---- tooltips + 1 s ticker -------------------------------------------------------------------
    let mut tooltip = nwg::Tooltip::default();
    nwg::Tooltip::builder()
        .register(&help_frames[0], HELP_ENABLED)
        .register(&enabled, HELP_ENABLED)
        .register(&help_frames[1], HELP_DEVICE)
        .register(&output, HELP_DEVICE)
        .register(&help_frames[2], HELP_INTERVAL)
        .register(&interval, HELP_INTERVAL)
        .register(&help_frames[3], HELP_AUTOSTART)
        .register(&autostart_cb, HELP_AUTOSTART)
        .build(&mut tooltip)?;

    let mut timer = nwg::AnimationTimer::default();
    nwg::AnimationTimer::builder()
        .parent(&window)
        .interval(Duration::from_secs(1))
        .build(&mut timer)?;
    timer.start();

    let ui = Rc::new(Ui {
        window,
        bands,
        enabled,
        output,
        capability,
        dedicated_warn,
        interval,
        autostart: autostart_cb,
        test,
        _save: save,
        _cancel: cancel,
        _tooltip: tooltip,
        _timer: timer,
        _help_icon: help_icon,
        _help_frames: help_frames,
        _fonts: fonts,
        _labels: labels,
        device_names: devices,
        health,
        cfg: cfg.clone(),
        live,
        default_device,
        test_running: Arc::new(AtomicBool::new(false)),
        painted_busy: RefCell::new(false),
    });

    refresh_status(&ui);
    refresh_device_info(&ui);

    let save_h = ui._save.handle;
    let cancel_h = ui._cancel.handle;
    let test_h = ui.test.handle;
    let output_h = ui.output.handle;
    let enabled_h = ui.enabled.handle;
    let win_h = ui.window.handle;
    let ui_ev = ui.clone();
    let handler = nwg::full_bind_event_handler(&ui.window.handle, move |evt, _data, handle| {
        use nwg::Event as E;
        match evt {
            E::OnTimerTick => refresh_status(&ui_ev),
            E::OnComboxBoxSelection if handle == output_h => refresh_device_info(&ui_ev),
            // The master switch applies immediately — it's the one control you might reach for in
            // a hurry, and making it wait for Save would be a trap.
            E::OnButtonClick if handle == enabled_h => {
                let on = ui_ev.enabled.check_state() == nwg::CheckBoxState::Checked;
                {
                    let mut c = ui_ev.cfg.lock().unwrap();
                    c.enabled = on;
                    let _ = c.save();
                }
                refresh_status(&ui_ev);
            }
            E::OnButtonClick if handle == test_h => fire_test(&ui_ev),
            E::OnButtonClick if handle == save_h => match apply_save(&ui_ev) {
                Ok(()) => nwg::stop_thread_dispatch(),
                Err(msg) => {
                    nwg::modal_error_message(&ui_ev.window, "Can't save", &msg);
                }
            },
            E::OnButtonClick if handle == cancel_h => nwg::stop_thread_dispatch(),
            E::OnWindowClose if handle == win_h => nwg::stop_thread_dispatch(),
            _ => {}
        }
    });

    nwg::dispatch_thread_events();
    nwg::unbind_event_handler(&handler);
    Ok(())
}

/// Currently selected device name, falling back to the saved one.
fn selected_device(ui: &Ui) -> String {
    ui.output
        .selection()
        .and_then(|i| ui.device_names.get(i).cloned())
        .unwrap_or_else(|| ui.cfg.lock().unwrap().output_device.clone())
}

/// Repaint the status strip and the Test button. Driven by the 1 s timer, so the countdown counts
/// down and a change made from the tray shows up here without reopening the window.
fn refresh_status(ui: &Ui) {
    let enabled = ui.cfg.lock().unwrap().enabled;
    let h = ui.health.snapshot();

    // (headline, detail, band index) — index into `bands`: 0 active, 1 failing, 2 off, 3 no engine.
    let (head, detail, which) = if !enabled {
        (
            "Disabled".to_string(),
            "The sub will fall asleep on its own in about 16 minutes".to_string(),
            2,
        )
    } else if h.is_failing() {
        let (a, b) = h.status_parts();
        (a, b, 1)
    } else if !ui.live && h.last_pulse.is_none() {
        // Standalone `settings`: no engine behind this window, so claiming "Active" would be a lie.
        (
            "Settings Only".to_string(),
            "The tray app runs the keep-awake engine".to_string(),
            3,
        )
    } else {
        let (a, b) = h.status_parts();
        (a, b, 0)
    };

    // Keep the checkbox honest if the state was changed from the tray while this window is open.
    let want = if enabled {
        nwg::CheckBoxState::Checked
    } else {
        nwg::CheckBoxState::Unchecked
    };
    if ui.enabled.check_state() != want {
        ui.enabled.set_check_state(want);
    }

    for (i, (hd, dt)) in ui.bands.iter().enumerate() {
        if i == which {
            hd.set_text(&format!("    \u{25cf}   {head}"));
            dt.set_text(&format!("             {detail}"));
            hd.set_visible(true);
            dt.set_visible(true);
        } else {
            hd.set_visible(false);
            dt.set_visible(false);
        }
    }

    let running = ui.test_running.load(Ordering::Relaxed);
    if running != *ui.painted_busy.borrow() {
        *ui.painted_busy.borrow_mut() = running;
        ui.test.set_enabled(!running);
        ui.test
            .set_text(if running { "Pulsing\u{2026}" } else { "Send Test Pulse" });
    }
}

/// Update the capability readout and the dedicated-device warning for the current selection.
fn refresh_device_info(ui: &Ui) {
    let dev = selected_device(ui);
    if dev.trim().is_empty() {
        ui.capability.set_text("No Device Selected");
        ui.dedicated_warn.set_text("");
        return;
    }
    let freq = ui.cfg.lock().unwrap().freq_hz;
    ui.capability
        .set_text(&device_guard::capability(&dev, freq).describe());

    let is_default = ui
        .default_device
        .as_deref()
        .is_some_and(|d| d.eq_ignore_ascii_case(dev.trim()));
    ui.dedicated_warn.set_text(if is_default {
        "\u{26a0}  This is your default playback device. The app pins its volume and sample rate \u{2014} \
         choose a dedicated device instead."
    } else {
        ""
    });
}

/// Fire one pulse using the CURRENTLY SELECTED device, so the button tests what's on screen rather
/// than what happens to be saved.
///
/// The pulse blocks for its full duration (5 s by default), so it runs on a worker thread; the
/// worker owns an atomic flag that `refresh_status` polls each tick to restore the button. Nothing
/// touches a GUI handle off the GUI thread.
fn fire_test(ui: &Rc<Ui>) {
    if ui.test_running.load(Ordering::Relaxed) {
        return;
    }
    let mut probe = ui.cfg.lock().unwrap().clone();
    probe.output_device = selected_device(ui);
    probe.sanitize();

    let h = ui.health.clone();
    let running = ui.test_running.clone();
    running.store(true, Ordering::Relaxed);
    refresh_status(ui);

    std::thread::spawn(move || {
        crate::scheduler::fire_once(&probe, &h);
        running.store(false, Ordering::Relaxed);
    });
}

/// Parse the controls into the shared config, persist, and apply. Returns Err(msg) to block Save.
fn apply_save(ui: &Ui) -> Result<(), String> {
    let device = selected_device(ui);
    if device.trim().is_empty() {
        return Err("Choose the output device the keep-awake pulse should use.".into());
    }
    let freq = ui.cfg.lock().unwrap().freq_hz;
    let cap = device_guard::capability(&device, freq);
    if !cap.is_ready() {
        return Err(format!(
            "\"{device}\" can't carry the silent pulse.\n\n{}\n\nChoose a device that runs at \
             96 kHz or higher.",
            cap.describe()
        ));
    }
    let interval = ui
        .interval
        .selection()
        .and_then(|i| INTERVALS.get(i).copied())
        .unwrap_or(DEFAULT_INTERVAL);

    let (dev, pct, auto) = {
        let mut c = ui.cfg.lock().unwrap();
        c.output_device = device;
        c.interval_min = interval as f32;
        c.enabled = ui.enabled.check_state() == nwg::CheckBoxState::Checked;
        c.autostart = ui.autostart.check_state() == nwg::CheckBoxState::Checked;
        c.sanitize();
        let _ = c.save();
        (c.output_device.clone(), c.device_volume_pct, c.autostart)
    };

    let _ = volume::set_endpoint_volume_percent(&dev, pct);
    let _ = autostart::set_enabled(auto);
    Ok(())
}
