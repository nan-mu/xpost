use std::fs::{File, OpenOptions};
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

use anyhow::{Context, Result, ensure};

pub fn claim() -> Result<File> {
    #[cfg(feature = "test-device")]
    let path = crate::fixture::device_path()?;
    #[cfg(not(feature = "test-device"))]
    let path = Path::new("/tmp/xpost-agent-usb.lock");
    let path: &Path = path.as_ref();

    // Automatic DUT selection and reconnect share one host-wide owner.
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o666)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .with_context(|| format!("open DUT lock {}", path.display()))?;
    ensure!(lock.metadata()?.is_file(), "DUT lock is not a regular file");
    // The descriptor remains live across all device sessions and resets.
    if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        let error = io::Error::last_os_error();
        return Err(error).context("claim exclusive DUT lock");
    }
    Ok(lock)
}
