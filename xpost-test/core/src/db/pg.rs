//! Shared Postgres DSN from compile-time `config/default.toml` `[postgres]`.
//! Formal path uses [`crate::db`] for schema ensure / wipe.

use anyhow::{Context, Result};
use log::info;
use tokio_postgres::{Client, NoTls};

use crate::config;

#[derive(Clone, Copy, Debug, Default)]
pub struct PgOpts {
    pub formal: bool,
}

impl PgOpts {
    pub fn schema(self) -> &'static str {
        if self.formal {
            config::postgres().formal_schema.as_str()
        } else {
            "public"
        }
    }

    pub fn dsn(self) -> String {
        config::postgres().dsn()
    }
}

pub async fn connect(opts: PgOpts) -> Result<Client> {
    let pg = config::postgres();
    let dsn = opts.dsn();
    let (client, conn) = tokio_postgres::connect(&dsn, NoTls)
        .await
        .with_context(|| format!("postgres connect db={}", pg.dbname))?;
    tokio::spawn(async move {
        if let Err(e) = conn.await {
            log::warn!("postgres connection closed: {e}");
        }
    });
    info!(
        "postgres connected {}:{} db={} schema={} user={} formal={}",
        pg.host,
        pg.port,
        pg.dbname,
        opts.schema(),
        pg.user,
        opts.formal
    );
    Ok(client)
}
