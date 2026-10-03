//! Server reply encoding ↔ client reply parsing (OK + all ERR domains).

use crate::r#type::command::Command;
use crate::r#type::reply::{MapError, OkReply, PerfError, ServerError, SyntaxError};
use crate::server::reply::{WireError, write_error, write_ok};

fn encode_ok(reply: &OkReply<'_>) -> Vec<u8> {
    let mut buf = Vec::new();
    write_ok(reply, |chunk| {
        buf.extend_from_slice(chunk);
        Ok::<(), ()>(())
    })
    .unwrap();
    buf
}

fn encode_err(error: &(impl WireError + ?Sized)) -> Vec<u8> {
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

fn roundtrip_server_err(cmd: &Command<'_>, err: ServerError) {
    let raw = encode_err(&err);
    let parsed = ServerError::try_from((raw.as_slice(), cmd)).expect("parse ServerError");
    assert!(core::mem::discriminant(&parsed) == core::mem::discriminant(&err));
}

fn roundtrip_map_err(cmd: &Command<'_>, err: MapError) {
    let raw = encode_err(&err);
    let parsed = MapError::try_from((raw.as_slice(), cmd)).expect("parse MapError");
    assert!(core::mem::discriminant(&parsed) == core::mem::discriminant(&err));
}

fn roundtrip_perf_err(cmd: &Command<'_>, err: PerfError) {
    let raw = encode_err(&err);
    let parsed = PerfError::try_from((raw.as_slice(), cmd)).expect("parse PerfError");
    assert!(core::mem::discriminant(&parsed) == core::mem::discriminant(&err));
}

fn roundtrip_syntax_err(cmd: &Command<'_>, err: SyntaxError) {
    let raw = encode_err(&err);
    let parsed = SyntaxError::try_from((raw.as_slice(), cmd)).expect("parse SyntaxError");
    assert!(core::mem::discriminant(&parsed) == core::mem::discriminant(&err));
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
fn err_server_all() {
    let cmd = Command::Ping;
    for err in [
        ServerError::Unknown,
        ServerError::Timeout,
        ServerError::Logic,
        ServerError::Busy,
        ServerError::Unsupported,
    ] {
        roundtrip_server_err(&cmd, err);
    }
}

#[test]
fn err_map_all() {
    let cmd = Command::GetMap { name: "counter" };
    for err in [
        MapError::NameNotFound,
        MapError::KeyNotFound,
        MapError::InvalidName,
        MapError::InvalidKey,
        MapError::InvalidValue,
        MapError::Full,
        MapError::Unsupported,
        MapError::BufferTooSmall,
    ] {
        roundtrip_map_err(&cmd, err);
    }
}

#[test]
fn err_perf_all() {
    let cmd = Command::GetPerf;
    for err in [
        PerfError::Unavailable,
        PerfError::Unsupported,
        PerfError::BufferTooSmall,
    ] {
        roundtrip_perf_err(&cmd, err);
    }
}

#[test]
fn err_syntax_all() {
    let cmd = Command::Ping;
    for err in [
        SyntaxError::BadArity,
        SyntaxError::InvalidUtf8,
        SyntaxError::LineTooLong,
    ] {
        roundtrip_syntax_err(&cmd, err);
    }
}

#[test]
fn err_domains_do_not_cross_parse() {
    let map_line = encode_err(&MapError::NameNotFound);
    let cmd = Command::GetMap { name: "x" };
    assert!(ServerError::try_from((map_line.as_slice(), &cmd)).is_err());
    assert!(PerfError::try_from((map_line.as_slice(), &cmd)).is_err());
    assert!(SyntaxError::try_from((map_line.as_slice(), &cmd)).is_err());
    assert!(MapError::try_from((map_line.as_slice(), &cmd)).is_ok());

    let perf_line = encode_err(&PerfError::Unavailable);
    assert!(MapError::try_from((perf_line.as_slice(), &Command::GetPerf)).is_err());
    assert!(ServerError::try_from((perf_line.as_slice(), &Command::GetPerf)).is_err());
}
