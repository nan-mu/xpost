use embassy_net::driver::{Driver, RxToken, TxToken};
use log::error;

use crate::datapath;
use crate::redirect::{
    XDP_ABORTED, XDP_DROP, XDP_PASS, XDP_REDIRECT, XDP_TX, clear_pending, ifindex_supported,
    queue_egress_frame, requeue_egress_front, take_egress_frame, take_pending,
};
use crate::rx_scratch;

/// Embedder hook: steal RX DMA across `PacketBuffer::drop` and TX by reference.
#[derive(Clone, Copy)]
pub struct ZcTx {
    pub arm: fn(payload: *mut u8, len: usize) -> bool,
    pub finish: fn(),
    /// If `arm` returned false, whether a heap copy is still worth trying.
    pub copy_ok: fn(len: usize) -> bool,
}

pub struct FirewallDevice<D: Driver> {
    inner: D,
    xdp_enable: bool,
    zc: Option<ZcTx>,
    /// When true (default), pace XDP_TX egress on each `receive()` poll.
    flush_egress_on_rx: bool,
}

impl<D: Driver> FirewallDevice<D> {
    pub fn new(device: D, xdp_enable: bool, zc: Option<ZcTx>) -> Self {
        Self {
            inner: device,
            xdp_enable,
            zc,
            flush_egress_on_rx: true,
        }
    }

    /// Skip `flush_egress` in `receive()` (still runs on `transmit()`).
    pub fn with_flush_egress_on_rx(mut self, enable: bool) -> Self {
        self.flush_egress_on_rx = enable;
        self
    }

    /// Drain a few XDP_TX / XDP_REDIRECT frames.
    ///
    /// Do not call this from `receive()`: smoltcp ingress loops `receive()` until
    /// empty, so a per-call flush submits many full-MTU copies in one poll and
    /// `esp_wifi_internal_tx` returns 257 (`ESP_ERR_NO_MEM`).
    fn flush_egress(&mut self, cx: &mut core::task::Context, limit: usize) {
        let mtu = self.inner.capabilities().max_transmission_unit;
        for _ in 0..limit {
            let Some(frame) = take_egress_frame() else {
                break;
            };
            let len = frame.len();
            if len > mtu {
                error!("[xdp] TX frame {} > wifi mtu {}", len, mtu);
                continue;
            }
            let Some(tx) = self.inner.transmit(cx) else {
                if !requeue_egress_front(frame) {
                    error!("[xdp] TX requeue full");
                }
                break;
            };
            tx.consume(len, |dst| {
                let n = len.min(dst.len());
                dst[..n].copy_from_slice(&frame[..n]);
            });
        }
    }
}

/// One XDP bounce per stack `transmit()` so echo can still get a TX token.
const MAX_FLUSH_PER_POLL: usize = 1;

impl<D: Driver> Driver for FirewallDevice<D> {
    type RxToken<'a>
        = FirewallRxToken<D::RxToken<'a>>
    where
        Self: 'a;
    type TxToken<'a>
        = FirewallTxToken<D::TxToken<'a>>
    where
        Self: 'a;

    fn receive(
        &mut self,
        cx: &mut core::task::Context,
    ) -> Option<(Self::RxToken<'_>, Self::TxToken<'_>)> {
        // Copy-path XDP_TX only leaves the static queue when something calls
        // `transmit()`. With u_pct=0 the stack has no echo/ARP to send, so
        // bounce would sit in the 16-deep queue and drop (100% Xloss). Flush
        // one frame per RX so bounce is paced by incoming packets, not by
        // smoltcp TX. Limit 1: a burst flush of full-MTU copies OOMs Wi-Fi
        // (`esp_wifi_internal_tx` → 257).
        if self.flush_egress_on_rx {
            self.flush_egress(cx, MAX_FLUSH_PER_POLL);
        }
        let (rx, tx) = self.inner.receive(cx)?;
        Some((
            FirewallRxToken {
                inner: rx,
                xdp_enable: self.xdp_enable,
                zc: self.zc,
            },
            FirewallTxToken { inner: tx },
        ))
    }

    fn transmit(&mut self, cx: &mut core::task::Context) -> Option<Self::TxToken<'_>> {
        // One bounce per stack TX so echo (34B) interleaves with XDP_TX (~1492B)
        // and leftover Wi-Fi heap (~10KB after STA init) is not exhausted.
        self.flush_egress(cx, MAX_FLUSH_PER_POLL);
        self.inner
            .transmit(cx)
            .map(|tx| FirewallTxToken { inner: tx })
    }

    fn link_state(&mut self, cx: &mut core::task::Context) -> embassy_net::driver::LinkState {
        self.inner.link_state(cx)
    }

    fn capabilities(&self) -> embassy_net::driver::Capabilities {
        self.inner.capabilities()
    }

    fn hardware_address(&self) -> embassy_net::driver::HardwareAddress {
        self.inner.hardware_address()
    }
}

pub struct FirewallRxToken<R: RxToken> {
    inner: R,
    xdp_enable: bool,
    zc: Option<ZcTx>,
}

impl<R: RxToken> RxToken for FirewallRxToken<R> {
    fn consume<Ret, F>(self, f: F) -> Ret
    where
        F: FnOnce(&mut [u8]) -> Ret,
    {
        let zc = self.zc;
        let ret = self.inner.consume(|buffer| {
            if !self.xdp_enable || !datapath::is_active() {
                return f(buffer);
            }
            clear_pending();
            let action = crate::xdp(buffer);
            match action {
                XDP_PASS => {
                    rx_scratch::end();
                    f(buffer)
                }
                XDP_DROP => {
                    rx_scratch::end();
                    // Must call `f`: RxToken::consume must return `Ret` from `f`, and
                    // smoltcp treats an empty frame as "dropped" (skips ingress).
                    // DMA is released when this inner closure returns (esp-radio
                    // PacketBuffer drop), not by invoking `f`.
                    f(&mut [])
                }
                XDP_ABORTED => {
                    error!("[xdp] aborted");
                    let _ = take_pending();
                    rx_scratch::end();
                    f(&mut [])
                }
                XDP_TX => {
                    rx_scratch::end();
                    let frame = crate::ctx::clip_to_md_window(buffer);
                    let armed = zc
                        .map(|z| (z.arm)(frame.as_mut_ptr(), frame.len()))
                        .unwrap_or(false);
                    if !armed {
                        // zc miss (outstanding/NO_MEM/too big): copy. Never drop
                        // here — skip-on-257 made 0µs echo Xloss 100%.
                        let _ = queue_egress_frame(frame);
                    }
                    f(&mut [])
                }
                XDP_REDIRECT => {
                    rx_scratch::end();
                    match take_pending() {
                        Some(ifindex) if ifindex_supported(ifindex) => {
                            let frame = crate::ctx::clip_to_md_window(buffer);
                            let armed = zc
                                .map(|z| (z.arm)(frame.as_mut_ptr(), frame.len()))
                                .unwrap_or(false);
                            if !armed {
                                let copy = zc.map(|z| (z.copy_ok)(frame.len())).unwrap_or(true);
                                if copy && !queue_egress_frame(frame) {
                                    error!("[xdp] REDIRECT: frame too large or queue full");
                                }
                            }
                        }
                        Some(ifindex) => {
                            error!("[xdp] redirect ifindex {} unsupported", ifindex);
                        }
                        None => {
                            error!("[xdp] XDP_REDIRECT without pending ifindex");
                        }
                    }
                    f(&mut [])
                }
                other => {
                    error!("[xdp] unexpected action {}", other);
                    let _ = take_pending();
                    rx_scratch::end();
                    f(&mut [])
                }
            }
        });
        if let Some(zc) = zc {
            (zc.finish)();
        }
        ret
    }
}

/// smoltcp TX token: DHCP/ARP/UDP go straight to the NIC (no TX XDP).
pub struct FirewallTxToken<T: TxToken> {
    inner: T,
}

impl<T: TxToken> TxToken for FirewallTxToken<T> {
    fn consume<R, F>(self, len: usize, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        self.inner.consume(len, f)
    }
}
