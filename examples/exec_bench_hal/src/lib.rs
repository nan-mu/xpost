#![no_std]

// The build script requires exactly one of these features.
#[cfg_attr(feature = "noop", path = "workloads/noop.rs")]
#[cfg_attr(feature = "prime", path = "workloads/prime.rs")]
#[cfg_attr(feature = "log2", path = "workloads/log2.rs")]
#[cfg_attr(feature = "copy", path = "workloads/copy.rs")]
#[cfg_attr(feature = "fletcher32", path = "workloads/fletcher32.rs")]
#[cfg_attr(feature = "bsort", path = "workloads/bsort.rs")]
#[cfg_attr(feature = "strcmp", path = "workloads/strcmp.rs")]
#[cfg_attr(feature = "matmul", path = "workloads/matmul.rs")]
mod workload;

const CONTEXT_BYTES: usize = 8258;
const HEADER_BYTES: usize = 64;

#[derive(Clone, Copy)]
#[repr(u32)]
enum Error {
    BadMagic = 1,
    BadVersion = 2,
    BadTask = 3,
    BadRange = 4,
    BadLayout = 5,
    BadVariant = 6,
    ShortBuffer = 7,
}

/// The task ID selected at compile time.
pub const TASK_ID: u8 = workload::TASK_ID;

pub(crate) fn word(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}

fn run(header: &[u8], payload: &mut [u8]) -> Result<[u32; 4], Error> {
    if &header[..4] != b"EXB2" {
        return Err(Error::BadMagic);
    }
    if header[4..8] != [2, 0, 64, 0] {
        return Err(Error::BadVersion);
    }
    if word(header, 8) != u32::from(TASK_ID) {
        return Err(Error::BadTask);
    }
    if word(header, 56) != 0 || word(header, 60) != 0 {
        return Err(Error::BadLayout);
    }
    let n = word(header, 12) as usize;
    let rounds = word(header, 16);
    let variant = word(header, 20);
    if !(workload::MIN_N..=workload::MAX_N).contains(&n) || !(1..=65536).contains(&rounds) {
        return Err(Error::BadRange);
    }
    if variant > workload::MAX_VARIANT {
        return Err(Error::BadVariant);
    }
    let (input_len, scratch_len) = workload::layout(n, variant);
    if word(header, 28) as usize != input_len || word(header, 32) as usize != scratch_len {
        return Err(Error::BadLayout);
    }
    let (input, scratch) = payload
        .get_mut(..input_len + scratch_len)
        .ok_or(Error::ShortBuffer)?
        .split_at_mut(input_len);
    let input: &[u8] = input;
    #[cfg(any(feature = "strcmp", feature = "matmul"))]
    workload::validate(input)?;

    let mut digest = word(header, 24) ^ 0x811c9dc5;
    let mut result = (0, 0);
    for round in 0..rounds {
        // Keep reads, computation and Scratch writes inside each measured round.
        use core::hint::black_box;
        result = workload::round(black_box(n), black_box(variant), black_box(input), scratch);
        black_box(&*scratch);
        digest = (digest.rotate_left(5) ^ result.0).wrapping_add(result.1 ^ round ^ 0x9e3779b9);
    }
    Ok([rounds, result.0, result.1, digest])
}

/// Execute the compiled workload and publish the EXB2 response in place.
pub fn execute(buffer: &mut [u8]) -> u32 {
    if buffer.len() != CONTEXT_BYTES {
        return Error::ShortBuffer as u32;
    }
    let (header, payload) = buffer.split_at_mut(HEADER_BYTES);
    let (status, fields) = match run(header, payload) {
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
