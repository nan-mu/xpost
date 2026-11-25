//! Host-test DUT. The feature replaces the real device before startup.

use std::fs::OpenOptions;
use std::io::{self, Write};
use std::path::PathBuf;
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result, ensure};
use xpost_serial::FlashResult;
use xpost_serial::client::flash::InstallOutcome;
use xpost_serial::client::ops::FlashOp;
use xpost_serial::command::{Command, EXEC_BATCH_BYTES};
use xpost_serial::reply::OkReply;

static PERF: [u8; 600] = [0x39; 600];
static EXB2: [u8; EXEC_BATCH_BYTES] = [0x5a; EXEC_BATCH_BYTES];

pub struct Fixture {
    events: PathBuf,
}

pub fn device_path() -> Result<PathBuf> {
    let path = std::env::var_os("XPOST_AGENT_TEST_DEVICE").context("set fixture DUT path")?;
    Ok(std::fs::canonicalize(path)?)
}

pub fn request_timeout() -> Option<Duration> {
    Some(Duration::from_millis(
        std::env::var("XPOST_AGENT_TEST_REQUEST_TIMEOUT_MS")
            .ok()?
            .parse()
            .ok()?,
    ))
}

impl Fixture {
    pub fn open() -> Result<Self> {
        let fixture = Self {
            events: std::env::var_os("XPOST_AGENT_TEST_EVENTS")
                .context("set fixture events path")?
                .into(),
        };
        fixture.event("board-info")?;
        ensure!(
            std::env::var_os("XPOST_AGENT_TEST_BOARD_FAIL").is_none(),
            "fixture Board Info failed"
        );
        Ok(fixture)
    }

    fn event(&self, text: &str) -> io::Result<()> {
        writeln!(
            OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.events)?,
            "{text}"
        )
    }

    pub fn flash(&self, op: FlashOp) -> io::Result<FlashResult> {
        match op {
            FlashOp::Clear { names } => {
                if names
                    .iter()
                    .any(|name| !matches!(name.as_str(), "valid" | "other"))
                {
                    return Err(io::Error::other("xdp image not found"));
                }
                self.event(&format!("erase {:?}", names))?;
                Ok(FlashResult::Done)
            }
            FlashOp::Fw { .. } => {
                self.event("flash firmware")?;
                Ok(FlashResult::Done)
            }
            FlashOp::Xdp { .. } => {
                self.event("flash container")?;
                Ok(FlashResult::Install(InstallOutcome::Created { offset: 0 }))
            }
            FlashOp::List => Ok(FlashResult::List(Vec::new())),
        }
    }

    pub fn request(&self, cmd: &Command<'_>) -> io::Result<OkReply<'_>> {
        let digest = || {
            PERF.iter().fold(0x811c9dc5_u32, |hash, byte| {
                (hash ^ u32::from(*byte)).wrapping_mul(0x01000193)
            })
        };
        Ok(match cmd {
            Command::Ping => OkReply::Ping,
            Command::GetInit => OkReply::Init,
            Command::GetFinish => OkReply::Finish,
            Command::EnableXdp => OkReply::EnableXdp,
            Command::DisableXdp => OkReply::DisableXdp,
            Command::EnableExec => OkReply::EnableExec,
            Command::DisableExec => OkReply::DisableExec,
            Command::ClearMap { .. } => OkReply::ClearMap,
            Command::UpdateMap { .. } => OkReply::UpdateMap,
            Command::GetMap { .. } => OkReply::GetMap {
                blob: b"array 1\n0 42\n",
            },
            Command::ClearPerf => OkReply::ClearPerf,
            Command::GetPerf => OkReply::GetPerf { blob: b"CAGT" },
            Command::PerfPrepare { kind, .. } => {
                self.event(&format!("prepare {kind:?}"))?;
                OkReply::PerfPrepare {
                    epoch: 7,
                    boot_id: 42,
                }
            }
            Command::PerfArm { epoch: 99 } => {
                self.event("slow arm begin")?;
                thread::sleep(Duration::from_millis(1100));
                self.event("slow arm end")?;
                OkReply::PerfArm { tick: 21 }
            }
            Command::PerfArm { .. } => OkReply::PerfArm { tick: 21 },
            Command::PerfFreeze { .. } => {
                self.event("freeze")?;
                OkReply::PerfFreeze {
                    tick: 24,
                    flags: 2,
                    len: PERF.len() as u32,
                    digest: digest(),
                }
            }
            Command::PerfRead {
                offset, max_bytes, ..
            } => {
                let offset = *offset as usize;
                let end = (offset + (*max_bytes).min(512) as usize).min(PERF.len());
                let blob = PERF
                    .get(offset..end)
                    .ok_or_else(|| io::Error::other("fixture perf offset"))?;
                OkReply::PerfRead {
                    total: PERF.len() as u32,
                    digest: digest(),
                    blob,
                }
            }
            Command::PerfRelease { .. } => {
                self.event("release")?;
                OkReply::PerfRelease
            }
            Command::ExecBatchReset => {
                self.event("exec reset")?;
                OkReply::ExecBatchReset
            }
            Command::ExecBatchWrite { offset, data } => {
                self.event(&format!(
                    "exec write {offset} {}",
                    core::str::from_utf8(data).unwrap()
                ))?;
                OkReply::ExecBatchWrite
            }
            Command::ExecBatchInvoke => {
                self.event("exec invoke")?;
                if std::env::var_os("XPOST_AGENT_TEST_INVOKE_TIMEOUT").is_some() {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "fixture invoke timed out",
                    ));
                }
                OkReply::ExecBatchInvoke {
                    boot_id: 42,
                    begin_tick: 20,
                    end_tick: 30,
                    action: 2,
                }
            }
            Command::ExecBatchRead { offset, max_bytes } => {
                let start = *offset as usize;
                let end = start + *max_bytes as usize;
                let blob = EXB2
                    .get(start..end)
                    .ok_or_else(|| io::Error::other("fixture EXB2 range"))?;
                OkReply::ExecBatchRead { blob }
            }
            Command::XdpFlash { slot, .. } => OkReply::XdpFlash {
                slot: *slot,
                generation: 9,
                mcu_us: 0,
            },
            Command::XdpSwitch {
                slot,
                generation,
                pass,
            } => {
                self.event("switch")?;
                if *generation == 99 {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "fixture switch timed out",
                    ));
                }
                OkReply::XdpSwitch {
                    ok: u8::from(*generation == 9),
                    rollback: u8::from(*generation == 8),
                    severe: u8::from(*generation == 7),
                    from_slot: 0,
                    to_slot: *slot,
                    generation: *generation,
                    quiesce_drop: u32::from(!pass),
                    quiesce_pass: u32::from(*pass),
                    mcu_us: 123,
                }
            }
            Command::GetXdpStatus => OkReply::GetXdpStatus {
                boot_id: 42,
                active_slot: 0,
                staged_slot: 1,
                generation: 9,
                updating: 0,
                severe: 0,
                quiesce_drop: 0,
                quiesce_pass: 0,
                last_error: 0,
            },
        })
    }
}
