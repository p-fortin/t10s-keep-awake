#!/usr/bin/env py
"""Automated hold-time profiler for the Adam T10S keep-awake project.

Reads the sub's POWER LED via the C922 webcam (GREEN = awake, RED = standby) and measures how
long a blip of a given length keeps the sub awake — repeatedly and unattended — so we can
characterize the erratic hold-time distribution and lock parameters with confidence.

Subcommands:
  capture   grab one frame -> image file (to locate the LED in the frame)
  autoroi   find the LED automatically and print a ready-to-paste --roi (use after a camera bump)
  probe     continuously classify the LED in an ROI (run while toggling the sub to validate)
  run       autonomous loop: fire blips of various lengths, time each hold, append to CSV

Deps: opencv-python, numpy. Fires blips via the project's Rust exe (single-shot `pulse`).

Examples:
  py led_watcher.py capture --index 0 --out frame.jpg
  py led_watcher.py autoroi --index 0
  py led_watcher.py probe   --index 0 --roi 610,330,60,60
  py led_watcher.py run     --index 0 --roi 610,330,60,60 --hours 8
"""
import argparse
import csv
import os
import random
import subprocess
import time
from datetime import datetime

import cv2
import numpy as np  # noqa: F401 (kept for clarity; cv2 returns numpy arrays)

SCRIPT_DIR = os.path.dirname(os.path.abspath(__file__))
DEFAULT_EXE = os.path.join(
    os.environ.get("LOCALAPPDATA", ""), "T10sKeepAwake", "target", "debug", "t10s_keep_awake.exe"
)
DEFAULT_DEVICE = "Speakers (USB Audio Device)"
FREQ = 50.0
DBFS = -24.0

# HSV classification (OpenCV ranges: H 0-179, S/V 0-255)
GREEN_H = (40, 90)
RED_H1 = (0, 12)
RED_H2 = (165, 180)
MIN_SAT = 60
MIN_VAL = 30  # the GREEN LED is dim (V median ~44); gate low on brightness, rely on saturation+hue
MIN_PIXELS = 3  # LED is a small dot; dark background contributes 0 saturated pixels, so 3 is safe

LOG_PATH = os.path.join(SCRIPT_DIR, "led_watcher.log")


def log(msg):
    line = f"{datetime.now().isoformat(timespec='seconds')}  {msg}"
    print(line, flush=True)
    try:
        with open(LOG_PATH, "a") as lf:
            lf.write(line + "\n")
    except Exception:
        pass


class Camera:
    """Self-healing camera wrapper: reopens on read failure so a transient USB/webcam hiccup
    (common when the display idles / USB power-saves) can never crash the overnight run."""

    def __init__(self, index):
        self.index = index
        self.cap = open_cam(index)

    def frame(self):
        for attempt in range(4):
            try:
                ok, f = self.cap.read()
            except Exception as e:
                ok, f = False, None
                log(f"camera read exception: {e}")
            if ok and f is not None:
                return f
            log(f"camera read failed (attempt {attempt + 1}/4); reopening index {self.index}")
            try:
                self.cap.release()
            except Exception:
                pass
            time.sleep(1.5)
            try:
                self.cap = open_cam(self.index)
            except Exception as e:
                log(f"camera reopen failed: {e}")
                time.sleep(3.0)
        log("camera unrecoverable this cycle; treating as OFF")
        return None

    def classify(self, roi):
        f = self.frame()
        return ("OFF", 0, 0) if f is None else classify(f, roi)

    def release(self):
        try:
            self.cap.release()
        except Exception:
            pass


def open_cam(index):
    cap = cv2.VideoCapture(index, cv2.CAP_DSHOW)
    if not cap.isOpened():
        cap = cv2.VideoCapture(index)
    if not cap.isOpened():
        raise SystemExit(f"could not open camera index {index}")
    cap.set(cv2.CAP_PROP_FRAME_WIDTH, 1280)
    cap.set(cv2.CAP_PROP_FRAME_HEIGHT, 720)
    # Try to lock exposure so it doesn't drift overnight (backend-dependent; hue is robust anyway).
    try:
        cap.set(cv2.CAP_PROP_AUTO_EXPOSURE, 0.25)
    except Exception:
        pass
    for _ in range(5):  # warm up / let auto settings settle
        cap.read()
        time.sleep(0.05)
    return cap


def grab(cap):
    ok, frame = cap.read()
    if not ok:
        time.sleep(0.2)
        ok, frame = cap.read()
    if not ok:
        raise RuntimeError("frame grab failed")
    return frame


def classify(frame, roi):
    """Return (state, green_px, red_px). state in {GREEN, RED, OFF}."""
    x, y, w, h = roi
    sub = frame[y:y + h, x:x + w]
    hsv = cv2.cvtColor(sub, cv2.COLOR_BGR2HSV)
    hue, sat, val = hsv[..., 0], hsv[..., 1], hsv[..., 2]
    bright = (sat >= MIN_SAT) & (val >= MIN_VAL)
    green = bright & (hue >= GREEN_H[0]) & (hue <= GREEN_H[1])
    red = bright & (((hue >= RED_H1[0]) & (hue <= RED_H1[1])) |
                    ((hue >= RED_H2[0]) & (hue <= RED_H2[1])))
    g, r = int(green.sum()), int(red.sum())
    if g >= MIN_PIXELS and g >= r:
        return "GREEN", g, r
    if r >= MIN_PIXELS and r > g:
        return "RED", g, r
    return "OFF", g, r


def debounced_wait(cam, roi, target, timeout, need=3, poll=2.0):
    """Wait until `target` state is read `need` times in a row. Returns True, or False on timeout."""
    end = time.time() + timeout
    streak = 0
    while time.time() < end:
        state, _, _ = cam.classify(roi)
        streak = streak + 1 if state == target else 0
        if streak >= need:
            return True
        time.sleep(poll)
    return False


def fire(exe, device, length_s, freq=FREQ, db=DBFS, decay=0.0):
    # freq may be a number (single tone) or a chord spec like "50+250+500" (frequencies summed).
    spec = str(freq)
    if "+" in spec:
        freq_args = ["--freqs", ",".join(p.strip() for p in spec.split("+"))]
    else:
        freq_args = ["--freq", spec]
    cmd = [exe, "pulse", "--device", device, *freq_args, f"--db={db}", "--dur", str(length_s)]
    if decay and decay > 0:
        cmd += ["--decay", str(decay)]  # exponential decay -> chime/bell character
    try:
        subprocess.run(cmd, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                        timeout=length_s + 15)
    except Exception as e:
        log(f"fire() error (pulse may not have played): {e}")


def parse_roi(s):
    parts = [int(v) for v in s.split(",")]
    if len(parts) != 4:
        raise SystemExit("--roi must be x,y,w,h")
    return tuple(parts)


def cmd_capture(args):
    cap = open_cam(args.index)
    frame = grab(cap)
    cap.release()
    cv2.imwrite(args.out, frame)
    print(f"saved {args.out}  ({frame.shape[1]}x{frame.shape[0]})")


def find_led(frame):
    """Locate the LED as the largest saturated red-or-green blob. Returns (cx, cy, area) or None.

    The camera gets bumped periodically (07-14, 07-24), which silently invalidates the ROI and makes
    every trial read as a failed wake. This finds the dot again without eyeballing pixel coordinates.
    """
    hsv = cv2.cvtColor(frame, cv2.COLOR_BGR2HSV)
    hue, sat, val = hsv[..., 0], hsv[..., 1], hsv[..., 2]
    bright = (sat >= MIN_SAT) & (val >= MIN_VAL)
    red = bright & (((hue >= RED_H1[0]) & (hue <= RED_H1[1])) |
                    ((hue >= RED_H2[0]) & (hue <= RED_H2[1])))
    green = bright & (hue >= GREEN_H[0]) & (hue <= GREEN_H[1])
    mask = (red | green).astype(np.uint8)
    n, _lab, stats, cent = cv2.connectedComponentsWithStats(mask, 8)
    if n <= 1:
        return None
    # Largest component wins; stray specks (floor reflections) are 1-2 px, the LED is >100.
    best = max(range(1, n), key=lambda i: stats[i, cv2.CC_STAT_AREA])
    return int(cent[best][0]), int(cent[best][1]), int(stats[best, cv2.CC_STAT_AREA])


def cmd_autoroi(args):
    cap = open_cam(args.index)
    try:
        frame = grab(cap)
    finally:
        cap.release()
    hit = find_led(frame)
    if hit is None:
        raise SystemExit("no LED found — is the sub powered? Try `capture` and look at the frame.")
    cx, cy, area = hit
    half = args.size // 2
    h, w = frame.shape[:2]
    x = max(0, min(cx - half, w - args.size))
    y = max(0, min(cy - half, h - args.size))
    roi = (x, y, args.size, args.size)
    state, g, r = classify(frame, roi)
    print(f"LED at ({cx},{cy})  blob={area}px  ->  classified {state} (green={g} red={r})")
    print(f"--roi {x},{y},{args.size},{args.size}")
    if args.out:
        annot = frame.copy()
        cv2.rectangle(annot, (x, y), (x + args.size, y + args.size), (0, 255, 255), 2)
        cv2.imwrite(args.out, annot)
        print(f"annotated frame -> {args.out}")
    if state == "OFF":
        raise SystemExit("WARNING: blob found but ROI classifies OFF — check thresholds before running.")


def debug_stats(frame, roi):
    """Return a string describing the brightest/saturated pixels in the ROI (for tuning)."""
    x, y, w, h = roi
    sub = frame[y:y + h, x:x + w]
    hsv = cv2.cvtColor(sub, cv2.COLOR_BGR2HSV)
    hue, sat, val = hsv[..., 0], hsv[..., 1], hsv[..., 2]
    bright = (sat >= 40) & (val >= 40)
    n = int(bright.sum())
    if n == 0:
        return "bright=0"
    return (f"bright={n:4d}  H[min/med/max]={int(hue[bright].min())}/{int(np.median(hue[bright]))}/"
            f"{int(hue[bright].max())}  S[min/med]={int(sat[bright].min())}/{int(np.median(sat[bright]))}"
            f"  V[med/max]={int(np.median(val[bright]))}/{int(val[bright].max())}")


def cmd_probe(args):
    roi = parse_roi(args.roi)
    cap = open_cam(args.index)
    print("Toggle the sub and watch the state track. Ctrl+C to stop.")
    try:
        while True:
            frame = grab(cap)
            state, g, r = classify(frame, roi)
            extra = "  " + debug_stats(frame, roi) if args.debug else ""
            print(f"{datetime.now():%H:%M:%S}  {state:5s}  green={g:5d} red={r:5d}{extra}", flush=True)
            if args.out:
                annot = frame.copy()
                x, y, w, h = roi
                cv2.rectangle(annot, (x, y), (x + w, y + h), (0, 255, 255), 2)
                cv2.imwrite(args.out, annot)
            time.sleep(args.interval)
    except KeyboardInterrupt:
        print("\nstopped")
    finally:
        cap.release()


def cmd_run(args):
    roi = parse_roi(args.roi)
    lengths = [float(x) for x in args.lengths.split(",")]
    cam = Camera(args.index)

    newfile = not os.path.exists(args.csv)
    f = open(args.csv, "a", newline="")
    wr = csv.writer(f)
    if newfile:
        wr.writerow(["timestamp", "length_ms", "woke", "wake_lag_s", "hold_s"])
        f.flush()

    log(f"RUN  lengths={[int(x) for x in lengths]} ms  for {args.hours} h  -> {args.csv}")
    log("waiting for the sub to be asleep (RED) to start...")
    debounced_wait(cam, roi, "RED", timeout=1400)

    end_time = time.time() + args.hours * 3600
    trial = 0
    try:
        while time.time() < end_time:
            block = lengths[:]
            random.shuffle(block)  # randomize order to avoid time-drift bias
            for length_ms in block:
                if time.time() >= end_time:
                    break
                trial += 1
                length_s = length_ms / 1000.0
                # Each trial is fully guarded: any error is logged and we move on — the run
                # must survive webcam hiccups, subprocess errors, etc. for the whole night.
                try:
                    if not debounced_wait(cam, roi, "RED", timeout=1400):
                        log("  (timeout waiting for sleep; continuing anyway)")
                    t0 = time.time()
                    fire(args.exe, args.device, length_s)
                    woke = debounced_wait(cam, roi, "GREEN", timeout=25, need=2, poll=1.0)
                    stamp = datetime.now().isoformat(timespec="seconds")
                    if not woke:
                        wr.writerow([stamp, f"{length_ms:.0f}", 0, "", 0])
                        f.flush()
                        log(f"[{trial}] {length_ms:.0f} ms -> NO WAKE")
                        continue
                    t_wake = time.time()
                    wake_lag = t_wake - t0
                    slept = debounced_wait(cam, roi, "RED", timeout=1500, need=3, poll=2.0)
                    hold = (time.time() - t_wake) if slept else -1
                    wr.writerow([stamp, f"{length_ms:.0f}", 1, f"{wake_lag:.1f}", f"{hold:.0f}"])
                    f.flush()
                    m, s = divmod(int(max(hold, 0)), 60)
                    tag = f"held {m}:{s:02d}" if slept else "OVER-HELD (timeout)"
                    log(f"[{trial}] {length_ms:.0f} ms -> {tag}  (wake lag {wake_lag:.1f}s)")
                except Exception as e:
                    log(f"[{trial}] {length_ms:.0f} ms -> TRIAL ERROR (recovered): {e}")
                    time.sleep(2.0)
    except KeyboardInterrupt:
        log("stopped by user")
    finally:
        f.close()
        cam.release()
        log(f"done. {trial} trials logged to {args.csv}")


def cmd_endurance(args):
    """KEEP-AN-AWAKE-SUB-AWAKE test (the production regime).

    Wake threshold != keep-alive threshold (hysteresis): the automated `run` mode only ever
    measures wake-from-sleep, but production keeps the sub perpetually awake and never wakes it.
    This mode establishes a known awake origin with a guaranteed-reset wake pulse, then fires ONLY
    the (imperceptible) candidate pulse at a sub-deadline cadence and verifies the sub never sleeps.

    A candidate that resets the standby timer survives many deadlines (stays GREEN for the whole
    window). One that does NOT reset lets the sub sleep ~one standby-timer after the last real
    signal -> caught as an early RED, usually on cycle 2. interval-min MUST be below the ~17.5 min
    standby timer or a good candidate will false-fail (we'd pulse after it already slept)."""
    roi = parse_roi(args.roi)
    if args.interval_min >= 16.5:
        log(f"WARNING: interval-min={args.interval_min} is close to/above the ~17.5 min standby "
            f"timer; a holding candidate may false-fail. Use <=15.")
    cam = Camera(args.index)
    interval_s = args.interval_min * 60.0
    dur_s = args.dur

    newfile = not os.path.exists(args.csv)
    f = open(args.csv, "a", newline="")
    wr = csv.writer(f)
    if newfile:
        wr.writerow(["timestamp", "event", "freq_hz", "db", "dur_s", "interval_min",
                     "cycle", "since_signal_s", "note"])
        f.flush()

    def record(event, cycle, since, note=""):
        wr.writerow([datetime.now().isoformat(timespec="seconds"), event, f"{args.freq:.0f}",
                     f"{args.db:.0f}", f"{dur_s:g}", f"{args.interval_min:g}", cycle,
                     f"{since:.0f}", note])
        f.flush()

    log(f"ENDURANCE  candidate {args.freq:.0f} Hz / {args.db:.0f} dBFS / {dur_s:g}s every "
        f"{args.interval_min:g} min  (keep-an-AWAKE-sub-awake)  for {args.hours} h  -> {args.csv}")

    # Establish a known timer origin with a guaranteed-reset (loud) wake pulse.
    log(f"  establishing awake origin: wake pulse {args.wake_freq:.0f} Hz / {args.wake_db:.0f} "
        f"dBFS / {args.wake_dur:g}s ...")
    fire(args.exe, args.device, args.wake_dur, args.wake_freq, args.wake_db)
    if not debounced_wait(cam, roi, "GREEN", timeout=25, need=2, poll=1.0):
        log("  WAKE FAILED — sub did not turn GREEN; aborting")
        record("abort", 0, 0, "wake pulse did not wake sub")
        f.close(); cam.release(); return
    last_signal = time.time()
    record("wake", 0, 0, "origin established")
    log("  sub AWAKE; beginning candidate cycles")

    end_time = time.time() + args.hours * 3600
    cycle = 0
    slept_early = False
    try:
        while time.time() < end_time:
            # Coast toward the next scheduled pulse, watching for an early sleep.
            fire_at = last_signal + interval_s
            red_streak = 0
            while time.time() < fire_at and time.time() < end_time:
                state, _, _ = cam.classify(roi)
                red_streak = red_streak + 1 if state == "RED" else 0
                if red_streak >= 3:
                    slept_early = True
                    break
                time.sleep(10.0)
            since = time.time() - last_signal
            if slept_early:
                m, s = divmod(int(since), 60)
                log(f"  FAIL: sub SLEPT {m}:{s:02d} after last signal on cycle {cycle} "
                    f"-> candidate does NOT hold the timer")
                record("fail", cycle, since, "slept before next pulse; candidate does not hold")
                break
            if time.time() >= end_time:
                break
            # Sub still awake at the deadline: fire the candidate; surviving past future
            # deadlines proves it reset the timer.
            cycle += 1
            fire(args.exe, args.device, dur_s, args.freq, args.db)
            still = debounced_wait(cam, roi, "GREEN", timeout=15, need=2, poll=1.0)
            last_signal = time.time()
            note = "still GREEN after pulse" if still else "WARNING: not GREEN right after pulse"
            m, s = divmod(int(since), 60)
            log(f"  cycle {cycle}: fired candidate at {m}:{s:02d} since last signal; {note}")
            record("pulse", cycle, since, note)
    except KeyboardInterrupt:
        log("  stopped by user")
    finally:
        if not slept_early and cycle >= 2:
            log(f"  HOLD CONFIRMED: candidate survived {cycle} deadlines ({args.freq:.0f} Hz / "
                f"{args.db:.0f} dBFS / {dur_s:g}s every {args.interval_min:g} min keeps an awake sub awake)")
            record("hold_confirmed", cycle, 0, f"survived {cycle} deadlines")
        f.close()
        cam.release()
        log(f"  endurance done. {cycle} candidate cycles.")


def measure_keepalive_hold(cam, roi, args, freq, dur_s, db, tag):
    """Measure the KEEP-ALIVE hold of ONE (freq/db/dur) pulse on an AWAKE sub.

    Method: fire a solid (loud) wake pulse to set a known origin, coast to just under the standby
    deadline, fire the candidate ONCE, then time how long until the sub sleeps (RED). If the
    candidate resets the timer, hold approaches the full ~17.5 min; if it does nothing, the sub
    sleeps ~(wake_hold - coast) later (a small baseline). Returns (hold_s, slept, note) or
    (None, False, reason) if a known origin could not be established (retries the wake once)."""
    coast_s = args.coast_min * 60.0
    cap_s = 1200.0  # ~20 min > full timer; safety ceiling on the sleep wait
    for attempt in range(2):
        fire(args.exe, args.device, args.wake_dur, args.wake_freq, args.wake_db)
        if not debounced_wait(cam, roi, "GREEN", timeout=25, need=2, poll=1.0):
            log(f"    {tag}: WAKE FAILED (attempt {attempt + 1}/2)")
            continue
        t_wake = time.time()
        red_streak = 0
        early = False
        while time.time() - t_wake < coast_s:
            state, _, _ = cam.classify(roi)
            red_streak = red_streak + 1 if state == "RED" else 0
            if red_streak >= 3:
                early = True
                break
            time.sleep(10.0)
        if early:
            el = (time.time() - t_wake) / 60.0
            log(f"    {tag}: wake timer ran short ({el:.1f}m < {args.coast_min:g}m coast); re-waking")
            continue
        # Sub is awake and near its deadline: fire the candidate and time the hold.
        fire(args.exe, args.device, dur_s, freq, db, getattr(args, "decay", 0.0))
        t_fire = time.time()
        slept = debounced_wait(cam, roi, "RED", timeout=cap_s, need=3, poll=2.0)
        hold = (time.time() - t_fire) if slept else cap_s
        note = "" if slept else "held past cap (>= full timer)"
        return hold, slept, note
    return None, False, "wake failed twice"


def cmd_search(args):
    """Autonomous 2-axis (level x length) search for the quietest, shortest pulse that still keeps
    an AWAKE sub awake >= target. For each length (outer), descend levels (quieter) until the
    keep-alive hold drops below target; the last passing level is that length's floor. Maps the
    frontier so we can pick the 'magical combo' minimizing both level and length."""
    roi = parse_roi(args.roi)
    # each entry is a frequency spec: a single tone "250" or a chord "50+250+500" (kept as strings)
    freqs = [x.strip() for x in args.freqs.split(",")]
    lengths = [float(x) for x in args.lengths.split(",")]
    levels = [float(x) for x in args.levels.split(",")]
    target_s = args.target_min * 60.0
    cam = Camera(args.index)

    newfile = not os.path.exists(args.csv)
    f = open(args.csv, "a", newline="")
    wr = csv.writer(f)
    if newfile:
        wr.writerow(["timestamp", "freq_hz", "db", "dur_s", "coast_min",
                     "hold_s", "held_min", "verdict", "note"])
        f.flush()

    def record(freq, db, dur_s, hold_s, verdict, note=""):
        wr.writerow([datetime.now().isoformat(timespec="seconds"), str(freq), f"{db:.0f}",
                     f"{dur_s:g}", f"{args.coast_min:g}", f"{hold_s:.0f}", f"{hold_s / 60:.1f}",
                     verdict, note])
        f.flush()

    log(f"SEARCH  freqs={freqs}Hz  lengths={['%g' % x for x in lengths]}s  "
        f"levels(dBFS)={[int(x) for x in levels]}  target>={args.target_min:g}m  "
        f"coast={args.coast_min:g}m  cap {args.hours}h  -> {args.csv}")

    end_time = time.time() + args.hours * 3600
    frontier = {}  # (freq, dur_s) -> (quietest holding db, hold_min)
    stop = False
    no_origin_streak = 0  # consecutive combos where the wake pulse never produced a GREEN read
    try:
        for freq in freqs:
            if stop:
                break
            for dur_s in lengths:
                if stop:
                    break
                last_pass = None
                for db in levels:
                    if time.time() >= end_time:
                        log("  time cap reached; stopping search")
                        stop = True
                        break
                    tag = f"{freq}Hz/{dur_s:g}s/{db:.0f}dB"
                    log(f"  --- measuring {tag}  (x{args.repeats}) ---")
                    holds = []
                    aborted = False
                    for rep in range(args.repeats):
                        if time.time() >= end_time:
                            stop = True
                            break
                        hold, slept, note = measure_keepalive_hold(cam, roi, args, freq, dur_s, db, tag)
                        if hold is None:
                            no_origin_streak += 1
                            log(f"    {tag} rep{rep + 1}: could not establish origin ({note}) "
                                f"[streak {no_origin_streak}]")
                            record(freq, db, dur_s, 0, "no_origin", note)
                            if no_origin_streak >= 2:
                                log("  ABORT: 2 consecutive wake failures — sub not waking or "
                                    "camera/LED ROI misaligned. Stopping. Re-aim ROI and relaunch.")
                                stop = True
                                aborted = True
                            break
                        no_origin_streak = 0
                        holds.append(hold)
                        rverdict = "HOLD" if hold >= target_s else "short"
                        record(freq, db, dur_s, hold, rverdict, f"{note} rep{rep + 1}".strip())
                        log(f"    {tag} rep{rep + 1}/{args.repeats}: {hold / 60:.1f} min -> {rverdict}")
                    if aborted or stop:
                        break
                    if not holds:
                        continue
                    worst = min(holds)
                    med = sorted(holds)[len(holds) // 2]
                    n_hold = sum(1 for h in holds if h >= target_s)
                    reliable = worst >= target_s  # every repeat latched -> usable in production
                    log(f"    {tag}: {n_hold}/{len(holds)} held >= {args.target_min:g}m; "
                        f"worst {worst / 60:.1f}m  med {med / 60:.1f}m  -> "
                        f"{'RELIABLE HOLD' if reliable else 'NOT reliable'}")
                    if reliable:
                        last_pass = (db, med / 60.0)
                    else:
                        log(f"    {tag} not reliable; stop descending level at "
                            f"{freq}Hz/{dur_s:g}s (quieter won't be more reliable)")
                        break
                if last_pass:
                    frontier[(freq, dur_s)] = last_pass
                    log(f"  FRONTIER @ {freq}Hz/{dur_s:g}s: quietest holding level = "
                        f"{last_pass[0]:.0f} dBFS (hold {last_pass[1]:.1f} min)")
                else:
                    log(f"  FRONTIER @ {freq}Hz/{dur_s:g}s: nothing held >= target")
    except KeyboardInterrupt:
        log("  stopped by user")
    finally:
        cam.release()
        f.close()
        if frontier:
            fr = "  ".join(f"{k[0]}Hz/{'%g' % k[1]}s:{v[0]:.0f}dB({v[1]:.0f}m)"
                           for k, v in sorted(frontier.items()))
            log(f"  SEARCH done. frontier (freq/length -> quietest holding level): {fr}")
        else:
            log("  SEARCH done. no holding combos at tested params.")


def main():
    ap = argparse.ArgumentParser(description="T10S LED hold-time profiler")
    sub = ap.add_subparsers(dest="cmd", required=True)

    c = sub.add_parser("capture")
    c.add_argument("--index", type=int, default=0)
    c.add_argument("--out", default=os.path.join(SCRIPT_DIR, "frame.jpg"))
    c.set_defaults(func=cmd_capture)

    a = sub.add_parser("autoroi", help="auto-locate the LED and print a --roi (after a camera bump)")
    a.add_argument("--index", type=int, default=0)
    a.add_argument("--size", type=int, default=100, help="ROI edge length in px")
    a.add_argument("--out", default=os.path.join(SCRIPT_DIR, "autoroi.jpg"))
    a.set_defaults(func=cmd_autoroi)

    p = sub.add_parser("probe")
    p.add_argument("--roi", required=True)
    p.add_argument("--index", type=int, default=0)
    p.add_argument("--interval", type=float, default=1.0)
    p.add_argument("--out", default=os.path.join(SCRIPT_DIR, "probe.jpg"))
    p.add_argument("--debug", action="store_true")
    p.set_defaults(func=cmd_probe)

    r = sub.add_parser("run")
    r.add_argument("--roi", required=True)
    r.add_argument("--index", type=int, default=0)
    r.add_argument("--lengths", default="300,350,400,450,500,550,600")
    r.add_argument("--hours", type=float, default=8.0)
    r.add_argument("--csv", default=os.path.join(SCRIPT_DIR, "led_holds.csv"))
    r.add_argument("--exe", default=DEFAULT_EXE)
    r.add_argument("--device", default=DEFAULT_DEVICE)
    r.set_defaults(func=cmd_run)

    e = sub.add_parser("endurance", help="keep-an-awake-sub-awake test (production regime)")
    e.add_argument("--roi", required=True)
    e.add_argument("--index", type=int, default=0)
    e.add_argument("--freq", type=float, default=40.0, help="candidate pulse frequency (Hz)")
    e.add_argument("--db", type=float, default=-45.0, help="candidate pulse level (dBFS)")
    e.add_argument("--dur", type=float, default=8.0, help="candidate pulse duration (s)")
    e.add_argument("--interval-min", dest="interval_min", type=float, default=14.0,
                   help="cadence in minutes; MUST be below the ~17.5 min standby timer")
    e.add_argument("--wake-freq", dest="wake_freq", type=float, default=50.0)
    e.add_argument("--wake-db", dest="wake_db", type=float, default=-24.0)
    e.add_argument("--wake-dur", dest="wake_dur", type=float, default=1.0)
    e.add_argument("--hours", type=float, default=1.5)
    e.add_argument("--csv", default=os.path.join(SCRIPT_DIR, "endurance.csv"))
    e.add_argument("--exe", default=DEFAULT_EXE)
    e.add_argument("--device", default=DEFAULT_DEVICE)
    e.set_defaults(func=cmd_endurance)

    s = sub.add_parser("search", help="autonomous level x length keep-alive frontier search")
    s.add_argument("--roi", required=True)
    s.add_argument("--index", type=int, default=0)
    s.add_argument("--freqs", default="40,50",
                   help="freq specs (outer axis), comma-separated; an entry may be a chord with "
                        "'+' e.g. --freqs 50+250+500,1000 (needs the chord-capable exe build)")
    s.add_argument("--lengths", default="8,2,0.8", help="candidate durations (s), any order")
    s.add_argument("--levels", default="-26,-30,-34,-38,-42",
                   help="candidate levels (dBFS), descend louder->quieter")
    s.add_argument("--target-min", dest="target_min", type=float, default=12.0,
                   help="keep-alive hold (min) required to count as HOLD")
    s.add_argument("--repeats", type=int, default=1,
                   help="measurements per combo; combo is RELIABLE only if all repeats hold")
    s.add_argument("--coast-min", dest="coast_min", type=float, default=12.0,
                   help="minutes to coast after the wake pulse before firing the candidate")
    s.add_argument("--decay", type=float, default=0.0,
                   help="candidate exponential decay time constant (s); >0 = chime/bell character")
    s.add_argument("--wake-freq", dest="wake_freq", type=float, default=50.0)
    s.add_argument("--wake-db", dest="wake_db", type=float, default=-24.0)
    s.add_argument("--wake-dur", dest="wake_dur", type=float, default=2.0)
    s.add_argument("--hours", type=float, default=10.0)
    s.add_argument("--csv", default=os.path.join(SCRIPT_DIR, "search.csv"))
    s.add_argument("--exe", default=DEFAULT_EXE)
    s.add_argument("--device", default=DEFAULT_DEVICE)
    s.set_defaults(func=cmd_search)

    args = ap.parse_args()
    args.func(args)


if __name__ == "__main__":
    main()
