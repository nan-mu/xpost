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

use futures::FutureExt;
use tokio::sync::{mpsc, oneshot};

use crate::client::error::{Error, Result};
use crate::client::port::Port;
use crate::client::serial::{InterruptTx, Log};
use crate::r#type::ops::{Flash, FlashOp};

/// Default partition label for the XDP flash span (`partitions.csv` Name column).
pub const DEFAULT_XDP_PARTITION: &str = "xdp";

enum FlashReq {
    Op {
        op: FlashOp,
        resp: oneshot::Sender<Result<FlashResult>>,
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

/// Handle for flash operations (dedicated tokio task; exclusive with App UART via [`Port`] gate).
pub struct FlashHandle {
    tx: mpsc::UnboundedSender<FlashReq>,
    join: Option<tokio::task::JoinHandle<()>>,
}

impl FlashHandle {
    pub async fn flash_fw(&self, firmware: &Path, partitions: &Path) -> Result<()> {
        match self
            .call(FlashOp::Fw {
                firmware: firmware.display().to_string(),
                partitions: partitions.display().to_string(),
            })
            .await?
        {
            FlashResult::Done => Ok(()),
            other => Err(Error::Msg(format!("unexpected flash result: {other:?}"))),
        }
    }

    pub async fn flash_xdp(&self, name: &str, path: &Path) -> Result<InstallOutcome> {
        match self
            .call(FlashOp::Xdp {
                name: name.to_string(),
                path: path.display().to_string(),
            })
            .await?
        {
            FlashResult::Install(o) => Ok(o),
            other => Err(Error::Msg(format!("unexpected flash result: {other:?}"))),
        }
    }

    /// `names` empty ⇒ clear entire XDP table (`*`).
    pub async fn clear_xdp(&self, names: &[String]) -> Result<()> {
        match self
            .call(FlashOp::Clear {
                names: names.to_vec(),
            })
            .await?
        {
            FlashResult::Done => Ok(()),
            other => Err(Error::Msg(format!("unexpected flash result: {other:?}"))),
        }
    }

    pub async fn list_xdp(&self) -> Result<Vec<XdpEntry>> {
        match self.call(FlashOp::List).await? {
            FlashResult::List(v) => Ok(v),
            other => Err(Error::Msg(format!("unexpected flash result: {other:?}"))),
        }
    }

    async fn call(&self, op: FlashOp) -> Result<FlashResult> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(FlashReq::Op { op, resp: resp_tx })
            .map_err(|_| Error::Msg("flash task closed".into()))?;
        resp_rx
            .await
            .map_err(|_| Error::Msg("flash response dropped".into()))?
    }

    pub async fn shutdown(mut self) {
        let _ = self.tx.send(FlashReq::Shutdown);
        if let Some(j) = self.join.take() {
            let _ = j.await;
        }
    }
}

impl Drop for FlashHandle {
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
    pub fn from_partitions_csv(partitions: impl Into<PathBuf>) -> Result<Self> {
        Self::from_partitions_csv_named(partitions, DEFAULT_XDP_PARTITION)
    }

    /// Build config from `partitions.csv` + partition label.
    pub fn from_partitions_csv_named(
        partitions: impl Into<PathBuf>,
        partition: impl Into<String>,
    ) -> Result<Self> {
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
    pub fn xdp_table(&self) -> Result<XdpTable> {
        let mut table = xdp_table_from_csv(&self.partitions, &self.partition)?;
        table.image_capacity = table.image_capacity.min(self.image_capacity);
        Ok(table)
    }
}

/// Spawn flash worker on shared [`Port`] (open-per-op download).
pub fn spawn(
    cfg: FlashConfig,
    port: Arc<Port>,
    log: Arc<Log>,
    interrupt: InterruptTx,
) -> FlashHandle {
    let (tx, rx) = mpsc::unbounded_channel();
    let join = tokio::spawn(worker(cfg, port, log, interrupt, rx));
    FlashHandle {
        tx,
        join: Some(join),
    }
}

async fn worker(
    cfg: FlashConfig,
    port: Arc<Port>,
    log: Arc<Log>,
    interrupt: InterruptTx,
    mut rx: mpsc::UnboundedReceiver<FlashReq>,
) {
    while let Some(req) = rx.recv().await {
        match req {
            FlashReq::Shutdown => break,
            FlashReq::Op { op, resp } => {
                let cfg = cfg.clone();
                let port = Arc::clone(&port);
                let log = Arc::clone(&log);
                let interrupt = interrupt.clone();
                let result =
                    tokio::task::spawn_blocking(move || run_op(&port, &cfg, &log, &interrupt, &op))
                        .map(|join| {
                            join.unwrap_or_else(|e| Err(Error::Msg(format!("flash join: {e}"))))
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
    log: &Log,
    interrupt: &InterruptTx,
    op: &FlashOp,
) -> Result<FlashResult> {
    // Kick App UART select (if any) so it can drop and release the gate; then
    // take download. Do not wait for a serial ack — flash is disruptive.
    let _ = interrupt.try_send(());
    log.write_line(&Flash::Begin(op.clone()).to_string());
    let result = match op {
        FlashOp::Fw {
            firmware,
            partitions,
        } => fw::flash_fw(port, Path::new(firmware), Path::new(partitions)).map(|_| FlashResult::Done),
        FlashOp::Xdp { name, path } => {
            xdp::install(port, cfg, name, Path::new(path)).map(FlashResult::Install)
        }
        FlashOp::Clear { names } => xdp::clear(port, cfg, names).map(|_| FlashResult::Done),
        FlashOp::List => xdp::list(port, cfg).map(FlashResult::List),
    };
    match &result {
        Ok(_) => log.write_line(&Flash::End(op.clone()).to_string()),
        Err(e) => log.write_line(
            &Flash::Failed {
                op: op.clone(),
                msg: e.to_string(),
            }
            .to_string(),
        ),
    }
    result
}
