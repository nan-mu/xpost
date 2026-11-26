//! Diagnostics for the legacy workload scripts.

use core::ptr::addr_of_mut;

use log::{error, info};
use xpost::app_ctrl::{self, AppMapError, AppXdpError};
use xpost::map::BPF_ANY;
use xpost_serial::reply::{MapError, OkReply, ServerError};
use xpost_serial::server::serial::RequestHandler;
use xpost_serial::server::{ReplyWriter, SerialHandlerBuilder};

use super::reply_xdp_err;

/// Map value size for `workload` (XDP ELF metadata from libbpf-mcu).
const VALUE_LEN: usize = 32;
const MAP_NAME: &str = "workload";
const KEY_IN: u8 = 0;
const KEY_OUT: u8 = 1;

static mut INVOKE_BUF: [u8; VALUE_LEN] = [0; VALUE_LEN];

pub(super) fn register<R, W, H: RequestHandler, const N: usize>(
    builder: SerialHandlerBuilder<R, W, H, N>,
) -> SerialHandlerBuilder<R, W, impl RequestHandler, N> {
    builder
        .on_enable_exec(|reply: ReplyWriter<'_>| {
            // In a diagnostic build, `enable exec` also runs the scale sweep and
            // reports the mean of many calls, because one call is too short to
            // time. The normal result path still runs, so the host's `OK finish`
            // wait is unchanged.
            scale_sweep();
            workload_bench();
            match run_exec() {
                Ok(()) => {
                    reply.ok(OkReply::EnableExec);
                    // Printed after the reply, not before. The host reads this
                    // line with a separate `get finish` call, and when it came
                    // first the reader picked it up together with the enable
                    // reply, so the later wait found nothing.
                    info!("OK finish");
                }
                Err(e) => reply_map_err(reply, e),
            }
        })
        .on_disable_exec(|reply: ReplyWriter<'_>| match app_ctrl::disable_xdp() {
            // Exec shares the leased image; disable releases the lease.
            Ok(()) => reply.ok(OkReply::DisableExec),
            Err(e) => reply_xdp_err(reply, e),
        })
        .on_get_map(
            |name: &str, reply: ReplyWriter<'_>| match app_ctrl::lock_map(name) {
                Ok((maps, idx)) => match maps.map_blob_len(idx) {
                    Ok(len) => reply.get_map(len, |write| {
                        let _ = maps.write_map_blob(idx, |chunk| {
                            let _ = write(chunk);
                        });
                    }),
                    Err(e) => reply_map_err(reply, e.into()),
                },
                Err(e) => reply_map_err(reply, e),
            },
        )
        .on_update_map(
            |name: &str, key: &[u8], value: &[u8], reply: ReplyWriter<'_>| match update_bytes(
                name, key, value,
            ) {
                Ok(()) => reply.ok(OkReply::UpdateMap),
                Err(e) => reply_map_err(reply, e),
            },
        )
        .on_clear_perf(|reply: ReplyWriter<'_>| {
            xpost::ext::perf::clear_perf();
            reply.ok(OkReply::ClearPerf);
        })
        .on_get_perf(|reply: ReplyWriter<'_>| {
            // Report what this experiment measures, by name: the engine call,
            // the wider interval around it, and the invocation count. The
            // format carries each name, so a reader needs no per-build table.
            let n = xpost::ext::perf::interp_report::payload_len();
            reply.get_perf(n, |write| {
                let mut sink = xpost::ext::perf::interp_report::WriteSink::new(write);
                xpost::ext::perf::interp_report::report(&mut sink);
            });
        })
}

/// Sweep `buf`'s loop count through the leased path and report the mean per run.
///
/// `buf` copies `param` bytes one at a time, so `param` is the work and the mean
/// must grow with it. A flat line means the rig measures its own overhead; a
/// large intercept means a fixed cost that would swamp small workloads;
/// non-linear growth means the interpreter does not do one pass per pass.
///
/// Batched because one microsecond of clock resolution divided over a thousand
/// runs is a nanosecond per run. Each run is a full flight through
/// `ingress::run`, so the lease check and the `IN_FLIGHT` claim are included in
/// every sample.
/// Sweep a workload's work size through the leased path.
///
/// The installed image decides which workload runs, so the host swaps the image
/// between resets. `buf` is the only workload whose work size is a free
/// parameter: it copies `param` bytes one at a time, so time must grow linearly
/// with it. `fib` runs a loop `param` times, bounded at 47. The rest clamp their
/// parameter hard enough that a sweep says little.
///
/// The frame is sized for the largest parameter, so an engine that reads
/// `data_end` sees the whole packet. The AOT images read the same range from the
/// carrier slice, so both engines sweep identical work.
///
/// Batched because one microsecond of clock resolution divided over two hundred
/// runs is a few nanoseconds per run.
/// Sweep the installed workload over its work size.
///
/// Every workload in exec_bench expresses its work in the `param` field at value
/// offset 4, so one firmware sweeps them all: the host writes a value whose
/// first field is the opcode, installs the matching image, and the size list is
/// chosen from that opcode.
///
/// # Why every workload gets a figure
///
/// The workloads differ in how much of `param` they use:
///
/// - `buf` copies `param` bytes and `fib` loops `param` times, so both span
///   ranges wide enough for a slope to stand well clear of the intercept.
/// - `memcpy` stops at 12, `fletcher32` at 20, `bsort` at 6. Their sources
///   clamp hard, so the range is narrow but the slope is still a slope.
/// - `incr`, `square` and `bitswap` do a fixed amount of work and only pass
///   `param` through. Their figures are flat by construction, and the flat line
///   is the result: it says what each engine costs for that fixed work, and it
///   is the cleanest measurement of the intercept the other figures have to
///   subtract.
fn scale_sweep() {
    use xpost_perf::sweep;
    // The opcode comes from the value the host seeded, so this firmware needs no
    // per-workload build.
    let mut head = [0u8; 8];
    {
        let (maps, idx) = match app_ctrl::lock_map(MAP_NAME) {
            Ok(v) => v,
            Err(e) => {
                error!("sweep: map: {e:?}");
                return;
            }
        };
        if let Err(e) = maps.hash_lookup(idx, &[KEY_IN], &mut head) {
            error!("sweep: lookup: {e:?}");
            return;
        }
    }
    let opcode = u32::from_le_bytes([head[0], head[1], head[2], head[3]]);
    let Some(params) = sweep::params_for(opcode) else {
        error!("sweep: unknown opcode {opcode}");
        return;
    };
    match app_ctrl::enable_xdp() {
        Ok(()) => {}
        Err(e) => {
            error!("sweep: cannot lease: {e:?}");
            return;
        }
    }
    let mut frame = [0u8; 8 + 1024 + 4];
    for &param in params {
        let n = sweep::frame_len_for(opcode, param);
        sweep::fill_op(&mut frame[..n], opcode, param);
        match xpost::ext::exec_call::invoke_batch(&mut frame[..n], sweep::RUNS) {
            Some((mean_ns, act)) => {
                info!(
                    "sweep op={opcode} param={param} runs={} mean_ns={mean_ns} act={act}",
                    sweep::RUNS
                );
            }
            None => info!(
                "sweep op={opcode} param={param} runs={} refused",
                sweep::RUNS
            ),
        }
    }
    #[cfg(feature = "interp")]
    info!("sweep steps={}", xpost::xdp::interp_take_steps());
    let _ = app_ctrl::disable_xdp();
}

/// Repeat the input already in the carrier map many times and report the mean.
///
/// The host writes the workload's input with `update map workload 0 <hex>` and
/// installs the matching image, so this measures whatever pair is in place. One
/// fixed input per workload, the same size the exam uses.
///
/// Repeating makes the number usable: a call takes tens of microseconds and the
/// clock resolves one, so a single call cannot be read with precision. Two
/// thousand calls divide that uncertainty and average over the cache and
/// interrupt noise a single sample carries.
fn workload_bench() {
    const RUNS: u32 = 2000;
    let mut value = [0u8; VALUE_LEN];
    {
        let (maps, idx) = match app_ctrl::lock_map(MAP_NAME) {
            Ok(v) => v,
            Err(e) => {
                error!("bench: map: {e:?}");
                return;
            }
        };
        if let Err(e) = maps.hash_lookup(idx, &[KEY_IN], &mut value) {
            error!("bench: lookup: {e:?}");
            return;
        }
    }
    match app_ctrl::enable_xdp() {
        Ok(()) => {}
        Err(e) => {
            error!("bench: cannot lease: {e:?}");
            return;
        }
    }
    match xpost::ext::exec_call::invoke_batch(&mut value, RUNS) {
        Some((mean_ns, act)) => info!("bench runs={RUNS} mean_ns={mean_ns} act={act}"),
        None => info!("bench runs={RUNS} refused"),
    }
    let _ = app_ctrl::disable_xdp();
}

fn reply_map_err(reply: ReplyWriter<'_>, e: AppMapError) {
    match e {
        AppMapError::Busy => reply.error(&ServerError::Busy),
        AppMapError::NoRt | AppMapError::Logic => reply.error(&ServerError::Logic),
        AppMapError::NameNotFound => reply.error(&MapError::NameNotFound),
        AppMapError::InvalidName => reply.error(&MapError::InvalidName),
        AppMapError::InvalidKey => reply.error(&MapError::InvalidKey),
        AppMapError::InvalidValue => reply.error(&MapError::InvalidValue),
        AppMapError::Full => reply.error(&MapError::Full),
        AppMapError::Unsupported => reply.error(&MapError::Unsupported),
    }
}

fn key_u8(key: &[u8]) -> Option<u8> {
    match key {
        b"0" => Some(0),
        b"1" => Some(1),
        [0] | [1] => Some(key[0]),
        _ => None,
    }
}

fn hex_nibble(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Decode `VALUE_LEN*2` hex digits into 32 raw bytes; otherwise pad/truncate copy.
fn decode_value_token(value: &[u8]) -> Option<[u8; VALUE_LEN]> {
    let mut val = [0u8; VALUE_LEN];
    if value.len() == VALUE_LEN * 2 {
        for i in 0..VALUE_LEN {
            let hi = hex_nibble(value[i * 2])?;
            let lo = hex_nibble(value[i * 2 + 1])?;
            val[i] = (hi << 4) | lo;
        }
        return Some(val);
    }
    let n = value.len().min(VALUE_LEN);
    val[..n].copy_from_slice(&value[..n]);
    Some(val)
}

/// Opaque HASH update: wire `key`/`value` tokens are raw bytes (or hex for 32B).
/// Other maps fall back to [`app_ctrl::update_map`].
fn update_bytes(name: &str, key: &[u8], value: &[u8]) -> Result<(), AppMapError> {
    if name != MAP_NAME {
        return app_ctrl::update_map(name, key, value);
    }
    let slot = key_u8(key).ok_or(AppMapError::InvalidKey)?;
    let (mut maps, idx) = app_ctrl::lock_map(MAP_NAME)?;
    let vs = maps.hash_value_size(idx)?;
    if vs != VALUE_LEN {
        return Err(AppMapError::Unsupported);
    }
    let val = decode_value_token(value).ok_or(AppMapError::InvalidValue)?;
    maps.hash_update(idx, &[slot], &val, BPF_ANY)
        .map_err(AppMapError::from)
}

/// Carrier path: copy map[0] while the control plane is open, lease only for
/// `invoke`, then release and copy the buffer to map[1].
/// No opcode / layout interpretation. After that map write succeeds, this App
/// logs `OK finish` outside `invoke`.
fn run_exec() -> Result<(), AppMapError> {
    let buf: &mut [u8; VALUE_LEN] = unsafe { &mut *addr_of_mut!(INVOKE_BUF) };
    buf.fill(0);
    let t_start = inst_us();

    {
        let (maps, idx) = app_ctrl::lock_map(MAP_NAME)?;
        let n = maps.hash_lookup(idx, &[KEY_IN], buf).map_err(|e| match e {
            xpost::map::MapUseError::NotFound => AppMapError::InvalidKey,
            other => AppMapError::from(other),
        })?;
        if n > VALUE_LEN {
            return Err(AppMapError::Logic);
        }
    }
    let t_load = inst_us();

    app_ctrl::enable_xdp().map_err(map_xdp_err)?;
    let t_lease = inst_us();
    let (act, _) = xpost::ext::exec_call::invoke(buf);
    let aborted = act == xpost_type::XdpAction::Aborted;
    let t_engine = inst_us();
    app_ctrl::disable_xdp().map_err(map_xdp_err)?;
    if aborted {
        return Err(AppMapError::Logic);
    }
    let t_release = inst_us();

    {
        let (mut maps, idx) = app_ctrl::lock_map(MAP_NAME)?;
        maps.hash_update(idx, &[KEY_OUT], buf, BPF_ANY)
            .map_err(AppMapError::from)?;
    }
    let t_store = inst_us();

    // The AOT baseline reports one `execution` figure for the whole of this
    // function's middle, and all eight workloads measured the same value. These
    // four segments split it, so the fixed cost is visible next to the work.
    info!(
        "exec seg: load={} lease={} engine={} release={} store={} total={}",
        t_load.wrapping_sub(t_start),
        t_lease.wrapping_sub(t_load),
        t_engine.wrapping_sub(t_lease),
        t_release.wrapping_sub(t_engine),
        t_store.wrapping_sub(t_release),
        t_store.wrapping_sub(t_start),
    );
    Ok(())
}

/// Device microseconds since boot. One source for every segment boundary, so
/// the differences are comparable.
fn inst_us() -> u32 {
    esp_hal::time::Instant::now()
        .duration_since_epoch()
        .as_micros() as u32
}

fn map_xdp_err(err: AppXdpError) -> AppMapError {
    match err {
        AppXdpError::Busy => AppMapError::Busy,
        AppXdpError::NoRt => AppMapError::NoRt,
        AppXdpError::Logic => AppMapError::Logic,
    }
}
