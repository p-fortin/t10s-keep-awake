#!/usr/bin/env py
"""Regenerate the research charts from the raw test-and-learn CSVs.

Usage:  py scripts/make_charts.py
Outputs PNG + SVG into docs/charts/. Reproducible: raw data lives in test-and-learn/*.csv.
Deps: matplotlib, numpy.
"""
import csv
import os
from collections import defaultdict

import matplotlib

matplotlib.use("Agg")
import matplotlib.pyplot as plt
import numpy as np

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
DATA = os.path.join(ROOT, "test-and-learn")
OUT = os.path.join(ROOT, "docs", "charts")
os.makedirs(OUT, exist_ok=True)

# Okabe-Ito colorblind-safe palette
INK = "#222222"
BLUE = "#0072B2"
ORANGE = "#E69F00"
GREEN = "#009E73"
RED = "#D55E00"
PURPLE = "#CC79A7"
GRID = "#DDDDDD"

plt.rcParams.update({
    "figure.dpi": 130,
    "savefig.dpi": 130,
    "font.family": "DejaVu Sans",
    "font.size": 11,
    "axes.titlesize": 13,
    "axes.titleweight": "bold",
    "axes.edgecolor": INK,
    "axes.labelcolor": INK,
    "text.color": INK,
    "xtick.color": INK,
    "ytick.color": INK,
    "axes.grid": True,
    "grid.color": GRID,
    "grid.linewidth": 0.8,
    "axes.spines.top": False,
    "axes.spines.right": False,
})


def save(fig, name):
    fig.tight_layout()
    for ext in ("png", "svg"):
        fig.savefig(os.path.join(OUT, f"{name}.{ext}"), bbox_inches="tight")
    plt.close(fig)
    print(f"wrote {name}.png/.svg")


def read_search(fname):
    """Read a search-family CSV (freq_hz, db, dur_s, hold_s, held_min, verdict)."""
    rows = []
    path = os.path.join(DATA, fname)
    if not os.path.exists(path):
        return rows
    for r in csv.DictReader(open(path)):
        try:
            rows.append({
                "freq": r["freq_hz"],
                "db": float(r["db"]),
                "dur": float(r["dur_s"]),
                "hold_min": float(r["held_min"]),
            })
        except (KeyError, ValueError):
            pass
    return rows


def chart_frequency_response():
    freqs, floors = [], []
    for r in csv.DictReader(open(os.path.join(DATA, "detector_floor.csv"))):
        freqs.append(float(r["freq_hz"]))
        floors.append(float(r["floor_dbfs"]))
    fig, ax = plt.subplots(figsize=(8.2, 4.6))
    ax.plot(freqs, floors, "-o", color=BLUE, lw=2.4, ms=7, zorder=3)
    ax.set_xscale("log")
    ax.set_xlabel("Pulse frequency (Hz, log scale)")
    ax.set_ylabel("Keep-alive detection floor (dBFS)")
    ax.set_title("T10S auto-sense detector: how quiet a pulse it will still latch, by frequency")
    ax.set_xticks([40, 100, 250, 500, 1000, 2000, 4000, 8000, 16000])
    ax.get_xaxis().set_major_formatter(matplotlib.ticker.ScalarFormatter())
    ax.set_ylim(-36, -8)
    ax.axhline(-12, color=RED, ls="--", lw=1.2, zorder=1)
    ax.annotate("−12 dBFS safety ceiling", (44, -11.4), color=RED, fontsize=9)
    ax.annotate("most sensitive\n(−32, 250–2 kHz)", (500, -32), textcoords="offset points",
                xytext=(-6, 10), ha="center", color=GREEN, fontsize=9, fontweight="bold")
    ax.annotate("still latching at 22 kHz\n— no cutoff", (16000, -12),
                textcoords="offset points", xytext=(-70, 14), color=ORANGE, fontsize=9,
                fontweight="bold")
    ax.annotate("lower = quieter\npulse still detected", (44, -26), fontsize=8.5, color="#555555")
    save(fig, "detector_frequency_response")


def chart_wake_erraticness():
    d = defaultdict(list)
    path = os.path.join(DATA, "led_holds.csv")
    for r in csv.DictReader(open(path)):
        if r.get("woke") == "1" and int(r["hold_s"]) > 0:
            d[int(r["length_ms"])].append(int(r["hold_s"]) / 60.0)
    lengths = sorted(d)
    fig, ax = plt.subplots(figsize=(8.2, 4.6))
    for L in lengths:
        ys = d[L]
        xs = np.full(len(ys), L) + np.random.default_rng(L).normal(0, L * 0.012, len(ys))
        ax.scatter(xs, ys, s=26, color=BLUE, alpha=0.55, edgecolor="none", zorder=3)
        ax.plot([L * 0.97, L * 1.03], [min(ys)] * 2, color=RED, lw=2.2, zorder=4)
    ax.scatter([], [], s=26, color=BLUE, alpha=0.55, label="individual trial")
    ax.plot([], [], color=RED, lw=2.2, label="worst-case (min) — governs the pulse interval")
    ax.set_xlabel("Blip length (ms)")
    ax.set_ylabel("Hold time before standby (min)")
    ax.set_title("Hold time is erratic: the same blip gives wildly different holds run-to-run")
    ax.legend(frameon=False, fontsize=9, loc="upper left")
    ax.set_ylim(0, 18)
    save(fig, "wake_hold_erraticness")


def chart_hold_vs_level():
    rows = read_search("search.csv") + read_search("phase2_floor.csv") + read_search("phase6_highfreq.csv")
    rows = [r for r in rows if r["dur"] == 8.0 and "+" not in r["freq"]]
    by_freq = defaultdict(list)
    for r in rows:
        by_freq[int(float(r["freq"]))].append((r["db"], r["hold_min"]))
    show = [50, 500, 4000, 16000]
    colors = {50: BLUE, 500: GREEN, 4000: ORANGE, 16000: PURPLE}
    fig, ax = plt.subplots(figsize=(8.2, 4.6))
    for f in show:
        pts = sorted(set(by_freq.get(f, [])))
        if not pts:
            continue
        xs = [p[0] for p in pts]
        ys = [p[1] for p in pts]
        lab = f"{f} Hz" if f < 1000 else f"{f // 1000} kHz"
        ax.plot(xs, ys, "-o", color=colors[f], lw=2, ms=6, label=lab)
    ax.axhline(12, color="#999999", ls=":", lw=1)
    ax.annotate("12-min 'reliable hold' bar", (-43, 12.6), fontsize=8.5, color="#666666")
    ax.set_xlabel("Pulse level (dBFS) — quieter →")
    ax.set_ylabel("Keep-alive hold (min)")
    ax.set_title("A sharp cliff: a few dB below the floor and the pulse does nothing")
    ax.invert_xaxis()
    ax.legend(frameon=False, fontsize=9, title="frequency", loc="center left")
    ax.set_ylim(0, 18)
    save(fig, "hold_vs_level_cliff")


def chart_chord_vs_single():
    labels = ["50 Hz\ntone", "250 Hz\ntone", "500 Hz\ntone", "50+250+500\nchord"]
    floors = [-24, -32, -32, -38]
    colors = [BLUE, BLUE, BLUE, GREEN]
    fig, ax = plt.subplots(figsize=(7.4, 4.6))
    bars = ax.bar(labels, floors, color=colors, width=0.62, zorder=3)
    for b, v in zip(bars, floors):
        # Sit just inside the bar's bottom edge — white-on-white if placed below it.
        ax.annotate(f"{v:g} dBFS", (b.get_x() + b.get_width() / 2, v + 0.9),
                    ha="center", va="bottom", color="white", fontsize=9, fontweight="bold")
    ax.set_ylabel("Quietest level that still latches (dBFS)")
    ax.set_title("A chord latches ~6 dB quieter per tone than any single component")
    ax.set_ylim(-40, 0)
    # Inside the green bar: below it this collides with the x tick label.
    # Keep it narrower than the bar (width 0.62) or it clips at both edges.
    ax.annotate("each tone\n6 dB quieter", (3, -1.4),
                ha="center", va="top", color="white", fontsize=8.5, fontweight="bold")
    save(fig, "chord_vs_single_tone")


if __name__ == "__main__":
    chart_frequency_response()
    chart_wake_erraticness()
    chart_hold_vs_level()
    chart_chord_vs_single()
    print(f"charts -> {OUT}")
