pub const TASK_ID: u8 = 3;
pub const MIN_N: usize = 0;
pub const MAX_N: usize = 4096;
pub const MAX_VARIANT: u32 = 0;

pub fn layout(n: usize, _: u32) -> (usize, usize) {
    (n, n)
}

pub fn round(_: usize, _: u32, input: &[u8], scratch: &mut [u8]) -> (u32, u32) {
    scratch.copy_from_slice(input);
    (input.len() as u32, 0)
}
