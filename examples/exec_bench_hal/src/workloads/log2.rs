pub const TASK_ID: u8 = 2;
pub const MIN_N: usize = 0;
pub const MAX_N: usize = 1024;
pub const MAX_VARIANT: u32 = 1;

pub fn layout(n: usize, variant: u32) -> (usize, usize) {
    (n * (4 << variant), 0)
}

pub fn round(_: usize, variant: u32, input: &[u8], _: &mut [u8]) -> (u32, u32) {
    let bits = input.chunks_exact(4 << variant).map(|bytes| {
        let low = crate::word(bytes, 0);
        if variant == 1 {
            let high = crate::word(bytes, 4);
            if let Some(bit) = high.checked_ilog2() {
                return bit + 32;
            }
        }
        low.checked_ilog2().unwrap_or(0)
    });
    let mut sum = 0;
    let mut hash = 0u32;
    for (i, bit) in bits.enumerate() {
        sum += bit;
        hash = hash.rotate_left(5) ^ (bit + i as u32);
    }
    (sum, hash)
}
