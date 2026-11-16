//! Record create, read, update, merge, delete and table scans.

use atlantis_core::{AtlantisError, Object, Record, RecordId, RecordKey, Value, DEFAULT_TABLE};
use atlantis_kv::keys;

use super::codec::{decode, Stored};
use super::RecordTx;

impl RecordTx {
    // ---- records ---------------------------------------------------------

    /// Takes an explicit key from `key`, or from an `id` field in `data`.
    pub(super) fn take_key(
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
        self.put_record(&record.id, rkey, &Stored::Record(record.clone()))?;
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

    pub(super) fn get_stored(&mut self, id: &RecordId) -> Result<Option<Stored>, AtlantisError> {
        let key = keys::record(&self.db, id)?;
        self.get_decoded(&key)
    }

    /// Reads a record. Edge records are returned as documents with `in` and
    /// `out` fields.
    pub fn get(&mut self, id: &RecordId) -> Result<Option<Record>, AtlantisError> {
        Ok(self.get_stored(id)?.map(Stored::into_record))
    }

    pub(super) fn not_found(id: &RecordId) -> AtlantisError {
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

    pub(super) fn write_back(&mut self, stored: Stored) -> Result<Record, AtlantisError> {
        let id = match &stored {
            Stored::Record(r) => &r.id,
            Stored::Edge(e) => &e.id,
        };
        let key = keys::record(&self.db, id)?;
        self.put_record(id, key, &stored)?;
        Ok(stored.into_record())
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
            .map(|(_, v)| decode::<Stored>(v).map(Stored::into_record))
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
}
