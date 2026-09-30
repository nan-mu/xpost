"""Parse firmware ``OK trace`` lines into nested timestamp frames (just_tx).

Flame / icicle views should use absolute ``t_us`` and enter/leave nesting —
not mean stage percentages.
"""

from __future__ import annotations

from dataclasses import dataclass


@dataclass(frozen=True)
class TraceEvent:
    kind: str
    t_us: int
    arg0: int
    arg1: int


# ProfSite (xpost_type) — SpanBegin/SpanEnd.arg0
PROF_SITE = {
    0: "XdpRun",
    1: "TxEnqueue",
    2: "TxDrainOne",
    3: "TxDrainBatch",
    4: "WifiRxDispatch",
    5: "TxSubmit",
}

# BPF helper id → short label (subset dispatched by xpost)
HELPER_NAME = {
    1: "map_lookup",
    2: "map_update",
    3: "map_delete",
    5: "ktime_get_ns",
    6: "trace_printk",
    28: "csum_diff",
    65: "adjust_tail",
}


@dataclass(frozen=True)
class Frame:
    """One closed interval on the absolute timeline (µs)."""

    name: str
    t0_us: int
    t1_us: int
    depth: int
    region: str  # "outside_xdp" | "inside_xdp" | "post_xdp"


def parse_ok_trace(line: str) -> list[TraceEvent]:
    """Parse ``OK trace len=… | Kind t_us=… arg0=… arg1=… | …``."""
    body = line.strip()
    for prefix in ("OK trace", "OK tracedump"):
        if prefix in body:
            body = body.split(prefix, 1)[1]
            break
    evs: list[TraceEvent] = []
    for seg in body.split("|"):
        seg = seg.strip()
        if not seg or seg.startswith("len=") or "t_us=" not in seg:
            continue
        parts = seg.split()
        if not parts:
            continue
        kind = parts[0]
        t_us = arg0 = arg1 = 0
        for p in parts[1:]:
            if "=" not in p:
                continue
            k, v = p.split("=", 1)
            try:
                n = int(v)
            except ValueError:
                continue
            if k == "t_us":
                t_us = n
            elif k == "arg0":
                arg0 = n
            elif k == "arg1":
                arg1 = n
        evs.append(TraceEvent(kind=kind, t_us=t_us, arg0=arg0, arg1=arg1))
    return evs


# Back-compat alias
parse_tracedump = parse_ok_trace


def _label(ev: TraceEvent) -> str:
    if ev.kind in ("SpanBegin", "SpanEnd"):
        site = PROF_SITE.get(ev.arg0, f"site{ev.arg0}")
        return f"{ev.kind}:{site}"
    if ev.kind in ("HelperEnter", "HelperLeave"):
        h = HELPER_NAME.get(ev.arg0, f"helper{ev.arg0}")
        return f"{ev.kind}:{h}"
    if ev.kind == "XdpEnd":
        return f"XdpEnd:action={ev.arg0}"
    return ev.kind


def frames_from_events(events: list[TraceEvent]) -> list[Frame]:
    """Build nested frames from Span*/Helper* enter/leave + point stamps.

    Point events (RxDelivered, TxSubBegin, …) become zero-width markers folded
    into an interval from the previous stamp at the same depth (adjacent Δt).
    Prefer enter/leave pairs for true nesting.
    """
    frames: list[Frame] = []
    # stack entries: (name, t0, depth, region)
    stack: list[tuple[str, int, int, str]] = []
    inside_xdp = False
    last_t: int | None = None

    def region() -> str:
        if inside_xdp:
            return "inside_xdp"
        if any(n.startswith("SpanBegin:WifiRxDispatch") or n == "WifiRxDispatch" for n, *_ in [(s[0],) for s in stack]):
            # after XdpEnd but still in WifiRxDispatch → post
            if any("XdpRun" in s[0] for s in stack):
                return "inside_xdp"
            return "post_xdp" if last_t is not None else "outside_xdp"
        return "outside_xdp"

    for ev in events:
        name = _label(ev)
        if ev.kind == "SpanBegin":
            site = PROF_SITE.get(ev.arg0, f"site{ev.arg0}")
            if site == "XdpRun":
                inside_xdp = True
            r = "inside_xdp" if site == "XdpRun" else ("outside_xdp" if site == "WifiRxDispatch" else region())
            if site == "WifiRxDispatch":
                r = "outside_xdp"
            stack.append((site, ev.t_us, len(stack), r))
        elif ev.kind == "SpanEnd" and stack:
            site = PROF_SITE.get(ev.arg0, f"site{ev.arg0}")
            # pop matching site from top if possible
            for i in range(len(stack) - 1, -1, -1):
                if stack[i][0] == site:
                    n, t0, depth, r = stack.pop(i)
                    if site == "XdpRun":
                        inside_xdp = False
                        r = "inside_xdp"
                    frames.append(Frame(name=n, t0_us=t0, t1_us=ev.t_us, depth=depth, region=r))
                    break
        elif ev.kind == "HelperEnter":
            h = HELPER_NAME.get(ev.arg0, f"helper{ev.arg0}")
            stack.append((h, ev.t_us, len(stack), "inside_xdp"))
            inside_xdp = True
        elif ev.kind == "HelperLeave" and stack:
            h = HELPER_NAME.get(ev.arg0, f"helper{ev.arg0}")
            for i in range(len(stack) - 1, -1, -1):
                if stack[i][0] == h:
                    n, t0, depth, r = stack.pop(i)
                    frames.append(
                        Frame(name=n, t0_us=t0, t1_us=ev.t_us, depth=depth, region="inside_xdp")
                    )
                    break
        elif ev.kind in ("XdpBegin",):
            inside_xdp = True
        elif ev.kind in ("XdpEnd",):
            inside_xdp = False
        last_t = ev.t_us
        _ = name
    return frames


@dataclass(frozen=True)
class CallStages:
    """Legacy 3-stage collapse (RxBegin/XdpBegin/XdpEnd/RxEnd only). Prefer frames_from_events."""

    pkt_len: int
    action: int | None
    xdp_dispatch_us: int
    xdp_execution_us: int
    post_action_us: int
    rx_processing_us: int
    t0_us: int

    def as_rows(self) -> list[tuple[str, int, float]]:
        tot = max(self.rx_processing_us, 1)
        return [
            ("rx_processing", self.rx_processing_us, 100.0),
            ("xdp_dispatch", self.xdp_dispatch_us, 100.0 * self.xdp_dispatch_us / tot),
            ("xdp_execution", self.xdp_execution_us, 100.0 * self.xdp_execution_us / tot),
            ("post_action", self.post_action_us, 100.0 * self.post_action_us / tot),
        ]


def extract_calls(events: list[TraceEvent]) -> list[CallStages]:
    """Legacy: Split ring dump into RxBegin…RxEnd calls (incomplete tails dropped)."""
    calls: list[CallStages] = []
    i = 0
    while i < len(events):
        if events[i].kind != "RxBegin":
            i += 1
            continue
        rx_b = events[i]
        xdp_b = xdp_e = rx_e = None
        action = None
        j = i + 1
        while j < len(events):
            if events[j].kind == "RxBegin":
                break
            if events[j].kind == "XdpBegin" and xdp_b is None:
                xdp_b = events[j]
            elif events[j].kind == "XdpEnd" and xdp_e is None:
                xdp_e = events[j]
                action = events[j].arg0
            elif events[j].kind == "RxEnd":
                rx_e = events[j]
                break
            j += 1
        if xdp_b and xdp_e and rx_e:
            calls.append(
                CallStages(
                    pkt_len=rx_b.arg0,
                    action=action,
                    xdp_dispatch_us=max(0, xdp_b.t_us - rx_b.t_us),
                    xdp_execution_us=max(0, xdp_e.t_us - xdp_b.t_us),
                    post_action_us=max(0, rx_e.t_us - xdp_e.t_us),
                    rx_processing_us=max(0, rx_e.t_us - rx_b.t_us),
                    t0_us=rx_b.t_us,
                )
            )
        i = j + 1 if j > i else i + 1
    return calls


def action_name(action: int | None) -> str:
    return {0: "Aborted", 1: "Drop", 2: "Pass", 3: "Tx"}.get(action or -1, f"?{action}")
