//! Decode the armed Trace export for the case switch.

use anyhow::{Result, ensure};
use uuid::Uuid;

const HEADER: usize = 57;
const EVENT: usize = 40;

pub(crate) struct Trace {
    pub epoch: u32,
    pub boot_id: u64,
    pub capacity: i32,
    pub dropped: i64,
    pub flags: i64,
    pub events: Vec<Event>,
}

pub(crate) struct Event {
    pub id: i32,
    pub sequence: i64,
    pub kind: &'static str,
    pub tick: u64,
    pub slot: i64,
    pub generation: i64,
}

pub(super) fn decode(bytes: &[u8], run: Uuid, epoch: u32, boot: u64) -> Result<Trace> {
    ensure!(bytes.len() >= HEADER, "truncated Trace header");
    let u16_at = |at| u16::from_le_bytes(bytes[at..at + 2].try_into().unwrap());
    let u32_at = |at| u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap());
    let u64_at = |at| u64::from_le_bytes(bytes[at..at + 8].try_into().unwrap());
    ensure!(
        u16_at(0) == 1 && u16_at(2) == 1,
        "unknown case Trace version"
    );
    ensure!(
        bytes[4..20] == *run.as_bytes() && u32_at(20) == epoch && u64_at(24) == boot,
        "case Trace belongs to another run, epoch, or boot"
    );
    ensure!(
        u64_at(32) == 1_000_000 && bytes[40] == 64,
        "unexpected DUT Trace clock"
    );
    let capacity = i32::try_from(u32_at(41))?;
    let written = u32_at(45) as usize;
    ensure!(
        capacity > 0 && written <= capacity as usize && (bytes.len() - HEADER) == written * EVENT,
        "invalid case Trace event count"
    );
    let mut events = Vec::with_capacity(written);
    for i in 0..written {
        let at = HEADER + i * EVENT;
        let id = i32::from(u16_at(at + 4));
        let kind = match bytes[at + 6] {
            1 => "begin",
            2 => "end",
            _ => anyhow::bail!("invalid case Trace event kind"),
        };
        ensure!(
            (1..=4).contains(&id) && bytes[at + 7] == 0,
            "unknown case Trace event"
        );
        events.push(Event {
            id,
            sequence: i64::from(u32_at(at)),
            kind,
            tick: u64_at(at + 8),
            slot: i64::from(u32_at(at + 32)),
            generation: i64::from(u32_at(at + 36)),
        });
    }
    Ok(Trace {
        epoch,
        boot_id: boot,
        capacity,
        dropped: i64::from(u32_at(49)),
        flags: i64::from(u32_at(53)),
        events,
    })
}

impl Trace {
    pub fn proves_publish(&self, generation: u32) -> bool {
        let expected = [
            (1, "begin"),
            (2, "begin"),
            (2, "end"),
            (3, "begin"),
            (3, "end"),
            (4, "begin"),
            (4, "end"),
            (1, "end"),
        ];
        self.flags == 0
            && self.dropped == 0
            && self.events.len() == expected.len()
            && self.events.windows(2).all(|w| w[0].tick <= w[1].tick)
            && self
                .events
                .iter()
                .zip(expected)
                .enumerate()
                .all(|(index, (event, (id, kind)))| {
                    event.sequence == index as i64
                        && event.id == id
                        && event.kind == kind
                        && event.slot == 1
                        && event.generation == i64::from(generation)
                })
    }
}
