//! Checkpoint files: a magic number, a format byte and the bincode-encoded
//! `(version, pairs)` of the latest committed state, written atomically.

use std::fs;
use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use atlantis_core::AtlantisError;

use crate::kv::KvPair;
use crate::wal::{self, CHECKPOINT_FILE};

use super::inner::Inner;

const CHECKPOINT_MAGIC: &[u8; 4] = b"ATKV";
const CHECKPOINT_FORMAT: u8 = 1;
/// Extension prefix of temporary checkpoint files (`records.tmp.<pid>.<n>`).
const CHECKPOINT_TMP_EXTENSION: &str = "tmp";

/// Writes a checkpoint of `pairs` at `version` to `path` atomically: write a
/// uniquely named temporary file, sync it, rename it over `path`, then sync
/// the directory.
pub(super) fn write_checkpoint(
    path: &Path,
    version: u64,
    pairs: &[KvPair],
) -> Result<(), AtlantisError> {
    let body = bincode::serialize(&(version, pairs))
        .map_err(|e| AtlantisError::Serialization(e.to_string()))?;
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);
    let tmp = path.with_extension(format!(
        "{CHECKPOINT_TMP_EXTENSION}.{}.{}",
        std::process::id(),
        TMP_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let written = (|| {
        let mut file = fs::File::create(&tmp)?;
        file.write_all(CHECKPOINT_MAGIC)?;
        file.write_all(&[CHECKPOINT_FORMAT])?;
        file.write_all(&body)?;
        file.sync_all()?;
        fs::rename(&tmp, path)
    })();
    if let Err(e) = written {
        let _ = fs::remove_file(&tmp);
        return Err(e.into());
    }
    // Make the rename itself durable.
    if let Some(dir) = path.parent() {
        wal::sync_dir(dir)?;
    }
    Ok(())
}

/// Removes temporary files left in `dir` by a checkpoint of `records.kv`
/// that crashed before its rename. They are never read.
pub(super) fn remove_stale_checkpoint_temps(dir: &Path) -> Result<(), AtlantisError> {
    let prefix = Path::new(CHECKPOINT_FILE).with_extension(format!("{CHECKPOINT_TMP_EXTENSION}."));
    let prefix = prefix.to_string_lossy();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let stale = entry
            .file_name()
            .to_str()
            .is_some_and(|name| name.starts_with(prefix.as_ref()));
        if stale && entry.file_type()?.is_file() {
            fs::remove_file(entry.path())?;
        }
    }
    Ok(())
}

/// Reads a checkpoint written by [`write_checkpoint`].
pub(super) fn read_checkpoint(path: &Path) -> Result<Inner, AtlantisError> {
    let bytes = fs::read(path)?;
    let header = CHECKPOINT_MAGIC.len() + 1;
    if bytes.len() < header || &bytes[..4] != CHECKPOINT_MAGIC {
        return Err(AtlantisError::Serialization(format!(
            "{} is not a key-value checkpoint",
            path.display()
        )));
    }
    if bytes[4] != CHECKPOINT_FORMAT {
        return Err(AtlantisError::Serialization(format!(
            "unsupported checkpoint format {}",
            bytes[4]
        )));
    }
    let (version, pairs): (u64, Vec<KvPair>) = bincode::deserialize(&bytes[header..])
        .map_err(|e| AtlantisError::Serialization(e.to_string()))?;
    let data = pairs
        .into_iter()
        .map(|(k, v)| (k, vec![(version, Some(v))]))
        .collect();
    Ok(Inner {
        data,
        version,
        ..Inner::default()
    })
}
