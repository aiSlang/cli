#!/usr/bin/env python3
"""portfolio-pnl — render a single self-contained dashboard.html.

Reads holdings + prices from the DB, calls analyze.build_report, and
emits ONE static HTML file with everything inline: all CSS in a single
<style> tag, all charts hand-authored as inline SVG strings, no external
requests, and only a tiny bit of inline JS for the drill-down toggles
(the page is fully viewable with JS disabled — <details> works natively).

Standard library only. No numpy / pandas / matplotlib / jinja.

    python3 scripts/render_dashboard.py [--db PATH] [--holdings PATH] [--out PATH]
"""
from __future__ import annotations

import argparse
import datetime
import html
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import db as dbmod          # noqa: E402
import portfolio as pf      # noqa: E402
import analyze as az        # noqa: E402

# ─────────────────────────────────────────────────────────────────────
# palette (aiSlang dark developer aesthetic)
# ─────────────────────────────────────────────────────────────────────
BG = "#0d1117"
PANEL = "#161b22"
PANEL2 = "#1c2230"
BORDER = "#30363d"
TEXT = "#c9d1d9"
MUTED = "#8b949e"
AMBER = "#f0883e"
GREEN = "#3fb950"
RED = "#f85149"
NEUTRAL = "#6e7681"

DASH = "—"


# ─────────────────────────────────────────────────────────────────────
# formatting helpers (None-safe)
# ─────────────────────────────────────────────────────────────────────
def esc(x) -> str:
    return html.escape("" if x is None else str(x))


def fmt_money(x, dp: int = 2) -> str:
    if x is None:
        return DASH
    return f"${x:,.{dp}f}"


def fmt_pct(x, dp: int = 2) -> str:
    """x is a fraction (0.23 -> 23.00%)."""
    if x is None:
        return DASH
    return f"{x * 100:.{dp}f}%"


def fmt_num(x, dp: int = 2) -> str:
    if x is None:
        return DASH
    return f"{x:,.{dp}f}"


def sign_color(x) -> str:
    if x is None:
        return MUTED
    return GREEN if x >= 0 else RED


def signed_pct(x, dp: int = 2) -> str:
    if x is None:
        return DASH
    s = "+" if x >= 0 else ""
    return f"{s}{x * 100:.{dp}f}%"


def signed_money(x, dp: int = 2) -> str:
    if x is None:
        return DASH
    s = "+" if x >= 0 else "-"
    return f"{s}${abs(x):,.{dp}f}"


def _minmax(vals):
    vals = [v for v in vals if v is not None]
    if not vals:
        return 0.0, 1.0
    lo, hi = min(vals), max(vals)
    if lo == hi:
        pad = abs(lo) * 0.05 or 1.0
        return lo - pad, hi + pad
    return lo, hi


def _scale(v, lo, hi, out_lo, out_hi):
    if hi == lo:
        return (out_lo + out_hi) / 2.0
    return out_lo + (v - lo) / (hi - lo) * (out_hi - out_lo)


# ─────────────────────────────────────────────────────────────────────
# SVG builder helpers — each returns a self-contained <svg> string
# ─────────────────────────────────────────────────────────────────────
def sparkline(values, w=560, h=90, color=AMBER) -> str:
    pts = [v for v in values if v is not None]
    if len(pts) < 2:
        return _empty_svg(w, h, "no series")
    lo, hi = _minmax(pts)
    pad = 6
    n = len(pts)
    coords = []
    for i, v in enumerate(pts):
        x = _scale(i, 0, n - 1, pad, w - pad)
        y = _scale(v, lo, hi, h - pad, pad)
        coords.append((x, y))
    poly = " ".join(f"{x:.2f},{y:.2f}" for x, y in coords)
    area = f"{pad},{h - pad} " + poly + f" {w - pad},{h - pad}"
    up = pts[-1] >= pts[0]
    stroke = GREEN if up else RED
    fill_id = "spark_up" if up else "spark_dn"
    lx, ly = coords[-1]
    return f"""<svg viewBox="0 0 {w} {h}" width="100%" height="{h}" preserveAspectRatio="none" role="img" aria-label="value sparkline">
  <defs><linearGradient id="{fill_id}" x1="0" y1="0" x2="0" y2="1">
    <stop offset="0" stop-color="{stroke}" stop-opacity="0.28"/>
    <stop offset="1" stop-color="{stroke}" stop-opacity="0"/>
  </linearGradient></defs>
  <polygon points="{area}" fill="url(#{fill_id})"/>
  <polyline points="{poly}" fill="none" stroke="{stroke}" stroke-width="2" stroke-linejoin="round" stroke-linecap="round"/>
  <circle cx="{lx:.2f}" cy="{ly:.2f}" r="3" fill="{stroke}"/>
</svg>"""


def donut(segments, size=180, thickness=34) -> str:
    """segments: list of (label, value, color). Renders an SVG-arc donut."""
    segments = [(l, v, c) for (l, v, c) in segments if v and v > 0]
    total = sum(v for _, v, _ in segments)
    if total <= 0:
        return _empty_svg(size, size, "no allocation")
    cx = cy = size / 2.0
    r = (size - thickness) / 2.0
    import math
    arcs = []
    ang = -math.pi / 2.0
    for label, v, color in segments:
        frac = v / total
        sweep = frac * 2 * math.pi
        a0, a1 = ang, ang + sweep
        x0 = cx + r * math.cos(a0)
        y0 = cy + r * math.sin(a0)
        x1 = cx + r * math.cos(a1)
        y1 = cy + r * math.sin(a1)
        large = 1 if sweep > math.pi else 0
        arcs.append(
            f'<path d="M {x0:.2f} {y0:.2f} A {r:.2f} {r:.2f} 0 {large} 1 {x1:.2f} {y1:.2f}" '
            f'fill="none" stroke="{color}" stroke-width="{thickness}"><title>{esc(label)}: {frac*100:.1f}%</title></path>'
        )
        ang = a1
    inner = "".join(arcs)
    return f"""<svg viewBox="0 0 {size} {size}" width="{size}" height="{size}" role="img" aria-label="allocation donut">
  <circle cx="{cx}" cy="{cy}" r="{r}" fill="none" stroke="{BORDER}" stroke-width="{thickness}" opacity="0.35"/>
  {inner}
  <text x="{cx}" y="{cy-4}" text-anchor="middle" fill="{TEXT}" font-size="15" font-weight="600">Alloc</text>
  <text x="{cx}" y="{cy+14}" text-anchor="middle" fill="{MUTED}" font-size="11">by weight</text>
</svg>"""


def gauge(safe, mid, aggr, w=340, h=46, show_labels=True) -> str:
    """3-segment horizontal barbell bar: safe(green)/mid(amber)/aggr(red)."""
    parts = [(safe, GREEN, "safe"), (mid, AMBER, "mid"), (aggr, RED, "aggr")]
    total = sum(p[0] or 0.0 for p in parts)
    if total <= 0:
        return _empty_svg(w, h, "no barbell data")
    pad = 2
    inner_w = w - 2 * pad
    x = pad
    rects, labels = [], []
    for val, color, name in parts:
        val = val or 0.0
        seg_w = inner_w * (val / total)
        rects.append(
            f'<rect x="{x:.2f}" y="{pad}" width="{max(0,seg_w):.2f}" height="{h-2*pad-(16 if show_labels else 0)}" '
            f'fill="{color}" rx="2"><title>{name}: {val*100:.1f}%</title></rect>'
        )
        if show_labels and seg_w > 26:
            labels.append(
                f'<text x="{x+seg_w/2:.2f}" y="{h-4}" text-anchor="middle" fill="{MUTED}" '
                f'font-size="10">{name} {val*100:.0f}%</text>'
            )
        x += seg_w
    return f"""<svg viewBox="0 0 {w} {h}" width="100%" height="{h}" role="img" aria-label="barbell allocation">
  {''.join(rects)}
  {''.join(labels)}
</svg>"""


def line_chart(series, w=520, h=200, color=AMBER, ref=None, ref_label=None) -> str:
    """series: list of (ts, price). Optional horizontal dashed ref line."""
    pts = [(t, p) for t, p in series if p is not None]
    if len(pts) < 2:
        return _empty_svg(w, h, "insufficient price history")
    prices = [p for _, p in pts]
    vals = list(prices)
    if ref is not None:
        vals = vals + [ref]
    lo, hi = _minmax(vals)
    padL, padR, padT, padB = 8, 8, 10, 22
    n = len(pts)
    coords = []
    for i, (_, p) in enumerate(pts):
        x = _scale(i, 0, n - 1, padL, w - padR)
        y = _scale(p, lo, hi, h - padB, padT)
        coords.append((x, y))
    poly = " ".join(f"{x:.2f},{y:.2f}" for x, y in coords)
    last_p = prices[-1]
    up = last_p >= prices[0]
    stroke = GREEN if up else RED
    extras = ""
    if ref is not None:
        ry = _scale(ref, lo, hi, h - padB, padT)
        extras += (
            f'<line x1="{padL}" y1="{ry:.2f}" x2="{w-padR}" y2="{ry:.2f}" '
            f'stroke="{AMBER}" stroke-width="1.3" stroke-dasharray="5 4" opacity="0.9"/>'
        )
        if ref_label:
            extras += (f'<text x="{padL+3}" y="{ry-4:.2f}" fill="{AMBER}" font-size="10">'
                       f'{esc(ref_label)}</text>')
    lx, ly = coords[-1]
    dot = f'<circle cx="{lx:.2f}" cy="{ly:.2f}" r="3" fill="{stroke}"/>'
    t0 = esc(pts[0][0])
    t1 = esc(pts[-1][0])
    axis = (f'<text x="{padL}" y="{h-6}" fill="{MUTED}" font-size="10">{t0}</text>'
            f'<text x="{w-padR}" y="{h-6}" text-anchor="end" fill="{MUTED}" font-size="10">{t1}</text>')
    return f"""<svg viewBox="0 0 {w} {h}" width="100%" height="{h}" role="img" aria-label="price line chart">
  <polyline points="{poly}" fill="none" stroke="{stroke}" stroke-width="1.8" stroke-linejoin="round"/>
  {extras}{dot}{axis}
</svg>"""


def area_chart(series, w=560, h=180, color=RED) -> str:
    """Underwater / drawdown area chart. series: list of (ts, dd_fraction<=0)."""
    pts = [(t, d) for t, d in series if d is not None]
    if len(pts) < 2:
        return _empty_svg(w, h, "no drawdown series")
    dds = [d for _, d in pts]
    lo = min(dds + [0.0])   # most negative
    hi = 0.0
    padL, padR, padT, padB = 8, 8, 10, 20
    n = len(pts)
    coords = []
    for i, (_, d) in enumerate(pts):
        x = _scale(i, 0, n - 1, padL, w - padR)
        y = _scale(d, lo, hi, h - padB, padT)
        coords.append((x, y))
    zero_y = _scale(0.0, lo, hi, h - padB, padT)
    poly = " ".join(f"{x:.2f},{y:.2f}" for x, y in coords)
    area = (f"{coords[0][0]:.2f},{zero_y:.2f} " + poly +
            f" {coords[-1][0]:.2f},{zero_y:.2f}")
    worst = min(dds)
    return f"""<svg viewBox="0 0 {w} {h}" width="100%" height="{h}" preserveAspectRatio="none" role="img" aria-label="drawdown area chart">
  <defs><linearGradient id="uw" x1="0" y1="0" x2="0" y2="1">
    <stop offset="0" stop-color="{RED}" stop-opacity="0.05"/>
    <stop offset="1" stop-color="{RED}" stop-opacity="0.45"/>
  </linearGradient></defs>
  <line x1="{padL}" y1="{zero_y:.2f}" x2="{w-padR}" y2="{zero_y:.2f}" stroke="{BORDER}" stroke-width="1"/>
  <polygon points="{area}" fill="url(#uw)"/>
  <polyline points="{poly}" fill="none" stroke="{RED}" stroke-width="1.5"/>
  <text x="{w-padR}" y="{h-5}" text-anchor="end" fill="{MUTED}" font-size="10">max DD {worst*100:.1f}%</text>
</svg>"""


def convexity_chart(curve, w=520, h=200) -> str:
    """curve: list of {shock, convexity}. Line chart, x=shock, y=convexity."""
    pts = [(c["shock"], c["convexity"]) for c in curve if c.get("convexity") is not None]
    if len(pts) < 2:
        return _empty_svg(w, h, "no convexity curve")
    xs = [s for s, _ in pts]
    ys = [c for _, c in pts]
    xlo, xhi = min(xs), max(xs)
    ylo, yhi = _minmax(ys + [0.0])
    padL, padR, padT, padB = 40, 12, 12, 24
    coords = []
    for s, c in pts:
        x = _scale(s, xlo, xhi, padL, w - padR)
        y = _scale(c, ylo, yhi, h - padB, padT)
        coords.append((x, y))
    poly = " ".join(f"{x:.2f},{y:.2f}" for x, y in coords)
    zero_y = _scale(0.0, ylo, yhi, h - padB, padT)
    dots = "".join(
        f'<circle cx="{x:.2f}" cy="{y:.2f}" r="3" fill="{AMBER}"><title>±{s*100:.0f}% shock: {c:+.4g}</title></circle>'
        for (x, y), (s, c) in zip(coords, pts))
    ticks = "".join(
        f'<text x="{_scale(s, xlo, xhi, padL, w-padR):.2f}" y="{h-6}" text-anchor="middle" '
        f'fill="{MUTED}" font-size="10">±{int(s*100)}%</text>' for s in xs)
    return f"""<svg viewBox="0 0 {w} {h}" width="100%" height="{h}" role="img" aria-label="convexity curve">
  <line x1="{padL}" y1="{zero_y:.2f}" x2="{w-padR}" y2="{zero_y:.2f}" stroke="{BORDER}" stroke-width="1" stroke-dasharray="3 3"/>
  <text x="4" y="{padT+8}" fill="{MUTED}" font-size="10">convexity</text>
  <polyline points="{poly}" fill="none" stroke="{AMBER}" stroke-width="2"/>
  {dots}{ticks}
</svg>"""


def heatmap(corr, w=380) -> str:
    """corr: {sym: {sym: value_or_None}}. SVG grid, red↔neutral↔green -1..+1."""
    syms = list(corr.keys())
    n = len(syms)
    if n == 0:
        return _empty_svg(w, 120, "no correlation data")
    label_w = 66
    cell = max(26, min(52, (w - label_w) // max(1, n)))
    grid_w = label_w + cell * n
    grid_h = label_w + cell * n
    els = []
    # column headers (rotated)
    for j, s in enumerate(syms):
        cx = label_w + j * cell + cell / 2
        els.append(
            f'<text x="{cx:.1f}" y="{label_w-6}" text-anchor="middle" fill="{MUTED}" '
            f'font-size="9" transform="rotate(-40 {cx:.1f} {label_w-6})">{esc(s)}</text>')
    for i, rs in enumerate(syms):
        ry = label_w + i * cell + cell / 2
        els.append(
            f'<text x="{label_w-6}" y="{ry+3:.1f}" text-anchor="end" fill="{MUTED}" '
            f'font-size="9">{esc(rs)}</text>')
        for j, cs in enumerate(syms):
            v = corr[rs].get(cs)
            x = label_w + j * cell
            y = label_w + i * cell
            fill = _corr_color(v)
            txt = "·" if v is None else f"{v:+.2f}"
            tc = TEXT if v is not None and abs(v) > 0.55 else MUTED
            els.append(
                f'<rect x="{x}" y="{y}" width="{cell-1}" height="{cell-1}" fill="{fill}" rx="2">'
                f'<title>{esc(rs)} / {esc(cs)}: {txt}</title></rect>'
                f'<text x="{x+cell/2:.1f}" y="{y+cell/2+3:.1f}" text-anchor="middle" '
                f'fill="{tc}" font-size="9">{txt}</text>')
    return (f'<svg viewBox="0 0 {grid_w} {grid_h}" width="100%" height="{grid_h}" '
            f'style="max-width:{grid_w}px;height:auto" role="img" aria-label="correlation heatmap">'
            + "".join(els) + "</svg>")


def _corr_color(v) -> str:
    if v is None:
        return "#21262d"
    v = max(-1.0, min(1.0, v))
    # -1 red, 0 neutral panel, +1 green
    if v >= 0:
        return _mix("#2b3038", GREEN, v)
    return _mix("#2b3038", RED, -v)


def _mix(c0, c1, t) -> str:
    t = max(0.0, min(1.0, t))
    a = tuple(int(c0[i:i+2], 16) for i in (1, 3, 5))
    b = tuple(int(c1[i:i+2], 16) for i in (1, 3, 5))
    m = tuple(round(a[k] + (b[k] - a[k]) * t) for k in range(3))
    return "#%02x%02x%02x" % m


def _empty_svg(w, h, msg) -> str:
    return (f'<svg viewBox="0 0 {w} {h}" width="100%" height="{h}" role="img" aria-label="{esc(msg)}">'
            f'<text x="{w/2}" y="{h/2}" text-anchor="middle" fill="{MUTED}" font-size="12">{esc(msg)}</text></svg>')


def weight_bar(frac, w=90, h=10, color=AMBER) -> str:
    frac = max(0.0, min(1.0, frac or 0.0))
    return (f'<svg viewBox="0 0 {w} {h}" width="{w}" height="{h}" class="wbar">'
            f'<rect x="0" y="0" width="{w}" height="{h}" fill="{BORDER}" rx="2" opacity="0.5"/>'
            f'<rect x="0" y="0" width="{w*frac:.2f}" height="{h}" fill="{color}" rx="2"/></svg>')


# ─────────────────────────────────────────────────────────────────────
# drawdown series from a value series
# ─────────────────────────────────────────────────────────────────────
def drawdown_series(vseries):
    out = []
    peak = None
    for ts, v in vseries:
        if peak is None or v > peak:
            peak = v
        dd = (v - peak) / peak if peak else 0.0
        out.append((ts, dd))
    return out


# ─────────────────────────────────────────────────────────────────────
# HTML section builders
# ─────────────────────────────────────────────────────────────────────
def scorecard(title, value, sub="", accent=TEXT, extra="") -> str:
    return f"""<div class="card">
    <div class="card-label">{esc(title)}</div>
    <div class="card-value" style="color:{accent}">{value}</div>
    <div class="card-sub">{sub}</div>
    {extra}
  </div>"""


def render_overview(a, s, vseries) -> str:
    t = a["totals"]
    risk = a["risk"]
    total_v = t.get("total_value")
    upnl = t.get("unrealized_pnl")
    pnl_pct = t.get("pnl_pct")
    pc = sign_color(upnl)

    values = [v for _, v in vseries]
    spark = sparkline(values)

    # donut over positions + cash
    palette = [AMBER, GREEN, "#58a6ff", "#bc8cff", "#d29922", "#39c5cf", "#db61a2"]
    segs = []
    for i, p in enumerate(a["positions"]):
        segs.append((p["symbol"], p.get("weight") or 0.0, palette[i % len(palette)]))
    segs.append(("cash", t.get("cash_weight") or 0.0, NEUTRAL))
    dnut = donut(segs)
    legend = "".join(
        f'<div class="leg"><span class="dot" style="background:{c}"></span>'
        f'{esc(l)} <span class="leg-pct">{fmt_pct(v,1)}</span></div>'
        for (l, v, c) in segs if v and v > 0)

    # health scorecards
    frag = s.get("fragility_score")
    cls = s.get("classification")
    cls_color = {"antifragile": GREEN, "robust": AMBER, "fragile": RED}.get(cls, MUTED)
    cards = "".join([
        scorecard("Sharpe (ann.)", fmt_num(risk.get("sharpe"), 2),
                  "risk-adjusted return", AMBER),
        scorecard("Max drawdown", fmt_pct(risk.get("max_drawdown"), 1),
                  "peak-to-trough", sign_color(risk.get("max_drawdown"))),
        scorecard("Fragility", fmt_num(frag, 1) + "<span class='u'>/100</span>",
                  f'<span class="pill" style="background:{cls_color}22;color:{cls_color};'
                  f'border:1px solid {cls_color}55">{esc(cls or DASH)}</span>', cls_color),
        scorecard("Barbell adherence", "", "safe / mid / aggr", TEXT,
                  extra='<div class="gauge">' + gauge(
                      s.get("barbell_safe"), s.get("barbell_mid"), s.get("barbell_aggr")) + "</div>"),
    ])

    # top movers
    movers = [p for p in a["positions"] if p.get("unrealized_pct") is not None]
    movers.sort(key=lambda p: p["unrealized_pct"], reverse=True)
    best = movers[:3]
    worst = list(reversed(movers[-3:])) if len(movers) >= 1 else []
    def mover_rows(rows):
        return "".join(
            f'<div class="mover"><span class="msym">{esc(p["symbol"])}</span>'
            f'<span class="mpct" style="color:{sign_color(p["unrealized_pct"])}">'
            f'{signed_pct(p["unrealized_pct"])}</span></div>' for p in rows) or \
            f'<div class="mover muted">{DASH}</div>'

    return f"""<section class="panel overview">
  <div class="hero">
    <div class="hero-main">
      <div class="hero-label">Total portfolio value</div>
      <div class="hero-value">{fmt_money(total_v)}</div>
      <div class="hero-pnl">
        <span style="color:{pc}">{signed_money(upnl)}</span>
        <span class="hero-pnl-pct" style="color:{sign_color(pnl_pct)}">({signed_pct(pnl_pct)})</span>
        <span class="muted">unrealized</span>
      </div>
      <div class="spark">{spark}</div>
    </div>
    <div class="hero-donut">
      {dnut}
      <div class="legend">{legend}</div>
    </div>
  </div>
  <div class="scorecards">{cards}</div>
  <div class="movers-wrap">
    <div class="movers-col"><div class="movers-title" style="color:{GREEN}">▲ Top gainers</div>{mover_rows(best)}</div>
    <div class="movers-col"><div class="movers-title" style="color:{RED}">▼ Top losers</div>{mover_rows(worst)}</div>
  </div>
</section>"""


def render_positions(a) -> str:
    rows = ""
    for p in a["positions"]:
        upct = p.get("unrealized_pct")
        upnl = p.get("unrealized_pnl")
        rows += f"""<tr>
      <td class="sym">{esc(p["symbol"])}</td>
      <td class="ac">{esc(p.get("asset_class"))}</td>
      <td class="num">{fmt_pct(p.get("weight"),1)}<div class="rowbar">{weight_bar(p.get("weight"))}</div></td>
      <td class="num">{fmt_money(p.get("value"))}</td>
      <td class="num">{fmt_money(p.get("cost"))}</td>
      <td class="num" style="color:{sign_color(upnl)}">{signed_money(upnl)}</td>
      <td class="num" style="color:{sign_color(upct)}">{signed_pct(upct)}</td>
    </tr>"""
    t = a["totals"]
    rows += f"""<tr class="cashrow">
      <td class="sym">cash</td><td class="ac">cash</td>
      <td class="num">{fmt_pct(t.get("cash_weight"),1)}<div class="rowbar">{weight_bar(t.get("cash_weight"), color=NEUTRAL)}</div></td>
      <td class="num">{fmt_money(t.get("cash"))}</td>
      <td class="num">{DASH}</td><td class="num">{DASH}</td><td class="num">{DASH}</td>
    </tr>"""
    return f"""<section class="panel">
  <h2>Positions</h2>
  <div class="tablewrap"><table class="positions">
    <thead><tr>
      <th>Symbol</th><th>Class</th><th class="num">Weight</th><th class="num">Value</th>
      <th class="num">Cost</th><th class="num">P&amp;L</th><th class="num">P&amp;L %</th>
    </tr></thead>
    <tbody>{rows}</tbody>
  </table></div>
</section>"""


def render_instruments(a, history) -> str:
    blocks = ""
    for p in a["positions"]:
        sym = p["symbol"]
        hist = history.get(sym, [])
        qty = p.get("quantity") or 0.0
        avg_cost = (p["cost"] / qty) if qty else None
        chart = line_chart(hist, ref=avg_cost,
                           ref_label=(f"cost {fmt_money(avg_cost)}" if avg_cost is not None else None))
        upct = p.get("unrealized_pct")
        summary = (
            f'<span class="isym">{esc(sym)}</span>'
            f'<span class="iac">{esc(p.get("asset_class"))}</span>'
            f'<span class="iprice">{fmt_money(p.get("price"))}</span>'
            f'<span class="ipct" style="color:{sign_color(upct)}">{signed_pct(upct)}</span>')
        blocks += f"""<details class="instrument" id="instrument-{esc(sym)}">
      <summary>{summary}</summary>
      <div class="ichart">{chart}</div>
      <div class="imeta">
        <span>qty {fmt_num(qty, 4)}</span>
        <span>avg cost {fmt_money(avg_cost)}</span>
        <span>value {fmt_money(p.get("value"))}</span>
        <span style="color:{sign_color(p.get("unrealized_pnl"))}">P&amp;L {signed_money(p.get("unrealized_pnl"))}</span>
      </div>
    </details>"""
    return f"""<section class="panel">
  <h2>Instruments <span class="hint">(click to expand)</span></h2>
  {blocks}
</section>"""


def render_risk(a, s, vseries) -> str:
    risk = a["risk"]
    curve = s.get("details", {}).get("convexity_curve", [])
    conv = convexity_chart(curve)
    barbell = gauge(s.get("barbell_safe"), s.get("barbell_mid"), s.get("barbell_aggr"),
                    w=420, h=54)
    hm = heatmap(a.get("correlation", {}))
    dd = drawdown_series(vseries)
    uw = area_chart(dd)

    stats = [
        ("Skew", fmt_num(s.get("skew"), 3)),
        ("Excess kurtosis", fmt_num(s.get("excess_kurtosis"), 3)),
        ("Tail ratio", fmt_num(s.get("tail_ratio"), 2)),
        ("ES 95% (daily)", fmt_pct(s.get("es95"), 2)),
        ("VaR 95% (daily)", fmt_pct(risk.get("var95"), 2)),
        ("Volatility (ann.)", fmt_pct(risk.get("vol_ann"), 1)),
        ("Sortino", fmt_num(risk.get("sortino"), 2)),
        ("HHI", fmt_num(risk.get("hhi"), 3)),
        ("Effective N", fmt_num(risk.get("eff_n"), 2)),
        ("Beta vs bench", fmt_num(risk.get("beta"), 2)),
        ("Stress asymmetry", fmt_num(s.get("stress_asymmetry"), 4)),
        ("Convexity (±10%)", fmt_num(s.get("convexity"), 4)),
    ]
    stat_cells = "".join(
        f'<div class="stat"><div class="stat-k">{esc(k)}</div><div class="stat-v">{v}</div></div>'
        for k, v in stats)

    drivers = s.get("details", {}).get("drivers", {})
    driver_bars = "".join(
        f'<div class="drow"><span class="dk">{esc(k)}</span>'
        f'<span class="dbar"><span class="dfill" style="width:{(v or 0)*100:.0f}%"></span></span>'
        f'<span class="dv">{fmt_num(v,2)}</span></div>'
        for k, v in drivers.items())

    return f"""<section class="panel">
  <h2>Risk &amp; anti-fragility</h2>
  <div class="risk-grid">
    <div class="chart-box"><h3>Convexity curve</h3>{conv}
      <p class="cap">Payoff response to a symmetric ±price shock. Near-zero for a linear long book; positive convexity comes from optionality.</p></div>
    <div class="chart-box"><h3>Barbell allocation</h3>{barbell}
      <p class="cap">Weight split into safe / fragile-middle / aggressive-convex sleeves. A thick amber middle drives fragility up.</p></div>
    <div class="chart-box"><h3>Correlation heatmap</h3>{hm}
      <p class="cap">Return correlations, −1 (red) to +1 (green).</p></div>
    <div class="chart-box"><h3>Underwater / drawdown</h3>{uw}
      <p class="cap">Running peak-to-current loss over the value series.</p></div>
  </div>
  <h3 class="frag-h">Fragility drivers</h3>
  <div class="drivers">{driver_bars}</div>
  <h3 class="frag-h">Distribution &amp; risk stats</h3>
  <div class="stats-grid">{stat_cells}</div>
</section>"""


# ─────────────────────────────────────────────────────────────────────
# recommendation section (latest orchestrator advice)
# ─────────────────────────────────────────────────────────────────────
def _action_color(action) -> str:
    a = (action or "").strip().lower()
    if a in ("trim", "exit", "hedge", "reduce", "sell"):
        return RED
    if a in ("add", "buy", "increase"):
        return GREEN
    if a in ("hold", "keep"):
        return MUTED
    return TEXT


def render_recommendation(reco, reco_ts, held_symbols) -> str:
    if reco is None:
        return f"""<section class="panel reco reco-empty">
  <h2>Recommendation</h2>
  <div class="reco-placeholder">No recommendation yet — run <code>./run.sh</code>
    (or <code>aislang chat advise portfolio.ais</code>) to generate one.</div>
</section>"""

    verdict = (reco.get("verdict") or "").strip().lower()
    if verdict == "rebalance":
        v_color, v_text = AMBER, "REBALANCE"
    elif verdict == "keep":
        v_color, v_text = GREEN, "KEEP"
    else:
        v_color, v_text = MUTED, esc(reco.get("verdict") or DASH).upper()

    confidence = reco.get("confidence")
    conf_tag = (f'<span class="reco-conf">{esc(confidence)}</span>'
                if confidence else "")
    ts_tag = (f'<span class="reco-ts muted">{esc(reco_ts)}</span>'
              if reco_ts else "")
    badge = (f'<div class="reco-head"><span class="reco-badge" '
             f'style="background:{v_color};color:{BG}">{v_text}</span>'
             f'{conf_tag}{ts_tag}</div>')

    summary = reco.get("summary")
    summary_html = f'<p class="reco-summary">{esc(summary)}</p>' if summary else ""
    fragility = reco.get("fragility_read")
    fragility_html = (f'<p class="reco-fragility muted">{esc(fragility)}</p>'
                      if fragility else "")

    held = set(held_symbols or [])
    actions = reco.get("actions") or []
    actions_html = ""
    if actions:
        rows = ""
        for act in actions:
            action = act.get("action")
            symbol = act.get("symbol")
            size = act.get("size")
            rationale = act.get("rationale")
            ac = _action_color(action)
            if symbol in held:
                sym_cell = f'<a href="#instrument-{esc(symbol)}">{esc(symbol)}</a>'
            else:
                sym_cell = esc(symbol)
            rows += f"""<tr>
      <td class="reco-act" style="color:{ac}">{esc(action)}</td>
      <td class="reco-sym">{sym_cell}</td>
      <td class="reco-size">{esc(size)}</td>
      <td class="reco-rat">{esc(rationale)}</td>
    </tr>"""
        actions_html = f"""<div class="tablewrap"><table class="reco-actions">
    <thead><tr>
      <th>Action</th><th>Symbol</th><th>Size</th><th>Rationale</th>
    </tr></thead>
    <tbody>{rows}</tbody>
  </table></div>"""

    return f"""<section class="panel reco">
  <h2>Recommendation</h2>
  {badge}
  {summary_html}
  {fragility_html}
  {actions_html}
</section>"""


# ─────────────────────────────────────────────────────────────────────
# page
# ─────────────────────────────────────────────────────────────────────
def render_page(holdings, report, vseries, history, reco=None, reco_ts=None) -> str:
    a, s = report["analytic"], report["strategic"]
    name = holdings.get("name") or "Portfolio"
    ccy = holdings.get("base_currency", "USD")
    ts = datetime.datetime.now().strftime("%Y-%m-%d %H:%M:%S")

    css = _css()
    held_symbols = [p["symbol"] for p in a["positions"]]
    recommendation = render_recommendation(reco, reco_ts, held_symbols)
    overview = render_overview(a, s, vseries)
    positions = render_positions(a)
    instruments = render_instruments(a, history)
    risk = render_risk(a, s, vseries)

    return f"""<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8"/>
<meta name="viewport" content="width=device-width, initial-scale=1"/>
<title>{esc(name)} — portfolio dashboard</title>
<style>{css}</style>
</head>
<body>
<div class="wrap">
  <header class="topbar">
    <div>
      <div class="brand">portfolio<span class="amber">-pnl</span></div>
      <h1>{esc(name)}</h1>
    </div>
    <div class="gen">
      <div>generated {esc(ts)}</div>
      <div class="muted">base currency {esc(ccy)}</div>
    </div>
  </header>
  {recommendation}
  {overview}
  {positions}
  {instruments}
  {risk}
  <footer class="foot">
    All figures are computed deterministically from local price history by
    <code>scripts/analyze.py</code>. This dashboard is a static, self-contained
    artifact — no external requests. <strong>Not financial advice.</strong>
  </footer>
</div>
<script>
// tiny drill-down helper: <details> already works without JS; this only
// adds a keyboard shortcut to expand/collapse all instrument rows.
document.addEventListener('keydown', function(e){{
  if (e.key === 'e' || e.key === 'E') {{
    var open = document.querySelectorAll('details.instrument[open]').length === 0;
    document.querySelectorAll('details.instrument').forEach(function(d){{ d.open = open; }});
  }}
}});
</script>
</body>
</html>"""


def _css() -> str:
    return f"""
:root {{ color-scheme: dark; }}
* {{ box-sizing: border-box; }}
body {{
  margin: 0; background: {BG}; color: {TEXT};
  font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, Helvetica, Arial, sans-serif;
  font-size: 14px; line-height: 1.5;
}}
.wrap {{ max-width: 1180px; margin: 0 auto; padding: 24px 20px 60px; }}
a {{ color: {AMBER}; }}
.muted {{ color: {MUTED}; }}
.amber {{ color: {AMBER}; }}
code {{ background: {PANEL2}; padding: 1px 5px; border-radius: 4px; font-size: 12px; }}

.topbar {{ display: flex; justify-content: space-between; align-items: flex-start;
  gap: 16px; padding-bottom: 18px; border-bottom: 1px solid {BORDER}; margin-bottom: 22px; }}
.brand {{ font-size: 12px; letter-spacing: 1px; text-transform: uppercase; color: {MUTED}; }}
.topbar h1 {{ margin: 4px 0 0; font-size: 26px; font-weight: 650; }}
.gen {{ text-align: right; font-size: 12px; color: {TEXT}; }}

.panel {{ background: {PANEL}; border: 1px solid {BORDER}; border-radius: 12px;
  padding: 20px 22px; margin-bottom: 20px; }}
.panel h2 {{ margin: 0 0 16px; font-size: 17px; font-weight: 600; }}
.panel h3 {{ margin: 0 0 10px; font-size: 13px; font-weight: 600; color: {MUTED};
  text-transform: uppercase; letter-spacing: .5px; }}
.hint {{ font-size: 12px; font-weight: 400; color: {MUTED}; text-transform: none; letter-spacing: 0; }}

/* ── recommendation ── */
.reco-head {{ display: flex; align-items: center; gap: 10px; flex-wrap: wrap; margin-bottom: 12px; }}
.reco-badge {{ display: inline-block; padding: 3px 12px; border-radius: 6px;
  font-size: 13px; font-weight: 700; letter-spacing: .5px; }}
.reco-conf {{ display: inline-block; padding: 1px 8px; border-radius: 10px; font-size: 11px;
  font-weight: 600; color: {MUTED}; background: {PANEL2}; border: 1px solid {BORDER};
  text-transform: capitalize; }}
.reco-ts {{ font-size: 12px; font-variant-numeric: tabular-nums; }}
.reco-summary {{ margin: 0 0 6px; font-size: 14px; }}
.reco-fragility {{ margin: 0 0 12px; font-size: 13px; }}
table.reco-actions {{ width: 100%; border-collapse: collapse; margin-top: 6px; }}
table.reco-actions th, table.reco-actions td {{ padding: 9px 12px; text-align: left;
  border-bottom: 1px solid {BORDER}; vertical-align: top; }}
table.reco-actions th {{ font-size: 11px; text-transform: uppercase; letter-spacing: .5px;
  color: {MUTED}; font-weight: 600; }}
td.reco-act {{ font-weight: 700; text-transform: capitalize; white-space: nowrap; }}
td.reco-sym {{ font-weight: 600; white-space: nowrap; }}
td.reco-size {{ font-variant-numeric: tabular-nums; white-space: nowrap; }}
td.reco-rat {{ color: {MUTED}; font-size: 13px; }}
.reco-placeholder {{ color: {MUTED}; font-size: 13px; background: {PANEL2};
  border: 1px dashed {BORDER}; border-radius: 8px; padding: 14px 16px; }}

/* ── overview hero ── */
.hero {{ display: flex; gap: 28px; flex-wrap: wrap; align-items: center; }}
.hero-main {{ flex: 1 1 380px; min-width: 300px; }}
.hero-label {{ color: {MUTED}; font-size: 13px; }}
.hero-value {{ font-size: 40px; font-weight: 700; letter-spacing: -1px; margin: 2px 0; }}
.hero-pnl {{ font-size: 16px; font-weight: 600; display: flex; gap: 8px; align-items: baseline; flex-wrap: wrap; }}
.hero-pnl .muted {{ font-weight: 400; font-size: 13px; }}
.hero-pnl-pct {{ font-size: 14px; }}
.spark {{ margin-top: 14px; }}
.hero-donut {{ flex: 0 0 auto; display: flex; gap: 16px; align-items: center; }}
.legend {{ display: flex; flex-direction: column; gap: 4px; font-size: 12px; }}
.leg {{ display: flex; align-items: center; gap: 6px; }}
.leg-pct {{ color: {MUTED}; }}
.dot {{ width: 10px; height: 10px; border-radius: 2px; display: inline-block; }}

/* ── scorecards ── */
.scorecards {{ display: grid; grid-template-columns: repeat(4, 1fr); gap: 12px; margin-top: 20px; }}
.card {{ background: {PANEL2}; border: 1px solid {BORDER}; border-radius: 10px; padding: 14px 16px; }}
.card-label {{ font-size: 12px; color: {MUTED}; }}
.card-value {{ font-size: 24px; font-weight: 700; margin: 4px 0; }}
.card-value .u {{ font-size: 13px; color: {MUTED}; font-weight: 500; }}
.card-sub {{ font-size: 12px; color: {MUTED}; }}
.pill {{ display: inline-block; padding: 1px 8px; border-radius: 10px; font-size: 11px;
  text-transform: capitalize; font-weight: 600; }}
.gauge {{ margin-top: 8px; }}

/* ── movers ── */
.movers-wrap {{ display: grid; grid-template-columns: 1fr 1fr; gap: 14px; margin-top: 18px; }}
.movers-col {{ background: {PANEL2}; border: 1px solid {BORDER}; border-radius: 10px; padding: 12px 14px; }}
.movers-title {{ font-size: 12px; font-weight: 600; margin-bottom: 8px; }}
.mover {{ display: flex; justify-content: space-between; padding: 3px 0; border-bottom: 1px solid {BORDER}22; }}
.mover.muted {{ color: {MUTED}; }}
.msym {{ font-weight: 600; }}
.mpct {{ font-variant-numeric: tabular-nums; }}

/* ── positions table ── */
.tablewrap {{ overflow-x: auto; }}
table.positions {{ width: 100%; border-collapse: collapse; font-variant-numeric: tabular-nums; }}
table.positions th, table.positions td {{ padding: 9px 12px; text-align: left;
  border-bottom: 1px solid {BORDER}; }}
table.positions th {{ font-size: 11px; text-transform: uppercase; letter-spacing: .5px; color: {MUTED}; font-weight: 600; }}
table.positions td.num, table.positions th.num {{ text-align: right; }}
table.positions td.sym {{ font-weight: 600; }}
table.positions td.ac {{ color: {MUTED}; font-size: 12px; }}
.rowbar {{ margin-top: 3px; }}
.cashrow td {{ color: {MUTED}; }}

/* ── instruments ── */
details.instrument {{ background: {PANEL2}; border: 1px solid {BORDER}; border-radius: 9px;
  margin-bottom: 8px; padding: 0 14px; }}
details.instrument summary {{ cursor: pointer; padding: 11px 0; display: flex; align-items: center;
  gap: 12px; list-style: none; }}
details.instrument summary::-webkit-details-marker {{ display: none; }}
details.instrument summary::before {{ content: "▸"; color: {AMBER}; font-size: 12px; }}
details.instrument[open] summary::before {{ content: "▾"; }}
.isym {{ font-weight: 700; min-width: 72px; }}
.iac {{ color: {MUTED}; font-size: 12px; flex: 1; }}
.iprice {{ font-variant-numeric: tabular-nums; }}
.ipct {{ font-variant-numeric: tabular-nums; min-width: 64px; text-align: right; }}
.ichart {{ padding: 6px 0 4px; }}
.imeta {{ display: flex; flex-wrap: wrap; gap: 16px; font-size: 12px; color: {MUTED};
  padding: 6px 0 12px; border-top: 1px solid {BORDER}; }}

/* ── risk ── */
.risk-grid {{ display: grid; grid-template-columns: 1fr 1fr; gap: 16px; }}
.chart-box {{ background: {PANEL2}; border: 1px solid {BORDER}; border-radius: 10px; padding: 14px 16px; }}
.cap {{ font-size: 11px; color: {MUTED}; margin: 8px 0 0; line-height: 1.4; }}
.frag-h {{ margin-top: 20px; }}
.drivers {{ display: grid; gap: 6px; max-width: 560px; }}
.drow {{ display: grid; grid-template-columns: 120px 1fr 44px; align-items: center; gap: 10px; font-size: 12px; }}
.dk {{ color: {MUTED}; text-transform: capitalize; }}
.dbar {{ background: {BORDER}; border-radius: 4px; height: 8px; overflow: hidden; }}
.dfill {{ display: block; height: 100%; background: linear-gradient(90deg, {GREEN}, {AMBER}, {RED}); }}
.dv {{ text-align: right; font-variant-numeric: tabular-nums; }}
.stats-grid {{ display: grid; grid-template-columns: repeat(4, 1fr); gap: 10px; }}
.stat {{ background: {PANEL2}; border: 1px solid {BORDER}; border-radius: 8px; padding: 10px 12px; }}
.stat-k {{ font-size: 11px; color: {MUTED}; }}
.stat-v {{ font-size: 17px; font-weight: 600; margin-top: 2px; font-variant-numeric: tabular-nums; }}

.foot {{ font-size: 12px; color: {MUTED}; margin-top: 8px; padding: 16px 4px; line-height: 1.6; }}
.foot strong {{ color: {AMBER}; }}

/* ── responsive ── */
@media (max-width: 820px) {{
  .scorecards {{ grid-template-columns: repeat(2, 1fr); }}
  .stats-grid {{ grid-template-columns: repeat(2, 1fr); }}
  .risk-grid {{ grid-template-columns: 1fr; }}
  .movers-wrap {{ grid-template-columns: 1fr; }}
  .hero-value {{ font-size: 32px; }}
  .drow {{ grid-template-columns: 90px 1fr 40px; }}
  .topbar {{ flex-direction: column; }}
  .gen {{ text-align: left; }}
}}
"""


# ─────────────────────────────────────────────────────────────────────
# main
# ─────────────────────────────────────────────────────────────────────
def main() -> int:
    ap = argparse.ArgumentParser(description="Render a self-contained portfolio dashboard.html")
    ap.add_argument("--db", default=dbmod.DEFAULT_DB)
    ap.add_argument("--holdings", default=os.path.join(dbmod.ROOT, "holdings.json"))
    ap.add_argument("--out", default=os.path.join(dbmod.ROOT, "dashboard.html"))
    args = ap.parse_args()

    holdings = dbmod.load_holdings(args.holdings)
    conn = dbmod.connect(args.db)
    dbmod.init_db(conn)
    dbmod.sync_holdings(conn, holdings)

    symbols = [p["symbol"] for p in holdings["positions"]]
    if holdings.get("benchmark"):
        symbols.append(holdings["benchmark"])
    latest = dbmod.latest_prices(conn)
    history = dbmod.all_price_history(conn, set(symbols))

    report = az.build_report(holdings, latest, history)
    a = report["analytic"]
    quantities = {r["symbol"]: r["quantity"] for r in a["positions"]}
    vseries = pf.portfolio_value_series(
        {k: history.get(k, []) for k in quantities}, quantities, holdings.get("cash", 0.0))

    import json
    _rr = conn.execute(
        "SELECT ts, actions_json FROM recommendations ORDER BY ts DESC LIMIT 1").fetchone()
    reco = json.loads(_rr["actions_json"]) if _rr else None      # None when none saved yet
    reco_ts = _rr["ts"] if _rr else None

    page = render_page(holdings, report, vseries, history, reco=reco, reco_ts=reco_ts)

    out = os.path.abspath(args.out)
    os.makedirs(os.path.dirname(out), exist_ok=True)
    with open(out, "w", encoding="utf-8") as fh:
        fh.write(page)
    print(out)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
