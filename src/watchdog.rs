//! XDP-driven MCU idle watchdog — feed via ringbuf, expiry via ISR tick.
//!
//! Feed stamps live in firmware RAM ([`FeedSlot`]); XDP pushes `EVT_FEED` records
//! into `dut_evt` ringbuf. Board timer ISR calls [`tick_isr`] for idle fallback.

use core::cell::RefCell;

use critical_section::Mutex;
use embassy_time::Instant;

/// Max concurrent feed channels.
pub const MAX_CHANNELS: usize = 8;

/// Opaque feed channel id.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FeedHandle(pub u32);

#[derive(Clone, Copy, Default, Debug)]
pub struct FeedSlot {
    pub last_feed_us: u64,
    pub idle_us: u64,
    pub armed: u32,
    pub _pad: u32,
}

/// Legacy name for [`FeedSlot`] (`wdt_feed` map layout).
pub type WdtFeedSlot = FeedSlot;

struct State {
    slots: [FeedSlot; MAX_CHANNELS],
    expire: [Option<fn(FeedHandle)>; MAX_CHANNELS],
}

/// No-op when feed uses `dut_evt` ringbuf (keeps `wdt_feed` map creatable).
pub fn bind_feed_map(_id: crate::map::ResourceId) {}

static STATE: Mutex<RefCell<State>> = Mutex::new(RefCell::new(State {
    slots: [FeedSlot {
        last_feed_us: 0,
        idle_us: 0,
        armed: 0,
        _pad: 0,
    }; MAX_CHANNELS],
    expire: [None; MAX_CHANNELS],
}));

/// Monotonic µs (Embassy); matches `ktime_get_ns` helper on RV32.
#[inline]
pub fn now_us() -> u64 {
    Instant::now().as_micros()
}

/// Configure idle timeout (µs) for `handle` while datapath is stopped.
pub fn configure_channel(handle: FeedHandle, idle_us: u64) -> Result<(), ()> {
    let idx = handle.0 as usize;
    if idx >= MAX_CHANNELS {
        return Err(());
    }
    critical_section::with(|cs| {
        STATE.borrow_ref_mut(cs).slots[idx].idle_us = idle_us;
    });
    Ok(())
}

/// Register hook invoked when `handle` is idle past `idle_us`.
pub fn set_expire_hook(handle: FeedHandle, hook: fn(FeedHandle)) {
    critical_section::with(|cs| {
        let h = handle.0 as usize;
        if h < MAX_CHANNELS {
            STATE.borrow_ref_mut(cs).expire[h] = Some(hook);
        }
    });
}

/// Arm channel: next feed must arrive within `idle_us` or expire fires.
pub fn arm(handle: FeedHandle) {
    let now = now_us();
    critical_section::with(|cs| {
        let slot = &mut STATE.borrow_ref_mut(cs).slots[handle.0 as usize];
        slot.armed = 1;
        slot.last_feed_us = now;
    });
}

pub fn disarm(handle: FeedHandle) {
    critical_section::with(|cs| {
        STATE.borrow_ref_mut(cs).slots[handle.0 as usize].armed = 0;
    });
}

/// Stamp activity now (ringbuf drain + overload path).
pub fn feed(handle: FeedHandle) {
    feed_at(handle, now_us());
}

/// Stamp activity at `now_us` (ISR-safe).
pub fn feed_at(handle: FeedHandle, now_us: u64) {
    let idx = handle.0 as usize;
    if idx >= MAX_CHANNELS {
        return;
    }
    critical_section::with(|cs| {
        STATE.borrow_ref_mut(cs).slots[idx].last_feed_us = now_us;
    });
}

/// Hardware timer ISR: idle check + ringbuf drain should run before this.
pub fn tick_isr() {
    let now = now_us();
    let mut fired: [(FeedHandle, fn(FeedHandle)); MAX_CHANNELS] =
        [(FeedHandle(0), |_| {}); MAX_CHANNELS];
    let mut n = 0usize;

    critical_section::with(|cs| {
        let st = STATE.borrow_ref(cs);
        for idx in 0..MAX_CHANNELS {
            let slot = &st.slots[idx];
            if slot.armed == 0 || slot.idle_us == 0 {
                continue;
            }
            let anchor = slot.last_feed_us;
            if anchor == 0 || now.saturating_sub(anchor) < slot.idle_us {
                continue;
            }
            if let Some(hook) = st.expire[idx] {
                if n < MAX_CHANNELS {
                    fired[n] = (FeedHandle(idx as u32), hook);
                    n += 1;
                }
            }
        }
    });

    for i in 0..n {
        let (h, hook) = fired[i];
        hook(h);
    }
}
