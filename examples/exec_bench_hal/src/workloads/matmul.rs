use crate::{Error, Workload, word};

pub struct Matmul;

impl Workload for Matmul {
    const TASK_ID: u8 = 7;
    const MIN_N: usize = 1;
    const MAX_N: usize = 26;
    const MAX_VARIANT: u32 = 0;

    fn layout(n: usize, _: u32) -> (usize, usize) {
        (8 * n * n, 4 * n * n)
    }

    fn validate(input: &[u8]) -> Result<(), Error> {
        // An EXB2 u32 in 0..255 has three zero high bytes.
        if input.chunks_exact(4).any(|bytes| bytes[1..] != [0; 3]) {
            return Err(Error::BadRange);
        }
        Ok(())
    }

    fn round(n: usize, _: u32, input: &[u8], scratch: &mut [u8]) -> (u32, u32) {
        let row_bytes = 4 * n;
        let (a, b) = input.split_at(n * row_bytes);
        let mut sums = [0u32; Self::MAX_N];
        for (a_row, c_row) in a
            .chunks_exact(row_bytes)
            .zip(scratch.chunks_exact_mut(row_bytes))
        {
            sums[..n].fill(0);
            for (a_element, b_row) in a_row.chunks_exact(4).zip(b.chunks_exact(row_bytes)) {
                let left = u32::from(a_element[0]);
                for (sum, b_element) in sums.iter_mut().zip(b_row.chunks_exact(4)) {
                    *sum += left * u32::from(b_element[0]);
                }
            }
            for (bytes, sum) in c_row.chunks_exact_mut(4).zip(&sums[..n]) {
                bytes.copy_from_slice(&sum.to_le_bytes());
            }
        }
        (word(scratch, 0), word(scratch, scratch.len() - 4))
    }
}
