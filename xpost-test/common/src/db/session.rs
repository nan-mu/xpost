//! Keep schema protection across preparation, collection, and reconnection.

use std::future::Future;

use anyhow::{Result, bail, ensure};
use tokio::sync::watch;
use tokio_postgres::Client;
use uuid::Uuid;

use super::{Database, protocol_v1};

// This two-integer advisory key is separate from the campaign bigint keys.
const SCHEMA_LOCK: i32 = 0x5850_4f53;

pub struct Collection<'a> {
    database: &'a Database,
    guard: Client,
    closed: watch::Receiver<bool>,
}

enum Preparation {
    Reconcile,
    AppendOnly,
}

impl Database {
    /// Reconcile before new collection and keep the schema protected.
    pub async fn prepare(&self) -> Result<Collection<'_>> {
        self.prepare_with(Preparation::Reconcile).await
    }

    /// Initialize an empty schema or validate an existing one without DDL.
    pub async fn prepare_append_only(&self) -> Result<Collection<'_>> {
        self.prepare_with(Preparation::AppendOnly).await
    }

    async fn prepare_with(&self, preparation: Preparation) -> Result<Collection<'_>> {
        let (mut guard, closed) = self.open().await?;
        ensure!(
            guard
                .query_one("SELECT pg_try_advisory_lock($1, 1)", &[&SCHEMA_LOCK])
                .await?
                .get::<_, bool>(0),
            "Protocol V1 schema is in use by another runner"
        );
        match preparation {
            Preparation::Reconcile => protocol_v1::reconcile(&mut guard).await?,
            Preparation::AppendOnly => protocol_v1::prepare_append_only(&mut guard).await?,
        }
        // This session still owns the exclusive lock while it takes the shared
        // lock. A competing preparer cannot slip in between these statements.
        lock_shared(&guard).await?;
        ensure!(
            guard
                .query_one("SELECT pg_advisory_unlock($1, 1)", &[&SCHEMA_LOCK])
                .await?
                .get::<_, bool>(0),
            "lost exclusive Protocol V1 schema lock"
        );
        Ok(Collection {
            database: self,
            guard,
            closed,
        })
    }

    /// Validate an existing schema under a shared lock. This does no DDL.
    pub async fn resume(&self) -> Result<Collection<'_>> {
        let (mut guard, closed) = self.open().await?;
        lock_shared(&guard).await?;
        protocol_v1::check(&mut guard).await?;
        Ok(Collection {
            database: self,
            guard,
            closed,
        })
    }
}

async fn lock_shared(client: &Client) -> Result<()> {
    ensure!(
        client
            .query_one("SELECT pg_try_advisory_lock_shared($1, 1)", &[&SCHEMA_LOCK])
            .await?
            .get::<_, bool>(0),
        "Protocol V1 schema preparation is in progress"
    );
    Ok(())
}

impl Collection<'_> {
    pub fn client(&self) -> &Client {
        &self.guard
    }

    pub async fn lock_campaign(&self, campaign: Uuid) -> Result<()> {
        let key = i64::from_be_bytes(campaign.as_bytes()[..8].try_into()?);
        ensure!(
            self.guard
                .query_one("SELECT pg_try_advisory_lock($1)", &[&key])
                .await?
                .get::<_, bool>(0),
            "campaign {campaign} already has an active runner"
        );
        Ok(())
    }

    /// Each writer independently holds a shared lock. A write already in flight
    /// remains protected even if the collection's guard connection is lost.
    pub async fn connect(&self) -> Result<Client> {
        ensure!(
            !self.guard.is_closed(),
            "Protocol V1 schema guard disconnected"
        );
        let client = self.database.connect().await?;
        lock_shared(&client).await?;
        ensure!(
            !self.guard.is_closed(),
            "Protocol V1 schema guard disconnected"
        );
        Ok(client)
    }

    /// End acquisition and write recovery when the guard connection closes.
    pub async fn protect<T>(&self, work: impl Future<Output = Result<T>>) -> Result<T> {
        let mut closed = self.closed.clone();
        tokio::select! {
            biased;
            _ = closed.wait_for(|closed| *closed) => bail!("Protocol V1 schema guard disconnected"),
            result = work => result,
        }
    }
}
