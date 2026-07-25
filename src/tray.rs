//! System-tray UI: the icon, its status tooltip, and the right-click menu.
//!
//! The engine runs on a background thread; the tray toggles `config.enabled` (which the engine
//! reads live), fires a test pulse, and opens the settings window. The icon switches to an amber
//! variant whenever `health` reports the pulse is failing, so a broken keep-alive is visible
//! without opening anything.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};

use crate::config::Config;
use crate::health;

/// Build the tray, run the Windows message loop until Quit, then signal the engine to stop.
///
/// `notice_slot` receives this thread's `NoticeSender`, so the engine can wake the GUI thread when
/// pulse health changes and the tray can repaint its icon/tooltip to match.
pub fn run(
    cfg: Arc<Mutex<Config>>,
    stop: Arc<AtomicBool>,
    health: health::Shared,
    notice_slot: Arc<Mutex<Option<nwg::NoticeSender>>>,
) -> Result<()> {
    nwg::init().context("nwg init")?;

    let mut window = nwg::MessageWindow::default();
    nwg::MessageWindow::builder()
        .build(&mut window)
        .context("build message window")?;

    let mut icon = nwg::Icon::default();
    nwg::Icon::builder()
        .source_bin(Some(include_bytes!("../assets/tray.ico")))
        .build(&mut icon)
        .context("build icon")?;

    // Amber "not pulsing" variant, shown when the engine reports a failing pulse.
    let mut icon_warn = nwg::Icon::default();
    nwg::Icon::builder()
        .source_bin(Some(include_bytes!("../assets/tray_warn.ico")))
        .build(&mut icon_warn)
        .context("build warning icon")?;

    let mut notice = nwg::Notice::default();
    nwg::Notice::builder()
        .parent(&window)
        .build(&mut notice)
        .context("build notice")?;
    *notice_slot.lock().unwrap() = Some(notice.sender());

    let mut tray = nwg::TrayNotification::default();
    nwg::TrayNotification::builder()
        .parent(&window)
        .icon(Some(&icon))
        .tip(Some("T10S Keep-Awake"))
        .build(&mut tray)
        .context("build tray")?;

    let mut menu = nwg::Menu::default();
    nwg::Menu::builder()
        .parent(&window)
        .popup(true)
        .build(&mut menu)
        .context("build menu")?;

    let mut item_enable = nwg::MenuItem::default();
    nwg::MenuItem::builder()
        .parent(&menu)
        .text("Enabled")
        .build(&mut item_enable)?;

    let mut item_disable = nwg::MenuItem::default();
    nwg::MenuItem::builder()
        .parent(&menu)
        .text("Disabled")
        .build(&mut item_disable)?;

    // A checkmark marks the state you're IN, not the action a click performs — "Enable/Disable"
    // alone never said which one was current.
    let start_enabled = cfg.lock().unwrap().enabled;
    item_enable.set_checked(start_enabled);
    item_disable.set_checked(!start_enabled);

    let mut sep1 = nwg::MenuSeparator::default();
    nwg::MenuSeparator::builder().parent(&menu).build(&mut sep1)?;

    let mut item_test = nwg::MenuItem::default();
    nwg::MenuItem::builder()
        .parent(&menu)
        .text("Send Test Pulse")
        .build(&mut item_test)?;

    let mut sep_test = nwg::MenuSeparator::default();
    nwg::MenuSeparator::builder().parent(&menu).build(&mut sep_test)?;

    let mut item_configure = nwg::MenuItem::default();
    nwg::MenuItem::builder()
        .parent(&menu)
        .text("Configure\u{2026}")
        .build(&mut item_configure)?;

    let mut sep2 = nwg::MenuSeparator::default();
    nwg::MenuSeparator::builder().parent(&menu).build(&mut sep2)?;

    let mut item_quit = nwg::MenuItem::default();
    nwg::MenuItem::builder()
        .parent(&menu)
        .text("Quit")
        .build(&mut item_quit)?;

    // Capture Copy handles for comparison inside the (Fn + 'static) event closure.
    let tray_h = tray.handle;
    let notice_h = notice.handle;
    let enable_h = item_enable.handle;
    let disable_h = item_disable.handle;
    let test_h = item_test.handle;
    let configure_h = item_configure.handle;
    let quit_h = item_quit.handle;

    // Remembers the last painted state so we only repaint / balloon on an actual transition.
    let was_failing = std::cell::Cell::new(false);

    let handler = nwg::full_bind_event_handler(&window.handle, move |evt, _data, handle| {
        use nwg::Event as E;
        match evt {
            E::OnNotice if handle == notice_h => {
                let h = health.snapshot();
                tray.set_tip(&h.tooltip());
                if h.is_failing() != was_failing.get() {
                    was_failing.set(h.is_failing());
                    if h.is_failing() {
                        tray.set_icon(&icon_warn);
                        tray.show(
                            h.last_error.as_deref().unwrap_or("pulse failed"),
                            Some("T10S Keep-Awake: Not Pulsing"),
                            Some(nwg::TrayNotificationFlags::ERROR_ICON),
                            None,
                        );
                    } else {
                        tray.set_icon(&icon);
                        tray.show(
                            "Pulsing again \u{2014} the sub is protected.",
                            Some("T10S Keep-Awake: Recovered"),
                            Some(nwg::TrayNotificationFlags::INFO_ICON),
                            None,
                        );
                    }
                }
            }
            E::OnContextMenu if handle == tray_h => {
                // Re-read the state each time the menu opens: the settings window can toggle it
                // too, and a stale checkmark is worse than none.
                let on = cfg.lock().unwrap().enabled;
                item_enable.set_checked(on);
                item_disable.set_checked(!on);
                let (x, y) = nwg::GlobalCursor::position();
                menu.popup(x, y);
            }
            E::OnMenuItemSelected => {
                if handle == enable_h {
                    set_enabled(&cfg, &tray, true);
                    item_enable.set_checked(true);
                    item_disable.set_checked(false);
                } else if handle == disable_h {
                    set_enabled(&cfg, &tray, false);
                    item_enable.set_checked(false);
                    item_disable.set_checked(true);
                } else if handle == test_h {
                    // Off the GUI thread: a pulse blocks for its full duration (5 s by default),
                    // which would freeze the tray if run inline.
                    let snapshot = cfg.lock().unwrap().clone();
                    let h = health.clone();
                    std::thread::spawn(move || crate::scheduler::fire_once(&snapshot, &h));
                } else if handle == configure_h {
                    // Modern WebView2 window; falls back to the native form if it can't start
                    // (e.g. no WebView2 runtime), so settings are never unreachable.
                    let (c, h) = (cfg.clone(), health.clone());
                    std::thread::spawn(move || {
                        if !crate::settings_web::open(c.clone(), h.clone(), true) {
                            crate::settings_ui::open_blocking(c, h, true);
                        }
                    });
                } else if handle == quit_h {
                    nwg::stop_thread_dispatch();
                }
            }
            _ => {}
        }
    });

    nwg::dispatch_thread_events();
    nwg::unbind_event_handler(&handler);
    stop.store(true, Ordering::Relaxed);
    Ok(())
}

fn set_enabled(cfg: &Arc<Mutex<Config>>, tray: &nwg::TrayNotification, enabled: bool) {
    {
        let mut c = cfg.lock().unwrap();
        c.enabled = enabled;
        let _ = c.save();
    }
    let msg = if enabled {
        "Keep-Awake Enabled"
    } else {
        "Keep-Awake Disabled"
    };
    tray.show(msg, Some("T10S Keep-Awake"), None, None);
}
