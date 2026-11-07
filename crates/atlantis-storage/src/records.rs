//! Record and edge store on top of the transactional key-value layer.
//!
//! This is the storage side of the unified record model: documents, graph
//! nodes and relational rows are all [`Record`]s in tables; relationships
//! are [`Edge`] records in edge tables, with adjacency entries on both
//! endpoints so traversals are prefix scans. Tables are created implicitly
//! (schemaless) on first write, or explicitly with
//! [`RecordTx::define_table`].

use std::collections::HashMap;
use std::sync::Arc;

use atlantis_core::{
    validate_ident, validate_user_table, AtlantisError, DatabaseRef, Direction, DistanceMetricKind,
    Edge, Object, Record, RecordId, RecordKey, Value, DEFAULT_TABLE,
};
use atlantis_kv::keys::{self, EdgeDir};
use atlantis_kv::{Isolation, KvStore, KvTransaction, TxMode, Versionstamp};
use serde::{de::DeserializeOwned, Deserialize, Serialize};

/// Leading byte of every stored value, so the format can evolve.
const FORMAT_V1: u8 = 1;

/// What a table holds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TableKind {
    /// Documents, graph nodes or rows.
    Normal,
    /// Relationships. Empty `from` / `to` lists allow any endpoint table.
    Edge { from: Vec<String>, to: Vec<String> },
}

/// A table definition stored in the catalog.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TableDef {
    pub name: String,
    pub kind: TableKind,
}

impl TableDef {
    pub fn is_edge(&self) -> bool {
        matches!(self.kind, TableKind::Edge { .. })
    }
}

/// A vector index definition stored in the catalog. The index itself is
/// derived data, rebuilt in memory from the records it covers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VectorIndexDef {
    pub name: String,
    /// Records carrying this label (as their table or a secondary label).
    pub label: String,
    /// The field holding the vector.
    pub field: String,
    pub dimension: usize,
    pub metric: DistanceMetricKind,
    pub m: usize,
    pub ef_construction: usize,
}

/// The outcome of a successful commit.
#[derive(Debug, Clone)]
pub struct CommitInfo {
    pub versionstamp: Versionstamp,
    /// Records and edges written or deleted by the transaction, each once.
    pub changed: Vec<RecordId>,
}

#[derive(Serialize, Deserialize)]
enum Stored {
    Record(Record),
    Edge(Edge),
}

fn encode<T: Serialize>(value: &T) -> Result<Vec<u8>, AtlantisError> {
    let mut out = vec![FORMAT_V1];
    bincode::serialize_into(&mut out, value)
        .map_err(|e| AtlantisError::Serialization(e.to_string()))?;
    Ok(out)
}

fn decode<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, AtlantisError> {
    match bytes.split_first() {
        Some((&FORMAT_V1, body)) => {
            bincode::deserialize(body).map_err(|e| AtlantisError::Serialization(e.to_string()))
        }
        _ => Err(AtlantisError::Serialization(
            "unknown stored value format".into(),
        )),
    }
}

/// Opens record transactions against a key-value backend.
#[derive(Clone)]
pub struct RecordStore {
    kv: Arc<dyn KvStore>,
}

impl RecordStore {
    pub fn new(kv: Arc<dyn KvStore>) -> Self {
        RecordStore { kv }
    }

    /// Starts a transaction scoped to one database.
    pub fn begin(
        &self,
        db: DatabaseRef,
        mode: TxMode,
        isolation: Isolation,
    ) -> Result<RecordTx, AtlantisError> {
        Ok(RecordTx {
            tx: self.kv.begin(mode, isolation)?,
            db,
            catalog_checked: false,
            changed: HashMap::new(),
        })
    }
}

/// A transaction over records and edges in one database.
///
/// Nothing is visible to other transactions until [`commit`](Self::commit).
/// Dropping the transaction without committing discards its writes.
pub struct RecordTx {
    tx: Box<dyn KvTransaction>,
    db: DatabaseRef,
    catalog_checked: bool,
    /// Changed records, keyed by their encoded storage key.
    changed: HashMap<Vec<u8>, RecordId>,
}

impl RecordTx {
    /// The database this transaction works in.
    pub fn database(&self) -> &DatabaseRef {
        &self.db
    }

    /// Commits all writes atomically.
    pub fn commit(mut self) -> Result<CommitInfo, AtlantisError> {
        let versionstamp = self.tx.commit()?;
        Ok(CommitInfo {
            versionstamp,
            changed: self.changed.into_values().collect(),
        })
    }

    /// `true` if this transaction has written or deleted any record.
    pub fn has_changes(&self) -> bool {
        !self.changed.is_empty()
    }

    fn put_record(
        &mut self,
        id: &RecordId,
        key: Vec<u8>,
        stored: &Stored,
    ) -> Result<(), AtlantisError> {
        self.changed
            .entry(key.clone())
            .or_insert_with(|| id.clone());
        self.tx.set(key, encode(stored)?)
    }

    fn delete_record_key(&mut self, id: &RecordId) -> Result<(), AtlantisError> {
        let key = keys::record(&self.db, id)?;
        self.tx.delete(&key)?;
        self.changed.entry(key).or_insert_with(|| id.clone());
        Ok(())
    }

    /// Discards all writes.
    pub fn cancel(mut self) -> Result<(), AtlantisError> {
        self.tx.cancel()
    }

    fn get_decoded<T: DeserializeOwned>(&mut self, key: &[u8]) -> Result<Option<T>, AtlantisError> {
        self.tx.get(key)?.map(|bytes| decode(&bytes)).transpose()
    }

    /// Registers the namespace and database in the catalog on first write.
    fn ensure_database(&mut self) -> Result<(), AtlantisError> {
        if self.catalog_checked {
            return Ok(());
        }
        let ns_key = keys::namespace_def(&self.db.ns);
        if self.tx.get(&ns_key)?.is_none() {
            self.tx.set(ns_key, encode(&self.db.ns)?)?;
        }
        let db_key = keys::database_def(&self.db);
        if self.tx.get(&db_key)?.is_none() {
            self.tx.set(db_key, encode(&self.db)?)?;
        }
        self.catalog_checked = true;
        Ok(())
    }

    // ---- catalog ---------------------------------------------------------

    /// Defines a table. Fails if the table already exists.
    pub fn define_table(&mut self, name: &str, kind: TableKind) -> Result<TableDef, AtlantisError> {
        validate_user_table(name)?;
        if let TableKind::Edge { from, to } = &kind {
            from.iter()
                .chain(to)
                .try_for_each(|t| validate_ident("table", t))?;
        }
        if self.table(name)?.is_some() {
            return Err(AtlantisError::SchemaViolation(format!(
                "table {name} is already defined"
            )));
        }
        self.ensure_database()?;
        let def = TableDef {
            name: name.to_string(),
            kind,
        };
        self.tx
            .set(keys::table_def(&self.db, name), encode(&def)?)?;
        Ok(def)
    }

    /// Returns a table definition.
    pub fn table(&mut self, name: &str) -> Result<Option<TableDef>, AtlantisError> {
        let key = keys::table_def(&self.db, name);
        self.get_decoded(&key)
    }

    /// Lists all tables in the database, ordered by name.
    pub fn tables(&mut self) -> Result<Vec<TableDef>, AtlantisError> {
        let range = keys::table_defs(&self.db);
        self.tx
            .scan(&range, None)?
            .iter()
            .map(|(_, v)| decode(v))
            .collect()
    }

    /// Returns the table definition, creating a schemaless one on first use.
    fn resolve_table(&mut self, name: &str, edge: bool) -> Result<TableDef, AtlantisError> {
        if let Some(def) = self.table(name)? {
            if def.is_edge() != edge {
                let (is, want) = if edge {
                    ("a normal", "an edge")
                } else {
                    ("an edge", "a normal")
                };
                return Err(AtlantisError::SchemaViolation(format!(
                    "{name} is {is} table; this operation needs {want} table"
                )));
            }
            return Ok(def);
        }
        if name != DEFAULT_TABLE {
            validate_user_table(name)?;
        }
        self.ensure_database()?;
        let kind = if edge {
            TableKind::Edge {
                from: vec![],
                to: vec![],
            }
        } else {
            TableKind::Normal
        };
        let def = TableDef {
            name: name.to_string(),
            kind,
        };
        self.tx
            .set(keys::table_def(&self.db, name), encode(&def)?)?;
        Ok(def)
    }

    // ---- records ---------------------------------------------------------

    /// Takes an explicit key from `key`, or from an `id` field in `data`.
    fn take_key(
        table: &str,
        key: Option<RecordKey>,
        data: &mut Object,
    ) -> Result<RecordKey, AtlantisError> {
        let from_data = data.remove("id");
        match (key, from_data) {
            (Some(k), None) => Ok(k),
            (None, None) => Ok(RecordKey::generate()),
            (None, Some(Value::RecordId(id))) if id.table == table => Ok(id.key),
            (None, Some(Value::RecordId(id))) => Err(AtlantisError::InvalidRecordId(format!(
                "id {id} belongs to table {}, not {table}",
                id.table
            ))),
            (None, Some(v)) => RecordKey::from_value(&v),
            (Some(_), Some(_)) => Err(AtlantisError::InvalidRecordId(
                "record key given both explicitly and as an id field".into(),
            )),
        }
    }

    /// Creates a record in `table`. The key comes from `key`, else from an
    /// `id` field in `data`, else a new ULID is generated. Fails if a
    /// record with that id exists.
    pub fn create(
        &mut self,
        table: &str,
        key: Option<RecordKey>,
        labels: Vec<String>,
        data: Object,
    ) -> Result<Record, AtlantisError> {
        let mut data = data;
        self.resolve_table(table, false)?;
        let key = Self::take_key(table, key, &mut data)?;
        let record = Record::new(RecordId::new(table, key)?, labels, data)?;
        let rkey = keys::record(&self.db, &record.id)?;
        if self.tx.get(&rkey)?.is_some() {
            return Err(AtlantisError::RecordExists(record.id.to_string()));
        }
        for label in &record.labels {
            self.tx
                .set(keys::label_entry(&self.db, label, &record.id)?, Vec::new())?;
        }
        self.put_record(&record.id.clone(), rkey, &Stored::Record(record.clone()))?;
        Ok(record)
    }

    /// Creates a graph node. The first label is the table; the remaining
    /// labels are secondary labels. A node without labels goes into the
    /// reserved `_default` table.
    pub fn create_node(&mut self, labels: &[&str], data: Object) -> Result<Record, AtlantisError> {
        let (table, rest) = match labels.split_first() {
            Some((first, rest)) => (*first, rest),
            None => (DEFAULT_TABLE, &[][..]),
        };
        self.create(
            table,
            None,
            rest.iter().map(|l| l.to_string()).collect(),
            data,
        )
    }

    fn get_stored(&mut self, id: &RecordId) -> Result<Option<Stored>, AtlantisError> {
        let key = keys::record(&self.db, id)?;
        self.get_decoded(&key)
    }

    /// Reads a record. Edge records are returned as documents with `in` and
    /// `out` fields.
    pub fn get(&mut self, id: &RecordId) -> Result<Option<Record>, AtlantisError> {
        Ok(self.get_stored(id)?.map(|stored| match stored {
            Stored::Record(r) => r,
            Stored::Edge(e) => edge_as_record(e),
        }))
    }

    /// Reads an edge record.
    pub fn get_edge(&mut self, id: &RecordId) -> Result<Option<Edge>, AtlantisError> {
        match self.get_stored(id)? {
            Some(Stored::Edge(e)) => Ok(Some(e)),
            Some(Stored::Record(_)) => Err(AtlantisError::SchemaViolation(format!(
                "{id} is a record, not an edge"
            ))),
            None => Ok(None),
        }
    }

    fn not_found(id: &RecordId) -> AtlantisError {
        AtlantisError::RecordNotFound(id.to_string())
    }

    /// Validates that a document's `id` field (if present) matches `id`.
    fn strip_id(id: &RecordId, data: &mut Object) -> Result<(), AtlantisError> {
        match data.remove("id") {
            None => Ok(()),
            Some(Value::RecordId(given)) if &given == id => Ok(()),
            Some(_) => Err(AtlantisError::SchemaViolation(format!(
                "the id of {id} cannot be changed"
            ))),
        }
    }

    fn write_back(&mut self, stored: Stored) -> Result<Record, AtlantisError> {
        let (id, view) = match &stored {
            Stored::Record(r) => (r.id.clone(), r.clone()),
            Stored::Edge(e) => (e.id.clone(), edge_as_record(e.clone())),
        };
        let key = keys::record(&self.db, &id)?;
        self.put_record(&id, key, &stored)?;
        Ok(view)
    }

    fn apply<F>(&mut self, id: &RecordId, change: F) -> Result<Record, AtlantisError>
    where
        F: FnOnce(&mut Object) -> Result<(), AtlantisError>,
    {
        match self.get_stored(id)?.ok_or_else(|| Self::not_found(id))? {
            Stored::Record(mut r) => {
                change(&mut r.data)?;
                self.write_back(Stored::Record(r))
            }
            Stored::Edge(mut e) => {
                change(&mut e.data)?;
                if e.data.contains_key("in") || e.data.contains_key("out") {
                    return Err(AtlantisError::SchemaViolation(format!(
                        "the endpoints of edge {id} cannot be changed"
                    )));
                }
                self.write_back(Stored::Edge(e))
            }
        }
    }

    /// Replaces a record's document. Labels and (for edges) endpoints are kept.
    pub fn update(&mut self, id: &RecordId, data: Object) -> Result<Record, AtlantisError> {
        let mut data = data;
        Self::strip_id(id, &mut data)?;
        self.apply(id, |doc| {
            *doc = data;
            Ok(())
        })
    }

    /// Merges fields into a record's document. A `Null` value removes the field.
    pub fn merge(&mut self, id: &RecordId, patch: Object) -> Result<Record, AtlantisError> {
        let mut patch = patch;
        Self::strip_id(id, &mut patch)?;
        self.apply(id, |doc| {
            for (k, v) in patch {
                if v.is_null() {
                    doc.remove(&k);
                } else {
                    doc.insert(k, v);
                }
            }
            Ok(())
        })
    }

    /// Deletes a record and returns whether it existed. A node that still
    /// has edges is only deleted with `detach = true`, which deletes the
    /// edges as well. Deleting an edge record removes its adjacency entries.
    pub fn delete(&mut self, id: &RecordId, detach: bool) -> Result<bool, AtlantisError> {
        let record = match self.get_stored(id)? {
            None => return Ok(false),
            Some(Stored::Edge(_)) => return self.delete_edge(id),
            Some(Stored::Record(r)) => r,
        };
        let edges = self.edge_ids(id, None, None)?;
        if !edges.is_empty() && !detach {
            return Err(AtlantisError::SchemaViolation(format!(
                "{id} still has {} edge(s); delete them first or use detach",
                edges.len()
            )));
        }
        for edge in edges {
            self.delete_edge(&edge)?;
        }
        for label in &record.labels {
            self.tx.delete(&keys::label_entry(&self.db, label, id)?)?;
        }
        self.delete_record_key(id)?;
        Ok(true)
    }

    fn decode_records(pairs: Vec<atlantis_kv::KvPair>) -> Result<Vec<Record>, AtlantisError> {
        pairs
            .iter()
            .map(|(_, v)| {
                decode::<Stored>(v).map(|s| match s {
                    Stored::Record(r) => r,
                    Stored::Edge(e) => edge_as_record(e),
                })
            })
            .collect()
    }

    /// Returns records of a table in key order.
    pub fn scan(
        &mut self,
        table: &str,
        limit: Option<usize>,
    ) -> Result<Vec<Record>, AtlantisError> {
        let pairs = self.tx.scan(&keys::records(&self.db, table), limit)?;
        Self::decode_records(pairs)
    }

    /// Returns records of a table whose keys fall in `[from, to)`, in key
    /// order. With composite array keys this selects, for example, one
    /// sensor's readings within a time window.
    pub fn scan_range(
        &mut self,
        table: &str,
        from: &RecordKey,
        to: &RecordKey,
        limit: Option<usize>,
    ) -> Result<Vec<Record>, AtlantisError> {
        let range = keys::record_range(&self.db, table, from, to)?;
        let pairs = self.tx.scan(&range, limit)?;
        Self::decode_records(pairs)
    }

    /// Returns all records carrying `label`, either as their table or as a
    /// secondary label.
    pub fn with_label(&mut self, label: &str) -> Result<Vec<Record>, AtlantisError> {
        let mut out = match self.table(label)? {
            Some(def) if !def.is_edge() => self.scan(label, None)?,
            _ => Vec::new(),
        };
        let range = keys::label_range(&self.db, label);
        for (key, _) in self.tx.scan(&range, None)? {
            let id = keys::parse_label_entry(&self.db, label, &key)?;
            if let Some(record) = self.get(&id)? {
                out.push(record);
            }
        }
        Ok(out)
    }

    /// Adds and removes secondary labels of a record. Adding the record's
    /// own table as a label is a no-op; it cannot be removed.
    pub fn set_labels(
        &mut self,
        id: &RecordId,
        add: &[String],
        remove: &[String],
    ) -> Result<Record, AtlantisError> {
        let mut record = match self.get_stored(id)?.ok_or_else(|| Self::not_found(id))? {
            Stored::Record(r) => r,
            Stored::Edge(_) => {
                return Err(AtlantisError::SchemaViolation(format!(
                    "edge {id} cannot carry labels"
                )))
            }
        };
        if remove.contains(&id.table) {
            return Err(AtlantisError::SchemaViolation(format!(
                "{} is the table of {id} and cannot be removed",
                id.table
            )));
        }
        let before = record.labels.clone();
        let mut labels: Vec<String> = before
            .iter()
            .filter(|l| !remove.contains(l))
            .cloned()
            .collect();
        labels.extend(add.iter().cloned());
        record = Record::new(record.id, labels, record.data)?;
        for gone in before.iter().filter(|l| !record.labels.contains(l)) {
            self.tx.delete(&keys::label_entry(&self.db, gone, id)?)?;
        }
        for new in record.labels.iter().filter(|l| !before.contains(l)) {
            self.tx
                .set(keys::label_entry(&self.db, new, id)?, Vec::new())?;
        }
        self.write_back(Stored::Record(record))
    }

    /// Defines a vector index. Fails if an index with that name exists.
    pub fn define_vector_index(&mut self, def: VectorIndexDef) -> Result<(), AtlantisError> {
        validate_ident("index", &def.name)?;
        validate_ident("label", &def.label)?;
        validate_ident("field", &def.field)?;
        if def.dimension == 0 {
            return Err(AtlantisError::SchemaViolation(
                "vector index dimension must be positive".into(),
            ));
        }
        if self.vector_index(&def.name)?.is_some() {
            return Err(AtlantisError::SchemaViolation(format!(
                "index {} is already defined",
                def.name
            )));
        }
        self.ensure_database()?;
        self.tx
            .set(keys::index_def(&self.db, &def.name), encode(&def)?)
    }

    /// Returns a vector index definition.
    pub fn vector_index(&mut self, name: &str) -> Result<Option<VectorIndexDef>, AtlantisError> {
        let key = keys::index_def(&self.db, name);
        self.get_decoded(&key)
    }

    /// Lists vector index definitions, ordered by name.
    pub fn vector_indexes(&mut self) -> Result<Vec<VectorIndexDef>, AtlantisError> {
        let range = keys::index_defs(&self.db);
        self.tx
            .scan(&range, None)?
            .iter()
            .map(|(_, v)| decode(v))
            .collect()
    }

    /// Drops an index definition. Returns whether it existed.
    pub fn drop_index(&mut self, name: &str) -> Result<bool, AtlantisError> {
        let key = keys::index_def(&self.db, name);
        let existed = self.tx.get(&key)?.is_some();
        if existed {
            self.tx.delete(&key)?;
        }
        Ok(existed)
    }

    // ---- edges -----------------------------------------------------------

    /// Creates an edge `from -[rel_type]-> to`. Both endpoints must exist.
    /// The edge table is created on first use; if it was defined with
    /// endpoint tables, the endpoints must belong to them. An `id` field in
    /// `data` sets the edge key.
    pub fn relate(
        &mut self,
        from: &RecordId,
        rel_type: &str,
        to: &RecordId,
        data: Object,
    ) -> Result<Edge, AtlantisError> {
        let mut data = data;
        let def = self.resolve_table(rel_type, true)?;
        if let TableKind::Edge {
            from: allowed_from,
            to: allowed_to,
        } = &def.kind
        {
            for (end, allowed, side) in [(from, allowed_from, "start"), (to, allowed_to, "end")] {
                if !allowed.is_empty() && !allowed.contains(&end.table) {
                    return Err(AtlantisError::SchemaViolation(format!(
                        "{rel_type} does not allow {} as its {side} node (allowed: {})",
                        end.table,
                        allowed.join(", ")
                    )));
                }
            }
        }
        for end in [from, to] {
            let key = keys::record(&self.db, end)?;
            let bytes = self.tx.get(&key)?.ok_or_else(|| Self::not_found(end))?;
            if matches!(decode::<Stored>(&bytes)?, Stored::Edge(_)) {
                return Err(AtlantisError::SchemaViolation(format!(
                    "{end} is an edge; edges cannot be endpoints of other edges"
                )));
            }
            // Rewrite the endpoint unchanged so that a concurrent delete of it
            // conflicts with this transaction instead of leaving a dangling edge.
            self.tx.set(key, bytes)?;
        }
        if data.contains_key("in") || data.contains_key("out") {
            return Err(AtlantisError::SchemaViolation(
                "in and out are set from the edge endpoints".into(),
            ));
        }
        let key = Self::take_key(rel_type, None, &mut data)?;
        let edge = Edge {
            id: RecordId::new(rel_type, key)?,
            from: from.clone(),
            to: to.clone(),
            data,
        };
        let rkey = keys::record(&self.db, &edge.id)?;
        if self.tx.get(&rkey)?.is_some() {
            return Err(AtlantisError::RecordExists(edge.id.to_string()));
        }
        self.tx.set(
            keys::adjacency(&self.db, from, EdgeDir::Out, &edge.id)?,
            Vec::new(),
        )?;
        self.tx.set(
            keys::adjacency(&self.db, to, EdgeDir::In, &edge.id)?,
            Vec::new(),
        )?;
        self.put_record(&edge.id.clone(), rkey, &Stored::Edge(edge.clone()))?;
        Ok(edge)
    }

    /// Deletes an edge and its adjacency entries. Returns whether it existed.
    pub fn delete_edge(&mut self, id: &RecordId) -> Result<bool, AtlantisError> {
        let Some(edge) = self.get_edge(id)? else {
            return Ok(false);
        };
        self.tx
            .delete(&keys::adjacency(&self.db, &edge.from, EdgeDir::Out, id)?)?;
        self.tx
            .delete(&keys::adjacency(&self.db, &edge.to, EdgeDir::In, id)?)?;
        self.delete_record_key(id)?;
        Ok(true)
    }

    fn dirs(direction: Direction) -> &'static [EdgeDir] {
        match direction {
            Direction::Outgoing => &[EdgeDir::Out],
            Direction::Incoming => &[EdgeDir::In],
            Direction::Both => &[EdgeDir::Out, EdgeDir::In],
        }
    }

    /// Ids of edges attached to `node`. `direction = None` means both.
    fn edge_ids(
        &mut self,
        node: &RecordId,
        direction: Option<Direction>,
        rel_type: Option<&str>,
    ) -> Result<Vec<RecordId>, AtlantisError> {
        let mut ids: Vec<RecordId> = Vec::new();
        for dir in Self::dirs(direction.unwrap_or(Direction::Both)) {
            let range = keys::adjacency_range(&self.db, node, Some(*dir), rel_type)?;
            for (key, _) in self.tx.scan(&range, None)? {
                let (_, edge) = keys::parse_adjacency(&self.db, node, &key)?;
                // A self-loop appears in both directions; report it once.
                if !ids.contains(&edge) {
                    ids.push(edge);
                }
            }
        }
        Ok(ids)
    }

    /// Edges attached to `node` in `direction`, optionally of one type.
    pub fn edges(
        &mut self,
        node: &RecordId,
        direction: Direction,
        rel_type: Option<&str>,
    ) -> Result<Vec<Edge>, AtlantisError> {
        let ids = self.edge_ids(node, Some(direction), rel_type)?;
        let mut out = Vec::with_capacity(ids.len());
        for id in ids {
            let edge = self.get_edge(&id)?.ok_or_else(|| {
                AtlantisError::Storage(format!("adjacency entry points at missing edge {id}"))
            })?;
            out.push(edge);
        }
        Ok(out)
    }

    /// Ids of the nodes reached from `node` over edges in `direction`.
    pub fn neighbors(
        &mut self,
        node: &RecordId,
        direction: Direction,
        rel_type: Option<&str>,
    ) -> Result<Vec<RecordId>, AtlantisError> {
        Ok(self
            .edges(node, direction, rel_type)?
            .into_iter()
            .map(|e| if &e.from == node { e.to } else { e.from })
            .collect())
    }
}

fn edge_as_record(edge: Edge) -> Record {
    let mut data = edge.to_object();
    data.remove("id");
    Record {
        id: edge.id,
        labels: Vec::new(),
        data,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use atlantis_kv::MemStore;

    fn store() -> RecordStore {
        RecordStore::new(Arc::new(MemStore::new()))
    }

    fn tx(store: &RecordStore) -> RecordTx {
        store
            .begin(
                DatabaseRef::default(),
                TxMode::ReadWrite,
                Isolation::Snapshot,
            )
            .unwrap()
    }

    fn obj(pairs: &[(&str, Value)]) -> Object {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect()
    }

    fn id(s: &str) -> RecordId {
        s.parse().unwrap()
    }

    #[test]
    fn create_get_update_merge_delete() {
        let s = store();
        let mut t = tx(&s);
        let rec = t
            .create(
                "Person",
                None,
                vec![],
                obj(&[("id", "alice".into()), ("age", 30i64.into())]),
            )
            .unwrap();
        assert_eq!(rec.id, id("Person:alice"));
        assert!(
            !rec.data.contains_key("id"),
            "the id lives in the record id, not the document"
        );
        assert!(matches!(
            t.create(
                "Person",
                Some(RecordKey::String("alice".into())),
                vec![],
                Object::new()
            ),
            Err(AtlantisError::RecordExists(_))
        ));

        let merged = t
            .merge(
                &rec.id,
                obj(&[("city", "Hue".into()), ("age", Value::Null)]),
            )
            .unwrap();
        assert_eq!(merged.get("city"), Some(&Value::from("Hue")));
        assert_eq!(merged.get("age"), None);
        let replaced = t.update(&rec.id, obj(&[("name", "Alice".into())])).unwrap();
        assert_eq!(replaced.data.len(), 1);
        assert!(t
            .update(&rec.id, obj(&[("id", Value::RecordId(id("Person:bob")))]))
            .is_err());
        t.commit().unwrap();

        let mut t = tx(&s);
        assert_eq!(
            t.get(&rec.id).unwrap().unwrap().get("name"),
            Some(&Value::from("Alice"))
        );
        assert!(t.delete(&rec.id, false).unwrap());
        assert!(!t.delete(&rec.id, false).unwrap());
        assert!(matches!(
            t.merge(&rec.id, Object::new()),
            Err(AtlantisError::RecordNotFound(_))
        ));
    }

    #[test]
    fn generated_keys_and_default_table() {
        let s = store();
        let mut t = tx(&s);
        let a = t.create_node(&[], Object::new()).unwrap();
        assert_eq!(a.id.table, DEFAULT_TABLE);
        assert!(matches!(a.id.key, RecordKey::Ulid(_)));
        assert!(
            t.create("_secret", None, vec![], Object::new()).is_err(),
            "reserved table names"
        );
        let b = t.create_node(&[], Object::new()).unwrap();
        let ids: Vec<RecordId> = t
            .scan(DEFAULT_TABLE, None)
            .unwrap()
            .into_iter()
            .map(|r| r.id)
            .collect();
        assert_eq!(ids, vec![a.id, b.id], "ULID keys keep creation order");
    }

    #[test]
    fn nodes_labels_and_edges() {
        let s = store();
        let mut t = tx(&s);
        let alice = t
            .create_node(&["Person", "Admin"], obj(&[("id", "alice".into())]))
            .unwrap();
        let bob = t
            .create_node(&["Person"], obj(&[("id", "bob".into())]))
            .unwrap();
        let acme = t
            .create_node(&["Company"], obj(&[("id", "acme".into())]))
            .unwrap();
        assert_eq!(alice.labels, vec!["Admin".to_string()]);
        assert_eq!(t.with_label("Admin").unwrap().len(), 1);
        assert_eq!(t.with_label("Person").unwrap().len(), 2);

        let knows = t
            .relate(
                &alice.id,
                "KNOWS",
                &bob.id,
                obj(&[("since", 2020i64.into())]),
            )
            .unwrap();
        t.relate(&alice.id, "WORKS_AT", &acme.id, Object::new())
            .unwrap();
        t.commit().unwrap();

        let mut t = tx(&s);
        assert_eq!(
            t.neighbors(&alice.id, Direction::Outgoing, None)
                .unwrap()
                .len(),
            2
        );
        assert_eq!(
            t.neighbors(&alice.id, Direction::Outgoing, Some("KNOWS"))
                .unwrap(),
            vec![bob.id.clone()]
        );
        assert_eq!(
            t.neighbors(&bob.id, Direction::Incoming, None).unwrap(),
            vec![alice.id.clone()]
        );
        assert!(t
            .neighbors(&bob.id, Direction::Outgoing, None)
            .unwrap()
            .is_empty());
        let as_record = t.get(&knows.id).unwrap().unwrap();
        assert_eq!(
            as_record.get("in"),
            Some(&Value::RecordId(alice.id.clone()))
        );
        assert_eq!(as_record.get("since"), Some(&Value::Integer(2020)));
        assert!(t.table("KNOWS").unwrap().unwrap().is_edge());
        assert!(
            t.create("KNOWS", None, vec![], Object::new()).is_err(),
            "edge tables hold only edges"
        );
    }

    #[test]
    fn delete_requires_detach_when_edges_exist() {
        let s = store();
        let mut t = tx(&s);
        let a = t.create_node(&["P"], Object::new()).unwrap();
        let b = t.create_node(&["P"], Object::new()).unwrap();
        let e = t.relate(&a.id, "R", &b.id, Object::new()).unwrap();
        t.relate(&a.id, "R", &a.id, Object::new()).unwrap(); // self-loop
        assert_eq!(
            t.edges(&a.id, Direction::Both, None).unwrap().len(),
            2,
            "self-loop counted once"
        );
        assert!(matches!(
            t.delete(&a.id, false),
            Err(AtlantisError::SchemaViolation(_))
        ));
        assert!(t.delete(&a.id, true).unwrap());
        assert!(t.get_edge(&e.id).unwrap().is_none());
        assert!(
            t.edges(&b.id, Direction::Both, None).unwrap().is_empty(),
            "no dangling adjacency"
        );
    }

    #[test]
    fn edge_endpoint_rules() {
        let s = store();
        let mut t = tx(&s);
        t.define_table(
            "WORKS_AT",
            TableKind::Edge {
                from: vec!["Person".into()],
                to: vec!["Company".into()],
            },
        )
        .unwrap();
        let p = t.create_node(&["Person"], Object::new()).unwrap();
        let c = t.create_node(&["Company"], Object::new()).unwrap();
        assert!(t.relate(&p.id, "WORKS_AT", &c.id, Object::new()).is_ok());
        assert!(matches!(
            t.relate(&c.id, "WORKS_AT", &p.id, Object::new()),
            Err(AtlantisError::SchemaViolation(_))
        ));
        assert!(matches!(
            t.relate(&p.id, "WORKS_AT", &id("Company:ghost"), Object::new()),
            Err(AtlantisError::RecordNotFound(_))
        ));
        assert!(t.define_table("WORKS_AT", TableKind::Normal).is_err());
        let names: Vec<String> = t.tables().unwrap().into_iter().map(|d| d.name).collect();
        assert_eq!(names, vec!["Company", "Person", "WORKS_AT"]);
    }

    #[test]
    fn databases_are_isolated() {
        let s = store();
        let mut t = tx(&s);
        t.create("Person", Some(RecordKey::Integer(1)), vec![], Object::new())
            .unwrap();
        t.commit().unwrap();
        let other = DatabaseRef::new("acme", "shop").unwrap();
        let mut t = s
            .begin(other, TxMode::ReadOnly, Isolation::Snapshot)
            .unwrap();
        assert!(t.get(&id("Person:1")).unwrap().is_none());
        assert!(t.tables().unwrap().is_empty());
    }

    #[test]
    fn composite_keys_support_time_range_scans() {
        let s = store();
        let mut t = tx(&s);
        for (sensor, hour) in [("s1", 1i64), ("s1", 2), ("s1", 3), ("s2", 1)] {
            let key = RecordKey::Array(vec![sensor.into(), hour.into()]);
            t.create("Reading", Some(key), vec![], obj(&[("v", hour.into())]))
                .unwrap();
        }
        let from = RecordKey::Array(vec!["s1".into(), 2i64.into()]);
        let to = RecordKey::Array(vec!["s1".into(), 99i64.into()]);
        let hits = t.scan_range("Reading", &from, &to, None).unwrap();
        let hours: Vec<&Value> = hits.iter().map(|r| r.get("v").unwrap()).collect();
        assert_eq!(hours, vec![&Value::Integer(2), &Value::Integer(3)]);
    }

    #[test]
    fn concurrent_creates_of_the_same_id_conflict() {
        let s = store();
        let mut a = tx(&s);
        let mut b = tx(&s);
        a.create("Person", Some(RecordKey::Integer(1)), vec![], Object::new())
            .unwrap();
        b.create("Person", Some(RecordKey::Integer(1)), vec![], Object::new())
            .unwrap();
        a.commit().unwrap();
        assert!(b.commit().unwrap_err().is_retryable());
    }

    #[test]
    fn concurrent_relate_and_delete_cannot_leave_a_dangling_edge() {
        let s = store();
        let mut setup = tx(&s);
        let a = setup
            .create_node(&["P"], obj(&[("id", "a".into())]))
            .unwrap();
        let b = setup
            .create_node(&["P"], obj(&[("id", "b".into())]))
            .unwrap();
        setup.commit().unwrap();

        let mut relate = tx(&s);
        let mut delete = tx(&s);
        relate.relate(&a.id, "R", &b.id, Object::new()).unwrap();
        assert!(
            delete.delete(&a.id, false).unwrap(),
            "sees no edges in its snapshot"
        );
        relate.commit().unwrap();
        assert!(delete.commit().unwrap_err().is_retryable());

        let mut check = tx(&s);
        assert!(check.get(&a.id).unwrap().is_some());
        assert_eq!(
            check.neighbors(&b.id, Direction::Incoming, None).unwrap(),
            vec![a.id]
        );
    }

    #[test]
    fn edges_cannot_be_endpoints() {
        let s = store();
        let mut t = tx(&s);
        let x = t.create_node(&["Q"], Object::new()).unwrap();
        let y = t.create_node(&["Q"], Object::new()).unwrap();
        let e = t.relate(&x.id, "S", &y.id, Object::new()).unwrap();
        assert!(matches!(
            t.relate(&e.id, "T", &y.id, Object::new()),
            Err(AtlantisError::SchemaViolation(_))
        ));
        assert!(matches!(
            t.relate(&y.id, "T", &e.id, Object::new()),
            Err(AtlantisError::SchemaViolation(_))
        ));
    }

    #[test]
    fn labels_can_be_added_and_removed() {
        let s = store();
        let mut t = tx(&s);
        let n = t.create_node(&["Person"], Object::new()).unwrap();
        let n = t
            .set_labels(&n.id, &["Admin".into(), "Person".into()], &[])
            .unwrap();
        assert_eq!(n.labels, vec!["Admin".to_string()]);
        assert_eq!(t.with_label("Admin").unwrap().len(), 1);
        t.set_labels(&n.id, &[], &["Admin".into()]).unwrap();
        assert!(
            t.with_label("Admin").unwrap().is_empty(),
            "label index entry removed"
        );
        assert!(
            t.set_labels(&n.id, &[], &["Person".into()]).is_err(),
            "table label is fixed"
        );
    }

    #[test]
    fn vector_index_definitions_live_in_the_catalog() {
        let s = store();
        let mut t = tx(&s);
        let def = VectorIndexDef {
            name: "chunk_embedding".into(),
            label: "Chunk".into(),
            field: "embedding".into(),
            dimension: 3,
            metric: DistanceMetricKind::Cosine,
            m: 16,
            ef_construction: 200,
        };
        t.define_vector_index(def.clone()).unwrap();
        assert!(t.define_vector_index(def.clone()).is_err());
        assert_eq!(
            t.vector_index("chunk_embedding").unwrap(),
            Some(def.clone())
        );
        assert_eq!(t.vector_indexes().unwrap(), vec![def]);
        assert!(
            t.tables().unwrap().is_empty(),
            "index definitions are not tables"
        );
        assert!(t.drop_index("chunk_embedding").unwrap());
        assert!(!t.drop_index("chunk_embedding").unwrap());
    }

    #[test]
    fn commit_reports_changed_records_once() {
        let s = store();
        let mut t = tx(&s);
        let a = t.create_node(&["P"], Object::new()).unwrap();
        t.merge(&a.id, obj(&[("x", 1i64.into())])).unwrap();
        let b = t.create_node(&["P"], Object::new()).unwrap();
        t.delete(&b.id, false).unwrap();
        let info = t.commit().unwrap();
        assert_eq!(info.changed.len(), 2);
        assert!(info.changed.contains(&a.id) && info.changed.contains(&b.id));
    }
}
