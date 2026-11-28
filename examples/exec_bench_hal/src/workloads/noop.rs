use crate::Workload;

pub struct Noop;

impl Workload for Noop {
    const TASK_ID: u8 = 0;
    const MIN_N: usize = 0;
    const MAX_N: usize = 0;
    const MAX_VARIANT: u32 = 0;

    fn layout(_: usize, _: u32) -> (usize, usize) {
        (0, 0)
    }

    fn round(_: usize, _: u32, _: &[u8], _: &mut [u8]) -> (u32, u32) {
        (0, 0)
    }
}
