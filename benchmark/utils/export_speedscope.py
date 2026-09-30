#!/usr/bin/env python3
"""Export just_tx Trace → speedscope JSON under xpost/target/.

Primary source: ``just_tx_run.report_raw`` (verbatim ``OK report <n> b64=…``).
Falls back to legacy ``just_tx_trace_call`` stage columns when no report_raw.
"""

from __future__ import annotations

import base64
import json
import struct
import sys
from collections import defaultdict
from pathlib import Path

from utils.db import read_sql
from utils.experiment.trace_flame import (
    HELPER_NAME,
    PROF_SITE,
    TraceEvent,
    frames_from_events,
)

_ROOT = Path(__file__).resolve().parents[2]  # xpost/
_OUT_DIR = _ROOT / "target"
_SCHEMA = "https://www.speedscope.app/file-format-schema.json"

# Must match xpost::ext::perf::report / xpost-serial::perf_report
_MASK_C, _MASK_A, _MASK_G, _MASK_T = 1, 2, 4, 8
_C_LEN, _A_LEN, _G_LEN = 184, 1408, 32
_T_LEN = 16 + 256 * 16 + 64 * 16
_KIND_NAME = {
    1: "RxBegin",
    2: "XdpBegin",
    3: "XdpEnd",
    4: "RxEnd",
    5: "SlowExecution",
    6: "RxOverflow",
    7: "RuntimeError",
    8: "UserMarker",
    9: "SpanBegin",
    10: "SpanEnd",
    11: "HelperEnter",
    12: "HelperLeave",
    13: "RxDelivered",
    14: "RxDiscarded",
    15: "TxSubBegin",
    16: "TxSubmitted",
    17: "TxFailed",
    18: "TxCompleted",
    19: "TxFailBadBuf",
    20: "TxFailToken",
    21: "TxEnq",
    22: "TxKick",
    23: "TxPoll",
    24: "TxCapWait",
    25: "TxNomemWait",
    26: "TxBlock",
    27: "TxEnqFailClaim",
    28: "TxEnqFailRing",
    29: "TxFlush",
    30: "TxGrpSrc",
    31: "TxNoRetain",
    32: "AppUdpRecv",
    33: "AppEchoBegin",
    34: "AppEchoEnd",
    35: "AppUdpSendBegin",
    36: "AppUdpSendEnd",
    37: "AppWifiTxBegin",
    38: "AppWifiTxEnd",
}


class _FrameTable:
    def __init__(self) -> None:
        self._idx: dict[str, int] = {}
        self.frames: list[dict[str, str]] = []

    def id(self, name: str) -> int:
        if name not in self._idx:
            self._idx[name] = len(self.frames)
            self.frames.append({"name": name})
        return self._idx[name]


def _parse_report_b64(msg: str) -> bytes:
    for part in msg.split():
        if part.startswith("b64="):
            return base64.b64decode(part[4:])
    raise ValueError("report_raw missing b64=")


def _mask_for_len(n: int) -> int | None:
    for mask in range(64):
        total = 0
        if mask & _MASK_C:
            total += _C_LEN
        if mask & _MASK_A:
            total += _A_LEN
        if mask & _MASK_G:
            total += _G_LEN
        if mask & _MASK_T:
            total += _T_LEN
        # span/pchit backends removed; kind-t is C+A?+G?+T only
        if total == n:
            return mask
    return None


def _trace_events_from_payload(payload: bytes) -> list[TraceEvent]:
    mask = _mask_for_len(len(payload))
    if mask is None or not (mask & _MASK_T):
        return []
    off = 0
    if mask & _MASK_C:
        off += _C_LEN
    if mask & _MASK_A:
        off += _A_LEN
    if mask & _MASK_G:
        off += _G_LEN
    buf = payload[off : off + _T_LEN]
    if len(buf) < 16:
        return []
    n_radio = min(struct.unpack_from("<I", buf, 0)[0], 256)
    n_app = min(struct.unpack_from("<I", buf, 8)[0], 64)
    out: list[TraceEvent] = []

    def take(base: int, n: int) -> None:
        for i in range(n):
            b = base + i * 16
            kind_b = buf[b]
            arg0, arg1, t_us = struct.unpack_from("<III", buf, b + 4)
            out.append(
                TraceEvent(
                    kind=_KIND_NAME.get(kind_b, f"Unknown{kind_b}"),
                    t_us=int(t_us),
                    arg0=int(arg0),
                    arg1=int(arg1),
                )
            )

    take(16, n_radio)
    take(16 + 256 * 16, n_app)
    return out


def _nest_frame_name(kind: str, arg0: int) -> str | None:
    """Stable frame name for nest O/C (HAL App* ≠ XDP Span*/Tx*)."""
    if kind in ("SpanBegin", "SpanEnd"):
        return PROF_SITE.get(arg0, f"site{arg0}")
    if kind in ("HelperEnter", "HelperLeave"):
        return HELPER_NAME.get(arg0, f"helper{arg0}")
    if kind in ("AppEchoBegin", "AppEchoEnd"):
        return "AppEcho"
    if kind in ("AppUdpSendBegin", "AppUdpSendEnd"):
        return "AppUdpSend"
    if kind in ("AppWifiTxBegin", "AppWifiTxEnd"):
        return "AppWifiTx"
    return None


def _label_event(kind: str, arg0: int) -> str | None:
    nest = _nest_frame_name(kind, arg0)
    if nest is not None:
        return nest
    # Point stamps — keep radio vs app names distinct.
    if kind in (
        "RxBegin",
        "XdpBegin",
        "XdpEnd",
        "RxEnd",
        "RxDelivered",
        "RxDiscarded",
        "RxOverflow",
        "TxSubBegin",
        "TxSubmitted",
        "TxFailed",
        "TxCompleted",
        "TxFailBadBuf",
        "TxFailToken",
        "TxEnq",
        "TxKick",
        "TxPoll",
        "TxCapWait",
        "TxNomemWait",
        "TxBlock",
        "TxEnqFailClaim",
        "TxEnqFailRing",
        "TxFlush",
        "TxGrpSrc",
        "TxNoRetain",
        "AppUdpRecv",
        "SlowExecution",
        "UserMarker",
        "RuntimeError",
    ):
        return kind
    return None


def _is_nest_open(kind: str) -> bool:
    return kind in (
        "SpanBegin",
        "HelperEnter",
        "AppEchoBegin",
        "AppUdpSendBegin",
        "AppWifiTxBegin",
    )


def _is_nest_close(kind: str) -> bool:
    return kind in (
        "SpanEnd",
        "HelperLeave",
        "AppEchoEnd",
        "AppUdpSendEnd",
        "AppWifiTxEnd",
    )

def profile_evented_from_raw(
    name: str, events: list[TraceEvent], frames: _FrameTable
) -> dict | None:
    """Build speedscope evented profile.

    Speedscope requires non-decreasing ``at``. Ring dumps often start mid-call
    and interleave async TX stamps — we sort by t_us, ignore unmatched closes,
    and clamp ``at``.
    """
    if not events:
        return None
    # Stable sort: wall time, then original index.
    ordered = sorted(enumerate(events), key=lambda iv: (iv[1].t_us, iv[0]))
    t0 = ordered[0][1].t_us
    ev_out: list[dict] = []
    stack: list[int] = []
    last_at = 0

    def emit(typ: str, fid: int, at: int) -> None:
        nonlocal last_at
        at = max(at, last_at)
        last_at = at
        ev_out.append({"type": typ, "frame": fid, "at": at})

    for _, e in ordered:
        label = _label_event(e.kind, e.arg0)
        if label is None:
            continue
        at = max(0, e.t_us - t0)
        fid = frames.id(label)
        if _is_nest_close(e.kind):
            if not stack:
                continue  # truncated head of ring
            if stack[-1] == fid:
                stack.pop()
                emit("C", fid, at)
            elif fid in stack:
                while stack and stack[-1] != fid:
                    emit("C", stack.pop(), at)
                if stack and stack[-1] == fid:
                    stack.pop()
                    emit("C", fid, at)
            # else: unmatched close (stale) — drop
        elif _is_nest_open(e.kind):
            emit("O", fid, at)
            stack.append(fid)
        else:
            # zero-width marker (instantaneous)
            emit("O", fid, at)
            emit("C", fid, at)

    end_at = last_at
    while stack:
        emit("C", stack.pop(), end_at)
    if not ev_out:
        return None
    return {
        "type": "evented",
        "name": name,
        "unit": "microseconds",
        "startValue": 0,
        "endValue": last_at,
        "events": ev_out,
    }


def profile_evented_from_calls(
    name: str, rows: list[dict], frames: _FrameTable
) -> dict | None:
    if not rows:
        return None
    f_rx = frames.id("WifiRxDispatch")
    f_disp = frames.id("dispatch")
    f_xdp = frames.id("XdpRun")
    f_post = frames.id("post")
    ev_out: list[dict] = []
    cursor = 0
    for r in rows:
        d, x, p = int(r["dispatch_us"]), int(r["exec_us"]), int(r["post_us"])
        tot = int(r["total_us"])
        span = max(tot, d + x + p)
        t = cursor
        ev_out += [
            {"type": "O", "frame": f_rx, "at": t},
            {"type": "O", "frame": f_disp, "at": t},
            {"type": "C", "frame": f_disp, "at": t + d},
            {"type": "O", "frame": f_xdp, "at": t + d},
            {"type": "C", "frame": f_xdp, "at": t + d + x},
            {"type": "O", "frame": f_post, "at": t + d + x},
            {"type": "C", "frame": f_post, "at": t + d + x + p},
            {"type": "C", "frame": f_rx, "at": t + span},
        ]
        cursor += span
    return {
        "type": "evented",
        "name": name,
        "unit": "microseconds",
        "startValue": 0,
        "endValue": cursor,
        "events": ev_out,
    }


def profile_sampled_from_calls(
    name: str, rows: list[dict], frames: _FrameTable
) -> dict | None:
    if not rows:
        return None
    f_rx = frames.id("WifiRxDispatch")
    f_disp = frames.id("dispatch")
    f_xdp = frames.id("XdpRun")
    f_post = frames.id("post")
    samples: list[list[int]] = []
    weights: list[int] = []
    for r in rows:
        d, x, p = int(r["dispatch_us"]), int(r["exec_us"]), int(r["post_us"])
        if d > 0:
            samples.append([f_rx, f_disp])
            weights.append(d)
        if x > 0:
            samples.append([f_rx, f_xdp])
            weights.append(x)
        if p > 0:
            samples.append([f_rx, f_post])
            weights.append(p)
    if not samples:
        return None
    return {
        "type": "sampled",
        "name": name,
        "unit": "microseconds",
        "startValue": 0,
        "endValue": sum(weights),
        "samples": samples,
        "weights": weights,
    }


def _load_from_report_raw() -> dict[str, list[tuple[int, list[TraceEvent]]]]:
    """fw_type → list of (run_id, events)."""
    try:
        df = read_sql(
            """
            SELECT id, fw_type, report_raw
            FROM just_tx_run
            WHERE kind = 't' AND report_raw IS NOT NULL AND report_raw <> ''
            ORDER BY fw_type, id
            """
        )
    except Exception:
        return {}
    if df is None or df.empty:
        return {}
    out: dict[str, list[tuple[int, list[TraceEvent]]]] = defaultdict(list)
    for row in df.itertuples(index=False):
        try:
            payload = _parse_report_b64(str(row.report_raw))
            evs = _trace_events_from_payload(payload)
        except Exception as e:
            print(f"skip run {row.id}: {e}", file=sys.stderr)
            continue
        if evs:
            out[str(row.fw_type)].append((int(row.id), evs))
    return dict(out)


def _load_calls() -> dict[str, list[dict]]:
    df = read_sql(
        """
        SELECT r.fw_type, c.dispatch_us, c.exec_us, c.post_us, c.total_us
        FROM just_tx_trace_call c
        JOIN just_tx_run r ON r.id = c.run_id
        WHERE r.kind = 't'
        ORDER BY r.fw_type, c.run_id, c.call_idx
        """
    )
    out: dict[str, list[dict]] = defaultdict(list)
    if df is None or df.empty:
        return {}
    for row in df.itertuples(index=False):
        out[str(row.fw_type)].append(
            {
                "dispatch_us": int(row.dispatch_us),
                "exec_us": int(row.exec_us),
                "post_us": int(row.post_us),
                "total_us": int(row.total_us),
            }
        )
    return dict(out)


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


def _radio_events(events: list[TraceEvent]) -> list[TraceEvent]:
    """Drop HAL app-ring kinds if present (async; not used for XDP flame)."""
    return [e for e in events if e.kind not in _APP_KINDS]


def build_xdp_file(runs: list[tuple[int, list[TraceEvent]]]) -> dict:
    frames = _FrameTable()
    profiles: list[dict] = []
    for run_id, evs in runs:
        lane = _radio_events(evs)
        p = profile_evented_from_raw(f"xdp run={run_id} (evented)", lane, frames)
        if p:
            profiles.append(p)
    all_lane = [e for _, evs in runs for e in _radio_events(evs)]
    fr = frames_from_events(all_lane)
    if fr:
        samples: list[list[int]] = []
        weights: list[int] = []
        root = "WifiRxDispatch"
        for f in fr:
            w = max(0, f.t1_us - f.t0_us)
            if w <= 0:
                continue
            path = (
                [frames.id(root)]
                if f.name == root
                else [frames.id(root), frames.id(f.name)]
            )
            samples.append(path)
            weights.append(w)
        if samples:
            profiles.append(
                {
                    "type": "sampled",
                    "name": "xdp (sampled all runs)",
                    "unit": "microseconds",
                    "startValue": 0,
                    "endValue": sum(weights),
                    "samples": samples,
                    "weights": weights,
                }
            )
    if not profiles:
        raise SystemExit("no xdp profiles")
    return {
        "$schema": _SCHEMA,
        "exporter": "xpost-benchmark export_speedscope (report_raw, xdp-only)",
        "name": "just_tx xdp Trace",
        "activeProfileIndex": 0,
        "shared": {"frames": frames.frames},
        "profiles": profiles,
    }


def main() -> None:
    _OUT_DIR.mkdir(parents=True, exist_ok=True)
    raw = _load_from_report_raw()
    xdp_runs = raw.get("xdp") or []
    if not xdp_runs:
        # legacy fallback
        calls = _load_calls().get("xdp") or []
        if not calls:
            raise SystemExit("no xdp kind-t report_raw (or legacy calls)")
        frames = _FrameTable()
        profiles = []
        p = profile_evented_from_calls("xdp (evented reconstructed)", calls, frames)
        if p:
            profiles.append(p)
        s = profile_sampled_from_calls("xdp (sampled / classic flame)", calls, frames)
        if s:
            profiles.append(s)
        doc = {
            "$schema": _SCHEMA,
            "exporter": "xpost-benchmark export_speedscope (legacy calls)",
            "name": "just_tx xdp Trace",
            "activeProfileIndex": 0,
            "shared": {"frames": frames.frames},
            "profiles": profiles,
        }
        out = _OUT_DIR / "just_tx-xdp-trace.speedscope.json"
        out.write_text(json.dumps(doc, separators=(",", ":")), encoding="utf-8")
        print(f"wrote {out}", file=sys.stderr)
        print(out)
        return

    doc = build_xdp_file(xdp_runs)
    out = _OUT_DIR / "just_tx-xdp-trace.speedscope.json"
    out.write_text(json.dumps(doc, separators=(",", ":")), encoding="utf-8")
    print(f"wrote {out}", file=sys.stderr)
    print(
        f"  profiles={len(doc['profiles'])} frames={[f['name'] for f in doc['shared']['frames']]}",
        file=sys.stderr,
    )
    print(out)


if __name__ == "__main__":
    main()
