//! Server reply encoding ↔ client reply parsing.

use crate::r#type::command::Command;
use crate::r#type::reply::{OkReply, ServerError};
use crate::server::reply::{write_error, write_ok};

fn encode_ok(reply: &OkReply<'_>) -> Vec<u8> {
    let mut buf = Vec::new();
    write_ok(reply, |chunk| {
        buf.extend_from_slice(chunk);
        Ok::<(), ()>(())
    })
    .unwrap();
    buf
}

fn encode_err(error: &ServerError) -> Vec<u8> {
    let mut buf = Vec::new();
    write_error(error, |chunk| {
        buf.extend_from_slice(chunk);
        Ok::<(), ()>(())
    })
    .unwrap();
    buf
}

fn assert_ok_eq(got: &OkReply<'_>, expect: &OkReply<'_>) {
    match (got, expect) {
        (OkReply::Ping, OkReply::Ping)
        | (OkReply::EnableXdp, OkReply::EnableXdp)
        | (OkReply::DisableXdp, OkReply::DisableXdp)
        | (OkReply::ClearMap, OkReply::ClearMap)
        | (OkReply::ClearPerf, OkReply::ClearPerf)
        | (OkReply::UpdateMap, OkReply::UpdateMap)
        | (OkReply::Init, OkReply::Init) => {}
        (OkReply::GetMap { blob: a }, OkReply::GetMap { blob: b })
        | (OkReply::GetPerf { blob: a }, OkReply::GetPerf { blob: b }) => {
            assert_eq!(a, b, "blob");
        }
        _ => panic!("ok reply variant mismatch"),
    }
}

fn roundtrip_ok(cmd: Command<'_>, reply: OkReply<'_>) {
    let raw = encode_ok(&reply);
    let parsed = OkReply::try_from((raw.as_slice(), &cmd)).expect("parse encoded OK");
    assert_ok_eq(&parsed, &reply);
}

fn assert_server_err_eq(got: &ServerError, expect: &ServerError) {
    match (got, expect) {
        (ServerError::Unknown, ServerError::Unknown)
        | (ServerError::Timeout, ServerError::Timeout)
        | (ServerError::Logic, ServerError::Logic)
        | (ServerError::Busy, ServerError::Busy)
        | (ServerError::Unsupported, ServerError::Unsupported) => {}
        _ => panic!("server error variant mismatch"),
    }
}

fn roundtrip_server_err(cmd: Command<'_>, err: ServerError) {
    let raw = encode_err(&err);
    let parsed = ServerError::try_from((raw.as_slice(), &cmd)).expect("parse encoded ERR");
    assert_server_err_eq(&parsed, &err);
}

#[test]
fn ok_ping() {
    roundtrip_ok(Command::Ping, OkReply::Ping);
}

#[test]
fn ok_enable_xdp() {
    roundtrip_ok(Command::EnableXdp, OkReply::EnableXdp);
}

#[test]
fn ok_disable_xdp() {
    roundtrip_ok(Command::DisableXdp, OkReply::DisableXdp);
}

#[test]
fn ok_clear_map() {
    roundtrip_ok(Command::ClearMap { name: "counter" }, OkReply::ClearMap);
}

#[test]
fn ok_clear_perf() {
    roundtrip_ok(Command::ClearPerf, OkReply::ClearPerf);
}

#[test]
fn ok_update_map() {
    roundtrip_ok(
        Command::UpdateMap {
            name: "counter",
            key: b"0",
            value: b"1",
        },
        OkReply::UpdateMap,
    );
}

#[test]
fn ok_init() {
    // Unsolicited settle line; host waits via GetInit (no UART request).
    roundtrip_ok(Command::GetInit, OkReply::Init);
}

#[test]
fn ok_get_map_empty_blob() {
    roundtrip_ok(
        Command::GetMap { name: "counter" },
        OkReply::GetMap { blob: b"" },
    );
}

#[test]
fn ok_get_map_blob() {
    let blob = b"\x00\x01\x02\xff";
    roundtrip_ok(
        Command::GetMap { name: "counter" },
        OkReply::GetMap { blob },
    );
}

#[test]
fn ok_get_perf_empty_blob() {
    roundtrip_ok(Command::GetPerf, OkReply::GetPerf { blob: b"" });
}

#[test]
fn ok_get_perf_blob() {
    let blob = b"perf-bytes";
    roundtrip_ok(Command::GetPerf, OkReply::GetPerf { blob });
}

#[test]
fn err_server_unknown() {
    roundtrip_server_err(Command::Ping, ServerError::Unknown);
}

#[test]
fn err_server_timeout() {
    roundtrip_server_err(Command::GetPerf, ServerError::Timeout);
}

#[test]
fn err_server_logic() {
    roundtrip_server_err(Command::EnableXdp, ServerError::Logic);
}

#[test]
fn err_server_busy() {
    roundtrip_server_err(Command::DisableXdp, ServerError::Busy);
}

#[test]
fn err_server_unsupported() {
    roundtrip_server_err(Command::ClearPerf, ServerError::Unsupported);
}
