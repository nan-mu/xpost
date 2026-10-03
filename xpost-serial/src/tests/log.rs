//! Transcript log checks over one full injectable-IO session.

use std::sync::OnceLock;

use crate::client::{Command, OkReply};
use crate::server::reply::ReplyWriter;
use crate::tests::harness::{TestError, with_session_logged};

const MAP_BLOB: &[u8] = b"\x01\x02\x03";
const PERF_BLOB: &[u8] = b"perf";

/// One complete App-mode run; shared by all log assert tests.
fn capture_full_session_log() -> &'static [String] {
    static LOG: OnceLock<Vec<String>> = OnceLock::new();
    LOG.get_or_init(|| {
        with_session_logged(
            |b| {
                b.on_ping(|reply: ReplyWriter<'_>| reply.ok(OkReply::Ping))
                    .on_enable_xdp(|reply: ReplyWriter<'_>| reply.ok(OkReply::EnableXdp))
                    .on_disable_xdp(|reply: ReplyWriter<'_>| reply.ok(OkReply::DisableXdp))
                    .on_clear_map(|name: &str, reply: ReplyWriter<'_>| {
                        assert_eq!(name, "counter");
                        reply.ok(OkReply::ClearMap);
                    })
                    .on_update_map(|name: &str, key: &[u8], value: &[u8], reply: ReplyWriter<'_>| {
                        assert_eq!((name, key, value), ("counter", &b"0"[..], &b"1"[..]));
                        reply.ok(OkReply::UpdateMap);
                    })
                    .on_clear_perf(|reply: ReplyWriter<'_>| reply.ok(OkReply::ClearPerf))
                    .on_get_map(|name: &str, reply: ReplyWriter<'_>| {
                        assert_eq!(name, "counter");
                        reply.get_map(MAP_BLOB.len(), |write| {
                            write(MAP_BLOB).unwrap();
                        });
                    })
                    .on_get_perf(|reply: ReplyWriter<'_>| {
                        reply.get_perf(PERF_BLOB.len(), |write| {
                            write(PERF_BLOB).unwrap();
                        });
                    })
                    .build()
            },
            |client| {
                client
                    .request::<TestError>(&Command::GetInit)
                    .expect("GetInit");
                client.request::<TestError>(&Command::Ping).expect("Ping");
                client
                    .request::<TestError>(&Command::EnableXdp)
                    .expect("EnableXdp");
                client
                    .request::<TestError>(&Command::DisableXdp)
                    .expect("DisableXdp");
                client
                    .request::<TestError>(&Command::ClearMap { name: "counter" })
                    .expect("ClearMap");
                client
                    .request::<TestError>(&Command::UpdateMap {
                        name: "counter",
                        key: b"0",
                        value: b"1",
                    })
                    .expect("UpdateMap");
                client
                    .request::<TestError>(&Command::ClearPerf)
                    .expect("ClearPerf");
                client
                    .request::<TestError>(&Command::GetMap { name: "counter" })
                    .expect("GetMap");
                client
                    .request::<TestError>(&Command::GetPerf)
                    .expect("GetPerf");
            },
        )
    })
}

/// Body after the leading RFC3339 timestamp.
fn body(line: &str) -> &str {
    line.split_once(' ').map(|(_, rest)| rest).unwrap_or(line)
}

fn bodies(log: &[String]) -> Vec<&str> {
    log.iter().map(|l| body(l)).collect()
}

/// Assert `needles` appear in order as subsequences of `haystack`.
fn assert_in_order(haystack: &[&str], needles: &[&str]) {
    let mut from = 0;
    for needle in needles {
        match haystack[from..].iter().position(|line| line == needle) {
            Some(rel) => from += rel + 1,
            None => panic!(
                "missing log line {needle:?} after index {from}\nfull bodies:\n{}",
                haystack.join("\n")
            ),
        }
    }
}

#[test]
fn log_has_rfc3339_prefix() {
    let log = capture_full_session_log();
    assert!(!log.is_empty(), "log should not be empty");
    for line in log {
        let stamp = line.split_whitespace().next().expect("timestamp token");
        assert_eq!(stamp.len(), 24, "rfc3339 millis stamp: {line}");
        assert!(stamp.ends_with('Z'), "stamp must end with Z: {line}");
        assert!(stamp.contains('T'), "stamp must contain T: {line}");
    }
}

#[test]
fn log_records_init() {
    let log = bodies(capture_full_session_log());
    assert_in_order(
        &log,
        &[
            "[cmd] → get init (host wait)",
            "[uart] OK init",
            "[cmd] ← OK init",
        ],
    );
}

#[test]
fn log_records_ping() {
    let log = bodies(capture_full_session_log());
    assert_in_order(
        &log,
        &["[cmd] → ping", "[uart] OK ping", "[cmd] ← OK ping"],
    );
}

#[test]
fn log_records_enable_disable_xdp() {
    let log = bodies(capture_full_session_log());
    assert_in_order(
        &log,
        &[
            "[cmd] → enable xdp",
            "[uart] OK enable xdp",
            "[cmd] ← OK enable xdp",
            "[cmd] → disable xdp",
            "[uart] OK disable xdp",
            "[cmd] ← OK disable xdp",
        ],
    );
}

#[test]
fn log_records_map_ops() {
    let log = bodies(capture_full_session_log());
    assert_in_order(
        &log,
        &[
            "[cmd] → clear map counter",
            "[uart] OK clear map",
            "[cmd] ← OK clear map",
            "[cmd] → update map counter 0 1",
            "[uart] OK update map",
            "[cmd] ← OK update map",
            "[cmd] → get map counter",
            "[uart] OK get map 3",
            "[cmd] ← OK get map",
        ],
    );
}

#[test]
fn log_records_perf_ops() {
    let log = bodies(capture_full_session_log());
    assert_in_order(
        &log,
        &[
            "[cmd] → clear perf",
            "[uart] OK clear perf",
            "[cmd] ← OK clear perf",
            "[cmd] → get perf",
            "[uart] OK get perf 4",
            "[cmd] ← OK get perf",
        ],
    );
}

#[test]
fn log_session_order_is_global() {
    // Coarse end-to-end order across the whole capture (init → … → get perf).
    let log = bodies(capture_full_session_log());
    assert_in_order(
        &log,
        &[
            "[cmd] → get init (host wait)",
            "[cmd] ← OK init",
            "[cmd] → ping",
            "[cmd] ← OK ping",
            "[cmd] → enable xdp",
            "[cmd] ← OK enable xdp",
            "[cmd] → disable xdp",
            "[cmd] ← OK disable xdp",
            "[cmd] → clear map counter",
            "[cmd] ← OK clear map",
            "[cmd] → update map counter 0 1",
            "[cmd] ← OK update map",
            "[cmd] → clear perf",
            "[cmd] ← OK clear perf",
            "[cmd] → get map counter",
            "[cmd] ← OK get map",
            "[cmd] → get perf",
            "[cmd] ← OK get perf",
        ],
    );
}
