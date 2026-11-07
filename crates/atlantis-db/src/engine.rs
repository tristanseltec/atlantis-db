//! The public `Engine` type — the main entry point for Atlantis DB.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use atlantis_core::{AtlantisError, DatabaseRef, EngineConfig, Value};
use atlantis_engine::{spawn_engine, EngineHandle};
use atlantis_kv::{Isolation, MemStore, TxMode};
use atlantis_storage::{RecordStore, RecordTx};

use crate::params::Params;
use crate::result_set::ResultSet;

/// File, inside the database directory, holding the record store checkpoint.
pub const RECORDS_FILE: &str = "records.kv";

/// The main entry point for Atlantis DB.
///
/// An `Engine` gives access to two stores while the multi-model migration
/// is in progress:
///
/// - the **record store** — namespaced tables of documents, graph nodes and
///   edges with ACID transactions ([`transaction`](Self::transaction),
///   [`read`](Self::read));
/// - the **Cypher graph store** — the original property graph queried with
///   [`execute`](Self::execute).
///
/// The two stores do not share data yet: Cypher queries do not see records
/// and vice versa. `Engine` is cheaply clonable and safe to share across
/// async tasks; clones share both stores.
///
/// # Example
///
/// ```no_run
/// use atlantis_db::{Direction, Engine, Object, Value};
///
/// # async fn example() -> Result<(), atlantis_db::Error> {
/// let engine = Engine::open_in_memory().await?;
/// engine.transaction(|tx| {
///     let mut alice = Object::new();
///     alice.insert("id".into(), Value::from("alice"));
///     let alice = tx.create_node(&["Person"], alice)?;
///     let bob = tx.create_node(&["Person"], Object::new())?;
///     tx.relate(&alice.id, "KNOWS", &bob.id, Object::new())?;
///     Ok(())
/// }).await?;
///
/// let friends = engine.read(|tx| {
///     tx.neighbors(&"Person:alice".parse()?, Direction::Outgoing, Some("KNOWS"))
/// }).await?;
/// assert_eq!(friends.len(), 1);
/// # Ok(())
/// # }
/// ```
#[derive(Clone)]
pub struct Engine {
    handle: EngineHandle,
    kv: MemStore,
    records: RecordStore,
    records_path: Option<PathBuf>,
    db: DatabaseRef,
}

impl Engine {
    /// Opens or creates a database at the given directory.
    ///
    /// If checkpoints exist in the directory, both stores are restored.
    /// Otherwise, a new empty database is created.
    pub async fn open(path: impl AsRef<Path>) -> Result<Self, AtlantisError> {
        let config = EngineConfig::builder()
            .disk_path(path.as_ref().to_path_buf())
            .build()?;
        Self::open_with_config(config).await
    }

    /// Opens or creates a database with a custom configuration.
    pub async fn open_with_config(config: EngineConfig) -> Result<Self, AtlantisError> {
        let records_path = config
            .storage
            .disk_path
            .as_ref()
            .map(|dir| dir.join(RECORDS_FILE));
        let kv = match &records_path {
            Some(path) => MemStore::open_or_create(path)?,
            None => MemStore::new(),
        };
        let handle = spawn_engine(config)?;
        Ok(Engine {
            handle,
            records: RecordStore::new(Arc::new(kv.clone())),
            kv,
            records_path,
            db: DatabaseRef::default(),
        })
    }

    /// Opens an in-memory database (no disk persistence).
    pub async fn open_in_memory() -> Result<Self, AtlantisError> {
        let config = EngineConfig::default();
        Self::open_with_config(config).await
    }

    /// The namespace and database used by record transactions.
    pub fn database(&self) -> &DatabaseRef {
        &self.db
    }

    /// Selects the namespace and database for record transactions on this
    /// handle. Other clones keep their own selection.
    pub fn use_db(&mut self, ns: &str, db: &str) -> Result<(), AtlantisError> {
        self.db = DatabaseRef::new(ns, db)?;
        Ok(())
    }

    fn run<T, F>(&self, mode: TxMode, isolation: Isolation, f: F) -> Result<T, AtlantisError>
    where
        F: FnOnce(&mut RecordTx) -> Result<T, AtlantisError>,
    {
        let mut tx = self.records.begin(self.db.clone(), mode, isolation)?;
        let value = f(&mut tx)?; // on error, dropping `tx` discards its writes
        tx.commit()?;
        Ok(value)
    }

    /// Runs `f` in a read-write transaction with snapshot isolation.
    ///
    /// If `f` returns `Ok`, all its writes commit atomically; if it returns
    /// `Err`, nothing is written. A commit can fail with
    /// [`AtlantisError::TxConflict`] when a concurrent transaction wrote the
    /// same keys first; this method does not retry — use
    /// [`transaction_with_retry`](Self::transaction_with_retry) for that.
    pub async fn transaction<T, F>(&self, f: F) -> Result<T, AtlantisError>
    where
        F: FnOnce(&mut RecordTx) -> Result<T, AtlantisError>,
    {
        self.run(TxMode::ReadWrite, Isolation::Snapshot, f)
    }

    /// Like [`transaction`](Self::transaction) with an explicit isolation
    /// level. [`Isolation::Serializable`] also fails the commit if anything
    /// the transaction read changed concurrently.
    pub async fn transaction_with_isolation<T, F>(
        &self,
        isolation: Isolation,
        f: F,
    ) -> Result<T, AtlantisError>
    where
        F: FnOnce(&mut RecordTx) -> Result<T, AtlantisError>,
    {
        self.run(TxMode::ReadWrite, isolation, f)
    }

    /// Runs `f` in a read-write transaction, re-running it from the start
    /// when the commit fails with a retryable conflict, up to
    /// `max_attempts` attempts in total.
    pub async fn transaction_with_retry<T, F>(
        &self,
        max_attempts: u32,
        mut f: F,
    ) -> Result<T, AtlantisError>
    where
        F: FnMut(&mut RecordTx) -> Result<T, AtlantisError>,
    {
        self.retry(max_attempts, || {
            self.run(TxMode::ReadWrite, Isolation::Snapshot, &mut f)
        })
    }

    fn retry<T>(
        &self,
        max_attempts: u32,
        mut attempt: impl FnMut() -> Result<T, AtlantisError>,
    ) -> Result<T, AtlantisError> {
        let mut tries = 0;
        loop {
            tries += 1;
            match attempt() {
                Err(e) if e.is_retryable() && tries < max_attempts.max(1) => continue,
                other => return other,
            }
        }
    }

    /// Runs `f` in a read-only transaction on a consistent snapshot.
    pub async fn read<T, F>(&self, f: F) -> Result<T, AtlantisError>
    where
        F: FnOnce(&mut RecordTx) -> Result<T, AtlantisError>,
    {
        self.run(TxMode::ReadOnly, Isolation::Snapshot, f)
    }

    /// Executes a Cypher query with parameters against the graph store.
    ///
    /// The `params` slice contains `(name, value)` pairs that are
    /// substituted for `$name` references in the query.
    pub async fn execute(
        &self,
        cypher: &str,
        params: &[(&str, Value)],
    ) -> Result<ResultSet, AtlantisError> {
        let params = Params::from(params).into_map();
        let result = self.handle.query(cypher, params).await?;
        Ok(ResultSet::new(result.columns, result.rows))
    }

    /// Writes both stores to disk (no-op for the record store when the
    /// engine has no disk path).
    pub async fn checkpoint(&self) -> Result<(), AtlantisError> {
        self.checkpoint_records()?;
        self.handle.checkpoint().await
    }

    fn checkpoint_records(&self) -> Result<(), AtlantisError> {
        match &self.records_path {
            Some(path) => self.kv.checkpoint(path),
            None => Ok(()),
        }
    }

    /// Shuts down the engine, flushing both stores to disk.
    pub async fn shutdown(&self) -> Result<(), AtlantisError> {
        self.checkpoint_records()?;
        self.handle.shutdown().await
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use atlantis_core::{Object, RecordKey};

    use super::*;

    #[tokio::test]
    async fn retry_reruns_the_closure_after_a_conflict() {
        let engine = Engine::open_in_memory().await.unwrap();
        let attempts = Cell::new(0);
        let result = engine
            .transaction_with_retry(3, |tx| {
                attempts.set(attempts.get() + 1);
                tx.get(&"Counter:1".parse()?)?; // read before the competing write
                if attempts.get() == 1 {
                    // A competing transaction commits the same record first.
                    engine.run(TxMode::ReadWrite, Isolation::Snapshot, |other| {
                        other.create(
                            "Counter",
                            Some(RecordKey::Integer(1)),
                            vec![],
                            Object::new(),
                        )
                    })?;
                    tx.create(
                        "Counter",
                        Some(RecordKey::Integer(1)),
                        vec![],
                        Object::new(),
                    )?;
                    return Ok("first");
                }
                tx.merge(&"Counter:1".parse()?, Object::new())?;
                Ok("second")
            })
            .await
            .unwrap();
        assert_eq!(result, "second");
        assert_eq!(attempts.get(), 2);
    }

    #[tokio::test]
    async fn retry_gives_up_after_max_attempts() {
        let engine = Engine::open_in_memory().await.unwrap();
        let attempts = Cell::new(0);
        let err = engine
            .retry(2, || {
                attempts.set(attempts.get() + 1);
                Err::<(), _>(AtlantisError::TxConflict("always".into()))
            })
            .unwrap_err();
        assert!(err.is_retryable());
        assert_eq!(attempts.get(), 2);
    }
}
