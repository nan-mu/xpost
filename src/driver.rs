use embassy_net::driver::{Driver, RxToken, TxToken};
use log::error;

use crate::patch::redirect::{
    clear_pending, ifindex_supported, queue_egress_frame, take_egress_frame, take_pending,
    XDP_ABORTED, XDP_DROP, XDP_PASS, XDP_REDIRECT, XDP_TX,
};

pub struct FirewallDevice<D: Driver> {
    inner: D,
}

impl<D: Driver> FirewallDevice<D> {
    pub fn new(device: D) -> Self {
        Self { inner: device }
    }

    /// Drain frames queued by XDP_TX / XDP_REDIRECT onto the wire.
    fn flush_egress(&mut self, cx: &mut core::task::Context) {
        while let Some(frame) = take_egress_frame() {
            let Some(tx) = self.inner.transmit(cx) else {
                // No TX credit — re-queue and try on the next poll.
                let _ = queue_egress_frame(&frame);
                break;
            };
            let len = frame.len();
            tx.consume(len, |dst| {
                let n = len.min(dst.len());
                dst[..n].copy_from_slice(&frame[..n]);
            });
        }
    }
}

impl<D: Driver> Driver for FirewallDevice<D> {
    type RxToken<'a>
        = FirewallRxToken<D::RxToken<'a>>
    where
        Self: 'a;
    type TxToken<'a>
        = D::TxToken<'a>
    where
        Self: 'a;

    fn receive(
        &mut self,
        cx: &mut core::task::Context,
    ) -> Option<(Self::RxToken<'_>, Self::TxToken<'_>)> {
        self.flush_egress(cx);
        let (rx, tx) = self.inner.receive(cx)?;
        Some((FirewallRxToken { inner: rx }, tx))
    }

    fn transmit(&mut self, cx: &mut core::task::Context) -> Option<Self::TxToken<'_>> {
        self.flush_egress(cx);
        self.inner.transmit(cx)
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
}

impl<R: RxToken> RxToken for FirewallRxToken<R> {
    fn consume<Ret, F>(self, f: F) -> Ret
    where
        F: FnOnce(&mut [u8]) -> Ret,
    {
        self.inner.consume(|buffer| {
            clear_pending();
            let action = crate::patch::xdp(buffer);
            match action {
                XDP_PASS => f(buffer),
                XDP_DROP => f(&mut []),
                XDP_ABORTED => {
                    error!("[xdp] aborted");
                    let _ = take_pending();
                    f(&mut [])
                }
                XDP_TX => {
                    if !queue_egress_frame(buffer) {
                        error!("[xdp] TX: frame too large or queue full");
                    }
                    f(&mut [])
                }
                XDP_REDIRECT => {
                    match take_pending() {
                        Some(ifindex) if ifindex_supported(ifindex) => {
                            if !queue_egress_frame(buffer) {
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
                    f(&mut [])
                }
            }
        })
    }
}
