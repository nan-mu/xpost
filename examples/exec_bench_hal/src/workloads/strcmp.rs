use crate::Error;

pub const TASK_ID: u8 = 6;
pub const MIN_N: usize = 0;
pub const MAX_N: usize = 4096;
pub const MAX_VARIANT: u32 = 0;

pub fn layout(n: usize, _: u32) -> (usize, usize) {
    (2 * (n + 1), 0)
}

pub fn validate(input: &[u8]) -> Result<(), Error> {
    if input[input.len() / 2 - 1] != 0 || input[input.len() - 1] != 0 {
        return Err(Error::BadLayout);
    }
    Ok(())
}

pub fn round(_: usize, _: u32, input: &[u8], _: &mut [u8]) -> (u32, u32) {
    let (left, right) = input.split_at(input.len() / 2);
    let n = left.len() - 1;
    for (index, (&a, &b)) in left[..n].iter().zip(right).enumerate() {
        if a != b || a == 0 {
            return ((i32::from(a) - i32::from(b)) as u32, index as u32 + 1);
        }
    }
    (0, n as u32)
}
