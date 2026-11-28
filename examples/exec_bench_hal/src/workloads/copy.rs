use crate::Workload;

pub struct Copy;

impl Workload for Copy {
    const TASK_ID: u8 = 3;
    const MIN_N: usize = 0;
    const MAX_N: usize = 4096;
    const MAX_VARIANT: u32 = 0;

    fn layout(n: usize, _: u32) -> (usize, usize) {
        (n, n)
    }

    fn round(_: usize, _: u32, input: &[u8], scratch: &mut [u8]) -> (u32, u32) {
        scratch.copy_from_slice(input);
        (input.len() as u32, 0)
    }
}
