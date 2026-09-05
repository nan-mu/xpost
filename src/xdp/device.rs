use embassy_net::driver::{Driver, RxToken, TxToken};
use log::error;

use crate::redirect::{
    XDP_ABORTED, XDP_DROP, XDP_PASS, XDP_REDIRECT, XDP_TX, queue_egress_frame,
    requeue_egress_front, take_egress_frame, take_pending,
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

pub struct Device<D: Driver> {
    inner: D,
    zc: ZcTx,
    /// When true (default), pace XDP_TX egress on each `receive()` poll.
    flush_egress_on_rx: bool,
}

impl<D: Driver> Device<D> {
    /// Construct an XDP device with its required board-specific zero-copy TX
    /// implementation. There is no copy-only constructor.
    pub fn new(device: D, zc: ZcTx) -> Self {
        Self {
            inner: device,
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

impl<D: Driver> Driver for Device<D> {
    type RxToken<'a>
        = DeviceRxToken<D::RxToken<'a>>
    where
        Self: 'a;
    type TxToken<'a>
        = DeviceTxToken<D::TxToken<'a>>
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
            DeviceRxToken {
                inner: rx,
                zc: self.zc,
            },
            DeviceTxToken { inner: tx },
        ))
    }

    fn transmit(&mut self, cx: &mut core::task::Context) -> Option<Self::TxToken<'_>> {
        // One bounce per stack TX so echo (34B) interleaves with XDP_TX (~1492B)
        // and leftover Wi-Fi heap (~10KB after STA init) is not exhausted.
        self.flush_egress(cx, MAX_FLUSH_PER_POLL);
        self.inner
            .transmit(cx)
            .map(|tx| DeviceTxToken { inner: tx })
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

pub struct DeviceRxToken<R: RxToken> {
    inner: R,
    zc: ZcTx,
}

impl<R: RxToken> RxToken for DeviceRxToken<R> {
    fn consume<Ret, F>(self, f: F) -> Ret
    where
        F: FnOnce(&mut [u8]) -> Ret,
    {
        let zc = self.zc;
        let ret = self.inner.consume(|buffer| {
            let Some(action) = crate::xdp::run_ingress(buffer) else {
                xdp_debug!("[xdp][device] pass: ingress snapshot unavailable bytes={}", buffer.len());
                return f(buffer);
            };
            match action {
                XDP_PASS => {
                    xdp_debug!("[xdp][device] pass: AOT returned XDP_PASS bytes={}", buffer.len());
                    rx_scratch::end();
                    f(buffer)
                }
                XDP_DROP => {
                    xdp_debug!("[xdp][device] drop: AOT returned XDP_DROP bytes={}", buffer.len());
                    rx_scratch::end();
                    // Must call `f`: RxToken::consume must return `Ret` from `f`, and
                    // smoltcp treats an empty frame as "dropped" (skips ingress).
                    // DMA is released when this inner closure returns (esp-radio
                    // PacketBuffer drop), not by invoking `f`.
                    f(&mut [])
                }
                XDP_ABORTED => {
                    xdp_debug!("[xdp][device] drop: AOT aborted bytes={}", buffer.len());
                    error!("[xdp] aborted");
                    let _ = take_pending();
                    rx_scratch::end();
                    f(&mut [])
                }
                XDP_TX | XDP_REDIRECT => {
                    rx_scratch::end();
                    let _ = take_pending();
                    let frame = crate::ctx::clip_to_md_window(buffer);
                    let armed = (zc.arm)(frame.as_mut_ptr(), frame.len());
                    if !armed {
                        // zc miss (outstanding/NO_MEM/too big): copy. Never drop
                        // here — skip-on-257 made 0µs echo Xloss 100%.
                        let queued = queue_egress_frame(frame);
                        #[cfg(not(feature = "debug"))]
                        let _ = queued;
                        xdp_debug!(
                            "[xdp][device] tx fallback: zero-copy hook rejected bytes={} queued={}",
                            frame.len(),
                            queued
                        );
                    } else {
                        xdp_debug!("[xdp][device] tx: zero-copy armed bytes={}", frame.len());
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
        (zc.finish)();
        ret
    }
}

/// smoltcp TX token: DHCP/ARP/UDP go straight to the NIC (no TX XDP).
pub struct DeviceTxToken<T: TxToken> {
    inner: T,
}

impl<T: TxToken> TxToken for DeviceTxToken<T> {
    fn consume<R, F>(self, len: usize, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        self.inner.consume(len, f)
    }
}
