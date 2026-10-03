//! Shared idle-watchdog slot layout (used by `wdt_feed` map and event-ringbuf).

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
