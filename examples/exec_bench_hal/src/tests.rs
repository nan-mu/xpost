extern crate std;

use std::{vec, vec::Vec};

use crate::Workload;
#[cfg(feature = "bsort")]
use crate::workloads::bsort::Bsort as Selected;
#[cfg(feature = "copy")]
use crate::workloads::copy::Copy as Selected;
#[cfg(feature = "fletcher32")]
use crate::workloads::fletcher32::Fletcher32 as Selected;
#[cfg(feature = "log2")]
use crate::workloads::log2::Log2 as Selected;
#[cfg(feature = "matmul")]
use crate::workloads::matmul::Matmul as Selected;
#[cfg(feature = "noop")]
use crate::workloads::noop::Noop as Selected;
#[cfg(feature = "prime")]
use crate::workloads::prime::Prime as Selected;
#[cfg(feature = "strcmp")]
use crate::workloads::strcmp::Strcmp as Selected;

fn request(task: u32, n: u32, variant: u32, input: &[u8], scratch: usize) -> Vec<u8> {
    let mut buffer = vec![0xcc; 8258];
    buffer[..64].fill(0);
    buffer[..8].copy_from_slice(b"EXB2\x02\0\x40\0");
    for (out, field) in buffer[8..40].chunks_exact_mut(4).zip([
        task,
        n,
        3,
        variant,
        1,
        input.len() as u32,
        scratch as u32,
        u32::MAX,
    ]) {
        out.copy_from_slice(&field.to_le_bytes());
    }
    buffer[64..64 + input.len()].copy_from_slice(input);
    buffer[64 + input.len()..64 + input.len() + scratch].fill(0xa5);
    buffer
}

fn response(mut buffer: Vec<u8>, fields: [u32; 5], scratch: &[u8]) -> Vec<u8> {
    let input_len = u32::from_le_bytes(buffer[28..32].try_into().unwrap()) as usize;
    buffer[64 + input_len..64 + input_len + scratch.len()].copy_from_slice(scratch);
    for (out, field) in buffer[36..56].chunks_exact_mut(4).zip(fields) {
        out.copy_from_slice(&field.to_le_bytes());
    }
    buffer
}

#[cfg(feature = "matmul")]
#[test]
fn matmul_publishes_all_four_results_and_preserves_input_and_padding() {
    let input: Vec<_> = (1u32..=8).flat_map(u32::to_le_bytes).collect();
    let output: Vec<_> = [19u32, 22, 43, 50]
        .into_iter()
        .flat_map(u32::to_le_bytes)
        .collect();
    let mut buffer = request(7, 2, 0, &input, 16);
    let expected = response(buffer.clone(), [0, 3, 19, 50, 0x91eee432], &output);
    assert_eq!(crate::execute::<Selected>(&mut buffer), 0);
    assert_eq!(buffer, expected);
}

#[cfg(feature = "noop")]
const LEGACY: (u32, &[u8], &[u8], [u32; 3]) = (0, b"", b"", [0, 0, 0x91efd7d5]);
#[cfg(feature = "prime")]
const LEGACY: (u32, &[u8], &[u8], [u32; 3]) = (19, b"", b"", [8, 19, 0x91efb91a]);
#[cfg(feature = "log2")]
const LEGACY: (u32, &[u8], &[u8], [u32; 3]) = (
    4,
    b"\0\0\0\0\x01\0\0\0\x04\0\0\0\xff\0\0\0",
    b"",
    [9, 0x48a, 0x91fe55a4],
);
#[cfg(feature = "copy")]
const LEGACY: (u32, &[u8], &[u8], [u32; 3]) = (
    4,
    b"\x44\x20\x82\x3c",
    b"\x44\x20\x82\x3c",
    [4, 0, 0x91efe859],
);
#[cfg(feature = "fletcher32")]
const LEGACY: (u32, &[u8], &[u8], [u32; 3]) =
    (3, b"\x01\x02\x03", b"", [0x04050204, 0, 0x199d264a]);
#[cfg(feature = "bsort")]
const LEGACY: (u32, &[u8], &[u8], [u32; 3]) = (
    4,
    b"\x08\0\0\0\x02\0\0\0\x01\0\0\0\x08\0\0\0",
    b"\x01\0\0\0\x02\0\0\0\x08\0\0\0\x08\0\0\0",
    [1, 8, 0x91efbaee],
);
#[cfg(feature = "strcmp")]
const LEGACY: (u32, &[u8], &[u8], [u32; 3]) = (4, b"abcz\0abdz\0", b"", [u32::MAX, 3, 0x664ce84e]);

#[cfg(not(feature = "matmul"))]
#[test]
fn pinned_workload_vector_matches_every_response_byte() {
    let (n, input, scratch, [lo, hi, digest]) = LEGACY;
    let mut buffer = request(Selected::TASK_ID.into(), n, 0, input, scratch.len());
    let expected = response(buffer.clone(), [0, 3, lo, hi, digest], scratch);
    assert_eq!(crate::execute::<Selected>(&mut buffer), 0);
    assert_eq!(buffer, expected);
}

fn rejects(mut buffer: Vec<u8>, status: u32) {
    let expected = response(buffer.clone(), [status, 0, 0, 0, 0], &[]);
    assert_eq!(crate::execute::<Selected>(&mut buffer), status);
    assert_eq!(buffer, expected);
}

#[test]
fn rejects_invalid_headers_before_running_work() {
    let task = u32::from(Selected::TASK_ID);
    let request = request(task, 1, 0, b"", 0);
    for (offset, value, status) in [(0, 0, 1), (4, 3, 2), (6, 63, 2), (8, 8, 3), (56, 1, 5)] {
        let mut buffer = request.clone();
        buffer[offset] = value;
        rejects(buffer, status);
    }
    let mut truncated = request[..8257].to_vec();
    assert_eq!(crate::execute::<Selected>(&mut truncated), 7);
    assert_eq!(truncated, request[..8257]);
}

fn check_round(n: u32, variant: u32, input: &[u8], output: &[u8], lo: u32, hi: u32) {
    let mut buffer = request(Selected::TASK_ID.into(), n, variant, input, output.len());
    let digest = (0..3).fold(1u32 ^ 0x811c9dc5, |d, round| {
        (d.rotate_left(5) ^ lo).wrapping_add(hi ^ round ^ 0x9e3779b9)
    });
    let expected = response(buffer.clone(), [0, 3, lo, hi, digest], output);
    assert_eq!(crate::execute::<Selected>(&mut buffer), 0);
    assert_eq!(buffer, expected);
}

#[cfg(feature = "matmul")]
#[test]
fn matmul_accepts_dimension_boundaries_and_rejects_invalid_values_and_layouts() {
    let task = u32::from(Selected::TASK_ID);
    for n in [1, 3, 26] {
        let a = (0..n * n).map(|i| (i * 17 + 31) % 256).collect::<Vec<_>>();
        let b = (0..n * n)
            .map(|i| {
                if n == 3 {
                    u32::from(i / n == i % n)
                } else {
                    (i * 29 + 255) % 256
                }
            })
            .collect::<Vec<_>>();
        let expected = (0..n * n)
            .map(|i| {
                (0..n)
                    .map(|k| a[(i / n * n + k) as usize] * b[(k * n + i % n) as usize])
                    .sum::<u32>()
            })
            .collect::<Vec<_>>();
        let input = a
            .into_iter()
            .chain(b)
            .flat_map(u32::to_le_bytes)
            .collect::<Vec<_>>();
        let output = expected
            .iter()
            .copied()
            .flat_map(u32::to_le_bytes)
            .collect::<Vec<_>>();
        check_round(
            n,
            0,
            &input,
            &output,
            expected[0],
            *expected.last().unwrap(),
        );
    }
    rejects(request(task, 0, 0, &[], 0), 4);
    rejects(request(task, 27, 0, &[], 0), 4);
    rejects(request(task, 1, 1, &[0; 8], 4), 6);
    rejects(request(task, 1, 0, &[0; 7], 4), 5);
    rejects(request(task, 1, 0, &[0; 8], 3), 5);
    rejects(request(task, 1, 0, &[0, 1, 0, 0, 0, 0, 0, 0], 4), 4);
}

#[cfg(feature = "noop")]
#[test]
fn noop_rejects_nonzero_n_and_rounds_outside_the_abi() {
    check_round(0, 0, b"", b"", 0, 0);
    rejects(request(0, 1, 0, b"", 0), 4);
    for rounds in [0u32, 65537] {
        let mut buffer = request(0, 0, 0, b"", 0);
        buffer[16..20].copy_from_slice(&rounds.to_le_bytes());
        rejects(buffer, 4);
    }
}

#[cfg(feature = "prime")]
#[test]
fn prime_handles_empty_and_upper_limit() {
    check_round(1, 0, b"", b"", 0, 0);
    check_round(8192, 0, b"", b"", 1028, 8191);
}

#[cfg(feature = "log2")]
#[test]
fn log2_uses_both_halves_of_a_64_bit_word() {
    let bytes = [0u64, 1 << 31, 1 << 32, u64::MAX]
        .into_iter()
        .flat_map(u64::to_le_bytes)
        .collect::<Vec<_>>();
    check_round(4, 1, &bytes, b"", 126, 0x8402);
}

#[cfg(feature = "copy")]
#[test]
fn copy_places_all_maximum_input_bytes_in_independent_scratch() {
    let input = (0..4096).map(|i| (i * 31) as u8).collect::<Vec<_>>();
    check_round(4096, 0, &input, &input, 4096, 0);
}

#[cfg(feature = "fletcher32")]
#[test]
fn fletcher32_checks_zero_even_odd_and_multiple_blocks() {
    check_round(0, 0, b"", b"", u32::MAX, 0);
    check_round(2, 0, &[1, 2], b"", 0x02010201, 0);
    for n in [2063, 4096] {
        let input = (0..n).map(|i| (i * 53 + 17) as u8).collect::<Vec<_>>();
        let words = input
            .chunks(2)
            .map(|bytes| u32::from(bytes[0]) + 256 * u32::from(*bytes.get(1).unwrap_or(&0)));
        let (s1, s2) = words.fold((0, 0), |(s1, s2), word| {
            let s1 = (s1 + word) % 65535;
            (s1, (s2 + s1) % 65535)
        });
        let nonzero = |sum| if sum == 0 { 65535 } else { sum };
        check_round(
            n as u32,
            0,
            &input,
            b"",
            (nonzero(s2) << 16) | nonzero(s1),
            0,
        );
    }
}

#[cfg(feature = "bsort")]
#[test]
fn bsort_preserves_duplicates_and_unsigned_values() {
    let mut words = (0..128).map(|i| u32::MAX - i / 2).collect::<Vec<_>>();
    let input: Vec<_> = words.iter().copied().flat_map(u32::to_le_bytes).collect();
    words.reverse();
    let output: Vec<_> = words.into_iter().flat_map(u32::to_le_bytes).collect();
    check_round(128, 0, &input, &output, u32::MAX - 63, u32::MAX);
    check_round(0, 0, b"", b"", 0, 0);
}

#[cfg(feature = "strcmp")]
#[test]
fn strcmp_counts_through_the_first_difference_or_embedded_nul() {
    check_round(0, 0, b"\0\0", b"", 0, 0);
    check_round(2, 0, b"\0a\0\0b\0", b"", 0, 1);
    check_round(2, 0, b"a\xff\0aA\0", b"", 190, 2);
    rejects(request(6, 2, 0, b"abcab\0", 0), 5);
}
