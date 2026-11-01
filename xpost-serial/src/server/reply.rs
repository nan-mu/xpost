//! Allocation-free reply output to a firmware-owned byte sink.
//!
//! The sink must write **all** bytes of each slice or return an error. A UART
//! adapter that exposes short writes must retry them before returning `Ok(())`.
//! Hold exclusive ownership of the output for the whole call, so logs or other
//! replies cannot interleave with a header or binary payload. On an I/O error,
//! a prefix may already have been sent; do not append an `ERR` to a partial blob.

use core::convert::Infallible;

use crate::r#type::reply::{ErrReply, OkReply};

/// Write a complete success reply, including its header's terminating LF.
///
/// Binary replies borrow their payload and write it verbatim after the header,
/// with no trailing newline. Only the decimal length uses a small stack buffer;
/// memory use does not grow with the payload. An empty blob has length zero.
/// [`OkReply::Init`] can be emitted unsolicited once firmware initialization
/// finishes; it does not require a request.
///
/// The callback's error is returned unchanged and no further writes are made.
pub fn write_ok<E>(
    reply: &OkReply<'_>,
    mut write_all: impl FnMut(&[u8]) -> Result<(), E>,
) -> Result<(), E> {
    match reply {
        OkReply::XdpFlash { slot, generation } => {
            return write_nums(
                b"OK xdp flash",
                &[u32::from(*slot), *generation],
                write_all,
            );
        }
        OkReply::XdpSwitch {
            ok,
            rollback,
            severe,
            from_slot,
            to_slot,
            generation,
            quiesce_drop,
            quiesce_pass,
        } => {
            return write_nums(
                b"OK xdp switch",
                &[
                    u32::from(*ok),
                    u32::from(*rollback),
                    u32::from(*severe),
                    u32::from(*from_slot),
                    u32::from(*to_slot),
                    *generation,
                    *quiesce_drop,
                    *quiesce_pass,
                ],
                write_all,
            );
        }
        OkReply::GetXdpStatus {
            boot_id,
            active_slot,
            staged_slot,
            generation,
            updating,
            severe,
            quiesce_drop,
            quiesce_pass,
            last_error,
        } => {
            return write_nums(
                b"OK get xdp",
                &[
                    *boot_id,
                    u32::from(*active_slot),
                    u32::from(*staged_slot),
                    *generation,
                    u32::from(*updating),
                    u32::from(*severe),
                    *quiesce_drop,
                    *quiesce_pass,
                    u32::from(*last_error),
                ],
                write_all,
            );
        }
        _ => {}
    }
    let line: &[u8] = match reply {
        OkReply::Ping => b"OK ping\n",
        OkReply::EnableXdp => b"OK enable xdp\n",
        OkReply::DisableXdp => b"OK disable xdp\n",
        OkReply::EnableExec => b"OK enable exec\n",
        OkReply::DisableExec => b"OK disable exec\n",
        OkReply::ClearMap => b"OK clear map\n",
        OkReply::ClearPerf => b"OK clear perf\n",
        OkReply::UpdateMap => b"OK update map\n",
        OkReply::Init => b"OK init\n",
        // Finish is observed on UART from eBPF printk; server must not emit it.
        OkReply::Finish => b"OK finish\n",
        OkReply::GetMap { blob } => return write_blob(b"OK get map ", blob, write_all),
        OkReply::GetPerf { blob } => return write_blob(b"OK get perf ", blob, write_all),
        OkReply::XdpFlash { .. }
        | OkReply::XdpSwitch { .. }
        | OkReply::GetXdpStatus { .. } => return Ok(()),
    };
    write_all(line)
}

fn write_nums<E>(
    prefix: &[u8],
    nums: &[u32],
    mut write_all: impl FnMut(&[u8]) -> Result<(), E>,
) -> Result<(), E> {
    let mut buf = [0u8; 128];
    let mut n = 0;
    buf[..prefix.len()].copy_from_slice(prefix);
    n += prefix.len();
    for num in nums {
        buf[n] = b' ';
        n += 1;
        n += write_u32(&mut buf[n..], *num);
    }
    buf[n] = b'\n';
    n += 1;
    write_all(&buf[..n])
}

fn write_u32(dst: &mut [u8], mut num: u32) -> usize {
    let mut tmp = [0u8; 10];
    let mut i = tmp.len();
    loop {
        i -= 1;
        tmp[i] = b'0' + (num % 10) as u8;
        num /= 10;
        if num == 0 {
            break;
        }
    }
    let digits = &tmp[i..];
    dst[..digits.len()].copy_from_slice(digits);
    digits.len()
}

fn write_blob<E>(
    prefix: &[u8],
    blob: &[u8],
    mut write_all: impl FnMut(&[u8]) -> Result<(), E>,
) -> Result<(), E> {
    write_blob_header(prefix, blob.len(), &mut write_all)?;
    if !blob.is_empty() {
        write_all(blob)?;
    }
    Ok(())
}

fn write_blob_header<E>(
    prefix: &[u8],
    mut n: usize,
    mut write_all: impl FnMut(&[u8]) -> Result<(), E>,
) -> Result<(), E> {
    // Enough decimal digits for usize on the compilation target plus one LF.
    let mut length = [0u8; usize::MAX.ilog10() as usize + 2];
    let mut start = length.len() - 1;
    length[start] = b'\n';
    loop {
        start -= 1;
        length[start] = b'0' + (n % 10) as u8;
        n /= 10;
        if n == 0 {
            break;
        }
    }
    write_all(prefix)?;
    write_all(&length[start..])
}

/// Explicit wire codec for an independently typed [`ErrReply`].
///
/// This server-only interface is separate from the shared error marker: local
/// host errors need not have a wire encoding. Implementations return one full
/// static `ERR ...\n` line. No formatting traits, allocation, or wrapper enum
/// are required. A borrowed `&dyn WireError` can select among domains at runtime.
pub trait WireError: ErrReply {
    /// Return the complete error line including its terminating LF, with no blob.
    fn wire_line(&self) -> &'static [u8];
}

/// Write an independently typed device/syntax error, including LF, with no blob.
///
/// Accepts [`crate::reply::ServerError`], [`crate::reply::MapError`], [`crate::reply::PerfError`] and [`super::ParseCommandError`]
/// directly through [`WireError`]. The sink contract of [`write_ok`] applies;
/// the sink's error is returned unchanged.
pub fn write_error<E>(
    error: &(impl WireError + ?Sized),
    mut write_all: impl FnMut(&[u8]) -> Result<(), E>,
) -> Result<(), E> {
    write_all(error.wire_line())
}

/// Infallible write-all sink compatible with `xpost::ext::perf::report_perf`.
/// Firmware adapters such as `esp_println::Printer::write_bytes` supply it.
/// The callback may borrow state; it is never boxed or allocated here.
pub type WriteBytes<'a> = dyn FnMut(&[u8]) -> Result<(), Infallible> + 'a;

pub(crate) enum ReplyState {
    Pending,
    Complete,
    Broken,
}

/// Permission to send exactly one reply for the current request.
///
/// The handler passes this value to a registered callback. Every output method
/// consumes it, preventing a second header/reply through the same value. The
/// callback must use this sink rather than print another header itself. Output
/// remains synchronous so no other request can run partway through a report.
pub struct ReplyWriter<'a> {
    write: &'a mut WriteBytes<'a>,
    state: &'a mut ReplyState,
}

impl<'a> ReplyWriter<'a> {
    pub(crate) fn new(write: &'a mut WriteBytes<'a>, state: &'a mut ReplyState) -> Self {
        Self { write, state }
    }

    /// Send a fixed success or an already available borrowed binary payload.
    pub fn ok(self, reply: OkReply<'_>) {
        infallible(write_ok(&reply, self.write));
        *self.state = ReplyState::Complete;
    }

    /// Send a flat, independently typed error without any payload.
    pub fn error(self, error: &(impl WireError + ?Sized)) {
        infallible(write_error(error, self.write));
        *self.state = ReplyState::Complete;
    }

    /// Send `OK get perf <len>\n`, then stream raw report chunks directly.
    ///
    /// `emit` can call `xpost::ext::perf::report_perf(write)` unchanged, with
    /// `len` from `report_payload_len()`. Validate perf availability before
    /// calling this method. No complete report buffer is created by the server.
    /// A length mismatch stops the serial task: the frame is already on the wire
    /// and cannot be repaired by appending an ERR line.
    pub fn get_perf(self, len: usize, emit: impl FnOnce(&mut WriteBytes<'_>)) {
        self.blob(b"OK get perf ", len, emit);
    }

    /// Stream a map payload with the same length/framing rules as [`Self::get_perf`].
    pub fn get_map(self, len: usize, emit: impl FnOnce(&mut WriteBytes<'_>)) {
        self.blob(b"OK get map ", len, emit);
    }

    fn blob(self, prefix: &[u8], len: usize, emit: impl FnOnce(&mut WriteBytes<'_>)) {
        *self.state = ReplyState::Broken;
        infallible(write_blob_header(prefix, len, &mut *self.write));
        let mut remaining = len;
        let mut overflow = false;
        emit(&mut |bytes: &[u8]| {
            if bytes.len() > remaining {
                overflow = true;
            }
            // Never let excess bytes corrupt the next frame.
            if !overflow {
                (self.write)(bytes)?;
                remaining -= bytes.len();
            }
            Ok(())
        });
        if remaining == 0 && !overflow {
            *self.state = ReplyState::Complete;
        }
    }
}

pub(crate) fn infallible(result: Result<(), Infallible>) {
    match result {
        Ok(()) => {}
        Err(never) => match never {},
    }
}
