use xpost_serial::command::EXEC_BATCH_BYTES;
use xpost_type::XdpAction;

use crate::exec_metrics;

type Entry = unsafe extern "C" fn(*mut u8, u64) -> u64;

unsafe extern "C" {
    fn exb2_hal_0(buf: *mut u8, len: u64) -> u64;
    fn exb2_hal_1(buf: *mut u8, len: u64) -> u64;
    fn exb2_hal_2(buf: *mut u8, len: u64) -> u64;
    fn exb2_hal_3(buf: *mut u8, len: u64) -> u64;
    fn exb2_hal_4(buf: *mut u8, len: u64) -> u64;
    fn exb2_hal_5(buf: *mut u8, len: u64) -> u64;
    fn exb2_hal_6(buf: *mut u8, len: u64) -> u64;
}

const ENTRIES: [Entry; 7] = [
    exb2_hal_0, exb2_hal_1, exb2_hal_2, exb2_hal_3, exb2_hal_4, exb2_hal_5, exb2_hal_6,
];

pub fn run(buf: &mut [u8; EXEC_BATCH_BYTES]) -> (u64, u64, u32) {
    let task = u32::from_le_bytes(buf[8..12].try_into().unwrap());
    // The default entry validates the header and rejects unknown task IDs.
    let entry = ENTRIES.get(task as usize).copied().unwrap_or(exb2_hal_0);
    exec_metrics::begin_invocation();
    let begin = exec_metrics::now();
    // The pinned native entry receives the entire writable EXB2 buffer.
    let status = unsafe { entry(buf.as_mut_ptr(), EXEC_BATCH_BYTES as u64) };
    let end = exec_metrics::now();
    let action = if status == 0 {
        XdpAction::Pass
    } else {
        XdpAction::Aborted
    };
    exec_metrics::end_invocation(action);
    (begin, end, status as u32)
}
