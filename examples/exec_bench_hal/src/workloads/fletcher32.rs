pub const TASK_ID: u8 = 4;
pub const MIN_N: usize = 0;
pub const MAX_N: usize = 4096;
pub const MAX_VARIANT: u32 = 0;

pub fn layout(n: usize, _: u32) -> (usize, usize) {
    (n, 0)
}

pub fn round(_: usize, _: u32, input: &[u8], _: &mut [u8]) -> (u32, u32) {
    fn fold(value: u32) -> u32 {
        (value & 0xffff) + (value >> 16)
    }

    let (mut s1, mut s2) = (0xffff, 0xffff);
    let mut blocks = input.chunks_exact(720);
    for block in &mut blocks {
        for pair in block.chunks_exact(2) {
            s1 += u32::from(u16::from_le_bytes([pair[0], pair[1]]));
            s2 += s1;
        }
        s1 = fold(s1);
        s2 = fold(s2);
    }
    for pair in blocks.remainder().chunks(2) {
        s1 += u32::from(u16::from_le_bytes([
            pair[0],
            pair.get(1).copied().unwrap_or(0),
        ]));
        s2 += s1;
    }
    ((fold(fold(s2)) << 16) | fold(fold(s1)), 0)
}
