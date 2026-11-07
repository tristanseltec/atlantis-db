//! The public `Engine` type — the main entry point for Atlantis DB.

use std::ops::{Deref, DerefMut};
use std::path::Path;

use atlantis_core::{AtlantisError, DatabaseRef, EngineConfig, Value};
use atlantis_engine::Datastore;
use atlantis_kv::{Isolation, TxMode};
use atlantis_storage::{CommitInfo, RecordTx};

use crate::params::Params;
use crate::result_set::ResultSet;

/// The main entry point for Atlantis DB.
///
/// Data lives in namespaced tables of records: documents, graph nodes and
/// edges. Cypher queries ([`execute`](Self::execute)) and the Rust record
/// API ([`transaction`](Self::transaction), [`read`](Self::read)) read and
/// write the same data. `Engine` is cheaply clonable and safe to share
/// across async tasks; clones share the database but each clone has its own
/// namespace/database selection ([`use_db`](Self::use_db)).
///
/// # Durability
///
/// An engine opened on a directory writes every commit to a write-ahead log
/// (`records.wal`) and fsyncs it before the commit returns (configurable
/// with [`SyncMode`](crate::SyncMode)). Checkpoints fold the log into
/// `records.kv`; they run periodically, on [`checkpoint`](Self::checkpoint),
/// and when the last clone is dropped. A directory can be open in only one
/// engine at a time.
///
/// # Example
///
/// ```no_run
/// use atlantis_db::{Engine, Value};
///
/// # async fn example() -> Result<(), atlantis_db::Error> {
/// let engine = Engine::open_in_memory().await?;
/// engine.execute(
///     "CREATE (:Person {id: 'alice', name: 'Alice'})-[:KNOWS]->(:Person {id: 'bob', name: 'Bob'})",
///     &[],
/// ).await?;
/// let friends = engine.execute(
///     "MATCH (a:Person {id: $id})-[:KNOWS]->(b) RETURN b.name AS friend",
///     &[("id", Value::from("alice"))],
/// ).await?;
/// assert_eq!(friends.row_count(), 1);
/// # Ok(())
/// # }
/// ```
#[derive(Clone)]
pub struct Engine {
    store: Datastore,
    db: DatabaseRef,
}

/// A transaction handed to [`Engine::transaction`] closures: the record API
/// (through `Deref` to [`RecordTx`]) plus Cypher via
/// [`execute`](Transaction::execute).
pub struct Transaction {
    inner: atlantis_engine::Transaction,
}

impl Transaction {
    /// Runs Cypher inside this transaction; it sees the transaction's own
    /// writes. `BEGIN`/`COMMIT` are not allowed here.
    pub fn execute(
        &mut self,
        cypher: &str,
        params: &[(&str, Value)],
    ) -> Result<ResultSet, AtlantisError> {
        let params = Params::from(params).into_map();
        let result = self.inner.execute(cypher, &params)?;
        Ok(ResultSet::new(result.columns, result.rows))
    }
}

impl Deref for Transaction {
    type Target = RecordTx;

    fn deref(&self) -> &RecordTx {
        &self.inner
    }
}

impl DerefMut for Transaction {
    fn deref_mut(&mut self) -> &mut RecordTx {
        &mut self.inner
    }
}

impl Engine {
    /// Opens or creates a database in the given directory.
    pub async fn open(path: impl AsRef<Path>) -> Result<Self, AtlantisError> {
        let config = EngineConfig::builder()
            .disk_path(path.as_ref().to_path_buf())
            .build()?;
        Self::open_with_config(config).await
    }

    /// Opens or creates a database with a custom configuration.
    pub async fn open_with_config(config: EngineConfig) -> Result<Self, AtlantisError> {
        Ok(Engine {
            store: Datastore::open(config)?,
            db: DatabaseRef::default(),
        })
    }

    /// Opens an in-memory database (nothing is written to disk).
    pub async fn open_in_memory() -> Result<Self, AtlantisError> {
        Self::open_with_config(EngineConfig::default()).await
    }

    /// The namespace and database this handle works in.
    pub fn database(&self) -> &DatabaseRef {
        &self.db
    }

    /// Selects the namespace and database for this handle. Other clones
    /// keep their own selection.
    pub fn use_db(&mut self, ns: &str, db: &str) -> Result<(), AtlantisError> {
        self.db = DatabaseRef::new(ns, db)?;
        Ok(())
    }

    fn run<T, F>(&self, mode: TxMode, isolation: Isolation, f: F) -> Result<T, AtlantisError>
    where
        F: FnOnce(&mut Transaction) -> Result<T, AtlantisError>,
    {
        let mut tx = Transaction {
            inner: self.store.begin(self.db.clone(), mode, isolation)?,
        };
        let value = f(&mut tx)?; // on error, dropping `tx` discards its writes
        let _: CommitInfo = tx.inner.commit()?;
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
        F: FnOnce(&mut Transaction) -> Result<T, AtlantisError>,
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
        F: FnOnce(&mut Transaction) -> Result<T, AtlantisError>,
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
        F: FnMut(&mut Transaction) -> Result<T, AtlantisError>,
    {
        retry(max_attempts, || {
            self.run(TxMode::ReadWrite, Isolation::Snapshot, &mut f)
        })
    }

    /// Runs `f` in a read-only transaction on a consistent snapshot.
    pub async fn read<T, F>(&self, f: F) -> Result<T, AtlantisError>
    where
        F: FnOnce(&mut Transaction) -> Result<T, AtlantisError>,
    {
        self.run(TxMode::ReadOnly, Isolation::Snapshot, f)
    }

    /// Executes a Cypher script with parameters and returns the result of
    /// its last statement.
    ///
    /// Each statement runs in its own transaction unless the script groups
    /// statements with `BEGIN; …; COMMIT;`.
    pub async fn execute(
        &self,
        cypher: &str,
        params: &[(&str, Value)],
    ) -> Result<ResultSet, AtlantisError> {
        let params = Params::from(params).into_map();
        let result = self.store.execute(&self.db, cypher, &params)?;
        Ok(ResultSet::new(result.columns, result.rows))
    }

    /// Folds the write-ahead log into the checkpoint file. Commits are
    /// durable without this; checkpoints bound log size and recovery time.
    pub async fn checkpoint(&self) -> Result<(), AtlantisError> {
        self.store.checkpoint()
    }

    /// Checkpoints. The database closes when the last clone is dropped.
    pub async fn shutdown(&self) -> Result<(), AtlantisError> {
        self.store.checkpoint()
    }
}

fn retry<T>(
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

    #[test]
    fn retry_gives_up_after_max_attempts() {
        let attempts = Cell::new(0);
        let err = retry(2, || {
            attempts.set(attempts.get() + 1);
            Err::<(), _>(AtlantisError::TxConflict("always".into()))
        })
        .unwrap_err();
        assert!(err.is_retryable());
        assert_eq!(attempts.get(), 2);
    }
}
