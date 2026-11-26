//! Staging for one batch across serial requests.

use crate::command::{
    Command, EXEC_BATCH_BYTES, EXEC_BATCH_READ_MAX_BYTES, EXEC_BATCH_WRITE_MAX_BYTES,
};
use crate::reply::{OkReply, ServerError};

use super::{ReplyWriter, command::decode_hex};

/// Receive buffer size for the largest write line, including a decimal offset.
pub const EXEC_BATCH_LINE_BYTES: usize =
    b"exec batch write ".len() + 10 + 1 + 2 * EXEC_BATCH_WRITE_MAX_BYTES;

enum Phase {
    Idle,
    Uploading(usize),
    Invoked,
}

/// Exclusive owner of a single fixed-size input and result buffer.
pub struct ExecBatch {
    bytes: [u8; EXEC_BATCH_BYTES],
    phase: Phase,
}

impl ExecBatch {
    pub const fn new() -> Self {
        Self {
            bytes: [0; EXEC_BATCH_BYTES],
            phase: Phase::Idle,
        }
    }

    /// Run at most once per upload. `invoke` returns begin tick, end tick and action.
    /// Return a server error only when the program could not be invoked.
    pub fn handle(
        &mut self,
        command: Command<'_>,
        reply: ReplyWriter<'_>,
        boot_id: u32,
        invoke: impl FnOnce(&mut [u8; EXEC_BATCH_BYTES]) -> Result<(u64, u64, u32), ServerError>,
    ) {
        match command {
            Command::ExecBatchReset => {
                self.phase = Phase::Uploading(0);
                reply.ok(OkReply::ExecBatchReset);
            }
            Command::ExecBatchWrite { offset, data } => self.write(offset, data, reply),
            Command::ExecBatchInvoke
                if matches!(self.phase, Phase::Uploading(EXEC_BATCH_BYTES)) =>
            {
                match invoke(&mut self.bytes) {
                    Ok((begin_tick, end_tick, action)) => {
                        self.phase = Phase::Invoked;
                        reply.ok(OkReply::ExecBatchInvoke {
                            boot_id,
                            begin_tick,
                            end_tick,
                            action,
                        });
                    }
                    Err(error) => reply.error(&error),
                }
            }
            Command::ExecBatchRead { offset, max_bytes } => self.read(offset, max_bytes, reply),
            _ => reply.error(&ServerError::Logic),
        }
    }

    fn write(&mut self, offset: u32, data: &[u8], reply: ReplyWriter<'_>) {
        let Phase::Uploading(start) = self.phase else {
            return reply.error(&ServerError::Logic);
        };
        let count = data.len() / 2;
        let end = start + count;
        if offset != start as u32 || end > EXEC_BATCH_BYTES {
            return reply.error(&ServerError::Logic);
        }
        if let Err(error) = decode_hex(data, &mut self.bytes[start..end]) {
            return reply.error(&error);
        }
        self.phase = Phase::Uploading(end);
        reply.ok(OkReply::ExecBatchWrite);
    }

    fn read(&self, offset: u32, max_bytes: u32, reply: ReplyWriter<'_>) {
        if !matches!(self.phase, Phase::Invoked)
            || offset > EXEC_BATCH_BYTES as u32
            || max_bytes == 0
        {
            return reply.error(&ServerError::Logic);
        }
        let start = offset as usize;
        let len = (max_bytes as usize).min(EXEC_BATCH_READ_MAX_BYTES);
        let end = (start + len).min(EXEC_BATCH_BYTES);
        reply.ok(OkReply::ExecBatchRead {
            blob: &self.bytes[start..end],
        });
    }
}
