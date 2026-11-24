use xpost_perf::network::{DICTIONARY_VERSION, probe_sequence, stimulus_payload};
use xpost_perf::session::{PerfError, PerfSession, Variant};
use xpost_perf::snapshot::{Boundary, SNAPSHOT_HEADER_LEN, Snapshot, decode_pair};
use xpost_perf::trace::{FLAG_OVERFLOW, PROBE_UNATTRIBUTED, TraceEvent, TraceHeader, TraceKind};
use xpost_perf::{DecodeError, Entry, Visitor, visit_entries};

const UUID: [u8; 16] = [0x39; 16];
const PROFILE: [u8; 32] = [0x47; 32];
const BOOT: u64 = 0x0123_4567_89ab_cdef;

#[test]
fn metrics_holds_two_stamped_counter_reports() {
    let mut session = PerfSession::<0, 512>::new(BOOT, 1_000_000, 64, DICTIONARY_VERSION);
    let (epoch, boot) = session.prepare(UUID, Variant::Metrics, PROFILE).unwrap();
    assert_eq!(boot, BOOT);
    assert_eq!(session.arm(epoch, 10), Err(PerfError::WrongPhase));
    assert_eq!(
        session.arm_metrics(epoch, 10, [].into_iter()),
        Err(PerfError::InvalidSnapshot)
    );
    let counters = |rx, discarded| [("socket_rx", rx), ("socket_discarded", discarded)];
    session
        .arm_metrics(epoch, 12, counters(14, 0).into_iter())
        .unwrap();
    assert_eq!(session.freeze(epoch, 22), Err(PerfError::WrongPhase));
    assert_eq!(
        session.freeze_metrics(epoch + 1, 22, counters(25, 7).into_iter()),
        Err(PerfError::WrongEpoch)
    );
    let report = session
        .freeze_metrics(epoch, 22, counters(25, 7).into_iter())
        .unwrap();
    let mut export = Vec::new();
    for offset in (0..report.len).step_by(37) {
        let mut bytes = [0; 37];
        let part = session.read(epoch, offset, 37, &mut bytes).unwrap();
        assert_eq!((part.total, part.digest), (report.len, report.digest));
        export.extend_from_slice(&bytes[..part.copied]);
    }
    assert_eq!(export.len(), 2 * (SNAPSHOT_HEADER_LEN + 10 + 19 + 26));
    assert_eq!(
        report.digest,
        export
            .iter()
            .fold(2_166_136_261u32, |hash, &byte| (hash ^ u32::from(byte))
                .wrapping_mul(16_777_619))
    );
    let (arm, freeze) = decode_pair(&export).unwrap();
    for (snapshot, boundary, tick, expected) in [
        (arm, Boundary::Arm, 12, counters(14, 0)),
        (freeze, Boundary::Freeze, 22, counters(25, 7)),
    ] {
        assert_eq!(snapshot.run_uuid, UUID);
        assert_eq!(snapshot.stamp.epoch, epoch);
        assert_eq!(snapshot.stamp.boot_id, BOOT);
        assert_eq!(snapshot.stamp.profile_hash, PROFILE);
        assert_eq!(snapshot.boundary, boundary);
        assert_eq!(snapshot.tick, tick);
        assert_eq!((snapshot.tick_hz, snapshot.tick_bits), (1_000_000, 64));
        struct Values(Vec<(String, u64)>);
        impl Visitor for Values {
            fn entry(&mut self, entry: &Entry<'_>) -> Result<(), DecodeError> {
                self.0.push((entry.name.into(), entry.value.0));
                Ok(())
            }
        }
        let mut values = Values(Vec::new());
        visit_entries(snapshot.pfr, &mut values).unwrap();
        assert_eq!(
            values.0,
            expected.map(|(name, value)| (name.to_owned(), value))
        );
    }
    let split = SNAPSHOT_HEADER_LEN + arm.pfr.len();
    assert!(Snapshot::decode(&export[split..]).unwrap().1.is_empty());
    for offset in [12, 28, 32, 40, 72, 80] {
        let mut changed = export.clone();
        changed[split + offset] ^= if offset == 72 { 0b1_0111 } else { 1 };
        assert!(decode_pair(&changed).is_none());
    }
    assert!(decode_pair(&export[..export.len() - 1]).is_none());
    let mut repeated = vec![0; export.len()];
    session.read(epoch, 0, export.len(), &mut repeated).unwrap();
    assert_eq!(repeated, export);
    session.release(epoch).unwrap();
    assert!(session.read(epoch, 0, 1, &mut repeated).is_err());
}

#[test]
fn trace_capabilities_and_overflow_do_not_invent_events() {
    let mut session = PerfSession::<1>::new(BOOT, 1_000_000, 64, DICTIONARY_VERSION);
    assert_eq!(
        session.prepare(UUID, Variant::Metrics, PROFILE),
        Err(PerfError::Unsupported)
    );
    let (epoch, _) = session.prepare(UUID, Variant::Trace, PROFILE).unwrap();
    session.arm(epoch, 1).unwrap();
    let first = TraceEvent {
        sequence: 0,
        event_id: 15,
        kind: TraceKind::Instant,
        context_id: 2,
        tick: 5,
        probe_seq: 24,
        span_id: 0,
        parent_span_id: 0,
        arg0: 32,
        arg1: 0,
    };
    session.push_event(epoch, first).unwrap();
    session
        .push_event(
            epoch,
            TraceEvent {
                sequence: 1,
                tick: 7,
                ..first
            },
        )
        .unwrap();
    let report = session.freeze(epoch, 9).unwrap();
    assert_eq!((report.flags, report.len), (FLAG_OVERFLOW, 97));
    let mut bytes = [0; 97];
    session.read(epoch, 0, bytes.len(), &mut bytes).unwrap();
    let header = TraceHeader::decode(&bytes).unwrap();
    assert_eq!(
        (header.dictionary_version, header.written, header.dropped),
        (DICTIONARY_VERSION, 1, 1)
    );
    assert_eq!(TraceEvent::decode(&bytes[57..]), Some(first));
}

#[test]
fn only_valid_jtx1_from_this_run_attributes_a_socket_event() {
    let mut payload = [0u8; 40];
    payload[..8].copy_from_slice(b"JTX1\x01\0\0\x20");
    payload[8..24].copy_from_slice(&UUID);
    payload[24..32].copy_from_slice(&44u64.to_be_bytes());
    assert_eq!(probe_sequence(&payload, &UUID), Some(44));
    assert_eq!(
        probe_sequence(&payload, &PROFILE[..16].try_into().unwrap()),
        None
    );
    assert_eq!(probe_sequence(&payload[..31], &UUID), None);
    payload[24..32].copy_from_slice(&PROBE_UNATTRIBUTED.to_be_bytes());
    assert_eq!(probe_sequence(&payload, &UUID), None);
}

#[test]
fn driver_accepts_only_udp_bounds_and_the_configured_firewall_tag_offset() {
    let mut frame = [0u8; 84];
    frame[12..14].copy_from_slice(&[0x08, 0x00]);
    frame[14] = 0x45;
    frame[16..18].copy_from_slice(&70u16.to_be_bytes());
    frame[23] = 17;
    frame[36..38].copy_from_slice(&8080u16.to_be_bytes());
    frame[38..40].copy_from_slice(&50u16.to_be_bytes());
    frame[42..44].copy_from_slice(&11u16.to_le_bytes());
    frame[44..52].copy_from_slice(b"JTX1\x01\0\0\x20");
    frame[52..68].copy_from_slice(&UUID);
    frame[68..76].copy_from_slice(&1u64.to_be_bytes());

    let payload = stimulus_payload(&frame).unwrap();
    assert_eq!(probe_sequence(payload, &UUID), None);
    assert_eq!(probe_sequence(&payload[2..], &UUID), Some(1));
    assert_eq!(probe_sequence(&payload[2..], &[0; 16]), None);
    assert!(stimulus_payload(&frame[..83]).is_none());

    frame[20] = 0x20;
    assert!(stimulus_payload(&frame).is_none());
    frame[20] = 0x40;
    assert!(stimulus_payload(&frame).is_some());
    frame[39] -= 1;
    assert!(stimulus_payload(&frame).is_none());
}
