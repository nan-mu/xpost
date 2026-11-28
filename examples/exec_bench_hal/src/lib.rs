#![no_std]

#[cfg(target_os = "none")]
pub mod firmware;
pub mod workloads;

pub trait Workload {
    const TASK_ID: u8;
    const MIN_N: usize;
    const MAX_N: usize;
    const MAX_VARIANT: u32;

    fn layout(n: usize, variant: u32) -> (usize, usize);
    fn validate(_input: &[u8]) -> Result<(), Error> {
        Ok(())
    }
    fn round(n: usize, variant: u32, input: &[u8], scratch: &mut [u8]) -> (u32, u32);
}

const CONTEXT_BYTES: usize = 8258;
const HEADER_BYTES: usize = 64;

#[derive(Clone, Copy)]
#[repr(u32)]
pub enum Error {
    BadMagic = 1,
    BadVersion = 2,
    BadTask = 3,
    BadRange = 4,
    BadLayout = 5,
    BadVariant = 6,
    ShortBuffer = 7,
}

pub(crate) fn word(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}

fn run<W: Workload>(header: &[u8], payload: &mut [u8]) -> Result<[u32; 4], Error> {
    if &header[..4] != b"EXB2" {
        return Err(Error::BadMagic);
    }
    if header[4..8] != [2, 0, 64, 0] {
        return Err(Error::BadVersion);
    }
    if word(header, 8) != u32::from(W::TASK_ID) {
        return Err(Error::BadTask);
    }
    if word(header, 56) != 0 || word(header, 60) != 0 {
        return Err(Error::BadLayout);
    }
    let n = word(header, 12) as usize;
    let rounds = word(header, 16);
    let variant = word(header, 20);
    if !(W::MIN_N..=W::MAX_N).contains(&n) || !(1..=65536).contains(&rounds) {
        return Err(Error::BadRange);
    }
    if variant > W::MAX_VARIANT {
        return Err(Error::BadVariant);
    }
    let (input_len, scratch_len) = W::layout(n, variant);
    if word(header, 28) as usize != input_len || word(header, 32) as usize != scratch_len {
        return Err(Error::BadLayout);
    }
    let (input, scratch) = payload
        .get_mut(..input_len + scratch_len)
        .ok_or(Error::ShortBuffer)?
        .split_at_mut(input_len);
    let input: &[u8] = input;
    W::validate(input)?;

    let mut digest = word(header, 24) ^ 0x811c9dc5;
    let mut result = (0, 0);
    for round in 0..rounds {
        // Keep reads, computation and Scratch writes inside each measured round.
        use core::hint::black_box;
        result = W::round(black_box(n), black_box(variant), black_box(input), scratch);
        black_box(&*scratch);
        digest = (digest.rotate_left(5) ^ result.0).wrapping_add(result.1 ^ round ^ 0x9e3779b9);
    }
    Ok([rounds, result.0, result.1, digest])
}

/// Execute the compiled workload and publish the EXB2 response in place.
pub fn execute<W: Workload>(buffer: &mut [u8]) -> u32 {
    if buffer.len() != CONTEXT_BYTES {
        return Error::ShortBuffer as u32;
    }
    let (header, payload) = buffer.split_at_mut(HEADER_BYTES);
    let (status, fields) = match run::<W>(header, payload) {
        Ok(fields) => (0, fields),
        Err(error) => (error as u32, [0; 4]),
    };
    for (out, field) in header[36..56]
        .chunks_exact_mut(4)
        .zip([status].into_iter().chain(fields))
    {
        out.copy_from_slice(&field.to_le_bytes());
    }
    status
}

#[cfg(test)]
mod tests;
