//! Write-ahead log for the durable in-memory store.
//!
//! A durable store directory holds three kinds of files:
//!
//! - `records.kv`: the latest checkpoint (all live key/values at version V);
//! - `records.wal`: the active log, one record per committed transaction;
//! - `records.wal.old`: the previous log while a checkpoint is in progress
//!   (or after a checkpoint crashed before deleting it).
//!
//! Every record is framed as `[len: u32 LE][crc32(payload): u32 LE][payload]`
//! where the payload is the bincode encoding of
//! `{ version: u64, writes: Vec<(key, Option<value>)> }` (`None` deletes).
//! A frame whose header is incomplete, whose length runs past the end of the
//! file, whose CRC does not match, or whose payload does not decode is
//! *bad*; reading a log stops at the first bad frame. Everything after it is
//! discarded, even if valid frames follow (mid-file corruption), so that the
//! recovered state is always a prefix of the commit history.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use atlantis_core::AtlantisError;
use serde::{Deserialize, Serialize};

/// Checkpoint file name inside a durable store directory.
pub(crate) const CHECKPOINT_FILE: &str = "records.kv";
/// Active write-ahead log file name.
pub(crate) const WAL_FILE: &str = "records.wal";
/// Previous write-ahead log, kept until the checkpoint covering it is durable.
pub(crate) const WAL_OLD_FILE: &str = "records.wal.old";

const HEADER_LEN: usize = 8;

/// When commits are forced to stable storage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum SyncMode {
    /// The log is `fdatasync`ed before a commit returns or becomes visible:
    /// an acknowledged commit survives power loss. Concurrent commits share
    /// one sync (group commit).
    #[default]
    Every,
    /// A background thread syncs the log at this interval; commits return
    /// without waiting. Power loss can lose the commits of the last interval.
    Interval(Duration),
    /// Never sync the log on commit; the OS flushes it eventually. Commits
    /// survive a process crash but may be lost on power loss.
    Never,
}

/// A handle to the active log file that can be synced without the store
/// lock. [`Wal::rotate`] replaces the file inside it.
pub(crate) type SyncHandle = Arc<Mutex<Arc<File>>>;

/// One committed transaction as stored in the log.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct WalRecord {
    pub version: u64,
    pub writes: Vec<(Vec<u8>, Option<Vec<u8>>)>,
}

/// Borrowed form of [`WalRecord`] used for encoding without copying the
/// transaction's buffers. `&[u8]` and `Vec<u8>` share one bincode encoding.
#[derive(Serialize)]
struct WalRecordRef<'a> {
    version: u64,
    writes: Vec<(&'a [u8], Option<&'a [u8]>)>,
}

const CRC_TABLE: [u32; 256] = {
    let mut table = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut c = i as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 != 0 {
                0xEDB8_8320 ^ (c >> 1)
            } else {
                c >> 1
            };
            k += 1;
        }
        table[i] = c;
        i += 1;
    }
    table
};

/// CRC-32 (IEEE 802.3, reflected, polynomial `0xEDB88320`).
pub(crate) fn crc32(data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &b in data {
        crc = CRC_TABLE[((crc ^ u32::from(b)) & 0xFF) as usize] ^ (crc >> 8);
    }
    !crc
}

/// Encodes one commit as a complete, framed log record.
pub(crate) fn encode_record<'a, I>(version: u64, writes: I) -> Result<Vec<u8>, AtlantisError>
where
    I: IntoIterator<Item = (&'a Vec<u8>, &'a Option<Vec<u8>>)>,
{
    let record = WalRecordRef {
        version,
        writes: writes
            .into_iter()
            .map(|(k, v)| (k.as_slice(), v.as_deref()))
            .collect(),
    };
    let payload =
        bincode::serialize(&record).map_err(|e| AtlantisError::Serialization(e.to_string()))?;
    let len = u32::try_from(payload.len()).map_err(|_| {
        AtlantisError::Storage(format!(
            "transaction too large for the write-ahead log ({} bytes)",
            payload.len()
        ))
    })?;
    let mut frame = Vec::with_capacity(HEADER_LEN + payload.len());
    frame.extend_from_slice(&len.to_le_bytes());
    frame.extend_from_slice(&crc32(&payload).to_le_bytes());
    frame.extend_from_slice(&payload);
    Ok(frame)
}

/// The readable prefix of a log file.
pub(crate) struct LogScan {
    /// Good records with the byte offset at which each frame starts.
    pub records: Vec<(u64, WalRecord)>,
    /// Length of the good prefix; bytes after it are torn or corrupt.
    pub valid_len: u64,
    pub file_len: u64,
}

/// Reads every good record of the log at `path`, stopping at the first bad
/// frame.
pub(crate) fn read_log(path: &Path) -> Result<LogScan, AtlantisError> {
    let bytes = fs::read(path)?;
    let mut records = Vec::new();
    let mut pos = 0usize;
    while let Some(header) = bytes.get(pos..pos + HEADER_LEN) {
        let len = u32::from_le_bytes([header[0], header[1], header[2], header[3]]) as usize;
        let crc = u32::from_le_bytes([header[4], header[5], header[6], header[7]]);
        let start = pos + HEADER_LEN;
        let Some(payload) = start.checked_add(len).and_then(|end| bytes.get(start..end)) else {
            break;
        };
        if crc32(payload) != crc {
            break;
        }
        let Ok(record) = bincode::deserialize::<WalRecord>(payload) else {
            break;
        };
        records.push((pos as u64, record));
        pos = start + len;
    }
    Ok(LogScan {
        records,
        valid_len: pos as u64,
        file_len: bytes.len() as u64,
    })
}

/// Shortens the file at `path` to `len` bytes and makes that durable.
pub(crate) fn truncate_file(path: &Path, len: u64) -> Result<(), AtlantisError> {
    let file = OpenOptions::new().write(true).open(path)?;
    file.set_len(len)?;
    file.sync_all()?;
    Ok(())
}

/// Makes renames, creations and deletions inside `dir` durable.
pub(crate) fn sync_dir(dir: &Path) -> io::Result<()> {
    #[cfg(unix)]
    if !dir.as_os_str().is_empty() {
        File::open(dir)?.sync_all()?;
    }
    #[cfg(not(unix))]
    let _ = dir;
    Ok(())
}

fn open_append(path: &Path) -> io::Result<File> {
    OpenOptions::new().create(true).append(true).open(path)
}

/// The active log of a durable store. Appended to only under the store's
/// write lock, so log order equals commit-version order. Syncing happens
/// outside that lock, through [`Wal::sync_handle`].
pub(crate) struct Wal {
    dir: PathBuf,
    file: File,
    size: u64,
    sync: SyncMode,
    /// A clone of `file` for syncing without the store lock.
    sync_handle: SyncHandle,
    /// Set when a failure left the files in an unknown state; every later
    /// append or rotation fails until the store is reopened (which recovers).
    broken: bool,
}

impl Wal {
    /// Opens (creating if needed) the active log in `dir`. Call after
    /// recovery has truncated it to its good prefix.
    pub(crate) fn open(dir: &Path, sync: SyncMode) -> Result<Self, AtlantisError> {
        let path = dir.join(WAL_FILE);
        let existed = path.exists();
        let file = open_append(&path)?;
        let size = file.metadata()?.len();
        if !existed {
            sync_dir(dir)?;
        }
        let sync_handle = Arc::new(Mutex::new(Arc::new(file.try_clone()?)));
        Ok(Wal {
            dir: dir.to_path_buf(),
            file,
            size,
            sync,
            sync_handle,
            broken: false,
        })
    }

    pub(crate) fn sync_mode(&self) -> SyncMode {
        self.sync
    }

    /// The handle the group-commit leader and the interval syncer sync.
    pub(crate) fn sync_handle(&self) -> SyncHandle {
        self.sync_handle.clone()
    }

    /// Marks the log unusable (a sync failed elsewhere).
    pub(crate) fn mark_broken(&mut self) {
        self.broken = true;
    }

    pub(crate) fn dir(&self) -> &Path {
        &self.dir
    }

    pub(crate) fn size(&self) -> u64 {
        self.size
    }

    fn check_usable(&self) -> Result<(), AtlantisError> {
        if self.broken {
            Err(AtlantisError::Storage(
                "write-ahead log is unusable after an earlier I/O failure; reopen the store".into(),
            ))
        } else {
            Ok(())
        }
    }

    /// Appends one framed record without syncing it; the caller makes it
    /// durable (group commit, the interval syncer, or not at all). On
    /// failure the log is cut back to its previous length; if even that
    /// fails the log is marked broken.
    pub(crate) fn append(&mut self, frame: &[u8]) -> Result<(), AtlantisError> {
        self.check_usable()?;
        if let Err(e) = self.file.write_all(frame) {
            if self.file.set_len(self.size).is_err() {
                self.broken = true;
            }
            return Err(AtlantisError::Storage(format!(
                "write-ahead log append failed: {e}"
            )));
        }
        self.size += frame.len() as u64;
        Ok(())
    }

    /// Moves the active log's records to `records.wal.old` and starts an
    /// empty active log. Afterwards `.old` holds every record not yet
    /// covered by a durable checkpoint.
    ///
    /// If `.old` already exists (an earlier checkpoint failed or crashed
    /// before deleting it) it cannot be overwritten, since the on-disk
    /// checkpoint may not cover it. Instead the active log is appended to
    /// it and synced, and only then is the active log emptied. A crash in
    /// between leaves records in both files (or a torn tail in `.old`);
    /// recovery skips versions it already applied, so this is safe.
    pub(crate) fn rotate(&mut self) -> Result<(), AtlantisError> {
        self.check_usable()?;
        let active = self.dir.join(WAL_FILE);
        let old = self.dir.join(WAL_OLD_FILE);
        if old.exists() {
            let bytes = fs::read(&active)?;
            let merged = (|| -> io::Result<()> {
                let mut old_file = OpenOptions::new().append(true).open(&old)?;
                old_file.write_all(&bytes)?;
                old_file.sync_data()?;
                self.file.set_len(0)?;
                self.file.sync_data()
            })();
            if let Err(e) = merged {
                self.broken = true;
                return Err(e.into());
            }
        } else {
            fs::rename(&active, &old)?;
            let reopened = (|| -> io::Result<File> {
                let file = open_append(&active)?;
                sync_dir(&self.dir)?;
                Ok(file)
            })();
            match reopened {
                Ok(file) => {
                    let clone = match file.try_clone() {
                        Ok(c) => c,
                        Err(e) => {
                            self.broken = true;
                            return Err(e.into());
                        }
                    };
                    *self.sync_handle.lock().unwrap_or_else(|e| e.into_inner()) = Arc::new(clone);
                    self.file = file;
                }
                Err(e) => {
                    self.broken = true;
                    return Err(e.into());
                }
            }
        }
        self.size = 0;
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn replace_file_for_test(&mut self, file: File) {
        *self.sync_handle.lock().unwrap_or_else(|e| e.into_inner()) =
            Arc::new(file.try_clone().expect("clone test file"));
        self.file = file;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn crc32_matches_reference_values() {
        assert_eq!(crc32(b""), 0);
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(
            crc32(b"The quick brown fox jumps over the lazy dog"),
            0x414F_A339
        );
    }

    #[test]
    fn records_roundtrip_and_reading_stops_at_bad_frames() {
        let dir =
            std::env::temp_dir().join(format!("atlantis-wal-test-{}", atlantis_core::Ulid::new()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join(WAL_FILE);

        let mut writes = BTreeMap::new();
        writes.insert(b"a".to_vec(), Some(b"1".to_vec()));
        writes.insert(b"b".to_vec(), None);
        let first = encode_record(1, &writes).unwrap();
        let second = encode_record(2, &writes).unwrap();
        let mut bytes = [first.clone(), second.clone()].concat();
        fs::write(&path, &bytes).unwrap();

        let scan = read_log(&path).unwrap();
        assert_eq!(scan.records.len(), 2);
        assert_eq!(scan.records[1].0, first.len() as u64);
        assert_eq!(
            scan.records[0].1,
            WalRecord {
                version: 1,
                writes: vec![(b"a".to_vec(), Some(b"1".to_vec())), (b"b".to_vec(), None)],
            }
        );
        assert_eq!(scan.valid_len, scan.file_len);

        // Flip one payload byte of the first record: nothing is readable,
        // even though the second frame is intact.
        bytes[HEADER_LEN + 2] ^= 0xFF;
        fs::write(&path, &bytes).unwrap();
        let scan = read_log(&path).unwrap();
        assert!(scan.records.is_empty());
        assert_eq!(scan.valid_len, 0);

        // A zero-filled tail (length 0, CRC 0) is rejected by decoding.
        let mut zeros = first.clone();
        zeros.extend_from_slice(&[0u8; 32]);
        fs::write(&path, &zeros).unwrap();
        let scan = read_log(&path).unwrap();
        assert_eq!(scan.records.len(), 1);
        assert_eq!(scan.valid_len, first.len() as u64);

        // An oversized length is treated as a torn frame.
        let mut huge = first.clone();
        huge.extend_from_slice(&u32::MAX.to_le_bytes());
        huge.extend_from_slice(&[0u8; 4]);
        fs::write(&path, &huge).unwrap();
        assert_eq!(read_log(&path).unwrap().valid_len, first.len() as u64);

        truncate_file(&path, 3).unwrap();
        assert_eq!(fs::metadata(&path).unwrap().len(), 3);
        fs::remove_dir_all(&dir).unwrap();
    }
}
