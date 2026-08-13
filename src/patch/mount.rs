//! Runtime mount table: which IRAM slot serves `rx` / `tx` / `call`.
//!
//! Boot values come from `mem.toml` → codegen (`HOT_SLOT_MOUNTS`). Remount requires
//! datapath stopped, then a write lock (read-heavy / write-light).

use crate::init_log;
use crate::patch::datapath;
use crate::patch::mem_map::{HOT_SLOT_COUNT, HOT_SLOT_MOUNTS, HOT_SLOT_NAMES};
use crate::patch::prog_table::SlotId;
use crate::patch::sync::RwCell;

/// Mount role for one hot slot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MountKind {
    Call,
    Rx,
    Tx,
}

impl MountKind {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "call" => Some(Self::Call),
            "rx" => Some(Self::Rx),
            "tx" => Some(Self::Tx),
            _ => None,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Call => "call",
            Self::Rx => "rx",
            Self::Tx => "tx",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MountError {
    /// Datapath still active — call [`datapath::disable`] first.
    Busy,
    /// `rx` or `tx` would appear on more than one slot.
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

    fn tx_slot(self) -> Option<SlotId> {
        self.mounts
            .iter()
            .position(|m| *m == MountKind::Tx)
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
    #[cfg(all(feature = "patch-sync-embassy", not(feature = "patch-sync-st")))]
    {
        // sync try_write at boot before executor traffic
        let Some(mut g) = TABLE.try_write() else {
            return;
        };
        g.mounts = mounts;
        log_table(&*g);
    }
    #[cfg(feature = "patch-sync-st")]
    {
        let mut g = TABLE.write();
        g.mounts = mounts;
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
    let mut tx = 0u8;
    for m in mounts {
        match m {
            MountKind::Call => {}
            MountKind::Rx => rx = rx.saturating_add(1),
            MountKind::Tx => tx = tx.saturating_add(1),
        }
    }
    if rx > 1 || tx > 1 {
        return Err(MountError::DuplicateRole);
    }
    Ok(())
}

/// Remount one slot. Requires datapath stopped; takes write lock.
#[cfg(all(feature = "patch-sync-embassy", not(feature = "patch-sync-st")))]
pub async fn remount(slot: SlotId, kind: MountKind) -> Result<(), MountError> {
    datapath::require_stopped().map_err(|_| MountError::Busy)?;
    let mut g = TABLE.write().await;
    let mut next = g.mounts;
    next[slot.index()] = kind;
    validate_unique(&next)?;
    g.mounts = next;
    init_log!(
        "[Mount] remount slot{} name={} -> {}",
        slot.index(),
        HOT_SLOT_NAMES[slot.index()],
        kind.as_str()
    );
    Ok(())
}

/// Remount one slot (ST backend).
#[cfg(feature = "patch-sync-st")]
pub fn remount(slot: SlotId, kind: MountKind) -> Result<(), MountError> {
    datapath::require_stopped().map_err(|_| MountError::Busy)?;
    let mut g = TABLE.write();
    let mut next = g.mounts;
    next[slot.index()] = kind;
    validate_unique(&next)?;
    g.mounts = next;
    init_log!(
        "[Mount] remount slot{} name={} -> {}",
        slot.index(),
        HOT_SLOT_NAMES[slot.index()],
        kind.as_str()
    );
    Ok(())
}

/// Try-read mount for `slot` (datapath / Phase B).
pub fn try_mount(slot: SlotId) -> Option<MountKind> {
    let g = TABLE.try_read()?;
    Some(g.mounts[slot.index()])
}

/// Try-read the unique `rx` slot, if any.
pub fn try_rx_slot() -> Option<SlotId> {
    TABLE.try_read()?.rx_slot()
}

/// Try-read the unique `tx` slot, if any.
pub fn try_tx_slot() -> Option<SlotId> {
    TABLE.try_read()?.tx_slot()
}

/// Whether `slot` is currently mounted as `call` (try_read).
pub fn try_is_call(slot: SlotId) -> Option<bool> {
    Some(try_mount(slot)? == MountKind::Call)
}
