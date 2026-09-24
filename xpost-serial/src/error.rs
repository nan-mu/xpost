//! Library errors.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("{0}")]
    Msg(String),

    #[error("not in app mode (current: {0})")]
    NotApp(String),

    #[error("not in download mode (current: {0})")]
    NotDownload(String),

    #[error("session closed")]
    Closed,

    #[error("image too large: {len} bytes > capacity {cap}")]
    ImageTooLarge { len: usize, cap: usize },

    #[error("xdp name mismatch: arg `{want}`, file `{got}`")]
    NameMismatch { want: String, got: String },

    #[error("verify mismatch at offset {offset:#x}: wrote {want:#04x}, read {got:#04x}")]
    VerifyMismatch { offset: usize, want: u8, got: u8 },

    #[error("verify length mismatch: wrote {wrote}, read {read}")]
    VerifyLen { wrote: usize, read: usize },

    #[error("request timeout after {0:?}")]
    RequestTimeout(std::time::Duration),

    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error(transparent)]
    Espflash(#[from] espflash::Error),

    #[error(transparent)]
    Serial(#[from] serialport::Error),

    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

pub type Result<T> = std::result::Result<T, Error>;
