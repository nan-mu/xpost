//! Quiescent hot swap of a pre-installed image.
//!
//! `flash` into a slot copies a cold image. It stages bytes and does not
//! publish the entry, and it refuses a slot that is running.
//! `switch` pauses new flights, waits until `IN_FLIGHT` is 0, then publishes
//! map aliases and the new entry. The slot it leaves stays `Staged` with the
//! generation it was published under. A failed publish restores the previous
//! aliases. The host remembers which slot to publish next. `IN_FLIGHT == 0`
//! does not drain the driver's async `XDP_TX` queue; map users are kept across
//! the swap for that reason.

use core::sync::atomic::Ordering;

/// Flash-read scratch for [`XdpRt::stage`]. Control path only.
static mut STAGE_BUF: [u8; DEFAULT_IMAGE_CAPACITY] = [0; DEFAULT_IMAGE_CAPACITY];

use crate::{
    error::{MapCheckError, XdpSwapError},
    map::MapRtGuard,
    xdp::{
        SlotId, XdpId, XdpLocation, XdpRt,
        ready::ReadyError,
        state::{
            ACTIVE, BOOT_ID, IN_FLIGHT, MAX_MAP_REFS, MAX_REQUIRED_MAPS, MAX_VMR_BINDS, PAUSE,
            PRIMARY_ENTRY, QUIESCE_DROPS, QUIESCE_PASS, QUIESCE_PASSES,
        },
        table::storage::DEFAULT_IMAGE_CAPACITY,
    },
};

/// What to do with a packet that arrives while new XDP calls are paused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QuiescePolicy {
    /// Fail closed. Distinct from a program `XDP_DROP` and from feature-off `PASS`.
    Drop,
    /// Fail open. Distinct from feature-off `PASS` (`ACTIVE` stays true).
    Pass,
}

/// Slot contents, separate from whether calls are admitted.
///
/// `Active` keeps the generation that published it. A later `switch` puts that
/// slot back to `Staged` with the same generation, so the other direction does
/// not copy the image again.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SlotPhase {
    Empty,
    Staged { generation: u32 },
    Active { generation: u32 },
}

/// Cold copy into a slot. `mcu_us` is 0: a copy stages bytes and publishes
/// nothing. The field stays on the wire for the reply format.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StageReport {
    pub generation: u32,
    pub mcu_us: u32,
}

/// Result of a successful or rolled-back commit. A hard restore failure is `Err`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SwitchReport {
    pub ok: bool,
    pub rolled_back: bool,
    pub severe: bool,
    pub from_slot: usize,
    pub to_slot: usize,
    pub generation: u32,
    pub quiesce_drop: u32,
    pub quiesce_pass: u32,
    /// `esp_hal` microseconds since boot, taken after the new entry is visible
    /// and before new flights are admitted. Zero when this reply did not publish.
    pub mcu_us: u32,
}

/// Admission plus slot summary. Slot `None` means that role is empty.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct XdpStatus {
    pub boot_id: u32,
    pub active_slot: Option<usize>,
    pub staged_slot: Option<usize>,
    pub staged_generation: u32,
    pub updating: bool,
    pub severe: bool,
    pub quiesce_drop: u32,
    pub quiesce_pass: u32,
    pub last_error: u8,
}

impl XdpRt {
/// Copy `id` into `slot`.
///
/// The slot is filled and left `Staged`. This does not change `PRIMARY_ENTRY`,
/// `ACTIVE`, or map aliases, and the returned `mcu_us` is 0 because nothing is
/// published. A dropped lease can fill every slot, including the primary
/// index. A slot that is `Active` is refused: replacing a running image means
/// `disable`, copy, `enable`.
pub fn stage(&mut self, id: XdpId, slot: SlotId) -> Result<StageReport, XdpSwapError> {
        if self.severe {
            return Err(self.fail(XdpSwapError::RollbackFailed));
        }
        self.preflight(id, slot, true)?;
        let (aot_abs, image_len, digest, dst, capacity) = {
            let meta = &self
                .images
                .get(id)
                .ok_or(XdpSwapError::UnknownImage)?
                .meta;
            let slot_state = self.slots.get(slot).ok_or(XdpSwapError::UnknownSlot)?;
            (
                meta.aot_abs(),
                meta.image_len,
                meta.digest,
                slot_state.slot.base() as *mut u8,
                slot_state.slot.len(),
            )
        };
        if image_len > capacity {
            return Err(self.fail(XdpSwapError::ImageTooLarge));
        }
        if image_len > DEFAULT_IMAGE_CAPACITY {
            return Err(self.fail(XdpSwapError::ScratchTooSmall));
        }
        // One control-path scratch. `stage` is not reentrant and must not run
        // from the RX callback, which is also what keeps this core off the
        // in-flight packet.
        let buf = unsafe { &mut *core::ptr::addr_of_mut!(STAGE_BUF) };
        self.table
            .read(aot_abs, &mut buf[..image_len])
            .map_err(|e| self.fail(e.into()))?;
        if xpost_elf::sha256(&buf[..image_len]) != digest {
            return Err(self.fail(XdpSwapError::Digest));
        }
        let generation = self.alloc_generation();
        unsafe {
            core::ptr::copy_nonoverlapping(buf.as_ptr(), dst, image_len);
            core::ptr::write_bytes(dst.add(image_len), 0, capacity - image_len);
        }
        Self::fence_instruction_cache();
        let copied = unsafe { core::slice::from_raw_parts(dst, image_len) };
        if copied != &buf[..image_len] {
            return Err(self.fail(XdpSwapError::Digest));
        }
        let slot_state = self
            .slots
            .get_mut(slot)
            .ok_or(XdpSwapError::UnknownSlot)?;
        slot_state.loaded_len = image_len;
        slot_state.image = Some(id);
        slot_state.phase = SlotPhase::Staged { generation };
        if let Some(image) = self.images.get_mut(id) {
            // The bytes now live in this slot, and `lease` will check that the
            // image it publishes is the one this slot holds.
            image.meta.set_location(XdpLocation::Slot { index: slot });
        }
        self.last_error = 0;
        Ok(StageReport { generation, mcu_us: 0 })
    }

    /// Quiesce, publish `slot`'s staged image, or restore the previous entry.
    pub fn activate(
        &mut self,
        slot: SlotId,
        generation: u32,
        policy: QuiescePolicy,
    ) -> Result<SwitchReport, XdpSwapError> {
        if !self.leased {
            return Err(self.fail(XdpSwapError::NotLeased));
        }
        if self.severe {
            return Err(self.fail(XdpSwapError::RollbackFailed));
        }
        let phase = self
            .slots
            .get(slot)
            .ok_or(XdpSwapError::UnknownSlot)?
            .phase;
        match phase {
            SlotPhase::Staged { generation: staged } if staged == generation => {}
            SlotPhase::Staged { .. } => return Err(self.fail(XdpSwapError::BadGeneration)),
            _ => return Err(self.fail(XdpSwapError::NoStaged)),
        }
        let id = self
            .slots
            .get(slot)
            .and_then(|s| s.image)
            .ok_or(XdpSwapError::NoStaged)?;
        self.preflight(id, slot, false)?;
        self.commit(slot, id, generation, policy)
    }

    pub fn status(&self) -> XdpStatus {
        let mut active_slot = None;
        let mut staged_slot = None;
        let mut staged_generation = 0;
        for (i, slot) in self.slots.iter().enumerate() {
            match slot.phase {
                SlotPhase::Active { .. } => active_slot = Some(i),
                SlotPhase::Staged { generation } => {
                    staged_slot = Some(i);
                    staged_generation = generation;
                }
                SlotPhase::Empty => {}
            }
        }
        XdpStatus {
            boot_id: BOOT_ID.load(Ordering::Relaxed),
            active_slot,
            staged_slot,
            staged_generation,
            updating: self.updating,
            severe: self.severe,
            quiesce_drop: QUIESCE_DROPS.load(Ordering::Relaxed) as u32,
            quiesce_pass: QUIESCE_PASSES.load(Ordering::Relaxed) as u32,
            last_error: self.last_error,
        }
    }

    fn alloc_generation(&mut self) -> u32 {
        let generation = self.next_generation;
        self.next_generation = self.next_generation.wrapping_add(1).max(1);
        generation
    }

    fn fail(&mut self, err: XdpSwapError) -> XdpSwapError {
        self.last_error = err.code();
        err
    }

    /// `staging` rejects a slot whose phase is Active. Commit rechecks the image.
    fn preflight(&mut self, id: XdpId, slot: SlotId, staging: bool) -> Result<(), XdpSwapError> {
        let meta_ok = {
            let Some(image) = self.images.get(id) else {
                return Err(self.fail(XdpSwapError::UnknownImage));
            };
            let meta = &image.meta;
            if meta.image_len == 0 {
                return Err(self.fail(XdpSwapError::ImageEmpty));
            }
            if meta.digest == [0u8; 32] {
                return Err(self.fail(XdpSwapError::Digest));
            }
            if let Err(e) = map_ready(meta.check_arch()) {
                return Err(self.fail(e));
            }
            if let Err(e) = map_ready(meta.check_helper_address()) {
                return Err(self.fail(e));
            }
            let Some(slot_state) = self.slots.get(slot) else {
                return Err(self.fail(XdpSwapError::UnknownSlot));
            };
            if staging {
                match slot_state.phase {
                    SlotPhase::Active { .. } => {
                        return Err(self.fail(XdpSwapError::ActiveSlot));
                    }
                    SlotPhase::Staged { .. } if slot_state.image != Some(id) => {
                        return Err(self.fail(XdpSwapError::SlotOccupied));
                    }
                    SlotPhase::Staged { .. } | SlotPhase::Empty => {}
                }
            }
            if meta.image_len > slot_state.slot.len() {
                return Err(self.fail(XdpSwapError::ImageTooLarge));
            }
            if let Err(e) = map_ready(meta.check_slot_address(&slot_state.slot)) {
                return Err(self.fail(e));
            }
            meta.required_maps.is_empty()
        };
        if meta_ok {
            return Ok(());
        }
        self.preflight_maps(id)
    }

    fn preflight_maps(&mut self, id: XdpId) -> Result<(), XdpSwapError> {
        let Some(lock) = crate::map::MAP_RT.try_get() else {
            return Err(self.fail(XdpSwapError::MapsRequired));
        };
        let maps = match lock.try_lock() {
            Ok(maps) => maps,
            Err(_) => return Err(self.fail(XdpSwapError::Busy)),
        };
        let Some(image) = self.images.get(id) else {
            return Err(self.fail(XdpSwapError::UnknownImage));
        };
        let reqs = &image.meta.required_maps;
        for req in reqs {
            if let Err(e) = maps.check_installed(req) {
                let err = match e {
                    MapCheckError::NotFound => XdpSwapError::MissingMap,
                    other => XdpSwapError::MapCheck(other),
                };
                return Err(self.fail(err));
            }
        }
        drop(maps);
        Ok(())
    }

    fn commit(
        &mut self,
        slot: SlotId,
        id: XdpId,
        generation: u32,
        policy: QuiescePolicy,
    ) -> Result<SwitchReport, XdpSwapError> {
        let pairs = {
            let Some(lock) = crate::map::MAP_RT.try_get() else {
                return Err(self.fail(XdpSwapError::MapsRequired));
            };
            let mut maps = lock.try_lock().map_err(|_| XdpSwapError::Busy)?;
            self.ensure_binds(&mut maps, id)?
        };
        let from_slot = self.primary;
        let old_binds = self.map_lease.vmr.alias_pairs();
        let drop_before = QUIESCE_DROPS.load(Ordering::Relaxed);
        let pass_before = QUIESCE_PASSES.load(Ordering::Relaxed);
        self.updating = true;
        QUIESCE_PASS.store(matches!(policy, QuiescePolicy::Pass), Ordering::SeqCst);
        PAUSE.store(true, Ordering::SeqCst);
        while IN_FLIGHT.load(Ordering::SeqCst) != 0 {
            core::hint::spin_loop();
        }
        let Some(lock) = crate::map::MAP_RT.try_get() else {
            self.leave_quiesce();
            return Err(self.fail(XdpSwapError::MapsRequired));
        };
        let mut maps = match lock.try_lock() {
            Ok(maps) => maps,
            Err(_) => {
                self.leave_quiesce();
                return Err(self.fail(XdpSwapError::Busy));
            }
        };
        if let Err(e) = self.publish_binds(&mut maps, &pairs) {
            return match self.restore_binds(&maps, old_binds.as_slice()) {
                Ok(()) => {
                    self.leave_quiesce();
                    self.last_error = e.code();
                    Ok(self.report(
                        false,
                        true,
                        from_slot,
                        slot,
                        generation,
                        drop_before,
                        pass_before,
                        0,
                    ))
                }
                Err(_) => {
                    self.severe = true;
                    self.updating = true;
                    self.last_error = XdpSwapError::RollbackFailed.code();
                    QUIESCE_PASS.store(false, Ordering::SeqCst);
                    Err(XdpSwapError::RollbackFailed)
                }
            };
        }
        let entry = self.slots[slot].slot.base();
        PRIMARY_ENTRY.store(entry, Ordering::Release);
        if from_slot != slot
            && let Some(prev) = self.slots.get_mut(from_slot)
            && let SlotPhase::Active { generation } = prev.phase
        {
            prev.phase = SlotPhase::Staged { generation };
        }
        if let Some(next) = self.slots.get_mut(slot) {
            next.phase = SlotPhase::Active { generation };
            next.image = Some(id);
        }
        self.primary = slot;
        if let Some(image) = self.images.get_mut(id) {
            image.meta.set_location(XdpLocation::Slot { index: slot });
        }
        let stamped = mcu_now();
        self.leave_quiesce();
        self.last_error = 0;
        let _ = ACTIVE.load(Ordering::Relaxed);
        Ok(self.report(
            true,
            false,
            from_slot,
            slot,
            generation,
            drop_before,
            pass_before,
            stamped,
        ))
    }

    fn leave_quiesce(&mut self) {
        PAUSE.store(false, Ordering::Release);
        self.updating = false;
    }

    fn report(
        &self,
        ok: bool,
        rolled_back: bool,
        from_slot: usize,
        to_slot: usize,
        generation: u32,
        drop_before: usize,
        pass_before: usize,
        mcu_us: u32,
    ) -> SwitchReport {
        SwitchReport {
            ok,
            rolled_back,
            severe: self.severe,
            from_slot,
            to_slot,
            generation,
            quiesce_drop: QUIESCE_DROPS.load(Ordering::Relaxed).saturating_sub(drop_before) as u32,
            quiesce_pass: QUIESCE_PASSES.load(Ordering::Relaxed).saturating_sub(pass_before) as u32,
            mcu_us,
        }
    }

    fn ensure_binds(
        &mut self,
        maps: &mut MapRtGuard<'_>,
        id: XdpId,
    ) -> Result<heapless::Vec<(usize, usize), MAX_VMR_BINDS>, XdpSwapError> {
        let binds: heapless::Vec<(alloc::string::String, usize), MAX_REQUIRED_MAPS> = self
            .images
            .get(id)
            .ok_or(XdpSwapError::UnknownImage)?
            .meta
            .aot_binds
            .clone();
        let mut pairs = heapless::Vec::new();
        let mut added = heapless::Vec::<usize, MAX_MAP_REFS>::new();
        let built = (|| {
            for (name, vma) in &binds {
                if *vma == 0 {
                    continue;
                }
                let idx = maps.index_of(name).ok_or(XdpSwapError::MissingMap)?;
                if !self.map_lease.refs.contains(&idx) {
                    if added.push(idx).is_err() {
                        return Err(XdpSwapError::AliasFull);
                    }
                    if self.map_lease.refs.push(idx).is_err() {
                        added.pop();
                        return Err(XdpSwapError::AliasFull);
                    }
                    if let Err(e) = maps.add_user(idx) {
                        added.pop();
                        self.map_lease.refs.pop();
                        return Err(XdpSwapError::Map(e));
                    }
                }
                pairs
                    .push((*vma, idx))
                    .map_err(|_| XdpSwapError::AliasFull)?;
            }
            Ok(pairs)
        })();
        if let Err(e) = &built {
            for idx in added.into_iter().rev() {
                self.map_lease.refs.retain(|r| *r != idx);
                let _ = maps.remove_user(idx);
            }
            return Err(self.fail(*e));
        }
        built
    }

    fn publish_binds(
        &mut self,
        maps: &mut MapRtGuard<'_>,
        pairs: &[(usize, usize)],
    ) -> Result<(), XdpSwapError> {
        crate::map::clear_map_aliases();
        crate::map::set_map_aliases(pairs).map_err(|_| XdpSwapError::AliasFull)?;
        crate::map::set_fast_arrays(maps, pairs);
        self.map_lease.vmr.clear();
        for &(vma, idx) in pairs {
            self.map_lease
                .vmr
                .insert(vma, idx)
                .map_err(|_| XdpSwapError::AliasFull)?;
        }
        Ok(())
    }

    fn restore_binds(
        &mut self,
        maps: &MapRtGuard<'_>,
        pairs: &[(usize, usize)],
    ) -> Result<(), XdpSwapError> {
        crate::map::clear_map_aliases();
        crate::map::set_map_aliases(pairs).map_err(|_| XdpSwapError::AliasFull)?;
        crate::map::set_fast_arrays(maps, pairs);
        self.map_lease.vmr.clear();
        for &(vma, idx) in pairs {
            self.map_lease
                .vmr
                .insert(vma, idx)
                .map_err(|_| XdpSwapError::AliasFull)?;
        }
        Ok(())
    }
}

pub(crate) fn mcu_now() -> u32 {
    esp_hal::time::Instant::now().duration_since_epoch().as_micros() as u32
}
fn map_ready(result: Result<(), ReadyError>) -> Result<(), XdpSwapError> {
    result.map_err(|e| match e {
        ReadyError::Arch => XdpSwapError::Arch,
        ReadyError::HelperAddress => XdpSwapError::HelperAddress,
        ReadyError::HelperAddressMismatch => XdpSwapError::HelperAddressMismatch,
        ReadyError::SlotAddress => XdpSwapError::SlotAddress,
        ReadyError::SlotAddressAlign => XdpSwapError::SlotAddressAlign,
        ReadyError::ImageEmpty => XdpSwapError::ImageEmpty,
        ReadyError::ImageTooLarge => XdpSwapError::ImageTooLarge,
        _ => XdpSwapError::UnknownImage,
    })
}
