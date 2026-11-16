//! Client [`Command`] formatting ↔ server [`parse_command`].

use core::fmt::Write;

use crate::server::command::{ParseCommandError, parse_command};
use crate::r#type::command::{Command, PerfVariant};

fn encode(cmd: &Command<'_>) -> String {
    cmd.to_string()
}

fn assert_cmd_eq(got: &Command<'_>, expect: &Command<'_>) {
    match (got, expect) {
        (Command::Ping, Command::Ping)
        | (Command::EnableXdp, Command::EnableXdp)
        | (Command::DisableXdp, Command::DisableXdp)
        | (Command::EnableExec, Command::EnableExec)
        | (Command::DisableExec, Command::DisableExec)
        | (Command::ClearPerf, Command::ClearPerf)
        | (Command::GetPerf, Command::GetPerf)
        | (Command::GetInit, Command::GetInit)
        | (Command::GetFinish, Command::GetFinish) => {}
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
        (
            Command::XdpFlash {
                name: a,
                slot: aslot,
            },
            Command::XdpFlash {
                name: b,
                slot: bslot,
            },
        ) => {
            assert_eq!(a, b);
            assert_eq!(aslot, bslot);
        }
        (
            Command::XdpSwitch {
                slot: aslot,
                generation: agen,
                pass: apass,
            },
            Command::XdpSwitch {
                slot: bslot,
                generation: bgen,
                pass: bpass,
            },
        ) => {
            assert_eq!(aslot, bslot);
            assert_eq!(agen, bgen);
            assert_eq!(apass, bpass);
        }
        (Command::GetXdpStatus, Command::GetXdpStatus) => {}
        (
            Command::PerfPrepare {
                uuid: au,
                kind: ak,
                profile: ap,
            },
            Command::PerfPrepare {
                uuid: bu,
                kind: bk,
                profile: bp,
            },
        ) => {
            assert_eq!(au, bu, "uuid");
            assert_eq!(ak, bk, "kind");
            assert_eq!(ap, bp, "profile");
        }
        (Command::PerfArm { epoch: a }, Command::PerfArm { epoch: b })
        | (Command::PerfFreeze { epoch: a }, Command::PerfFreeze { epoch: b })
        | (Command::PerfRelease { epoch: a }, Command::PerfRelease { epoch: b }) => {
            assert_eq!(a, b, "epoch");
        }
        (
            Command::PerfRead {
                epoch: ae,
                offset: ao,
                max_bytes: am,
            },
            Command::PerfRead {
                epoch: be,
                offset: bo,
                max_bytes: bm,
            },
        ) => {
            assert_eq!((ae, ao, am), (be, bo, bm));
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
fn enable_exec() {
    roundtrip(Command::EnableExec);
}

#[test]
fn disable_exec() {
    roundtrip(Command::DisableExec);
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

#[test]
fn xdp_stage() {
    roundtrip(Command::XdpFlash {
        name: "hot_mark",
        slot: 1,
    });
}

#[test]
fn xdp_activate() {
    roundtrip(Command::XdpSwitch {
        slot: 1,
        generation: 3,
        pass: false,
    });
}

#[test]
fn get_xdp_status() {
    roundtrip(Command::GetXdpStatus);
}

#[test]
fn get_finish_is_not_wire() {
    let mut s = String::new();
    assert!(write!(s, "{}", Command::GetFinish).is_err());
    assert!(s.is_empty());

    match parse_command(b"get finish") {
        Err(ParseCommandError::Unknown) => {}
        Err(_) => panic!("expected Unknown for `get finish`"),
        Ok(_) => panic!("`get finish` must not parse as a wire command"),
    }
}

fn sample_uuid() -> [u8; 16] {
    let mut uuid = [0u8; 16];
    let mut i = 0;
    while i < uuid.len() {
        uuid[i] = (i as u8).wrapping_mul(17).wrapping_add(0x0a);
        i += 1;
    }
    uuid
}

fn sample_profile() -> [u8; 32] {
    let mut profile = [0u8; 32];
    let mut i = 0;
    while i < profile.len() {
        profile[i] = (i as u8).wrapping_mul(3).wrapping_add(0x5c);
        i += 1;
    }
    profile
}

#[test]
fn perf_prepare_arm_freeze_read_release_roundtrip() {
    roundtrip(Command::PerfPrepare {
        uuid: sample_uuid(),
        kind: PerfVariant::Metrics,
        profile: sample_profile(),
    });
    roundtrip(Command::PerfPrepare {
        uuid: sample_uuid(),
        kind: PerfVariant::Trace,
        profile: sample_profile(),
    });
    roundtrip(Command::PerfArm { epoch: 7 });
    roundtrip(Command::PerfFreeze { epoch: 7 });
    roundtrip(Command::PerfRead {
        epoch: 7,
        offset: 40,
        max_bytes: 600,
    });
    roundtrip(Command::PerfRelease { epoch: 7 });
}

#[test]
fn perf_prepare_bad_hex_or_variant_is_bad_arity() {
    let line = Command::PerfPrepare {
        uuid: sample_uuid(),
        kind: PerfVariant::Metrics,
        profile: sample_profile(),
    }
    .to_string();
    let mut bad_hex = line.clone();
    let at = "perf prepare ".len();
    bad_hex.replace_range(at..at + 1, "g");
    assert!(matches!(
        parse_command(bad_hex.as_bytes()),
        Err(ParseCommandError::BadArity)
    ));
    let bad_kind = line.replacen("metrics", "neither", 1);
    assert!(matches!(
        parse_command(bad_kind.as_bytes()),
        Err(ParseCommandError::BadArity)
    ));
    let extra = format!("{line} extra");
    assert!(matches!(
        parse_command(extra.as_bytes()),
        Err(ParseCommandError::BadArity)
    ));
}
