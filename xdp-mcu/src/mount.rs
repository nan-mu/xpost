//! Runtime mount table: which IRAM slot serves `rx` / `call`.
//!
//! Boot values come from `mem.toml` → codegen (`HOT_SLOT_MOUNTS`). Remount requires
//! datapath stopped, then a write lock (read-heavy / write-light).
//!
//! Datapath [`try_rx_slot`] reads an atomic index so 热切换 can flip the entry
//! without `datapath::disable`.

use core::sync::atomic::{AtomicU8, Ordering};

use crate::datapath;
use crate::init_log;
use crate::mem_map::{HOT_SLOT_COUNT, HOT_SLOT_MOUNTS, HOT_SLOT_NAMES};
use crate::prog_table::SlotId;
use crate::sync::RwCell;

/// Sentinel for [`RX_INDEX`]: no `rx` mount.
const RX_NONE: u8 = 0xFF;

static RX_INDEX: AtomicU8 = AtomicU8::new(RX_NONE);

fn store_rx_index(slot: Option<SlotId>) {
    let v = slot.map(|s| s.0).unwrap_or(RX_NONE);
    RX_INDEX.store(v, Ordering::Release);
}

fn sync_rx_index(t: &MountTable) {
    store_rx_index(t.rx_slot());
}

/// Mount role for one hot slot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MountKind {
    Call,
    Rx,
}

impl MountKind {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "call" => Some(Self::Call),
            "rx" => Some(Self::Rx),
            _ => None,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Call => "call",
            Self::Rx => "rx",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MountError {
    /// Datapath still active — call [`datapath::disable`] first.
    Busy,
    /// `rx` would appear on more than one slot.
    DuplicateRole,
    /// Unknown mount string / bad slot index.
    BadArg,
}

#[derive(Clone, Copy)]
struct MountTable {
    mounts: [MountKind; HOT_SLOT_COUNT],
}

impl MountTable {
    const fn from_boot() -> Self {
        // Filled in `init` from HOT_SLOT_MOUNTS (const fn cannot parse easily).
        Self {
            mounts: [MountKind::Call; HOT_SLOT_COUNT],
        }
    }

    fn rx_slot(self) -> Option<SlotId> {
        self.mounts
            .iter()
            .position(|m| *m == MountKind::Rx)
            .and_then(|i| SlotId::from_u8(i as u8))
    }
}

static TABLE: RwCell<MountTable> = RwCell::new(MountTable::from_boot());

fn boot_mounts() -> [MountKind; HOT_SLOT_COUNT] {
    let mut out = [MountKind::Call; HOT_SLOT_COUNT];
    let mut i = 0;
    while i < HOT_SLOT_COUNT {
        out[i] = MountKind::parse(HOT_SLOT_MOUNTS[i]).unwrap_or(MountKind::Call);
        i += 1;
    }
    out
}

/// Load boot mounts from codegen (call once at startup after map init).
pub fn init() {
    let mounts = boot_mounts();
    // Boot: datapath not enabled yet; take write without gate.
    {
        // sync try_write at boot before executor traffic
        let Some(mut g) = TABLE.try_write() else {
            return;
        };
        g.mounts = mounts;
        sync_rx_index(&*g);
        log_table(&*g);
    }
}

fn log_table(t: &MountTable) {
    let mut i = 0;
    while i < HOT_SLOT_COUNT {
        init_log!(
            "[Mount] slot{} name={} mount={}",
            i,
            HOT_SLOT_NAMES[i],
            t.mounts[i].as_str()
        );
        i += 1;
    }
}

fn validate_unique(mounts: &[MountKind; HOT_SLOT_COUNT]) -> Result<(), MountError> {
    let mut rx = 0u8;
    for m in mounts {
        match m {
            MountKind::Call => {}
            MountKind::Rx => rx = rx.saturating_add(1),
        }
    }
    if rx > 1 {
        return Err(MountError::DuplicateRole);
    }
    Ok(())
}

/// Remount one slot. Requires datapath stopped; takes write lock.
pub async fn remount(slot: SlotId, kind: MountKind) -> Result<(), MountError> {
    datapath::require_stopped().map_err(|_| MountError::Busy)?;
    let mut g = TABLE.write().await;
    let mut next = g.mounts;
    next[slot.index()] = kind;
    validate_unique(&next)?;
    g.mounts = next;
    sync_rx_index(&*g);
    init_log!(
        "[Mount] remount slot{} name={} -> {}",
        slot.index(),
        HOT_SLOT_NAMES[slot.index()],
        kind.as_str()
    );
    Ok(())
}

/// Remount one slot (ST backend).

fn apply_flip_rx(t: &mut MountTable, dst: SlotId) -> Result<(), MountError> {
    let Some(old) = t.rx_slot() else {
        return Err(MountError::BadArg);
    };
    if old == dst {
        return Err(MountError::BadArg);
    }
    if t.mounts[dst.index()] != MountKind::Call {
        return Err(MountError::BadArg);
    }
    let mut next = t.mounts;
    next[old.index()] = MountKind::Call;
    next[dst.index()] = MountKind::Rx;
    validate_unique(&next)?;
    t.mounts = next;
    store_rx_index(Some(dst));
    init_log!(
        "[Mount] flip_rx {} -> {} (old -> call)",
        old.index(),
        dst.index()
    );
    Ok(())
}

/// 热切换: `dst` (must be `call`) becomes `rx`; previous `rx` becomes `call`.
/// Does **not** require datapath stopped.
pub async fn flip_rx(dst: SlotId) -> Result<(), MountError> {
    let mut g = TABLE.write().await;
    apply_flip_rx(&mut g, dst)
}

/// 热切换 (ST backend).

/// Try-read mount for `slot`.
pub fn try_mount(slot: SlotId) -> Option<MountKind> {
    let g = TABLE.try_read()?;
    Some(g.mounts[slot.index()])
}

/// Datapath entry slot (`Acquire` load of the atomic index).
pub fn try_rx_slot() -> Option<SlotId> {
    SlotId::from_u8(RX_INDEX.load(Ordering::Acquire))
}

/// Whether `slot` is currently mounted as `call` (try_read).
pub fn try_is_call(slot: SlotId) -> Option<bool> {
    Some(try_mount(slot)? == MountKind::Call)
}
