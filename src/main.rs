//! T10s_Keep_Awake — keeps the Adam T10S subwoofer out of auto-standby by firing a brief
//! ultrasonic pulse into a dedicated output (USB dongle -> isolator -> the sub's RCA input).
//!
//! Subcommands:
//!   (none) / run   tray app: keep-awake engine + system-tray icon
//!   settings       open the settings window on its own
//!   pulse          fire one pulse to a device (calibration / test tool)
//!   list-devices   list output devices
//!   device-info    a device's sample rates, exclusive-mode support, and current volume

mod audio;
mod audio_exclusive;
mod autostart;
mod config;
mod device_guard;
mod health;
mod monitor;
mod scheduler;
mod settings_ui;
mod settings_web;
mod tray;
mod volume;

use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Result;
use clap::{Args, Parser, Subcommand};

use config::Config;

#[derive(Parser, Debug)]
#[command(name = "t10s_keep_awake", about = "Adam T10S keep-awake utility")]
struct Cli {
    #[command(subcommand)]
    cmd: Option<Command>,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Run the keep-awake engine (default).
    Run,
    /// List available output devices.
    ListDevices,
    /// Print a device's default (shared-mode) sample rate -> max output freq is SR/2 (Nyquist).
    DeviceInfo {
        #[arg(long)]
        device: Option<String>,
    },
    /// Open the settings window directly (without the tray).
    Settings,
    /// Play a calibration pulse (or repeat with --hold).
    Pulse(PulseArgs),
}

#[derive(Args, Debug)]
struct PulseArgs {
    /// Output device name (default: system default output).
    #[arg(long)]
    device: Option<String>,
    /// Tone frequency in Hz.
    #[arg(long, default_value_t = 50.0)]
    freq: f32,
    /// Chord: comma-separated frequencies summed into one pulse (e.g. --freqs 50,250,500).
    /// Overrides --freq when present. Each component is at --db; the sum is clamped to avoid clip.
    #[arg(long)]
    freqs: Option<String>,
    /// Level in dBFS (<= 0). Pass as --db=-24 for negatives.
    #[arg(long = "db", default_value_t = -24.0, allow_hyphen_values = true)]
    dbfs: f32,
    /// Pulse duration in seconds.
    #[arg(long, default_value_t = 0.25)]
    dur: f32,
    /// Fade in/out in milliseconds.
    #[arg(long, default_value_t = 15.0)]
    fade: f32,
    /// Exponential decay time constant in seconds (0 = flat sustain). Gives a chime/bell character.
    #[arg(long, default_value_t = 0.0)]
    decay: f32,
    /// Use WASAPI exclusive mode (lets us exceed the 24 kHz shared-mode ceiling). Needs --rate.
    #[arg(long)]
    exclusive: bool,
    /// Exclusive-mode sample rate in Hz (e.g. 96000, 192000). Max output freq is rate/2.
    #[arg(long, default_value_t = 96000)]
    rate: u32,
    /// Repeat on an interval (endurance/hold mode). Ctrl+C to stop.
    #[arg(long)]
    hold: bool,
    /// Minutes between pulses in --hold mode.
    #[arg(long, default_value_t = 14.0)]
    interval: f32,

    /// Fire ONE pulse (at --dur), start the hold counter immediately, and press ENTER when the
    /// sub sleeps. Best for longer blips that latch on the first hit (no climb needed).
    #[arg(long)]
    time_hold: bool,

    /// Interactive duration sweep: blips climb in length; ENTER stops them and times the hold.
    #[arg(long)]
    sweep: bool,
    /// Sweep: starting duration in seconds.
    #[arg(long, default_value_t = 0.2)]
    sweep_start: f32,
    /// Sweep: duration increment per blip, in seconds.
    #[arg(long, default_value_t = 0.025)]
    sweep_step: f32,
    /// Sweep: maximum duration in seconds (stops if reached).
    #[arg(long, default_value_t = 1.0)]
    sweep_max: f32,
    /// Sweep: seconds between blips (allow for the sub's wake lag).
    #[arg(long, default_value_t = 8.0)]
    sweep_secs: f32,
}

impl PulseArgs {
    /// Frequencies to sum for one pulse: the parsed `--freqs` chord if given, else `[--freq]`.
    fn freq_list(&self) -> Vec<f32> {
        match &self.freqs {
            Some(s) => {
                let v: Vec<f32> = s
                    .split(',')
                    .filter_map(|x| x.trim().parse::<f32>().ok())
                    .collect();
                if v.is_empty() { vec![self.freq] } else { v }
            }
            None => vec![self.freq],
        }
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        None | Some(Command::Run) => run_app(),
        Some(Command::ListDevices) => {
            println!("Output devices:");
            for name in audio::list_output_devices()? {
                println!("  - {name}");
            }
            Ok(())
        }
        Some(Command::DeviceInfo { device }) => {
            let (default_sr, max_sr) = audio::device_sample_rate_info(device.as_deref())?;
            println!(
                "device: {}\n  default sample rate: {} Hz (max output {} Hz)\n  \
                 max supported sample rate: {} Hz (max output {} Hz)",
                device.as_deref().unwrap_or("<default>"),
                default_sr,
                default_sr / 2,
                max_sr,
                max_sr / 2
            );
            if let Some(dev) = device.as_deref() {
                println!("  exclusive-mode rate support:");
                let rates = [44100u32, 48000, 88200, 96000, 176400, 192000, 384000];
                match audio_exclusive::probe_exclusive_support(dev, &rates) {
                    Ok(lines) => {
                        for l in lines {
                            println!("    {l}");
                        }
                    }
                    Err(e) => println!("    probe failed: {e:#}"),
                }
                match volume::get_endpoint_volume_percent(dev) {
                    Ok(pct) => println!("  current Windows volume: {pct}%"),
                    Err(e) => println!("  volume read failed: {e:#}"),
                }
            }
            Ok(())
        }
        Some(Command::Settings) => {
            let cfg = Config::load_or_default()?;
            // Standalone `settings` has no engine behind it, so its health handle notifies nobody.
            let health = health::Shared::new(std::sync::Arc::new(|| {}));
            let shared = std::sync::Arc::new(std::sync::Mutex::new(cfg));
            // WebView2 window, with the native form as a fallback if it can't start.
            if !settings_web::open(shared.clone(), health.clone(), false) {
                settings_ui::open_blocking(shared, health, false);
            }
            Ok(())
        }
        Some(Command::Pulse(args)) => run_pulse(args),
    }
}

/// Run the tray app: config + engine thread + system-tray UI (message loop on the main thread).
fn run_app() -> Result<()> {
    let _guard = init_logging();
    let cfg = Config::load_or_default()?;
    tracing::info!(?cfg, "loaded config");
    // Pin the output device's Windows volume to the configured % (safety cap; non-fatal).
    // Runs on a spawned thread: its COM (MTA) init is per-thread and must not touch the main
    // thread, which nwg needs in STA.
    if !cfg.output_device.trim().is_empty() {
        let name = cfg.output_device.clone();
        let pct = cfg.device_volume_pct;
        let h = std::thread::spawn(move || volume::set_endpoint_volume_percent(&name, pct));
        match h.join() {
            Ok(Ok(())) => {}
            Ok(Err(e)) => tracing::warn!(error = %format!("{e:#}"), "could not pin device volume"),
            Err(_) => tracing::warn!("volume pin thread panicked"),
        }
    }
    // Keep the HKCU Run entry in sync with the config's autostart preference.
    if let Err(e) = autostart::set_enabled(cfg.autostart) {
        tracing::warn!(error = %format!("{e:#}"), "could not sync autostart");
    }
    let cfg = Arc::new(Mutex::new(cfg));
    let stop = Arc::new(AtomicBool::new(false));

    // The engine starts before the tray exists, so it signals through a slot the tray fills in
    // once its Notice is built on the GUI thread. Until then, health changes are simply recorded.
    let notice_slot: Arc<Mutex<Option<nwg::NoticeSender>>> = Arc::new(Mutex::new(None));
    let slot_for_health = notice_slot.clone();
    let health = health::Shared::new(Arc::new(move || {
        if let Some(tx) = slot_for_health.lock().unwrap().as_ref() {
            tx.notice();
        }
    }));

    let engine_cfg = cfg.clone();
    let engine_stop = stop.clone();
    let engine_health = health.clone();
    let engine = std::thread::spawn(move || scheduler::run(engine_cfg, engine_stop, engine_health));

    tray::run(cfg, stop, health, notice_slot)?; // blocks on the Windows message loop until Quit
    let _ = engine.join();
    Ok(())
}

/// File logging under %APPDATA%\T10sKeepAwake\logs (daily rolling). Returns the flush guard.
fn init_logging() -> Option<tracing_appender::non_blocking::WorkerGuard> {
    let dir = Config::dir().ok()?.join("logs");
    if std::fs::create_dir_all(&dir).is_err() {
        return None;
    }
    let file = tracing_appender::rolling::daily(&dir, "t10s.log");
    let (nb, guard) = tracing_appender::non_blocking(file);
    tracing_subscriber::fmt()
        .with_writer(nb)
        .with_ansi(false)
        .init();
    Some(guard)
}

fn run_pulse(a: PulseArgs) -> Result<()> {
    if a.dbfs > 0.0 {
        eprintln!("error: --db must be <= 0 (dBFS)");
        std::process::exit(2);
    }
    if a.time_hold {
        return run_hold_test(a);
    }
    if a.sweep {
        return run_sweep(a);
    }
    let target = a.device.as_deref().unwrap_or("<default output>");
    println!(
        "Pulse: {} Hz \u{b7} {} dBFS \u{b7} {} s  ->  {}",
        a.freq, a.dbfs, a.dur, target
    );

    if a.hold {
        println!(
            "HOLD / ENDURANCE - pulse every {} min. Watch the sub LED. Ctrl+C to stop.\n",
            a.interval
        );
        let start = Instant::now();
        let interval = Duration::from_secs_f32(a.interval * 60.0);
        let mut count = 0u32;
        loop {
            count += 1;
            let e = start.elapsed().as_secs();
            println!(
                "[+{:02}:{:02}:{:02}]  \u{266a} PULSE #{count}  ({} Hz \u{b7} {} dBFS \u{b7} {}s)",
                e / 3600,
                (e % 3600) / 60,
                e % 60,
                a.freq,
                a.dbfs,
                a.dur
            );
            audio::play_pulse(a.device.as_deref(), &a.freq_list(), a.dbfs, a.dur, a.fade, a.decay, false)?;
            let next = Instant::now() + interval;
            while Instant::now() < next {
                use std::io::Write;
                let el = start.elapsed().as_secs();
                let rem = next.saturating_duration_since(Instant::now()).as_secs();
                print!(
                    "\r  \u{23f1}  running {:02}:{:02}:{:02}   |   next pulse in {:02}:{:02}   |   fired: {count}    ",
                    el / 3600,
                    (el % 3600) / 60,
                    el % 60,
                    rem / 60,
                    rem % 60
                );
                let _ = std::io::stdout().flush();
                std::thread::sleep(Duration::from_millis(500));
            }
            print!("\r{}\r", " ".repeat(78));
        }
    }

    if a.exclusive {
        let dev = a
            .device
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("--exclusive requires --device"))?;
        println!("  (exclusive mode @ {} Hz, max output {} Hz)", a.rate, a.rate / 2);
        audio_exclusive::play_pulse_exclusive(dev, &a.freq_list(), a.dbfs, a.dur, a.rate, a.fade)?;
    } else {
        audio::play_pulse(a.device.as_deref(), &a.freq_list(), a.dbfs, a.dur, a.fade, a.decay, true)?;
    }
    println!("done.");
    Ok(())
}

/// Fire one pulse, then time how long the sub stays awake (single ENTER when it sleeps).
fn run_hold_test(a: PulseArgs) -> Result<()> {
    use std::io::Write;
    use std::sync::atomic::Ordering;
    use std::time::Instant;

    let len_ms = a.dur * 1000.0;
    println!(
        "HOLD TEST  {} Hz / {} dBFS / {:.0} ms  ->  {}",
        a.freq,
        a.dbfs,
        len_ms,
        a.device.as_deref().unwrap_or("<default output>")
    );
    print!("  \u{266a} firing {:.0} ms pulse ... ", len_ms);
    let _ = std::io::stdout().flush();
    audio::play_pulse(a.device.as_deref(), &a.freq_list(), a.dbfs, a.dur, a.fade, a.decay, false)?;
    println!("done.  Timing hold \u{2014} press ENTER the moment the sub SLEEPS (red).\n");

    let started = Instant::now();
    let stop = Arc::new(AtomicBool::new(false));
    let s2 = stop.clone();
    let timer = std::thread::spawn(move || {
        while !s2.load(Ordering::Relaxed) {
            let e = started.elapsed().as_secs();
            print!("\r  \u{23f1}  holding {:02}:{:02}   ", e / 60, e % 60);
            let _ = std::io::stdout().flush();
            std::thread::sleep(Duration::from_millis(250));
        }
    });

    let mut l = String::new();
    let _ = std::io::stdin().read_line(&mut l);
    stop.store(true, Ordering::Relaxed);
    let _ = timer.join();
    let held = started.elapsed().as_secs();

    let msg = format!(
        "{:.0} ms blip ({} Hz / {} dBFS)  ->  held {}:{:02} ({} s)",
        len_ms,
        a.freq,
        a.dbfs,
        held / 60,
        held % 60,
        held
    );
    println!("\n\n>>> RESULT: {msg} <<<");
    if let Ok(dir) = Config::dir() {
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::fs::write(dir.join("last_sweep.txt"), &msg);
    }
    println!("\nPress Enter to close.");
    let mut c = String::new();
    let _ = std::io::stdin().read_line(&mut c);
    Ok(())
}

/// Interactive duration sweep + hold timer:
///   Phase 1 — blips climb in duration on a timer.
///   ENTER  — stops the blips, records the last blip length, and starts a live hold up-counter.
///   ENTER  — stops the counter when the sub sleeps; reports "<len> ms -> held <mm:ss>".
fn run_sweep(a: PulseArgs) -> Result<()> {
    use std::io::Write;
    use std::sync::atomic::Ordering;
    use std::time::Instant;

    let device = a.device.clone();
    let (freq, dbfs) = (a.freq, a.dbfs);
    let (start, step, max) = (a.sweep_start, a.sweep_step, a.sweep_max);
    let gap = Duration::from_secs_f32(a.sweep_secs.max(1.0));

    println!(
        "SWEEP  {freq} Hz / {dbfs} dBFS   from {:.0} ms  (+{:.0} ms every {:.0}s, max {:.0} ms)  ->  {}",
        start * 1000.0,
        step * 1000.0,
        a.sweep_secs,
        max * 1000.0,
        device.as_deref().unwrap_or("<default output>")
    );
    println!(">>> Watch the LED. Press ENTER at the blip you want to test — it stops and starts the hold timer. <<<\n");

    // --- Phase 1: climbing blips ---
    let current = Arc::new(Mutex::new(0f32));
    let stop_blips = Arc::new(AtomicBool::new(false));
    let cur2 = current.clone();
    let sb2 = stop_blips.clone();
    let dev = device.clone();
    let blips = std::thread::spawn(move || {
        let mut d = start;
        while d <= max + 1e-6 && !sb2.load(Ordering::Relaxed) {
            *cur2.lock().unwrap() = d;
            print!("  \u{266a} blip {:>4.0} ms ... ", d * 1000.0);
            let _ = std::io::stdout().flush();
            let fade = (d * 1000.0 / 4.0).clamp(2.0, 15.0);
            if let Err(e) = audio::play_pulse(dev.as_deref(), &[freq], dbfs, d, fade, 0.0, false) {
                eprintln!("(pulse error: {e:#})");
            }
            println!("done");
            let mut left = gap;
            let tick = Duration::from_millis(200);
            while left > Duration::ZERO && !sb2.load(Ordering::Relaxed) {
                let ch = left.min(tick);
                std::thread::sleep(ch);
                left = left.saturating_sub(ch);
            }
            d += step;
        }
    });

    let mut l1 = String::new();
    let _ = std::io::stdin().read_line(&mut l1);
    stop_blips.store(true, Ordering::Relaxed);
    let _ = blips.join();
    let len_ms = *current.lock().unwrap() * 1000.0;

    // --- Phase 2: hold up-counter ---
    println!(
        "\n>>> Blips STOPPED at {:.0} ms. Timing hold... press ENTER when the sub SLEEPS (red). <<<",
        len_ms
    );
    let started = Instant::now();
    let stop_timer = Arc::new(AtomicBool::new(false));
    let st2 = stop_timer.clone();
    let timer = std::thread::spawn(move || {
        while !st2.load(Ordering::Relaxed) {
            let e = started.elapsed().as_secs();
            print!("\r  \u{23f1}  holding {:02}:{:02}   ", e / 60, e % 60);
            let _ = std::io::stdout().flush();
            std::thread::sleep(Duration::from_millis(250));
        }
    });

    let mut l2 = String::new();
    let _ = std::io::stdin().read_line(&mut l2);
    stop_timer.store(true, Ordering::Relaxed);
    let _ = timer.join();
    let held = started.elapsed().as_secs();

    let msg = format!(
        "{:.0} ms blip ({} Hz / {} dBFS)  ->  held {}:{:02} ({} s)",
        len_ms,
        freq,
        dbfs,
        held / 60,
        held % 60,
        held
    );
    println!("\n\n>>> RESULT: {msg} <<<");
    if let Ok(dir) = Config::dir() {
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::fs::write(dir.join("last_sweep.txt"), &msg);
    }

    println!("\nPress Enter to close.");
    let mut c = String::new();
    let _ = std::io::stdin().read_line(&mut c);
    Ok(())
}
