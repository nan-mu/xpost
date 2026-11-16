//! Which pipeline produced an image, kept apart from the engine that runs it.
//!
//! AOT and JIT both enter the device as native RV32 code, and that path stores
//! engine code 0. The code alone cannot say which pipeline built the bytes.

/// How the image was produced.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pipeline {
    /// Native firmware. No XDP ELF is loaded.
    Hal,
    /// The same C source compiled to RV32, not a translation of eBPF.
    Aot,
    /// Host JIT of eBPF to RV32. The device runs the machine code.
    Jit,
    /// eBPF bytecode for the MCU interpreter.
    Rbpf,
}

/// Short mode name stored in the database.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Hal,
    Aot,
    Jit,
    Rbpf,
}

impl Mode {
    pub const fn as_db(self) -> &'static str {
        match self {
            Self::Hal => "hal",
            Self::Aot => "aot",
            Self::Jit => "jit",
            Self::Rbpf => "rbpf",
        }
    }
}

/// Engine code stored with a published native or interpreter image.
pub const NATIVE_CODE: u8 = 0;
/// Engine code stored with interpreter bytecode.
pub const INTERP_CODE: u8 = 1;

/// Why a pipeline and an engine code do not name a mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IdentityError {
    /// `rbpf` was given native machine code.
    MachineCodeAsRbpf,
    /// A native pipeline was given interpreter bytecode.
    BytecodeAsNative,
    /// HAL was given an XDP engine code.
    HalHasNoEngine,
    /// A pipeline that loads an image omitted the engine code.
    MissingEngine,
    /// The code is neither native nor interpreter.
    UnknownEngine(u8),
    /// A required SHA-256 was left as 32 zero bytes.
    EmptyHash,
    /// HAL was given an XDP image or a slot. It has neither.
    HalHasNoImage,
    /// A pipeline that publishes an image omitted the XDP hash or the slot.
    MissingImage,
    /// The slot cannot hold an image.
    EmptySlot,
}

/// Pipeline plus the engine code the device will store.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BuildIdentity {
    pipeline: Pipeline,
    engine_code: Option<u8>,
}

impl BuildIdentity {
    /// Reject pairs that would file one pipeline's bytes as another mode.
    pub const fn new(pipeline: Pipeline, engine_code: Option<u8>) -> Result<Self, IdentityError> {
        match pipeline {
            Pipeline::Hal => match engine_code {
                None => Ok(Self {
                    pipeline,
                    engine_code,
                }),
                Some(_) => Err(IdentityError::HalHasNoEngine),
            },
            Pipeline::Aot | Pipeline::Jit => match engine_code {
                Some(NATIVE_CODE) => Ok(Self {
                    pipeline,
                    engine_code,
                }),
                Some(INTERP_CODE) => Err(IdentityError::BytecodeAsNative),
                Some(code) => Err(IdentityError::UnknownEngine(code)),
                None => Err(IdentityError::MissingEngine),
            },
            Pipeline::Rbpf => match engine_code {
                Some(INTERP_CODE) => Ok(Self {
                    pipeline,
                    engine_code,
                }),
                Some(NATIVE_CODE) => Err(IdentityError::MachineCodeAsRbpf),
                Some(code) => Err(IdentityError::UnknownEngine(code)),
                None => Err(IdentityError::MissingEngine),
            },
        }
    }

    pub const fn pipeline(self) -> Pipeline {
        self.pipeline
    }

    pub const fn engine_code(self) -> Option<u8> {
        self.engine_code
    }

    pub const fn mode(self) -> Mode {
        match self.pipeline {
            Pipeline::Hal => Mode::Hal,
            Pipeline::Aot => Mode::Aot,
            Pipeline::Jit => Mode::Jit,
            Pipeline::Rbpf => Mode::Rbpf,
        }
    }
}

/// Where a published image sits, and how many bytes that slot can hold.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Slot {
    pub address: u32,
    pub capacity: u32,
}

/// One image after the pipeline check, with the hashes and the slot that were loaded.
///
/// HAL has no XDP ELF and no slot. The other three pipelines require both, and a
/// zero hash or a zero capacity is not a recorded image.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ImageRecord {
    identity: BuildIdentity,
    firmware_sha256: [u8; 32],
    xdp_sha256: Option<[u8; 32]>,
    slot: Option<Slot>,
}

impl ImageRecord {
    pub const fn new(
        identity: BuildIdentity,
        firmware_sha256: [u8; 32],
        xdp_sha256: Option<[u8; 32]>,
        slot: Option<Slot>,
    ) -> Result<Self, IdentityError> {
        if empty_hash(&firmware_sha256) {
            return Err(IdentityError::EmptyHash);
        }
        match identity.pipeline {
            Pipeline::Hal => match (xdp_sha256, slot) {
                (None, None) => Ok(Self {
                    identity,
                    firmware_sha256,
                    xdp_sha256,
                    slot,
                }),
                _ => Err(IdentityError::HalHasNoImage),
            },
            Pipeline::Aot | Pipeline::Jit | Pipeline::Rbpf => match (xdp_sha256, slot) {
                (Some(hash), Some(slot)) if !empty_hash(&hash) && slot.capacity > 0 => Ok(Self {
                    identity,
                    firmware_sha256,
                    xdp_sha256,
                    slot: Some(slot),
                }),
                (Some(hash), Some(_)) if empty_hash(&hash) => Err(IdentityError::EmptyHash),
                (Some(_), Some(_)) => Err(IdentityError::EmptySlot),
                _ => Err(IdentityError::MissingImage),
            },
        }
    }

    pub const fn identity(self) -> BuildIdentity {
        self.identity
    }

    pub const fn firmware_sha256(self) -> [u8; 32] {
        self.firmware_sha256
    }

    pub const fn xdp_sha256(self) -> Option<[u8; 32]> {
        self.xdp_sha256
    }

    pub const fn slot(self) -> Option<Slot> {
        self.slot
    }
}

const fn empty_hash(hash: &[u8; 32]) -> bool {
    let mut i = 0;
    while i < 32 {
        if hash[i] != 0 {
            return false;
        }
        i += 1;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jit_native_is_not_aot() {
        let jit = BuildIdentity::new(Pipeline::Jit, Some(NATIVE_CODE)).unwrap();
        let aot = BuildIdentity::new(Pipeline::Aot, Some(NATIVE_CODE)).unwrap();
        assert_eq!(jit.engine_code(), aot.engine_code());
        assert_eq!(jit.mode().as_db(), "jit");
        assert_eq!(aot.mode().as_db(), "aot");
        assert_ne!(jit.mode(), aot.mode());
    }

    #[test]
    fn rbpf_rejects_native_machine_code() {
        let err = BuildIdentity::new(Pipeline::Rbpf, Some(NATIVE_CODE)).unwrap_err();
        assert_eq!(err, IdentityError::MachineCodeAsRbpf);
    }

    #[test]
    fn native_pipelines_reject_bytecode() {
        assert_eq!(
            BuildIdentity::new(Pipeline::Jit, Some(INTERP_CODE)).unwrap_err(),
            IdentityError::BytecodeAsNative
        );
        assert_eq!(
            BuildIdentity::new(Pipeline::Aot, Some(INTERP_CODE)).unwrap_err(),
            IdentityError::BytecodeAsNative
        );
    }

    #[test]
    fn hal_has_no_engine_code() {
        let hal = BuildIdentity::new(Pipeline::Hal, None).unwrap();
        assert_eq!(hal.mode().as_db(), "hal");
        assert_eq!(
            BuildIdentity::new(Pipeline::Hal, Some(NATIVE_CODE)).unwrap_err(),
            IdentityError::HalHasNoEngine
        );
    }

    #[test]
    fn rbpf_accepts_interpreter_code() {
        let rbpf = BuildIdentity::new(Pipeline::Rbpf, Some(INTERP_CODE)).unwrap();
        assert_eq!(rbpf.mode().as_db(), "rbpf");
    }

    fn hash(byte: u8) -> [u8; 32] {
        [byte; 32]
    }

    fn slot() -> Slot {
        Slot {
            address: 0x4200_0000,
            capacity: 64 * 1024,
        }
    }

    #[test]
    fn the_same_native_code_keeps_jit_and_aot_apart_when_the_hashes_differ() {
        let jit = ImageRecord::new(
            BuildIdentity::new(Pipeline::Jit, Some(NATIVE_CODE)).unwrap(),
            hash(1),
            Some(hash(2)),
            Some(slot()),
        )
        .unwrap();
        let aot = ImageRecord::new(
            BuildIdentity::new(Pipeline::Aot, Some(NATIVE_CODE)).unwrap(),
            hash(1),
            Some(hash(3)),
            Some(slot()),
        )
        .unwrap();
        assert_eq!(jit.identity().engine_code(), aot.identity().engine_code());
        assert_eq!(jit.identity().mode().as_db(), "jit");
        assert_eq!(aot.identity().mode().as_db(), "aot");
        assert_ne!(jit.xdp_sha256(), aot.xdp_sha256());
        assert_eq!(jit.slot().unwrap().capacity, 64 * 1024);
    }

    #[test]
    fn a_zero_hash_or_a_zero_capacity_is_not_a_recorded_image() {
        let jit = BuildIdentity::new(Pipeline::Jit, Some(NATIVE_CODE)).unwrap();
        assert_eq!(
            ImageRecord::new(jit, [0; 32], Some(hash(2)), Some(slot())).unwrap_err(),
            IdentityError::EmptyHash
        );
        assert_eq!(
            ImageRecord::new(jit, hash(1), Some([0; 32]), Some(slot())).unwrap_err(),
            IdentityError::EmptyHash
        );
        assert_eq!(
            ImageRecord::new(
                jit,
                hash(1),
                Some(hash(2)),
                Some(Slot {
                    address: 0x4200_0000,
                    capacity: 0,
                }),
            )
            .unwrap_err(),
            IdentityError::EmptySlot
        );
        assert_eq!(
            ImageRecord::new(jit, hash(1), None, Some(slot())).unwrap_err(),
            IdentityError::MissingImage
        );
    }

    #[test]
    fn hal_records_firmware_only() {
        let hal = BuildIdentity::new(Pipeline::Hal, None).unwrap();
        let record = ImageRecord::new(hal, hash(9), None, None).unwrap();
        assert!(record.xdp_sha256().is_none());
        assert!(record.slot().is_none());
        assert_eq!(
            ImageRecord::new(hal, hash(9), Some(hash(2)), None).unwrap_err(),
            IdentityError::HalHasNoImage
        );
    }
}
