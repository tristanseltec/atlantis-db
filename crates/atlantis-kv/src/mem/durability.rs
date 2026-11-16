//! Group commit: tracking how far the write-ahead log is appended and
//! synced, leader/follower syncing, and the interval sync thread.

use std::sync::{Condvar, Mutex, MutexGuard, Weak};

use atlantis_core::AtlantisError;

use crate::wal::SyncHandle;

/// Group-commit state: how far the log is appended and synced.
#[derive(Default)]
pub(super) struct SyncState {
    /// Version of the latest record appended to the log.
    pub(super) appended: u64,
    /// Every record up to this version is synced.
    pub(super) synced: u64,
    /// Number of syncs performed (one per group of commits).
    pub(super) syncs: u64,
    /// A leader is syncing right now.
    syncing: bool,
    /// Set once a sync failed; the log is then unusable.
    failed: Option<String>,
}

#[derive(Default)]
pub(super) struct Durability {
    state: Mutex<SyncState>,
    synced: Condvar,
    /// The log file to sync (`None` for stores without a log).
    pub(super) handle: Mutex<Option<SyncHandle>>,
}

impl Durability {
    pub(super) fn lock(&self) -> MutexGuard<'_, SyncState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub(super) fn note_appended(&self, version: u64) {
        let mut st = self.lock();
        st.appended = st.appended.max(version);
    }

    pub(super) fn synced(&self) -> u64 {
        self.lock().synced
    }

    pub(super) fn failure(&self) -> Option<String> {
        self.lock().failed.clone()
    }

    /// Syncs the log through everything appended so far, as one leader.
    /// Returns the version synced to.
    fn sync_once(
        &self,
        mut st: MutexGuard<'_, SyncState>,
    ) -> (MutexGuard<'_, SyncState>, Result<(), String>) {
        st.syncing = true;
        let target = st.appended;
        drop(st);
        // Read `appended` before taking the file: a rotation swaps the file
        // only while nothing is staged, so every record up to `target` is in
        // the file we sync.
        let file = self
            .handle
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map(|h| h.lock().unwrap_or_else(|e| e.into_inner()).clone());
        let result = match file {
            Some(f) => f.sync_data().map_err(|e| e.to_string()),
            None => Ok(()),
        };
        let mut st = self.lock();
        st.syncing = false;
        match &result {
            Ok(()) => {
                st.synced = st.synced.max(target);
                st.syncs += 1;
            }
            Err(e) => {
                st.failed
                    .get_or_insert_with(|| format!("write-ahead log sync failed: {e}"));
            }
        }
        self.synced.notify_all();
        (st, result)
    }

    /// Waits until the log is synced through `version`, leading a sync when
    /// none is running (group commit).
    pub(super) fn wait_synced(&self, version: u64) -> Result<(), AtlantisError> {
        let mut st = self.lock();
        loop {
            if let Some(e) = &st.failed {
                return Err(AtlantisError::Storage(format!(
                    "commit outcome unknown: {e}; reopen the store"
                )));
            }
            if st.synced >= version {
                return Ok(());
            }
            if st.syncing {
                st = self.synced.wait(st).unwrap_or_else(|e| e.into_inner());
            } else {
                st = self.sync_once(st).0;
            }
        }
    }
}

/// Syncs the log every `every` until the store is dropped.
pub(super) fn spawn_interval_syncer(durability: Weak<Durability>, every: std::time::Duration) {
    let spawned = std::thread::Builder::new()
        .name("atlantis-wal-sync".into())
        .spawn(move || loop {
            std::thread::sleep(every);
            let Some(d) = durability.upgrade() else {
                return;
            };
            let st = d.lock();
            if st.failed.is_some() {
                return;
            }
            if st.appended > st.synced && !st.syncing {
                let _ = d.sync_once(st);
            }
        });
    if let Err(e) = spawned {
        tracing::warn!("could not start the write-ahead log sync thread: {e}");
    }
}
