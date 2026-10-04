//! Flash task: inbox [`FlashOp`]; logs begin/end (flash is disruptive, no port wait).

mod fw;
mod partitions;
mod xdp;

pub use partitions::xdp_table_from_csv;
pub use xdp::{
    InstallOutcome, XdpEntry, XdpTable, list_entries, plan_install, xdp_name_from_bytes,
    xdp_name_from_file,
};

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use futures::FutureExt;
use futures::executor::block_on;
use tokio::sync::{mpsc, oneshot};

use std::io;
use crate::client::ops::{FlashEvent, FlashOp};
use crate::client::port::Port;
use crate::client::record::{SharedRecord, WriteRecord};

/// Default partition label for the XDP flash span (`partitions.csv` Name column).
pub const DEFAULT_XDP_PARTITION: &str = "xdp";

enum FlashReq {
    Op {
        op: FlashOp,
        resp: oneshot::Sender<io::Result<FlashResult>>,
    },
    Shutdown,
}

/// Outcome of a flash-task operation.
#[derive(Clone, Debug)]
pub enum FlashResult {
    Done,
    Install(InstallOutcome),
    List(Vec<XdpEntry>),
}

/// Internal flash worker handle (owned by [`crate::client::SerialHandler`]).
pub(crate) struct FlashWorker {
    tx: mpsc::UnboundedSender<FlashReq>,
    join: Option<tokio::task::JoinHandle<()>>,
}

impl FlashWorker {
    pub(crate) fn call(&self, op: FlashOp) -> io::Result<FlashResult> {
        block_on(self.call_async(op))
    }

    async fn call_async(&self, op: FlashOp) -> io::Result<FlashResult> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(FlashReq::Op { op, resp: resp_tx })
            .map_err(|_| std::io::Error::other("flash task closed"))?;
        resp_rx
            .await
            .map_err(|_| std::io::Error::other("flash response dropped"))?
    }

    pub(crate) fn shutdown(mut self) {
        let _ = self.tx.send(FlashReq::Shutdown);
        if let Some(j) = self.join.take() {
            let _ = block_on(j);
        }
    }
}

impl Drop for FlashWorker {
    fn drop(&mut self) {
        let _ = self.tx.send(FlashReq::Shutdown);
        if let Some(j) = self.join.take() {
            j.abort();
        }
    }
}

/// Flash worker config. XDP span is always resolved from [`Self::partitions`].
#[derive(Clone)]
pub struct FlashConfig {
    /// Board `partitions.csv` (source of truth for the XDP flash span).
    pub partitions: PathBuf,
    /// Partition Name column (default [`DEFAULT_XDP_PARTITION`]).
    pub partition: String,
    /// Host cap on a single XDP ELF (also min'd with CSV-derived capacity).
    pub image_capacity: usize,
}

impl FlashConfig {
    /// Build config from a board `partitions.csv` (partition label `"xdp"`).
    pub fn from_partitions_csv(partitions: impl Into<PathBuf>) -> io::Result<Self> {
        Self::from_partitions_csv_named(partitions, DEFAULT_XDP_PARTITION)
    }

    /// Build config from `partitions.csv` + partition label.
    pub fn from_partitions_csv_named(
        partitions: impl Into<PathBuf>,
        partition: impl Into<String>,
    ) -> io::Result<Self> {
        let partitions = partitions.into();
        let partition = partition.into();
        let table = xdp_table_from_csv(&partitions, &partition)?;
        Ok(Self {
            partitions,
            partition,
            image_capacity: table.image_capacity,
        })
    }

    /// Resolve [`XdpTable`] from [`Self::partitions`] / [`Self::partition`].
    pub fn xdp_table(&self) -> io::Result<XdpTable> {
        let mut table = xdp_table_from_csv(&self.partitions, &self.partition)?;
        table.image_capacity = table.image_capacity.min(self.image_capacity);
        Ok(table)
    }
}

/// Spawn flash worker on shared [`Port`] (open-per-op download).
pub(crate) fn spawn(cfg: FlashConfig, port: Arc<Port>, record: SharedRecord) -> FlashWorker {
    let (tx, rx) = mpsc::unbounded_channel();
    let join = tokio::spawn(worker(cfg, port, record, rx));
    FlashWorker {
        tx,
        join: Some(join),
    }
}

async fn worker(
    cfg: FlashConfig,
    port: Arc<Port>,
    record: SharedRecord,
    mut rx: mpsc::UnboundedReceiver<FlashReq>,
) {
    while let Some(req) = rx.recv().await {
        match req {
            FlashReq::Shutdown => break,
            FlashReq::Op { op, resp } => {
                let cfg = cfg.clone();
                let port = Arc::clone(&port);
                let record = Arc::clone(&record);
                let result = tokio::task::spawn_blocking(move || run_op(&port, &cfg, &record, &op))
                    .map(|join| {
                        join.unwrap_or_else(|e| Err(std::io::Error::other(format!("flash join: {e}"))))
                    })
                    .await;
                let _ = resp.send(result);
            }
        }
    }
}

fn run_op(
    port: &Port,
    cfg: &FlashConfig,
    record: &SharedRecord,
    op: &FlashOp,
) -> io::Result<FlashResult> {
    // Abort any App UART `select!` so its session drops and releases the gate;
    // then open download. Do not wait for a serial ack — flash is disruptive.
    port.interrupt().signal();
    {
        let mut rec = record.lock();
        FlashEvent::Begin(op.clone()).write_to(&mut rec);
    }
    record.mark_init_after(SystemTime::now());
    let result = match op {
        FlashOp::Fw {
            firmware,
            partitions,
        } => fw::flash_fw(port, Path::new(firmware), Path::new(partitions))
            .map(|_| FlashResult::Done),
        FlashOp::Xdp { name, path } => {
            xdp::install(port, cfg, name, Path::new(path)).map(FlashResult::Install)
        }
        FlashOp::Clear { names } => xdp::clear(port, cfg, names).map(|_| FlashResult::Done),
        FlashOp::List => xdp::list(port, cfg).map(FlashResult::List),
    };
    {
        let mut rec = record.lock();
        match &result {
            Ok(FlashResult::List(entries)) => {
                for e in entries {
                    rec.flash(&format!(
                        "FLASH list name={} offset={} len={}",
                        e.name, e.offset, e.len
                    ));
                }
                FlashEvent::End(op.clone()).write_to(&mut rec);
            }
            Ok(FlashResult::Install(o)) => {
                rec.flash(&format!("FLASH install {o:?}"));
                FlashEvent::End(op.clone()).write_to(&mut rec);
            }
            Ok(FlashResult::Done) => FlashEvent::End(op.clone()).write_to(&mut rec),
            Err(e) => FlashEvent::Failed {
                op: op.clone(),
                msg: e.to_string(),
            }
            .write_to(&mut rec),
        }
    }
    result
}
