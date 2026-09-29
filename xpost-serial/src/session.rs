//! Exclusive USB-UART session: Download (espflash) vs App (line I/O).

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime};

use espflash::connection::{Connection, ResetAfterOperation, ResetBeforeOperation};
use espflash::flasher::Flasher;
use espflash::image_format::idf::IdfBootloaderFormat;
use espflash::image_format::ImageFormat;
use espflash::flasher::{FlashData, FlashSettings};
use espflash::target::{DefaultProgressCallback, ProgressCallbacks};
use log::debug;
use serialport::{SerialPort, SerialPortType, UsbPortInfo};

use crate::error::{Error, Result};
use crate::event::{
    Chip, Event, FlashPhase, FlashTarget, Image, IoData, IoRole, Kind, Mode, SessionWhy,
    port_label,
};
use crate::sink::Sink;
use crate::table::{
    ColdTable, InstallOutcome, JUST_DROP_TABLE, XdpEntry, list_entries, plan_install,
    xdp_name_from_bytes,
};

const DEFAULT_BAUD: u32 = 115_200;
const DEFAULT_IMAGE_CAPACITY: usize = 0x1000;
const READ_BLOCK: u32 = 0x1000; // FLASH_SECTOR_SIZE (espflash CLI default)
const READ_MAX_IN_FLIGHT: u32 = 64; // espflash CLI default
const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

/// Progress bridge: espflash callbacks → [`Sink`] Flash events.
struct SinkProgress<'a, S: Sink> {
    sink: &'a S,
    seq: &'a AtomicU64,
    target: FlashTarget,
    addr: u32,
    phase: FlashPhase,
    total: u32,
}

impl<'a, S: Sink> SinkProgress<'a, S> {
    fn emit(&self, done: u32) {
        let seq = self.seq.fetch_add(1, Ordering::Relaxed);
        self.sink.emit(Event {
            seq,
            ts: SystemTime::now(),
            kind: Kind::Flash {
                target: self.target.clone(),
                phase: self.phase,
                addr: Some(self.addr),
                len: self.total,
                done,
            },
        });
    }
}

impl<S: Sink> ProgressCallbacks for SinkProgress<'_, S> {
    fn init(&mut self, addr: u32, total: usize) {
        self.addr = addr;
        self.total = total as u32;
        self.emit(0);
    }

    fn update(&mut self, current: usize) {
        self.emit(current as u32);
    }

    fn verifying(&mut self) {}

    fn finish(&mut self, _skipped: bool) {
        self.emit(self.total);
    }
}

enum AppRx {
    Line(String),
    Died(String),
}

/// Host serial session owned by xpost-agent (or tests).
pub struct Session<S: Sink> {
    sink: S,
    seq: AtomicU64,
    mode: Mode,
    chip: Chip,
    port: Option<PathBuf>,
    baud: u32,
    /// Soft cap for a single XDP ELF container (just_drop default `0x1000`).
    pub image_capacity: usize,
    /// Cold table span for [`Self::list_xdp`] / [`Self::install_xdp`].
    pub cold_table: ColdTable,
    /// Active download flasher (ROM/stub). Dropped when leaving Download.
    flasher: Option<Flasher>,
    /// App-mode serial reader.
    app: Option<AppHalf>,
}

struct AppHalf {
    writer: Arc<Mutex<Box<dyn SerialPort>>>,
    rx: Receiver<AppRx>,
    stop: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
}

impl Drop for AppHalf {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

impl<S: Sink> Session<S> {
    /// Open a session. Does not enter Download until `write_bin` / `program`.
    pub fn open(port: Option<&Path>, chip: Chip, sink: S) -> Result<Self> {
        let port_buf = port.map(|p| p.to_path_buf());
        let mut s = Self {
            sink,
            seq: AtomicU64::new(0),
            mode: Mode::Closed,
            chip,
            port: port_buf.clone(),
            baud: DEFAULT_BAUD,
            image_capacity: DEFAULT_IMAGE_CAPACITY,
            cold_table: JUST_DROP_TABLE,
            flasher: None,
            app: None,
        };
        s.transition(
            Mode::App,
            SessionWhy::Open {
                port: port_label(port),
                baud: DEFAULT_BAUD,
                chip: chip.to_string(),
            },
        );
        // Start App reader so boot logs are captured when we reset_to_app later.
        // Before first flash there may be no device — soft-fail open of reader.
        if let Err(e) = s.ensure_app_reader() {
            s.emit_error("open", format!("app reader deferred: {e}"));
        }
        Ok(s)
    }

    pub fn mode(&self) -> Mode {
        self.mode
    }

    pub fn close(mut self) -> Result<()> {
        self.leave_download_silent();
        self.stop_app();
        if self.mode != Mode::Closed {
            self.transition(Mode::Closed, SessionWhy::Close);
        }
        Ok(())
    }

    /// Write Compact ELF (or any raw image): write → read-flash → byte compare.
    pub fn write_bin(&mut self, addr: u32, path: &Path) -> Result<()> {
        let data = fs::read(path).map_err(|e| {
            Error::Msg(format!("read {}: {e}", path.display()))
        })?;
        if data.len() > self.image_capacity {
            return Err(Error::ImageTooLarge {
                len: data.len(),
                cap: self.image_capacity,
            });
        }
        self.enter_download()?;
        self.write_bin_on_flasher(addr, &data, FlashTarget::Bin)?;
        Ok(())
    }

    /// Read the cold table and list packed XDP ELF entries (name / relative offset / len).
    ///
    /// Always returns the session to App mode (read uses download); otherwise
    /// subsequent `request` / `counters` fail with `not in app mode`.
    pub fn list_xdp(&mut self) -> Result<Vec<XdpEntry>> {
        let result = (|| {
            let bytes = self.read_cold_table()?;
            list_entries(&bytes)
        })();
        self.reset_to_app()?;
        result
    }

    /// Erase the entire cold table (all packed XDP ELF containers).
    pub fn clear_xdp(&mut self) -> Result<()> {
        const SECTOR: u32 = 0x1000;
        let abs = self.cold_table.base;
        let len = self.cold_table.len;
        if abs % SECTOR != 0 || len % SECTOR != 0 || len == 0 {
            return Err(Error::Msg(format!(
                "cold table {abs:#x}+{len:#x} must be non-empty and {SECTOR:#x}-aligned"
            )));
        }
        self.enter_download()?;
        {
            let seq = self.seq.fetch_add(1, Ordering::Relaxed);
            self.sink.emit(Event {
                seq,
                ts: SystemTime::now(),
                kind: Kind::Flash {
                    target: FlashTarget::ColdErase,
                    phase: FlashPhase::Writing,
                    addr: Some(abs),
                    len,
                    done: 0,
                },
            });
        }
        {
            let flasher = self.flasher.as_mut().ok_or(Error::Closed)?;
            flasher.erase_region(abs, len)?;
        }
        {
            let seq = self.seq.fetch_add(1, Ordering::Relaxed);
            self.sink.emit(Event {
                seq,
                ts: SystemTime::now(),
                kind: Kind::Flash {
                    target: FlashTarget::ColdErase,
                    phase: FlashPhase::Verified,
                    addr: Some(abs),
                    len,
                    done: len,
                },
            });
        }
        Ok(())
    }

    /// Install an XDP ELF by program name: overwrite same name, else append.
    ///
    /// `name` must match the name inside `path` (agent extracts via `xpost-elf`).
    pub fn install_xdp(&mut self, name: &str, path: &Path) -> Result<InstallOutcome> {
        let data = fs::read(path).map_err(|e| {
            Error::Msg(format!("read {}: {e}", path.display()))
        })?;
        if data.len() > self.cold_table.image_capacity.min(self.image_capacity) {
            return Err(Error::ImageTooLarge {
                len: data.len(),
                cap: self.cold_table.image_capacity.min(self.image_capacity),
            });
        }
        let file_name = xdp_name_from_bytes(&data)?;
        if file_name != name {
            return Err(Error::NameMismatch {
                want: name.to_string(),
                got: file_name,
            });
        }
        let new_len = data.len() as u32;
        let table = self.read_cold_table()?;
        let entries = list_entries(&table)?;
        let outcome = plan_install(&entries, name, new_len, self.cold_table.len)?;
        let (rel, old_len) = match outcome {
            InstallOutcome::Overwritten { offset } => {
                let old = entries
                    .iter()
                    .find(|e| e.offset == offset)
                    .map(|e| e.len)
                    .unwrap_or(new_len);
                (offset, old)
            }
            InstallOutcome::Created { offset } => (offset, new_len),
        };
        let abs = self.cold_table.base.saturating_add(rel);
        // Reconnect before erase: after a large read_flash the ROM often returns
        // EraseRegion Other(0x1) on the same session.
        self.leave_download_silent();
        self.enter_download()?;
        // Clear old span when overwriting (esp. shrink) so a trailing stale ELF
        // header cannot confuse the next cold-table scan. espflash erase_region
        // requires sector-aligned size (FLASH_SECTOR_SIZE = 0x1000).
        const SECTOR: u32 = 0x1000;
        let erase_len = {
            let need = old_len.max(new_len);
            need.saturating_add(SECTOR - 1) / SECTOR * SECTOR
        };
        if erase_len > 0 {
            let flasher = self.flasher.as_mut().ok_or(Error::Closed)?;
            flasher.erase_region(abs, erase_len)?;
        }
        self.write_bin_on_flasher(
            abs,
            &data,
            FlashTarget::Xdp {
                name: name.to_string(),
            },
        )?;
        Ok(outcome)
    }

    fn read_cold_table(&mut self) -> Result<Vec<u8>> {
        self.enter_download()?;
        let abs = self.cold_table.base;
        let len = self.cold_table.len;
        let tmp = tempfile_path()?;
        {
            let progress = SinkProgress {
                sink: &self.sink,
                seq: &self.seq,
                target: FlashTarget::Xdp {
                    name: "(cold-table)".into(),
                },
                addr: abs,
                phase: FlashPhase::Reading,
                total: len,
            };
            progress.emit(0);
            let flasher = self.flasher.as_mut().ok_or(Error::Closed)?;
            flasher.read_flash(abs, len, READ_BLOCK, READ_MAX_IN_FLIGHT, tmp.clone())?;
            progress.emit(len);
        }
        let data = fs::read(&tmp).map_err(|e| Error::Msg(format!("read cold table: {e}")))?;
        let _ = fs::remove_file(&tmp);
        if data.len() < len as usize {
            return Err(Error::Msg(format!(
                "short cold-table read {} < {len}",
                data.len()
            )));
        }
        Ok(data[..len as usize].to_vec())
    }

    /// Flash app ELF + partitions, optional name-based XDP install, then reset to App.
    pub fn program(&mut self, img: &Image<'_>) -> Result<()> {
        let firmware = fs::read(img.firmware).map_err(|e| {
            Error::Msg(format!("read firmware {}: {e}", img.firmware.display()))
        })?;
        if !img.partitions.is_file() {
            return Err(Error::Msg(format!(
                "partition table not found: {}",
                img.partitions.display()
            )));
        }

        self.enter_download()?;
        self.flash_firmware_inner(&firmware, img.partitions)?;

        if let Some(xdp) = &img.xdp {
            // flash_firmware_inner ends in download after partition-table write.
            // Wipe the cold table so lease(0) matches this profile's xdp_name
            // (otherwise a prior just_drop entry stays at offset 0 and just_tx appends).
            self.leave_download_silent();
            self.clear_xdp()?;
            self.leave_download_silent();
            self.install_xdp(xdp.name, xdp.path)?;
        }

        self.reset_to_app()?;
        Ok(())
    }

    pub fn reset_to_app(&mut self) -> Result<()> {
        if let Some(mut flasher) = self.flasher.take() {
            let chip = flasher.chip();
            let _ = flasher.connection().reset_after(true, chip);
            // Drop flasher to release the port.
            drop(flasher);
        }
        self.stop_app();
        if self.mode != Mode::App {
            self.transition(Mode::App, SessionWhy::ResetToApp);
        } else {
            self.emit_session(Mode::App, Mode::App, SessionWhy::ResetToApp);
        }
        // USB re-enumerate: brief pause then reopen App reader.
        thread::sleep(Duration::from_millis(800));
        self.ensure_app_reader()?;
        Ok(())
    }

    /// Write one command line; return first `OK `/`ERR ` reply. Interleaved log stays in transcript.
    pub fn request(&mut self, cmd: &str) -> Result<String> {
        self.request_timeout(cmd, DEFAULT_REQUEST_TIMEOUT)
    }

    pub fn request_timeout(&mut self, cmd: &str, timeout: Duration) -> Result<String> {
        if self.mode != Mode::App {
            return Err(Error::NotApp(self.mode.to_string()));
        }
        self.ensure_app_reader()?;
        let line = cmd.trim_end_matches(['\r', '\n']);
        self.emit_io(IoRole::CmdTx, line.to_string());
        {
            let app = self.app.as_mut().ok_or(Error::Closed)?;
            let mut w = app.writer.lock().map_err(|_| Error::Msg("writer lock".into()))?;
            w.write_all(line.as_bytes())?;
            w.write_all(b"\n")?;
            w.flush()?;
        }
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                self.emit_error("request", format!("timeout waiting for OK/ERR to `{line}`"));
                return Err(Error::RequestTimeout(timeout));
            }
            let app = self.app.as_mut().ok_or(Error::Closed)?;
            match app.rx.recv_timeout(remaining) {
                Ok(AppRx::Line(rx)) => {
                    if is_reply_line(&rx) {
                        self.emit_io(IoRole::CmdRx, rx.clone());
                        return Ok(rx);
                    }
                    // MCU-initiated traffic interleaved while waiting for the reply.
                    self.emit_io(IoRole::UartRx, rx);
                }
                Ok(AppRx::Died(msg)) => {
                    self.emit_error("request", msg.clone());
                    self.stop_app();
                    return Err(Error::Msg(msg));
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    self.emit_error("request", format!("timeout waiting for OK/ERR to `{line}`"));
                    return Err(Error::RequestTimeout(timeout));
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(Error::Msg("app reader disconnected".into()));
                }
            }
        }
    }

    /// Non-blocking: drain pending App UART lines into the sink (no command written).
    ///
    /// Reply-shaped lines (`OK `/`ERR `) are emitted as [`IoRole::CmdRx`]; others as
    /// [`IoRole::UartRx`]. All drained lines are returned for callers that scrape boot
    /// markers (e.g. `xpost ready`).
    pub fn drain_uart(&mut self) -> Result<Vec<String>> {
        if self.mode != Mode::App {
            return Err(Error::NotApp(self.mode.to_string()));
        }
        self.ensure_app_reader()?;
        let mut out = Vec::new();
        loop {
            let app = self.app.as_mut().ok_or(Error::Closed)?;
            match app.rx.try_recv() {
                Ok(AppRx::Line(rx)) => {
                    if is_reply_line(&rx) {
                        self.emit_io(IoRole::CmdRx, rx.clone());
                    } else {
                        self.emit_io(IoRole::UartRx, rx.clone());
                    }
                    out.push(rx);
                }
                Ok(AppRx::Died(msg)) => {
                    self.emit_error("drain_uart", msg.clone());
                    self.stop_app();
                    return Err(Error::Msg(msg));
                }
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    return Err(Error::Msg("app reader disconnected".into()));
                }
            }
        }
        Ok(out)
    }

    /// Host-timestamped sink lines with `ts ∈ [since, until]` (FileSink ring).
    pub fn serial_window(&self, since: SystemTime, until: SystemTime) -> Vec<String> {
        self.sink.lines_in_range(since, until)
    }

    // —— internals ——

    fn write_bin_on_flasher(&mut self, addr: u32, data: &[u8], target: FlashTarget) -> Result<()> {
        let len = data.len() as u32;
        {
            let mut progress = SinkProgress {
                sink: &self.sink,
                seq: &self.seq,
                target: target.clone(),
                addr,
                phase: FlashPhase::Writing,
                total: len,
            };
            let flasher = self.flasher.as_mut().ok_or(Error::Closed)?;
            // espflash `write_bins_to_flash(..., reboot=true)` hard-resets after
            // FlashDeflEnd, so the stub is gone. Reconnect before readback.
            flasher.write_bin_to_flash(addr, data, &mut progress)?;
        }
        self.leave_download_silent();
        self.enter_download()?;

        // Read-back via temp file (espflash API writes to path).
        let tmp = tempfile_path()?;
        {
            let progress = SinkProgress {
                sink: &self.sink,
                seq: &self.seq,
                target: target.clone(),
                addr,
                phase: FlashPhase::Reading,
                total: len,
            };
            progress.emit(0);
            let flasher = self.flasher.as_mut().ok_or(Error::Closed)?;
            // Compare only source len (write path may have padded to 4 bytes).
            let read_len = len;
            flasher.read_flash(addr, read_len, READ_BLOCK, READ_MAX_IN_FLIGHT, tmp.clone())?;
            progress.emit(len);
        }

        let read = fs::read(&tmp).map_err(|e| Error::Msg(format!("readback file: {e}")))?;
        let _ = fs::remove_file(&tmp);

        if read.len() < data.len() {
            self.emit_error(
                "verify",
                format!("short readback {} < {}", read.len(), data.len()),
            );
            return Err(Error::VerifyLen {
                wrote: data.len(),
                read: read.len(),
            });
        }
        for (i, (&want, &got)) in data.iter().zip(read.iter()).enumerate() {
            if want != got {
                self.emit_error(
                    "verify",
                    format!("mismatch at {i:#x}: wrote {want:#04x} read {got:#04x}"),
                );
                return Err(Error::VerifyMismatch {
                    offset: i,
                    want,
                    got,
                });
            }
        }

        let seq = self.seq.fetch_add(1, Ordering::Relaxed);
        self.sink.emit(Event {
            seq,
            ts: SystemTime::now(),
            kind: Kind::Flash {
                target,
                phase: FlashPhase::Verified,
                addr: Some(addr),
                len,
                done: len,
            },
        });
        Ok(())
    }

    fn flash_firmware_inner(&mut self, firmware: &[u8], partitions: &Path) -> Result<()> {
        // Build the real just_drop table (custom data subtype 0x40). esp-idf-part
        // panics if IdfBootloaderFormat tries to *parse* that CSV/bin, so flash the
        // app with the chip default table, then overwrite 0x8000 with our bin.
        let part_bin = crate::partitions::partition_table_bin(partitions)?;
        let flasher = self.flasher.as_mut().ok_or(Error::Closed)?;
        let chip = flasher.chip();
        let xtal = chip.xtal_frequency(flasher.connection())?;
        let flash_data = FlashData::new(FlashSettings::default(), 0, None, chip, xtal);
        let idf = IdfBootloaderFormat::new(firmware, &flash_data, None, None, None, None)?;
        let format = ImageFormat::from(idf);
        let total: u32 = format
            .metadata()
            .get("app_size")
            .and_then(|s| s.parse().ok())
            .unwrap_or(firmware.len() as u32);

        let seq = self.seq.fetch_add(1, Ordering::Relaxed);
        self.sink.emit(Event {
            seq,
            ts: SystemTime::now(),
            kind: Kind::Flash {
                target: FlashTarget::Firmware,
                phase: FlashPhase::Writing,
                addr: Some(0x10000),
                len: total,
                done: 0,
            },
        });

        let flasher = self.flasher.as_mut().ok_or(Error::Closed)?;
        flasher.load_image_to_flash(&mut DefaultProgressCallback, format)?;

        let seq = self.seq.fetch_add(1, Ordering::Relaxed);
        self.sink.emit(Event {
            seq,
            ts: SystemTime::now(),
            kind: Kind::Flash {
                target: FlashTarget::Firmware,
                phase: FlashPhase::Verified,
                addr: Some(0x10000),
                len: total,
                done: total,
            },
        });

        // Overwrite default partition table with board CSV (incl. xdp @ 0x310000).
        self.leave_download_silent();
        self.enter_download()?;
        self.write_bin_on_flasher(
            crate::partitions::PART_TABLE_OFFSET,
            &part_bin,
            FlashTarget::PartitionTable,
        )?;
        Ok(())
    }

    fn enter_download(&mut self) -> Result<()> {
        self.stop_app();
        if self.flasher.is_some() {
            if self.mode != Mode::Download {
                self.transition(Mode::Download, SessionWhy::ResetToDownload);
            }
            return Ok(());
        }
        if self.mode != Mode::Download {
            self.transition(Mode::Download, SessionWhy::ResetToDownload);
        }
        let flasher = connect_flasher(self.port.as_deref(), self.chip, self.baud)?;
        self.flasher = Some(flasher);
        Ok(())
    }

    fn leave_download_silent(&mut self) {
        self.flasher.take();
    }

    fn stop_app(&mut self) {
        self.app.take();
    }

    fn ensure_app_reader(&mut self) -> Result<()> {
        if self.app.is_some() {
            return Ok(());
        }
        if self.flasher.is_some() {
            return Err(Error::Msg(
                "cannot open app reader while download flasher holds the port".into(),
            ));
        }
        let (name, usb) = resolve_port(self.port.as_deref())?;
        let port = serialport::new(&name, self.baud)
            .timeout(Duration::from_millis(200))
            .open()?;
        let writer_port = port.try_clone()?;
        let reader_port = port;
        let stop = Arc::new(AtomicBool::new(false));
        let stop_t = stop.clone();
        let (tx, rx) = mpsc::channel();
        let join = thread::spawn(move || app_reader_loop(reader_port, tx, stop_t));
        let _ = usb; // reserved for future USB pid resets
        self.app = Some(AppHalf {
            writer: Arc::new(Mutex::new(writer_port)),
            rx,
            stop,
            join: Some(join),
        });
        Ok(())
    }

    fn transition(&mut self, to: Mode, why: SessionWhy) {
        let from = self.mode;
        self.mode = to;
        self.emit_session(from, to, why);
    }

    fn emit_session(&self, from: Mode, to: Mode, why: SessionWhy) {
        let seq = self.seq.fetch_add(1, Ordering::Relaxed);
        self.sink.emit(Event {
            seq,
            ts: SystemTime::now(),
            kind: Kind::Session { from, to, why },
        });
    }

    fn emit_io(&self, role: IoRole, line: String) {
        let seq = self.seq.fetch_add(1, Ordering::Relaxed);
        self.sink.emit(Event {
            seq,
            ts: SystemTime::now(),
            kind: Kind::Io {
                role,
                data: IoData::Line(line),
            },
        });
    }

    fn emit_error(&self, ctx: &'static str, msg: String) {
        let seq = self.seq.fetch_add(1, Ordering::Relaxed);
        self.sink.emit(Event {
            seq,
            ts: SystemTime::now(),
            kind: Kind::Error { ctx, msg },
        });
    }
}

fn app_reader_loop(port: Box<dyn SerialPort>, tx: Sender<AppRx>, stop: Arc<AtomicBool>) {
    let mut reader = BufReader::new(port);
    let mut buf = String::new();
    while !stop.load(Ordering::Acquire) {
        buf.clear();
        match reader.read_line(&mut buf) {
            Ok(0) => {
                thread::sleep(Duration::from_millis(20));
            }
            Ok(_) => {
                let line = buf.trim_end_matches(['\r', '\n']).to_string();
                if line.is_empty() {
                    continue;
                }
                if tx.send(AppRx::Line(line)).is_err() {
                    break;
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::TimedOut => continue,
            Err(e) => {
                let _ = tx.send(AppRx::Died(e.to_string()));
                break;
            }
        }
    }
}

fn connect_flasher(port: Option<&Path>, chip: Chip, baud: u32) -> Result<Flasher> {
    let (name, usb) = resolve_port(port)?;
    debug!("opening serial {name} for download");
    let serial = serialport::new(&name, 115_200)
        .flow_control(serialport::FlowControl::None)
        .open_native()?;
    let connection = Connection::new(
        serial,
        usb,
        ResetAfterOperation::HardReset,
        ResetBeforeOperation::DefaultReset,
        baud,
    );
    let flasher = Flasher::connect(
        connection,
        true,  // use stub
        false, // verify (espflash segment verify; we do our own readback)
        false, // skip
        Some(chip.as_espflash()),
        Some(baud),
    )?;
    Ok(flasher)
}

fn resolve_port(port: Option<&Path>) -> Result<(String, UsbPortInfo)> {
    let ports = serialport::available_ports().unwrap_or_default();
    if let Some(want) = port {
        let want_s = want.display().to_string();
        let canon = fs::canonicalize(want).ok();
        for p in &ports {
            if p.port_name == want_s
                || canon
                    .as_ref()
                    .and_then(|_c| fs::canonicalize(&p.port_name).ok())
                    .as_ref()
                    == canon.as_ref()
            {
                let usb = match &p.port_type {
                    SerialPortType::UsbPort(u) => u.clone(),
                    _ => UsbPortInfo {
                        vid: 0,
                        pid: 0,
                        serial_number: None,
                        manufacturer: None,
                        product: None,
                    },
                };
                return Ok((p.port_name.clone(), usb));
            }
        }
        // Port path given but not listed — try opening by name anyway.
        return Ok((
            want_s,
            UsbPortInfo {
                vid: 0,
                pid: 0,
                serial_number: None,
                manufacturer: None,
                product: None,
            },
        ));
    }

    let usb_ports: Vec<_> = ports
        .into_iter()
        .filter(|p| matches!(p.port_type, SerialPortType::UsbPort(_)))
        // macOS exposes each USB-UART as both `/dev/cu.*` and `/dev/tty.*`.
        // Prefer callout (`cu.`) devices; fall back to all if none.
        .collect();
    let cu_only: Vec<_> = usb_ports
        .iter()
        .filter(|p| {
            let n = p.port_name.as_str();
            n.contains("/cu.") || n.starts_with("cu.")
        })
        .cloned()
        .collect();
    let usb_ports = if cu_only.is_empty() {
        usb_ports
    } else {
        cu_only
    };
    // Deduplicate by USB identity when multiple callout paths remain.
    let mut unique: Vec<(String, UsbPortInfo)> = Vec::new();
    for p in &usb_ports {
        let usb = match &p.port_type {
            SerialPortType::UsbPort(u) => u.clone(),
            _ => continue,
        };
        let dup = unique.iter().any(|(_, u)| {
            u.vid == usb.vid
                && u.pid == usb.pid
                && u.serial_number == usb.serial_number
        });
        if !dup {
            unique.push((p.port_name.clone(), usb));
        }
    }
    match unique.len() {
        0 => Err(Error::Msg("no USB serial ports found".into())),
        1 => Ok(unique.into_iter().next().unwrap()),
        _ => Err(Error::Msg(format!(
            "multiple USB serial ports; pass --port explicitly: {}",
            unique
                .iter()
                .map(|(n, _)| n.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ))),
    }
}

fn tempfile_path() -> Result<PathBuf> {
    let mut p = std::env::temp_dir();
    p.push(format!(
        "xpost-serial-readback-{}-{}.bin",
        std::process::id(),
        Instant::now().elapsed().as_nanos()
    ));
    Ok(p)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sink::VecSink;

    #[test]
    fn transcript_ordering_and_render() {
        let sink = VecSink::new();
        let seq = AtomicU64::new(0);
        let emit = |kind: Kind| {
            let s = seq.fetch_add(1, Ordering::Relaxed);
            sink.emit(Event {
                seq: s,
                ts: SystemTime::UNIX_EPOCH,
                kind,
            });
        };
        emit(Kind::Session {
            from: Mode::Closed,
            to: Mode::App,
            why: SessionWhy::Open {
                port: "/dev/tty".into(),
                baud: 115200,
                chip: "esp32c3".into(),
            },
        });
        emit(Kind::Flash {
            target: FlashTarget::Xdp {
                name: "just_drop".into(),
            },
            phase: FlashPhase::Writing,
            addr: Some(0x310000),
            len: 10,
            done: 10,
        });
        emit(Kind::Io {
            role: IoRole::CmdTx,
            data: IoData::Line("PING".into()),
        });
        emit(Kind::Io {
            role: IoRole::UartRx,
            data: IoData::Line("INFO boot".into()),
        });
        emit(Kind::Io {
            role: IoRole::CmdRx,
            data: IoData::Line("OK PING".into()),
        });
        let lines = sink.transcript();
        assert!(
            lines[0].contains("[session] open "),
            "got {}",
            lines[0]
        );
        assert!(
            lines[1].contains("[xdp] writing name=just_drop "),
            "got {}",
            lines[1]
        );
        assert!(lines[2].ends_with("[cmd] → PING"), "got {}", lines[2]);
        assert!(lines[3].ends_with("[uart] INFO boot"), "got {}", lines[3]);
        assert!(lines[4].ends_with("[cmd] ← OK PING"), "got {}", lines[4]);
        let ev = sink.events();
        for (i, e) in ev.iter().enumerate() {
            assert_eq!(e.seq, i as u64);
        }
    }

    #[test]
    fn image_capacity_gate() {
        let sink = VecSink::new();
        // open may fail to attach reader without hardware — still constructs.
        let mut s = Session::open(None, Chip::Esp32c3, sink).unwrap();
        s.image_capacity = 4;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("big.bin");
        fs::write(&path, [0u8; 8]).unwrap();
        let err = s.write_bin(0x310000, &path).unwrap_err();
        match err {
            Error::ImageTooLarge { len: 8, cap: 4 } => {}
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn request_reply_is_first_ok_or_err() {
        assert!(super::is_reply_line("OK PING"));
        assert!(super::is_reply_line("ERR busy"));
        assert!(!super::is_reply_line("INFO boot"));
        assert!(!super::is_reply_line("ok lower"));
    }
}

/// Line that completes [`Session::request`].
pub(crate) fn is_reply_line(line: &str) -> bool {
    line.starts_with("OK ") || line.starts_with("ERR ")
}
