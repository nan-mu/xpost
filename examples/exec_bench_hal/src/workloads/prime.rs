pub const TASK_ID: u8 = 1;
pub const MIN_N: usize = 0;
pub const MAX_N: usize = 8192;
pub const MAX_VARIANT: u32 = 0;

pub fn layout(_: usize, _: u32) -> (usize, usize) {
    (0, 0)
}

pub fn round(n: usize, _: u32, _: &[u8], _: &mut [u8]) -> (u32, u32) {
    if n < 2 {
        return (0, 0);
    }
    let (mut count, mut largest) = (1, 2);
    for candidate in (3..=n).step_by(2) {
        let divisors = (3..).step_by(2).take_while(|d| d * d <= candidate);
        if !divisors.into_iter().any(|d| candidate % d == 0) {
            count += 1;
            largest = candidate as u32;
        }
    }
    (count, largest)
}
