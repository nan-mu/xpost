#!/usr/bin/env python3
"""Export chronological flame **charts** for just_tx XDP — split by continuous path.

Each chart is one continuous nest path, time-normalized to that episode
(no multi-ms silence between packets).

Paths:
  - rx:  WifiRxDispatch → … → XdpRun / map_lookup / TxEnqueue / RxEnd
  - tx:  TxSubmit → TxDrainOne → TxBlock / TxSubmitted / … (async after kick)

Input (first hit wins):
  1. ``target/dev-flame.report`` (scp from TR3000 ``/tmp/xpost-dev-flame.report``)
  2. ``XPOST_FLAME_REPORT`` env path
  3. argv[1] report file
  4. formal ``just_tx_run`` kind-t ``report_raw`` (legacy)
"""

from __future__ import annotations

import colorsys
import hashlib
import os
import subprocess
import sys
from dataclasses import dataclass
from pathlib import Path

from utils.export_speedscope import (
    _is_nest_close,
    _is_nest_open,
    _label_event,
    _parse_report_b64,
    _trace_events_from_payload,
)
from utils.trace_flame import TraceEvent

_ROOT = Path(__file__).resolve().parents[2]
_OUT = _ROOT / "target"
_DEV_REPORT = _OUT / "dev-flame.report"

_APP_KINDS = frozenset(
    {
        "AppUdpRecv",
        "AppEchoBegin",
        "AppEchoEnd",
        "AppUdpSendBegin",
        "AppUdpSendEnd",
        "AppWifiTxBegin",
        "AppWifiTxEnd",
    }
)


@dataclass(frozen=True)
class Interval:
    name: str
    t0: int
    t1: int
    depth: int


def _radio_events(events: list[TraceEvent]) -> list[TraceEvent]:
    return [e for e in events if e.kind not in _APP_KINDS]


_RX_ROOT = "WifiRxDispatch"
# TxSubmit wraps one poll attempt; TxDrainOne nests inside (legacy root if absent).
_TX_ROOTS = frozenset({"TxSubmit", "TxDrainOne", "TxDrainBatch"})


def _intervals(events: list[TraceEvent]) -> list[Interval]:
    ordered = sorted(enumerate(events), key=lambda iv: (iv[1].t_us, iv[0]))
    if not ordered:
        return []
    stack: list[tuple[str, int, int]] = []
    out: list[Interval] = []
    for _, e in ordered:
        label = _label_event(e.kind, e.arg0)
        if label is None:
            continue
        t = e.t_us
        if _is_nest_open(e.kind):
            if label == _RX_ROOT:
                while stack and stack[-1][0] in _TX_ROOTS:
                    n, t0, d = stack.pop()
                    out.append(Interval(n, t0, max(t, t0 + 1), d))
            elif label in _TX_ROOTS:
                while any(s[0] == _RX_ROOT for s in stack):
                    n, t0, d = stack.pop()
                    out.append(Interval(n, t0, max(t, t0 + 1), d))
            stack.append((label, t, len(stack)))
        elif _is_nest_close(e.kind):
            idx = next((i for i in range(len(stack) - 1, -1, -1) if stack[i][0] == label), None)
            if idx is None:
                continue
            while len(stack) > idx + 1:
                n, t0, d = stack.pop()
                out.append(Interval(n, t0, max(t, t0 + 1), d))
            n, t0, d = stack.pop()
            out.append(Interval(n, t0, max(t, t0 + 1), d))
        else:
            out.append(Interval(label, t, t + 1, len(stack)))
    if ordered:
        tend = ordered[-1][1].t_us
        while stack:
            n, t0, d = stack.pop()
            out.append(Interval(n, t0, max(tend, t0 + 1), d))
    return out


def _episodes(intervals: list[Interval], root: str) -> list[list[Interval]]:
    """Each closed ``root`` span + nested intervals whose [t0,t1] ⊆ root window."""
    roots = sorted([i for i in intervals if i.name == root and i.depth == 0], key=lambda z: z.t0)
    if not roots:
        roots = sorted([i for i in intervals if i.name == root], key=lambda z: z.t0)
    eps: list[list[Interval]] = []
    for r in roots:
        kids = [
            i
            for i in intervals
            if i.t0 >= r.t0 and i.t1 <= r.t1 and (i.name != root or i.t0 == r.t0)
        ]
        base = r.depth
        rel = [
            Interval(i.name, i.t0, i.t1, i.depth - base)
            for i in kids
            if i.depth >= base
        ]
        if rel:
            eps.append(sorted(rel, key=lambda z: (z.t0, z.depth)))
    return eps


_TX_ONLY = frozenset(
    {
        "TxSubmit",
        "TxDrainOne",
        "TxBlock",
        "TxSubmitted",
        "TxNoRetain",
        "TxFailed",
        "TxCapWait",
        "TxPoll",
        "TxSubBegin",
        "TxCompleted",
        "TxNomemWait",
    }
)


def _tx_episodes(intervals: list[Interval]) -> list[list[Interval]]:
    """Prefer TxSubmit windows; fall back to TxDrainOne for older reports."""
    submits = sorted([i for i in intervals if i.name == "TxSubmit"], key=lambda z: z.t0)
    if submits:
        roots = submits
        root_name = "TxSubmit"
    else:
        roots = sorted([i for i in intervals if i.name == "TxDrainOne"], key=lambda z: z.t0)
        root_name = "TxDrainOne"

    eps: list[list[Interval]] = []
    for d in roots:
        trail_end = d.t1 + 80
        kids = [
            i
            for i in intervals
            if i.name in _TX_ONLY
            and (
                (i.name == root_name and i.t0 == d.t0 and i.t1 == d.t1)
                or (
                    i.name != root_name
                    and (
                        (i.t0 >= d.t0 and i.t1 <= d.t1)
                        or (i.t0 >= d.t1 and i.t0 <= trail_end)
                    )
                )
            )
        ]
        if not kids:
            kids = [d]
        base_d = d.depth
        rel = [Interval(i.name, i.t0, i.t1, max(0, i.depth - base_d)) for i in kids]
        seen: set[tuple[str, int, int]] = set()
        uniq: list[Interval] = []
        for i in sorted(rel, key=lambda z: (z.t0, z.depth)):
            k = (i.name, i.t0, i.t1)
            if k in seen:
                continue
            seen.add(k)
            uniq.append(i)
        eps.append(uniq)
    return eps


def _hash_color(name: str) -> str:
    h = int(hashlib.md5(name.encode()).hexdigest()[:8], 16)
    hue = (h % 1000) / 1000.0
    r, g, b = colorsys.hsv_to_rgb(
        0.55 + 0.25 * hue,
        0.55 + 0.35 * (h % 7) / 7,
        0.55 + 0.35 * ((h >> 3) % 5) / 5,
    )
    return f"#{int(r * 255):02x}{int(g * 255):02x}{int(b * 255):02x}"


def _esc(s: str) -> str:
    return (
        s.replace("&", "&amp;")
        .replace("<", "&lt;")
        .replace(">", "&gt;")
        .replace('"', "&quot;")
    )


def _render_chart(
    intervals: list[Interval],
    *,
    title: str,
    width: int = 1200,
    row_h: int = 20,
    pad_l: int = 10,
    pad_r: int = 10,
    header: int = 52,
    footer: int = 28,
) -> str:
    if not intervals:
        raise SystemExit("no intervals")
    t_min = min(i.t0 for i in intervals)
    t_max = max(i.t1 for i in intervals)
    span = max(t_max - t_min, 1)
    max_depth = max(i.depth for i in intervals)
    height = header + (max_depth + 1) * row_h + footer
    plot_w = width - pad_l - pad_r

    def x_of(t: int) -> float:
        return pad_l + (t - t_min) / span * plot_w

    rects: list[str] = []
    for iv in sorted(intervals, key=lambda z: (z.depth, z.t0)):
        x0 = x_of(iv.t0)
        w = max(x_of(iv.t1) - x0, 2.0)
        y = header + iv.depth * row_h
        dur = iv.t1 - iv.t0
        tip = f"{iv.name}  +{iv.t0 - t_min}…+{iv.t1 - t_min}µs (Δ{dur}µs)"
        label = iv.name if w > 56 else (iv.name[: max(1, int(w / 7))] if w > 18 else "")
        rects.append(
            f'<g class="frame">'
            f"<title>{_esc(tip)}</title>"
            f'<rect x="{x0:.2f}" y="{y}" width="{w:.2f}" height="{row_h - 1}" '
            f'fill="{_hash_color(iv.name)}" stroke="#111" stroke-width="0.4"/>'
            f'<text x="{x0 + 3:.2f}" y="{y + row_h - 5}" font-size="11" '
            f'font-family="Menlo,Consolas,monospace" fill="#e8e8e8">{_esc(label)}</text>'
            f"</g>"
        )

    ticks: list[str] = []
    for k in range(5):
        t = t_min + span * k // 4
        x = x_of(t)
        ticks.append(
            f'<line x1="{x:.2f}" y1="{height - footer + 2}" x2="{x:.2f}" '
            f'y2="{height - footer + 8}" stroke="#888"/>'
            f'<text x="{x:.2f}" y="{height - 6}" text-anchor="middle" font-size="10" '
            f'font-family="Menlo,Consolas,monospace" fill="#aaa">{t - t_min}µs</text>'
        )

    return f"""<?xml version="1.0" encoding="UTF-8"?>
<svg xmlns="http://www.w3.org/2000/svg" width="{width}" height="{height}" viewBox="0 0 {width} {height}">
  <rect width="100%" height="100%" fill="#1e1e1e"/>
  <text x="12" y="22" font-size="14" font-family="Menlo,Consolas,monospace" fill="#f0f0f0">{_esc(title)}</text>
  <text x="12" y="40" font-size="11" font-family="Menlo,Consolas,monospace" fill="#9a9a9a">
    one continuous path · x = t_us within episode · y = nest depth · span={span}µs
  </text>
  {"".join(rects)}
  {"".join(ticks)}
</svg>
"""


def _svg_to_png(svg: Path, png: Path) -> None:
    subprocess.run(
        ["rsvg-convert", "-w", "1400", "-o", str(png), str(svg)],
        check=True,
    )


def _write(ivs: list[Interval], stem: str, title: str) -> Path:
    svg = _OUT / f"{stem}.flame.svg"
    png = _OUT / f"{stem}.flame.png"
    svg.write_text(_render_chart(ivs, title=title), encoding="utf-8")
    _svg_to_png(svg, png)
    print(f"wrote {png}", file=sys.stderr)
    return png


def _events_from_report_text(text: str) -> list[TraceEvent]:
    return _trace_events_from_payload(_parse_report_b64(text.strip()))


def _pick_report() -> tuple[str, list[TraceEvent]]:
    candidates: list[Path] = []
    if len(sys.argv) > 1:
        candidates.append(Path(sys.argv[1]))
    env = os.environ.get("XPOST_FLAME_REPORT")
    if env:
        candidates.append(Path(env))
    candidates.append(_DEV_REPORT)
    for p in candidates:
        if p.is_file() and p.stat().st_size > 0:
            text = p.read_text(encoding="utf-8", errors="replace")
            return f"file:{p.name}", _events_from_report_text(text)

    from utils.db import read_sql

    df = read_sql(
        """
        SELECT id, report_raw
        FROM just_tx_run
        WHERE kind = 't' AND fw_type = 'xdp'
          AND report_raw IS NOT NULL AND report_raw <> ''
        ORDER BY id DESC
        LIMIT 1
        """
    )
    if df is None or df.empty:
        raise SystemExit(
            "no report: put TR3000 /tmp/xpost-dev-flame.report → "
            f"{_DEV_REPORT} (or pass path / XPOST_FLAME_REPORT)"
        )
    row = df.iloc[0]
    return f"run{int(row.id)}", _trace_events_from_payload(
        _parse_report_b64(str(row.report_raw))
    )


def _pick_episode(eps: list[list[Interval]], *, prefer_nested: bool) -> list[Interval] | None:
    if not eps:
        return None
    scored: list[tuple] = []
    for e in eps:
        names = {i.name for i in e}
        dur = max(i.t1 for i in e) - min(i.t0 for i in e)
        depth = max(i.depth for i in e)
        if prefer_nested:
            complete = int("XdpRun" in names and "WifiRxDispatch" in names)
            scored.append((complete, depth, -abs(dur - 50), e))
        else:
            complete = int("TxSubmit" in names) * 2 + int("TxDrainOne" in names)
            scored.append((complete, depth, -abs(dur - 100), e))
    scored.sort(key=lambda z: (z[0], z[1], z[2]), reverse=True)
    return scored[0][3]


def main() -> None:
    tag, evs = _pick_report()
    ivs = _intervals(_radio_events(evs))
    _OUT.mkdir(parents=True, exist_ok=True)

    for p in _OUT.glob("just_tx-xdp-*-path-*.flame.*"):
        p.unlink()
    for p in _OUT.glob("just_tx-xdp-*-overview.flame.*"):
        p.unlink()

    rx_eps = _episodes(ivs, "WifiRxDispatch")
    tx_eps = _tx_episodes(ivs)
    rx = _pick_episode(rx_eps, prefer_nested=True)
    tx = _pick_episode(tx_eps, prefer_nested=False)

    print(
        f"src={tag} rx_episodes={len(rx_eps)} tx_episodes={len(tx_eps)}",
        file=sys.stderr,
    )
    outs: list[Path] = []
    stem_base = "just_tx-xdp-dev" if tag.startswith("file:") else f"just_tx-xdp-{tag}"
    if rx:
        span = max(i.t1 for i in rx) - min(i.t0 for i in rx)
        outs.append(
            _write(
                rx,
                f"{stem_base}-path-rx",
                f"just_tx xdp · path RX (WifiRxDispatch) · {span}µs",
            )
        )
        print(
            "  RX frames: "
            + " → ".join(dict.fromkeys(i.name for i in sorted(rx, key=lambda z: (z.t0, z.depth)))),
            file=sys.stderr,
        )
    if tx:
        span = max(i.t1 for i in tx) - min(i.t0 for i in tx)
        outs.append(
            _write(
                tx,
                f"{stem_base}-path-tx",
                f"just_tx xdp · path TX (TxSubmit) · {span}µs",
            )
        )
        print(
            "  TX frames: "
            + " → ".join(dict.fromkeys(i.name for i in sorted(tx, key=lambda z: (z.t0, z.depth)))),
            file=sys.stderr,
        )

    for p in outs:
        print(p)


if __name__ == "__main__":
    main()
