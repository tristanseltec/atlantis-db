//! The key layout: how catalog entries, records, adjacency entries and
//! label index entries map onto one ordered key space.
//!
//! Strings are encoded with [`KeyWriter::str`], so every segment is
//! self-delimiting and literal markers such as `/r` can never be confused
//! with data.
//!
//! | Key | Holds |
//! |---|---|
//! | `/sys/ns {ns}` | namespace definition |
//! | `/sys/ns {ns} /db {db}` | database definition |
//! | `/sys/ns {ns} /db {db} /tb {tb}` | table definition |
//! | `/ns {ns} /db {db} /tb {tb} /r {key}` | record or edge |
//! | `/ns {ns} /db {db} /tb {tb} /g {key} {dir} {edgeTable} {edgeKey}` | adjacency entry |
//! | `/ns {ns} /db {db} /lb {label} {table} {key}` | secondary label index entry |
//!
//! `{dir}` is `o` for an outgoing edge and `i` for an incoming one.

use atlantis_core::{AtlantisError, DatabaseRef, RecordId, RecordKey};

use crate::encoding::{prefix_end, KeyReader, KeyWriter};

const SYS: &[u8] = b"/sys";
const NS: &[u8] = b"/ns";
const DB: &[u8] = b"/db";
const TB: &[u8] = b"/tb";
const REC: &[u8] = b"/r";
const GRAPH: &[u8] = b"/g";
const LABEL: &[u8] = b"/lb";

/// Direction of an adjacency entry, relative to the node that owns it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EdgeDir {
    /// The owning node is the edge's start (`in`) node.
    Out,
    /// The owning node is the edge's end (`out`) node.
    In,
}

impl EdgeDir {
    fn byte(self) -> u8 {
        match self {
            EdgeDir::Out => b'o',
            EdgeDir::In => b'i',
        }
    }

    fn from_byte(b: u8) -> Result<Self, AtlantisError> {
        match b {
            b'o' => Ok(EdgeDir::Out),
            b'i' => Ok(EdgeDir::In),
            _ => Err(AtlantisError::Encoding(format!(
                "bad edge direction byte {b:#04x}"
            ))),
        }
    }
}

/// A half-open key range `[start, end)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyRange {
    pub start: Vec<u8>,
    pub end: Vec<u8>,
}

impl KeyRange {
    /// All keys starting with `prefix`.
    pub fn prefix(prefix: Vec<u8>) -> Self {
        let end = prefix_end(&prefix).unwrap_or_else(|| vec![0xFF; prefix.len() + 1]);
        KeyRange { start: prefix, end }
    }
}

fn db_prefix(db: &DatabaseRef) -> KeyWriter {
    let mut w = KeyWriter::new();
    w.raw(NS).str(&db.ns).raw(DB).str(&db.db);
    w
}

fn table_prefix(db: &DatabaseRef, table: &str) -> KeyWriter {
    let mut w = db_prefix(db);
    w.raw(TB).str(table);
    w
}

/// Catalog key of a namespace definition.
pub fn namespace_def(ns: &str) -> Vec<u8> {
    KeyWriter::new().raw(SYS).raw(NS).str(ns).finish()
}

/// Catalog key of a database definition.
pub fn database_def(db: &DatabaseRef) -> Vec<u8> {
    KeyWriter::new()
        .raw(SYS)
        .raw(NS)
        .str(&db.ns)
        .raw(DB)
        .str(&db.db)
        .finish()
}

/// Catalog key of a table definition.
pub fn table_def(db: &DatabaseRef, table: &str) -> Vec<u8> {
    let mut w = KeyWriter::from_prefix(&database_def(db));
    w.raw(TB).str(table).finish()
}

/// All table definitions in a database.
pub fn table_defs(db: &DatabaseRef) -> KeyRange {
    let mut w = KeyWriter::from_prefix(&database_def(db));
    KeyRange::prefix(w.raw(TB).finish())
}

/// Key of a record (or edge record).
pub fn record(db: &DatabaseRef, id: &RecordId) -> Result<Vec<u8>, AtlantisError> {
    let mut w = table_prefix(db, &id.table);
    w.raw(REC).record_key(&id.key)?;
    Ok(w.finish())
}

/// All records of one table, in key order.
pub fn records(db: &DatabaseRef, table: &str) -> KeyRange {
    let mut w = table_prefix(db, table);
    KeyRange::prefix(w.raw(REC).finish())
}

/// Records of one table whose keys fall in `[from, to)`.
pub fn record_range(
    db: &DatabaseRef,
    table: &str,
    from: &RecordKey,
    to: &RecordKey,
) -> Result<KeyRange, AtlantisError> {
    let mut start = table_prefix(db, table);
    start.raw(REC).record_key(from)?;
    let mut end = table_prefix(db, table);
    end.raw(REC).record_key(to)?;
    Ok(KeyRange {
        start: start.finish(),
        end: end.finish(),
    })
}

/// Adjacency entry for `node` pointing at the edge `edge`.
pub fn adjacency(
    db: &DatabaseRef,
    node: &RecordId,
    dir: EdgeDir,
    edge: &RecordId,
) -> Result<Vec<u8>, AtlantisError> {
    let mut w = table_prefix(db, &node.table);
    w.raw(GRAPH)
        .record_key(&node.key)?
        .u8(dir.byte())
        .record_id(edge)?;
    Ok(w.finish())
}

/// Adjacency entries of `node`, optionally narrowed to one direction and
/// one edge table.
pub fn adjacency_range(
    db: &DatabaseRef,
    node: &RecordId,
    dir: Option<EdgeDir>,
    edge_table: Option<&str>,
) -> Result<KeyRange, AtlantisError> {
    let mut w = table_prefix(db, &node.table);
    w.raw(GRAPH).record_key(&node.key)?;
    if let Some(dir) = dir {
        w.u8(dir.byte());
        if let Some(table) = edge_table {
            w.str(table);
        }
    }
    Ok(KeyRange::prefix(w.finish()))
}

/// Decodes the direction and edge id from an adjacency key of `node`.
pub fn parse_adjacency(
    db: &DatabaseRef,
    node: &RecordId,
    key: &[u8],
) -> Result<(EdgeDir, RecordId), AtlantisError> {
    let prefix = adjacency_range(db, node, None, None)?.start;
    let rest = key
        .strip_prefix(prefix.as_slice())
        .ok_or_else(|| AtlantisError::Encoding("adjacency key belongs to another node".into()))?;
    let mut r = KeyReader::new(rest);
    let dir = EdgeDir::from_byte(r.u8()?)?;
    let edge = r.record_id()?;
    Ok((dir, edge))
}

/// Label index entry: `record` carries secondary label `label`.
pub fn label_entry(
    db: &DatabaseRef,
    label: &str,
    record: &RecordId,
) -> Result<Vec<u8>, AtlantisError> {
    let mut w = db_prefix(db);
    w.raw(LABEL).str(label).record_id(record)?;
    Ok(w.finish())
}

/// All records carrying secondary label `label`.
pub fn label_range(db: &DatabaseRef, label: &str) -> KeyRange {
    let mut w = db_prefix(db);
    KeyRange::prefix(w.raw(LABEL).str(label).finish())
}

/// Decodes the record id from a label index key.
pub fn parse_label_entry(
    db: &DatabaseRef,
    label: &str,
    key: &[u8],
) -> Result<RecordId, AtlantisError> {
    let prefix = label_range(db, label).start;
    let rest = key
        .strip_prefix(prefix.as_slice())
        .ok_or_else(|| AtlantisError::Encoding("label key belongs to another label".into()))?;
    KeyReader::new(rest).record_id()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> DatabaseRef {
        DatabaseRef::default()
    }

    fn id(s: &str) -> RecordId {
        s.parse().unwrap()
    }

    #[test]
    fn records_of_a_table_are_contiguous_and_ordered() {
        let range = records(&db(), "Person");
        let a = record(&db(), &id("Person:1")).unwrap();
        let b = record(&db(), &id("Person:2")).unwrap();
        let c = record(&db(), &id("Person:alice")).unwrap();
        let other = record(&db(), &id("Persona:1")).unwrap();
        for k in [&a, &b, &c] {
            assert!(range.start <= *k && *k < range.end);
        }
        assert!(!(range.start <= other && other < range.end));
        assert!(a < b && b < c, "integer keys sort before string keys");
    }

    #[test]
    fn databases_are_isolated() {
        let other = DatabaseRef::new("acme", "shop").unwrap();
        let k = record(&other, &id("Person:1")).unwrap();
        let range = records(&db(), "Person");
        assert!(!(range.start <= k && k < range.end));
    }

    #[test]
    fn adjacency_roundtrip_and_narrowing() {
        let alice = id("Person:alice");
        let edge = id("KNOWS:1");
        let key = adjacency(&db(), &alice, EdgeDir::Out, &edge).unwrap();
        assert_eq!(
            parse_adjacency(&db(), &alice, &key).unwrap(),
            (EdgeDir::Out, edge.clone())
        );

        let all = adjacency_range(&db(), &alice, None, None).unwrap();
        let out_knows = adjacency_range(&db(), &alice, Some(EdgeDir::Out), Some("KNOWS")).unwrap();
        let in_any = adjacency_range(&db(), &alice, Some(EdgeDir::In), None).unwrap();
        assert!(all.start <= key && key < all.end);
        assert!(out_knows.start <= key && key < out_knows.end);
        assert!(!(in_any.start <= key && key < in_any.end));
        // A record key never falls inside an adjacency range.
        let rec = record(&db(), &alice).unwrap();
        assert!(!(all.start <= rec && rec < all.end));
    }

    #[test]
    fn label_entries_roundtrip() {
        let rec = id("Person:alice");
        let key = label_entry(&db(), "Admin", &rec).unwrap();
        let range = label_range(&db(), "Admin");
        assert!(range.start <= key && key < range.end);
        assert_eq!(parse_label_entry(&db(), "Admin", &key).unwrap(), rec);
        let other = label_range(&db(), "Admins");
        assert!(!(other.start <= key && key < other.end));
    }

    #[test]
    fn catalog_keys_nest() {
        let tables = table_defs(&db());
        let t = table_def(&db(), "Person");
        assert!(tables.start <= t && t < tables.end);
        assert!(namespace_def("atlantis") < database_def(&db()));
    }
}
