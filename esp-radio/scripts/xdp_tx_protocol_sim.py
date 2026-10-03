#!/usr/bin/env python3
"""Host simulation of esp-radio xdp_tx ownership vs Espressif tx_by_ref contract.

Mirrors claim / retain_tx / release and the current transmit() fail path
(single release after tx_by_ref). Writes NDJSON to the debug session log.
"""

from __future__ import annotations

import json
import time
from dataclasses import dataclass, field
from pathlib import Path

LOG_PATH = Path("/Users/nan/bs/.cursor/debug-cb3b7e.log")
SESSION = "cb3b7e"
RUN_ID = "host-sim-1"

EMPTY, OWNED, RELEASING = 0, 1, 2


@dataclass
class Token:
    state: int = EMPTY
    refs: int = 0
    eb: int | None = None
    freed_ebs: list[int] = field(default_factory=list)

    def claim(self, eb: int) -> bool:
        if eb == 0 or self.state != EMPTY:
            return False
        self.state = OWNED
        self.eb = eb
        self.refs = 1
        return True

    def retain_tx(self) -> None:
        if self.state != OWNED:
            return
        self.refs += 1

    def release(self) -> None:
        if self.refs == 0:
            return
        old = self.refs
        self.refs -= 1
        if old != 1:
            return
        if self.state != OWNED:
            return
        self.state = RELEASING
        eb = self.eb
        self.eb = None
        if eb is not None:
            self.freed_ebs.append(eb)
        self.state = EMPTY


def log(hypothesis_id: str, location: str, message: str, data: dict) -> None:
    entry = {
        "sessionId": SESSION,
        "runId": RUN_ID,
        "hypothesisId": hypothesis_id,
        "location": location,
        "message": message,
        "data": data,
        "timestamp": int(time.time() * 1000),
    }
    with LOG_PATH.open("a", encoding="utf-8") as f:
        f.write(json.dumps(entry, ensure_ascii=False) + "\n")


def simulate_current_transmit(wifi_balances_fail_ref: bool) -> dict:
    """Espressif docs: tx_by_ref firstly increases ref, then forwards.

    wifi_balances_fail_ref: if True, blob calls free_cb on failure before return.
    Empirical wifi_zctx did an *extra* drop on failure, suggesting False is common.
    """
    tok = Token()
    eb = 0xE1
    assert tok.claim(eb)
    # Blob always refs first (API contract).
    tok.retain_tx()
    rc_ok = False  # inject failure (e.g. ESP_ERR_NO_MEM)
    if not rc_ok and wifi_balances_fail_ref:
        tok.release()  # blob free_cb
    # Current esp-radio: single release after tx_by_ref returns.
    tok.release()
    return {
        "wifi_balances_fail_ref": wifi_balances_fail_ref,
        "refs_after": tok.refs,
        "state_after": tok.state,
        "eb_still_held": tok.eb,
        "freed": list(tok.freed_ebs),
        "leak": tok.state != EMPTY or tok.refs > 0 or tok.eb is not None,
    }


def simulate_zctx_double_drop_on_fail() -> dict:
    tok = Token()
    eb = 0xE2
    assert tok.claim(eb)
    tok.retain_tx()  # blob ref first
    # fail, blob does NOT free
    tok.release()  # RX drop
    tok.release()  # zctx extra drop on fail
    return {
        "refs_after": tok.refs,
        "state_after": tok.state,
        "freed": list(tok.freed_ebs),
        "leak": tok.state != EMPTY or tok.refs > 0,
    }


def simulate_success_wifi_never_frees(n: int = 16) -> dict:
    """H-B: success path where blob refs but never free_cb → pool exhaustion."""
    pool = [Token() for _ in range(16)]
    exhausted_at = None
    for i in range(n):
        eb = 0x1000 + i
        slot = next((t for t in pool if t.claim(eb)), None)
        if slot is None:
            exhausted_at = i
            break
        slot.retain_tx()
        slot.release()  # RX drops; wifi still holds refs=1, state OWNED
    owned = sum(1 for t in pool if t.state == OWNED)
    return {
        "attempts": n,
        "owned_tokens": owned,
        "exhausted_at": exhausted_at,
        "pool_dead": owned == 16,
    }


def main() -> None:
    r_unbalanced = simulate_current_transmit(wifi_balances_fail_ref=False)
    log(
        "A",
        "xdp_tx_protocol_sim.py:fail_no_balance",
        "current single-release after fail when Wi-Fi refs but does not free",
        r_unbalanced,
    )

    r_balanced = simulate_current_transmit(wifi_balances_fail_ref=True)
    log(
        "A",
        "xdp_tx_protocol_sim.py:fail_with_balance",
        "current single-release after fail when Wi-Fi balances ref",
        r_balanced,
    )

    r_zctx = simulate_zctx_double_drop_on_fail()
    log(
        "A",
        "xdp_tx_protocol_sim.py:zctx_double_drop",
        "legacy wifi_zctx extra drop on fail recovers eb",
        r_zctx,
    )

    r_pool = simulate_success_wifi_never_frees(20)
    log(
        "B",
        "xdp_tx_protocol_sim.py:success_no_free_cb",
        "success handoff without free_cb exhausts token pool",
        r_pool,
    )

    log(
        "C",
        "xdp_tx_protocol_sim.py:pass_empty_note",
        "Pass empty window is code-path note; see xdp_dispatch debug",
        {"pass_empty_enqueues": True, "needs_device_or_unit": True},
    )

    print(json.dumps({"unbalanced": r_unbalanced, "balanced": r_balanced, "zctx": r_zctx, "pool": r_pool}, indent=2))


if __name__ == "__main__":
    main()
