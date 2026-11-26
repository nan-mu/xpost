//! Medium EXB2 inputs and independent expected responses from v0.1.1.
//!
//! One sample invokes the engine once with the whole 8258-byte buffer.
//! Both the input and output digests include the scratch and padding.

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

pub(super) const PROGRAMS: [&str; 7] = [
    "noop",
    "prime",
    "log2",
    "copy",
    "fletcher32",
    "bsort",
    "strcmp",
];
pub(super) const BUF: usize = 8258;
const HEADER: usize = 64;
pub(super) const ROUNDS: u32 = 3;
pub(super) const SEED: u32 = 1;
pub(super) const ANALYSIS_VERSION: &str = "exb2-equality/nms1-v1";

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) struct Sampling {
    pub(super) runs: i32,
    pub(super) samples: i32,
}

impl Sampling {
    pub(super) const MEDIUM: Self = Self {
        runs: 2,
        samples: 1,
    };
    pub(super) const FULL: Self = Self {
        runs: 5,
        samples: 100,
    };
}

struct Vector {
    n: u32,
    input: &'static [u8],
    scratch: &'static [u8],
    result: [u32; 2],
    digest: u32,
}

// Byte-exact references from tests/exb2-host-vectors.md at Gitea b7af1ff.
const VECTORS: [Vector; 7] = [
    Vector {
        n: 0,
        input: &[],
        scratch: &[],
        result: [0, 0],
        digest: 0x91efd7d5,
    },
    Vector {
        n: 19,
        input: &[],
        scratch: &[],
        result: [8, 19],
        digest: 0x91efb91a,
    },
    Vector {
        n: 4,
        input: b"\0\0\0\0\x01\0\0\0\x04\0\0\0\xff\0\0\0",
        scratch: &[],
        result: [9, 0x48a],
        digest: 0x91fe55a4,
    },
    Vector {
        n: 4,
        input: b"\x44\x20\x82\x3c",
        scratch: b"\x44\x20\x82\x3c",
        result: [4, 0],
        digest: 0x91efe859,
    },
    Vector {
        n: 3,
        input: b"\x01\x02\x03",
        scratch: &[],
        result: [0x04050204, 0],
        digest: 0x199d264a,
    },
    Vector {
        n: 4,
        input: b"\x08\0\0\0\x02\0\0\0\x01\0\0\0\x08\0\0\0",
        scratch: b"\x01\0\0\0\x02\0\0\0\x08\0\0\0\x08\0\0\0",
        result: [1, 8],
        digest: 0x91efbaee,
    },
    Vector {
        n: 4,
        input: b"abcz\0abdz\0",
        scratch: &[],
        result: [u32::MAX, 3],
        digest: 0x664ce84e,
    },
];

fn buffers(task: usize, vector: &Vector) -> (Vec<u8>, Vec<u8>) {
    let mut request = vec![0xcc; BUF];
    request[..HEADER].fill(0);
    request[..8].copy_from_slice(b"EXB2\x02\0\x40\0");
    let fields = [
        task as u32,
        vector.n,
        ROUNDS,
        0,
        SEED,
        vector.input.len() as u32,
        vector.scratch.len() as u32,
        u32::MAX,
    ];
    for (out, field) in request[8..40].chunks_exact_mut(4).zip(fields) {
        out.copy_from_slice(&field.to_le_bytes());
    }
    let scratch = HEADER + vector.input.len();
    request[HEADER..scratch].copy_from_slice(vector.input);
    request[scratch..scratch + vector.scratch.len()].fill(0xa5);
    let mut expected = request.clone();
    for (out, field) in expected[36..56].chunks_exact_mut(4).zip([
        0,
        ROUNDS,
        vector.result[0],
        vector.result[1],
        vector.digest,
    ]) {
        out.copy_from_slice(&field.to_le_bytes());
    }
    expected[scratch..scratch + vector.scratch.len()].copy_from_slice(vector.scratch);
    (request, expected)
}

pub(super) fn conditions(sampling: Sampling) -> Value {
    let workloads = PROGRAMS.iter().zip(&VECTORS).enumerate().map(|(task, (program, vector))| {
        let (request, expected) = buffers(task, vector);
        let mut input = json!({ "program": program, "task": task, "n": vector.n, "seed": SEED, "variant": 0,
            "batch_count": ROUNDS, "input_bytes": vector.input.len(), "scratch_bytes": vector.scratch.len(),
            "request_sha256": super::case_store::hex(&Sha256::digest(request)),
            "response_sha256": super::case_store::hex(&Sha256::digest(expected)),
            "expected_status": 0, "expected_completed": ROUNDS, "expected_result": vector.result,
            "expected_digest": vector.digest,
        });
        // Preserve existing medium workload identities for resume.
        if sampling != Sampling::MEDIUM {
            input["sampling"] = json!({ "runs": sampling.runs, "samples_per_run": sampling.samples });
            input["analysis_version"] = json!(ANALYSIS_VERSION);
        }
        input
    }).collect::<Vec<_>>();
    json!({ "buffer_bytes": BUF, "header_bytes": HEADER, "workloads": workloads,
        "runs_per_program": sampling.runs, "samples_per_run": sampling.samples,
        "batches_per_program": sampling.runs * sampling.samples })
}

pub(super) fn request(task: usize) -> (u32, Vec<u8>, Vec<u8>) {
    let vector = &VECTORS[task];
    let (input, expected) = buffers(task, vector);
    (vector.n, input, expected)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_sampling_changes_identity_without_changing_workload_vectors() {
        let medium = conditions(Sampling::MEDIUM);
        let full = conditions(Sampling::FULL);
        assert_eq!(medium["batches_per_program"], 2);
        assert_eq!(full["batches_per_program"], 500);
        assert_eq!(medium["runs_per_program"], 2);
        assert_eq!(full["runs_per_program"], 5);
        for task in 0..PROGRAMS.len() {
            let mut full_input = full["workloads"][task].as_object().unwrap().clone();
            assert_eq!(
                full_input.remove("sampling"),
                Some(json!({ "runs": 5, "samples_per_run": 100 }))
            );
            assert_eq!(
                full_input.remove("analysis_version"),
                Some(json!(ANALYSIS_VERSION))
            );
            assert_eq!(Value::Object(full_input), medium["workloads"][task]);
        }
    }

    #[test]
    fn copy_vector_separates_input_scratch_and_padding() {
        let (input, output) = buffers(3, &VECTORS[3]);
        assert_eq!(input.len(), 8258);
        assert_eq!(&input[0..8], b"EXB2\x02\0\x40\0");
        assert_eq!(&input[36..40], &[0xff; 4]);
        assert_eq!(
            &output[36..56],
            &[
                0, 0, 0, 0, 3, 0, 0, 0, 4, 0, 0, 0, 0, 0, 0, 0, 0x59, 0xe8, 0xef, 0x91
            ]
        );
        assert_eq!(
            &input[64..72],
            &[0x44, 0x20, 0x82, 0x3c, 0xa5, 0xa5, 0xa5, 0xa5]
        );
        assert_eq!(
            &output[64..72],
            &[0x44, 0x20, 0x82, 0x3c, 0x44, 0x20, 0x82, 0x3c]
        );
        assert!(output[72..].iter().all(|&b| b == 0xcc));
    }
}
