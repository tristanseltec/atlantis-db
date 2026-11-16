//! The datastore: storage, transactions, Cypher execution and indexes.

use std::collections::HashMap;
use std::fs::{self, File, TryLockError};
use std::ops::{Deref, DerefMut};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use atlantis_core::{AtlantisError, DatabaseRef, EngineConfig, SyncMode};
use atlantis_cypher::ast::Statement;
use atlantis_kv::{Isolation, MemStore, TxMode};
use atlantis_storage::{CommitInfo, RecordStore, RecordTx};

use crate::exec::{Exec, Params, QueryResult};
use crate::vector_index::VectorIndexes;

/// Lock file that keeps a database directory open in one process at a time.
pub const LOCK_FILE: &str = "LOCK";
/// File name of the legacy graph-store checkpoint, which this version
/// cannot read.
const LEGACY_CHECKPOINT: &str = "checkpoint.db";

struct Inner {
    kv: MemStore,
    records: RecordStore,
    vectors: VectorIndexes,
    config: EngineConfig,
    dir: Option<PathBuf>,
    _lock: Option<File>,
    stop: Arc<AtomicBool>,
    checkpointer: std::sync::Mutex<Option<JoinHandle<()>>>,
}

impl Drop for Inner {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(handle) = self
            .checkpointer
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
        {
            // The thread only holds a weak reference, so it exits promptly.
            if handle.thread().id() != std::thread::current().id() {
                let _ = handle.join();
            }
        }
        // Fold the log into the checkpoint on a clean close (best effort;
        // the log alone already makes every commit recoverable).
        let _ = self.kv.checkpoint_durable();
    }
}

/// A handle to an open database. Cheap to clone; clones share everything.
#[derive(Clone)]
pub struct Datastore {
    inner: Arc<Inner>,
}

fn kv_sync(sync: SyncMode) -> atlantis_kv::SyncMode {
    match sync {
        SyncMode::Every => atlantis_kv::SyncMode::Every,
        SyncMode::Interval(d) => atlantis_kv::SyncMode::Interval(d),
        SyncMode::Never => atlantis_kv::SyncMode::Never,
    }
}

fn lock_dir(dir: &Path) -> Result<File, AtlantisError> {
    fs::create_dir_all(dir)?;
    let lock = File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join(LOCK_FILE))?;
    match lock.try_lock() {
        Ok(()) => Ok(lock),
        Err(TryLockError::WouldBlock) => Err(AtlantisError::Storage(format!(
            "database directory {} is already open in another engine",
            dir.display()
        ))),
        Err(TryLockError::Error(e)) => Err(e.into()),
    }
}

impl Datastore {
    /// Opens the database described by `config`: in memory when it has no
    /// `disk_path`, otherwise durable in that directory.
    pub fn open(config: EngineConfig) -> Result<Self, AtlantisError> {
        config.validate()?;
        let (kv, lock) = match &config.storage.disk_path {
            Some(dir) => {
                let lock = lock_dir(dir)?;
                let has_new = dir.join("records.kv").exists() || dir.join("records.wal").exists();
                if dir.join(LEGACY_CHECKPOINT).exists() && !has_new {
                    return Err(AtlantisError::Storage(format!(
                        "{} holds data in the legacy graph-store format ({LEGACY_CHECKPOINT}), which this version cannot read; \
                         move it aside to start a new database there",
                        dir.display()
                    )));
                }
                (
                    MemStore::open_durable(dir, kv_sync(config.storage.sync))?,
                    Some(lock),
                )
            }
            None => (MemStore::new(), None),
        };
        let inner = Arc::new(Inner {
            records: RecordStore::new(Arc::new(kv.clone())),
            vectors: VectorIndexes::new(
                config.vector.hnsw.clone(),
                config.vector.effective_build_threads(),
            ),
            dir: config.storage.disk_path.clone(),
            kv,
            _lock: lock,
            stop: Arc::new(AtomicBool::new(false)),
            checkpointer: std::sync::Mutex::new(None),
            config,
        });
        if inner.dir.is_some() && inner.config.checkpoint_interval_secs > 0 {
            let handle = spawn_checkpointer(
                Arc::downgrade(&inner),
                inner.stop.clone(),
                Duration::from_secs(inner.config.checkpoint_interval_secs),
            );
            *inner.checkpointer.lock().unwrap_or_else(|e| e.into_inner()) = Some(handle);
        }
        Ok(Datastore { inner })
    }

    /// The database directory, if the datastore is on disk.
    pub fn dir(&self) -> Option<&Path> {
        self.inner.dir.as_deref()
    }

    /// The isolation of read-write transactions that do not choose one
    /// (`transaction.isolation`).
    pub fn default_isolation(&self) -> Isolation {
        self.inner.config.transaction.isolation
    }

    /// Starts a transaction in database `db`.
    pub fn begin(
        &self,
        db: DatabaseRef,
        mode: TxMode,
        isolation: Isolation,
    ) -> Result<Transaction, AtlantisError> {
        Ok(Transaction {
            rtx: self.inner.records.begin(db, mode, isolation)?,
            store: self.clone(),
            index_changes: Vec::new(),
            failed: false,
        })
    }

    /// Runs a Cypher script in database `db` and returns the result of its
    /// last statement.
    ///
    /// Statements outside `BEGIN … COMMIT` each run in their own
    /// transaction. Statements between `BEGIN` and `COMMIT` run in one
    /// transaction; `ROLLBACK` discards it, and a script ending inside an
    /// open block is rolled back with an error.
    pub fn execute(
        &self,
        db: &DatabaseRef,
        script: &str,
        params: &Params,
    ) -> Result<QueryResult, AtlantisError> {
        let statements = atlantis_cypher::parse_script(script)?;
        let mut last = QueryResult::default();
        let mut open: Option<Transaction> = None;
        for statement in &statements {
            match statement {
                Statement::Begin => {
                    if open.is_some() {
                        return Err(AtlantisError::InvalidQuery(
                            "BEGIN inside an open transaction".into(),
                        ));
                    }
                    open = Some(self.begin(
                        db.clone(),
                        TxMode::ReadWrite,
                        self.default_isolation(),
                    )?);
                }
                Statement::Commit => {
                    let tx = open.take().ok_or_else(|| {
                        AtlantisError::InvalidQuery("COMMIT without BEGIN".into())
                    })?;
                    tx.commit()?;
                }
                Statement::Rollback => {
                    open.take().ok_or_else(|| {
                        AtlantisError::InvalidQuery("ROLLBACK without BEGIN".into())
                    })?;
                }
                other => {
                    last = match open.as_mut() {
                        Some(tx) => tx.run(other, params)?,
                        None => {
                            let mut tx = self.begin(
                                db.clone(),
                                TxMode::ReadWrite,
                                self.default_isolation(),
                            )?;
                            let result = tx.run(other, params)?;
                            tx.commit()?;
                            result
                        }
                    };
                }
            }
        }
        if open.is_some() {
            return Err(AtlantisError::InvalidQuery(
                "script ended inside BEGIN without COMMIT; rolled back".into(),
            ));
        }
        Ok(last)
    }

    /// Folds the write-ahead log into the checkpoint file. Commits are
    /// already durable without it; checkpoints bound the log size and the
    /// recovery time. A no-op for in-memory datastores.
    pub fn checkpoint(&self) -> Result<(), AtlantisError> {
        self.inner.kv.checkpoint_durable()
    }

    /// Bytes in the write-ahead log since the last checkpoint.
    pub fn wal_size(&self) -> u64 {
        self.inner.kv.wal_size()
    }
}

fn spawn_checkpointer(
    store: Weak<Inner>,
    stop: Arc<AtomicBool>,
    every: Duration,
) -> JoinHandle<()> {
    std::thread::Builder::new()
        .name("atlantis-checkpoint".into())
        .spawn(move || {
            let mut last = Instant::now();
            while !stop.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(50).min(every));
                if last.elapsed() < every {
                    continue;
                }
                last = Instant::now();
                // Hold the datastore only long enough to clone the store
                // handle, so dropping the last engine never waits on (or
                // runs its shutdown on) this thread.
                let Some(kv) = store.upgrade().map(|inner| inner.kv.clone()) else {
                    return;
                };
                // Errors surface on the next explicit checkpoint or commit.
                let _ = kv.checkpoint_durable();
            }
        })
        .expect("spawn checkpoint thread")
}

/// A transaction: the record API (through `Deref` to [`RecordTx`]) plus
/// Cypher execution. Dropping it without committing discards its writes.
pub struct Transaction {
    rtx: RecordTx,
    store: Datastore,
    index_changes: Vec<String>,
    /// Set when a Cypher statement failed part-way; such a transaction can
    /// only be rolled back, because the statement's partial writes are in it.
    failed: bool,
}

impl Transaction {
    /// Runs one parsed statement inside this transaction.
    fn run(
        &mut self,
        statement: &Statement,
        params: &Params,
    ) -> Result<QueryResult, AtlantisError> {
        let inner = &self.store.inner;
        let mut exec = Exec {
            tx: &mut self.rtx,
            params,
            store: &inner.records,
            vectors: &inner.vectors,
            vector_defaults: &inner.config.vector,
            index_changes: Vec::new(),
            regexes: HashMap::new(),
        };
        let result = exec.run(statement);
        self.index_changes.append(&mut exec.index_changes);
        if result.is_err() {
            self.failed = true;
        }
        result
    }

    /// Runs a single Cypher statement (or a `;`-separated script without
    /// transaction control) inside this transaction.
    pub fn execute(&mut self, cypher: &str, params: &Params) -> Result<QueryResult, AtlantisError> {
        let mut last = QueryResult::default();
        for statement in atlantis_cypher::parse_script(cypher)? {
            if matches!(
                statement,
                Statement::Begin | Statement::Commit | Statement::Rollback
            ) {
                return Err(AtlantisError::InvalidQuery(
                    "BEGIN/COMMIT/ROLLBACK cannot be used inside a transaction".into(),
                ));
            }
            last = self.run(&statement, params)?;
        }
        Ok(last)
    }

    /// Commits, then brings derived vector indexes up to date.
    pub fn commit(self) -> Result<CommitInfo, AtlantisError> {
        if self.failed {
            return Err(AtlantisError::InvalidQuery(
                "a statement in this transaction failed; it was rolled back".into(),
            ));
        }
        let db = self.rtx.database().clone();
        let Transaction {
            rtx,
            store,
            index_changes,
            ..
        } = self;
        let info = rtx.commit()?;
        let inner = &store.inner;
        for name in &index_changes {
            inner.vectors.invalidate(&db, name);
        }
        // The data is committed; a failure to update derived indexes must not
        // be reported as a failed commit. `apply` drops the affected indexes
        // on error so the next search rebuilds them from records.
        let _ = inner.vectors.apply(&inner.records, &db, &info.changed);
        Ok(info)
    }
}

impl Deref for Transaction {
    type Target = RecordTx;

    fn deref(&self) -> &RecordTx {
        &self.rtx
    }
}

impl DerefMut for Transaction {
    fn deref_mut(&mut self) -> &mut RecordTx {
        &mut self.rtx
    }
}
