pub const TASK_ID: u8 = 5;
pub const MIN_N: usize = 0;
pub const MAX_N: usize = 128;
pub const MAX_VARIANT: u32 = 0;

pub fn layout(n: usize, _: u32) -> (usize, usize) {
    (4 * n, 4 * n)
}

pub fn round(_: usize, _: u32, input: &[u8], scratch: &mut [u8]) -> (u32, u32) {
    let mut words = [0u32; MAX_N];
    let words = &mut words[..input.len() / 4];
    for (word, bytes) in words.iter_mut().zip(input.chunks_exact(4)) {
        *word = u32::from_le_bytes(bytes.try_into().unwrap());
    }
    words.sort_unstable();
    for (&word, bytes) in words.iter().zip(scratch.chunks_exact_mut(4)) {
        bytes.copy_from_slice(&word.to_le_bytes());
    }
    (
        words.first().copied().unwrap_or(0),
        words.last().copied().unwrap_or(0),
    )
}
