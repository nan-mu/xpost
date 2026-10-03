//! Client [`Command`] formatting ↔ server [`parse_command`].

use core::fmt::Write;

use crate::r#type::command::Command;
use crate::server::command::{ParseCommandError, parse_command};

fn encode(cmd: &Command<'_>) -> String {
    cmd.to_string()
}

fn assert_cmd_eq(got: &Command<'_>, expect: &Command<'_>) {
    match (got, expect) {
        (Command::Ping, Command::Ping)
        | (Command::EnableXdp, Command::EnableXdp)
        | (Command::DisableXdp, Command::DisableXdp)
        | (Command::ClearPerf, Command::ClearPerf)
        | (Command::GetPerf, Command::GetPerf)
        | (Command::GetInit, Command::GetInit) => {}
        (Command::ClearMap { name: a }, Command::ClearMap { name: b })
        | (Command::GetMap { name: a }, Command::GetMap { name: b }) => {
            assert_eq!(a, b, "map name");
        }
        (
            Command::UpdateMap {
                name: a,
                key: ak,
                value: av,
            },
            Command::UpdateMap {
                name: b,
                key: bk,
                value: bv,
            },
        ) => {
            assert_eq!(a, b, "map name");
            assert_eq!(ak, bk, "map key");
            assert_eq!(av, bv, "map value");
        }
        _ => panic!("command variant mismatch"),
    }
}

fn parse_ok<'a>(line: &'a [u8]) -> Command<'a> {
    match parse_command(line) {
        Ok(cmd) => cmd,
        Err(_) => panic!("parse encoded command failed"),
    }
}

fn roundtrip(cmd: Command<'_>) {
    let line = encode(&cmd);
    let parsed = parse_ok(line.as_bytes());
    assert_cmd_eq(&parsed, &cmd);
    // UART path appends LF; parser must accept it too.
    let with_lf = format!("{line}\n");
    let parsed_lf = parse_ok(with_lf.as_bytes());
    assert_cmd_eq(&parsed_lf, &cmd);
}

#[test]
fn ping() {
    roundtrip(Command::Ping);
}

#[test]
fn enable_xdp() {
    roundtrip(Command::EnableXdp);
}

#[test]
fn disable_xdp() {
    roundtrip(Command::DisableXdp);
}

#[test]
fn clear_map() {
    roundtrip(Command::ClearMap { name: "counter" });
}

#[test]
fn get_map() {
    roundtrip(Command::GetMap { name: "counter" });
}

#[test]
fn update_map() {
    roundtrip(Command::UpdateMap {
        name: "counter",
        key: b"0",
        value: b"1",
    });
}

#[test]
fn clear_perf() {
    roundtrip(Command::ClearPerf);
}

#[test]
fn get_perf() {
    roundtrip(Command::GetPerf);
}

#[test]
fn get_init_is_not_wire() {
    // Host-only: Display must refuse serialization.
    let mut s = String::new();
    assert!(write!(s, "{}", Command::GetInit).is_err());
    assert!(s.is_empty());

    // Server must not accept a `get init` request line.
    match parse_command(b"get init") {
        Err(ParseCommandError::Unknown) => {}
        Err(_) => panic!("expected Unknown for `get init`"),
        Ok(_) => panic!("`get init` must not parse as a wire command"),
    }
}
