//! Ad-hoc / flame debug (`run = ["dev"]`).

use anyhow::Result;

use crate::config::FileConfig;
use crate::pipeline::Harness;

use super::just_tx::{JustTxOpts, run_just_tx};

/// Hard-coded flame Trace capture (no Postgres).
pub async fn run_dev(cfg: &FileConfig, harness: &mut Harness) -> Result<()> {
    run_just_tx(
        cfg,
        harness,
        JustTxOpts {
            is_dev: true,
            formal: false,
        },
    )
    .await
}
