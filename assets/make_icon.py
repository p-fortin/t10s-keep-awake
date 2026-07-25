#!/usr/bin/env py
"""Generate the tray icons (16/20/24/32/48 px, 32-bit BGRA) with no external deps.

Design: a **sonar ping** — a solid woofer dot with two arcs radiating right, on a dark rounded
tile. It reads as "something is being emitted on purpose", which is exactly what the app does,
and the arcs stay legible when Windows squashes it to 16 px.

Two variants:
  tray.ico       green arcs   — engine healthy, pulses landing
  tray_warn.ico  amber arcs, broken, with a gap + dot ("!") — engine NOT pulsing

Rendered at 8x and box-downsampled, so the curves are properly antialiased rather than jagged.

Run: py assets/make_icon.py   (writes assets/tray.ico and assets/tray_warn.ico)
"""
import math
import os
import struct

SS = 8  # supersample factor

BG = (24, 25, 28)          # dark tile
BG_EDGE = (44, 46, 52)     # subtle rim so the tile reads on any taskbar colour
OK = (32, 220, 110)        # healthy green
OK_HOT = (150, 255, 190)   # brighter core of the dot
WARN = (255, 176, 32)      # amber
WARN_HOT = (255, 224, 150)


def _shade(base, hot, t):
    """Blend base->hot by t in 0..1."""
    return tuple(round(base[i] + (hot[i] - base[i]) * t) for i in range(3))


def render(size, warn=False):
    """Return size*size list of (B,G,R,A), antialiased via SSxSS supersampling."""
    S = size * SS
    c = (S - 1) / 2.0
    corner = S * 0.22
    half = S / 2.0 - 0.5

    # Sonar geometry: dot slightly left of centre, arcs sweeping to the right.
    dot_cx = c - S * 0.17
    dot_r = S * 0.115
    arcs = [(S * 0.30, S * 0.052), (S * 0.46, S * 0.052)]
    spread = math.radians(58)  # half-angle of each arc

    acc = [[0.0, 0.0, 0.0, 0.0] for _ in range(size * size)]

    for y in range(S):
        for x in range(S):
            dx, dy = x - c, y - c
            # rounded-square tile mask
            ax, ay = abs(dx) - (half - corner), abs(dy) - (half - corner)
            if ax > 0 and ay > 0:
                inside = (ax * ax + ay * ay) <= corner * corner
            else:
                inside = abs(dx) <= half and abs(dy) <= half
            if not inside:
                continue

            # base tile, with a slightly lighter rim
            edge_d = min(half - abs(dx), half - abs(dy))
            col = BG_EDGE if edge_d < S * 0.03 else BG
            a = 255

            # woofer dot
            dd = math.hypot(x - dot_cx, dy)
            if dd <= dot_r:
                t = max(0.0, 1.0 - dd / dot_r)
                col = _shade(WARN if warn else OK, WARN_HOT if warn else OK_HOT, t * 0.9)

            # radiating arcs
            else:
                ang = math.atan2(dy, x - dot_cx)
                for i, (r, w) in enumerate(arcs):
                    if abs(ang) <= spread and abs(dd - r) <= w:
                        if warn:
                            # break the outer arc into a gap + tick so it reads as "!"
                            if i == 1 and abs(ang) < math.radians(16):
                                continue
                            col = WARN
                        else:
                            col = OK
                        break

            px = acc[(y // SS) * size + (x // SS)]
            px[0] += col[2]  # B
            px[1] += col[1]  # G
            px[2] += col[0]  # R
            px[3] += a

    n = SS * SS
    out = []
    for p in acc:
        out.append((round(p[0] / n), round(p[1] / n), round(p[2] / n), round(p[3] / n)))
    return out


def ico_image(size, warn):
    pixels = render(size, warn)
    hdr = struct.pack("<IiiHHIIiiII", 40, size, size * 2, 1, 32, 0, 0, 0, 0, 0, 0)
    xor = bytearray()
    for y in range(size - 1, -1, -1):  # rows are bottom-up in an ICO
        for x in range(size):
            b, g, r, a = pixels[y * size + x]
            xor += bytes((b, g, r, a))
    and_stride = ((size + 31) // 32) * 4
    and_mask = bytes(and_stride * size)  # zero => the alpha channel governs
    return hdr + bytes(xor) + and_mask


def build(path, warn=False, sizes=(16, 20, 24, 32, 48)):
    images = [ico_image(s, warn) for s in sizes]
    out = struct.pack("<HHH", 0, 1, len(images))  # ICONDIR
    offset = 6 + 16 * len(images)
    for s, img in zip(sizes, images):
        w = 0 if s >= 256 else s
        out += struct.pack("<BBBBHHII", w, w, 0, 0, 1, 32, len(img), offset)
        offset += len(img)
    for img in images:
        out += img
    with open(path, "wb") as f:
        f.write(out)
    print(f"wrote {path} ({len(out)} bytes, sizes={sizes}, warn={warn})")


if __name__ == "__main__":
    here = os.path.dirname(os.path.abspath(__file__))
    build(os.path.join(here, "tray.ico"), warn=False)
    build(os.path.join(here, "tray_warn.ico"), warn=True)


# --- help "?" icon -----------------------------------------------------------------------------
HELP_FG = (90, 106, 128)     # slate grey-blue, reads as UI chrome rather than an alert
HELP_BG = (255, 255, 255, 0)  # transparent


def _seg_dist(px, py, ax, ay, bx, by):
    """Distance from point to line segment AB — used to stroke the tail with round caps."""
    vx, vy = bx - ax, by - ay
    wx, wy = px - ax, py - ay
    L2 = vx * vx + vy * vy
    t = 0.0 if L2 == 0 else max(0.0, min(1.0, (wx * vx + wy * vy) / L2))
    return math.hypot(px - (ax + t * vx), py - (ay + t * vy))


def render_help(size):
    """A circled question mark, drawn as strokes: ring + hook arc + tail + dot.

    The hook ends where the tail begins so the glyph reads as one connected mark rather than
    detached pieces — which is exactly what breaks first at 16 px.
    """
    S = size * SS
    c = (S - 1) / 2.0
    r_out = S * 0.46
    ring_w = S * 0.075

    hook_cy = c - S * 0.115
    hook_r = S * 0.150
    stroke = S * 0.075
    # Hook sweeps from lower-left, over the top, round to the right and slightly under.
    hook_from, hook_to = math.radians(-205), math.radians(35)
    # Where the hook ends, the tail takes over and runs down to the centre.
    hx = c + hook_r * math.cos(hook_to)
    hy = hook_cy + hook_r * math.sin(hook_to)
    tail_bx, tail_by = c, c + S * 0.105
    dot_cy = c + S * 0.245
    dot_r = S * 0.058

    acc = [[0.0, 0.0, 0.0, 0.0] for _ in range(size * size)]
    for y in range(S):
        for x in range(S):
            dx, dy = x - c, y - c
            hit = False

            if abs(math.hypot(dx, dy) - r_out) <= ring_w / 2:
                hit = True
            if not hit:
                hd = math.hypot(x - c, y - hook_cy)
                if abs(hd - hook_r) <= stroke / 2:
                    ang = math.atan2(y - hook_cy, x - c)
                    a1 = ang if ang >= hook_from else ang + 2 * math.pi
                    if hook_from <= a1 <= hook_to + 2 * math.pi and (
                        hook_from <= ang <= hook_to or a1 <= hook_to + 2 * math.pi * 0
                    ):
                        hit = True
            if not hit and _seg_dist(x, y, hx, hy, tail_bx, tail_by) <= stroke / 2:
                hit = True
            if not hit and math.hypot(dx, y - dot_cy) <= dot_r:
                hit = True

            if hit:
                px = acc[(y // SS) * size + (x // SS)]
                px[0] += HELP_FG[2]
                px[1] += HELP_FG[1]
                px[2] += HELP_FG[0]
                px[3] += 255

    n = SS * SS
    return [(round(p[0] / n), round(p[1] / n), round(p[2] / n), round(p[3] / n)) for p in acc]


def help_ico_image(size):
    pixels = render_help(size)
    hdr = struct.pack("<IiiHHIIiiII", 40, size, size * 2, 1, 32, 0, 0, 0, 0, 0, 0)
    xor = bytearray()
    for y in range(size - 1, -1, -1):
        for x in range(size):
            b, g, r, a = pixels[y * size + x]
            xor += bytes((b, g, r, a))
    and_stride = ((size + 31) // 32) * 4
    return hdr + bytes(xor) + bytes(and_stride * size)


def build_help(path, sizes=(16, 20, 24, 32)):
    images = [help_ico_image(s) for s in sizes]
    out = struct.pack("<HHH", 0, 1, len(images))
    offset = 6 + 16 * len(images)
    for s, img in zip(sizes, images):
        out += struct.pack("<BBBBHHII", s, s, 0, 0, 1, 32, len(img), offset)
        offset += len(img)
    for img in images:
        out += img
    with open(path, "wb") as f:
        f.write(out)
    print(f"wrote {path} ({len(out)} bytes, sizes={sizes})")


def build_rgba(path, size=64):
    """Raw RGBA bytes of the tray icon, for tao's `Icon::from_rgba` (window/taskbar icon).

    tao wants raw pixels, and pulling in an image decoder just to unpack our own ICO would be
    silly — so emit the same artwork in the format it actually asks for.
    """
    px = render(size, warn=False)
    out = bytearray()
    for b, g, r, a in px:
        out += bytes((r, g, b, a))
    with open(path, "wb") as f:
        f.write(out)
    print(f"wrote {path} ({len(out)} bytes, {size}x{size} RGBA)")
