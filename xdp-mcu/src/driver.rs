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
}

pub struct FirewallDevice<D: Driver> {
    inner: D,
    xdp_enable: bool,
    zc: Option<ZcTx>,
}

impl<D: Driver> FirewallDevice<D> {
    pub fn new(device: D, xdp_enable: bool, zc: Option<ZcTx>) -> Self {
        Self {
            inner: device,
            xdp_enable,
            zc,
        }
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
                    let armed = zc.map(|z| (z.arm)(buffer.as_mut_ptr(), buffer.len())).unwrap_or(false);
                    if !armed {
                        if zc.is_some() {
                            error!("[zctx] arm failed len={} fallback copy", buffer.len());
                        }
                        if !queue_egress_frame(buffer) {
                            error!("[xdp] TX: frame too large or queue full");
                        }
                    }
                    f(&mut [])
                }
                XDP_REDIRECT => {
                    rx_scratch::end();
                    match take_pending() {
                        Some(ifindex) if ifindex_supported(ifindex) => {
                            let armed = zc
                                .map(|z| (z.arm)(buffer.as_mut_ptr(), buffer.len()))
                                .unwrap_or(false);
                            if !armed && !queue_egress_frame(buffer) {
                                error!("[xdp] REDIRECT: frame too large or queue full");
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
