//! App-mode serial protocol over [`Port`] + transcript record.
//!
//! UART reader appends into [`SharedRecord`]; [`run_op`] waits via
//! [`SharedRecord::wait_reply`] and returns a [`Found`] index set.

pub mod perf;

pub use perf::{Report, decode as decode_perf, mask_for_len};

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use futures::FutureExt;

use crate::client::error::{Interrupted, RequestError, RequestTimeout};
use crate::client::port::AppLink;
use crate::client::record::{Found, SharedRecord, WriteRecord};
use crate::client::{DEFAULT_REQUEST_TIMEOUT, GET_MAP_TIMEOUT};
use crate::r#type::command::{Command, GET_FINISH_TIMEOUT, GET_INIT_TIMEOUT};

/// Occupy App UART, exchange one command (or host-only wait), release fd on return.
pub(crate) async fn run_op<E>(
    link: &AppLink,
    record: &SharedRecord,
    cmd: &Command<'_>,
) -> core::result::Result<Found, E>
where
    E: RequestError,
{
    let req_timeout = match cmd {
        Command::GetInit => GET_INIT_TIMEOUT,
        Command::GetFinish => GET_FINISH_TIMEOUT,
        Command::GetMap { .. } => GET_MAP_TIMEOUT,
        _ => DEFAULT_REQUEST_TIMEOUT,
    };

    let mut uart = link.open_uart(Arc::clone(record))?;
    {
        let mut rec = record.lock();
        cmd.write_to(&mut rec);
    }
    // GetInit / GetFinish: open reader only — never serialize / write a UART verb.
    // GetInit: lower bound is last flash Begin (else 15s lookback if never flashed).
    // GetFinish: short lookback so `OK finish` printed during `enable exec` is accepted.
    let after = match cmd {
        Command::GetInit => record.init_after(),
        Command::GetFinish => SystemTime::now()
            .checked_sub(Duration::from_millis(500))
            .unwrap_or(SystemTime::UNIX_EPOCH),
        _ => uart.write_command(cmd.to_string().as_str())?,
    };

    let found = tokio::select! {
        biased;
        _ = link.interrupt().wait().fuse() => Err(Interrupted.into()),
        _ = tokio::time::sleep(req_timeout).fuse() => {
            Err(RequestTimeout(req_timeout).into())
        }
        msg = record.wait_reply::<E>(cmd, after).fuse() => msg,
    }?;

    {
        let ok = record.materialize_ok::<E>(&found)?;
        record.lock().cmd_rx(ok_label(&ok));
    }
    Ok(found)
}

pub(crate) fn ok_label(ok: &crate::r#type::reply::OkReply<'_>) -> &'static str {
    use crate::r#type::reply::OkReply;
    match ok {
        OkReply::Ping => "OK ping",
        OkReply::EnableXdp => "OK enable xdp",
        OkReply::DisableXdp => "OK disable xdp",
        OkReply::EnableExec => "OK enable exec",
        OkReply::DisableExec => "OK disable exec",
        OkReply::ClearMap => "OK clear map",
        OkReply::ClearPerf => "OK clear perf",
        OkReply::GetMap { .. } => "OK get map",
        OkReply::UpdateMap => "OK update map",
        OkReply::GetPerf { .. } => "OK get perf",
        OkReply::Init => "OK init",
        OkReply::Finish => "OK finish",
        OkReply::XdpStage { .. } => "OK xdp stage",
        OkReply::XdpActivate { .. } => "OK xdp activate",
        OkReply::XdpRollback { .. } => "OK xdp rollback",
        OkReply::GetXdpStatus { .. } => "OK get xdp",
    }
}
