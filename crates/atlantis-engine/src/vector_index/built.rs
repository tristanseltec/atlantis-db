//! One built index: the HNSW graph, its slot bookkeeping and the columns
//! kept for filtered search.

use std::collections::HashMap;

use atlantis_core::{AtlantisError, DistanceMetricKind, NodeId, RecordId, Value};
use atlantis_storage::{RecordTx, VectorIndexDef};
use atlantis_vector::{AllowSet, HnswIndex, HnswParams};

use super::{column::Column, FieldFilter};

pub(super) struct Built {
    pub(super) def: VectorIndexDef,
    pub(super) hnsw: HnswIndex,
    /// Internal id → record id.
    pub(super) records: HashMap<u64, RecordId>,
    /// Record id text → internal id of its current vector.
    pub(super) slots: HashMap<String, u64>,
    pub(super) next: u64,
    /// Removed entries still present as HNSW tombstones.
    deleted: usize,
    /// Field name → column, for filtered search.
    pub(super) columns: HashMap<String, Column>,
}

impl Built {
    pub(super) fn new(def: VectorIndexDef, defaults: &HnswParams) -> Self {
        let params = HnswParams {
            m: def.m,
            m0: def.m * 2,
            ef_construction: def.ef_construction,
            ..defaults.clone()
        };
        let hnsw = HnswIndex::new(params, def.dimension, def.metric);
        Built {
            def,
            hnsw,
            records: HashMap::new(),
            slots: HashMap::new(),
            next: 0,
            deleted: 0,
            columns: HashMap::new(),
        }
    }

    /// Records `record`'s fields in the columns for `slot`.
    fn set_columns(&mut self, slot: u64, record: &atlantis_core::Record) {
        for (field, column) in self.columns.iter_mut() {
            column.set(slot, record.get(field).cloned().unwrap_or(Value::Null));
        }
    }

    /// Builds the columns for `fields` that do not exist yet, from the
    /// records of the index's label.
    pub(super) fn add_columns(
        &mut self,
        tx: &mut RecordTx,
        fields: &[&str],
    ) -> Result<(), AtlantisError> {
        let missing: Vec<String> = fields
            .iter()
            .filter(|f| !self.columns.contains_key(**f))
            .map(|f| f.to_string())
            .collect();
        if missing.is_empty() {
            return Ok(());
        }
        let mut built: Vec<(String, Column)> = missing
            .into_iter()
            .map(|f| (f, Column::default()))
            .collect();
        for record in tx.with_label(&self.def.label)? {
            let Some(&slot) = self.slots.get(&record.id.to_string()) else {
                continue;
            };
            for (field, column) in &mut built {
                column.set(slot, record.get(field).cloned().unwrap_or(Value::Null));
            }
        }
        self.columns.extend(built);
        Ok(())
    }

    /// The live slots that meet every filter.
    pub(super) fn allow_set(&self, filters: &[FieldFilter]) -> AllowSet {
        let mut sets = filters.iter().map(|f| {
            self.columns
                .get(&f.field)
                .map(|c| c.matching(f))
                .unwrap_or_default()
        });
        let mut allow = sets.next().unwrap_or_default();
        for s in sets {
            if allow.is_empty() {
                break;
            }
            allow.intersect(&s);
        }
        allow
    }

    pub(super) fn vector_of(&self, value: Option<&Value>) -> Option<Vec<f32>> {
        let v: Vec<f32> = match value? {
            Value::Vector(v) => v.clone(),
            Value::Array(items) => items
                .iter()
                .map(|x| x.as_float().map(|f| f as f32))
                .collect::<Option<_>>()?,
            _ => return None,
        };
        (v.len() == self.def.dimension).then_some(v)
    }

    fn remove(&mut self, id: &RecordId) {
        if let Some(slot) = self.slots.remove(&id.to_string()) {
            self.hnsw.remove(NodeId(slot));
            self.records.remove(&slot);
            self.deleted += 1;
            for column in self.columns.values_mut() {
                if let Some(v) = column.values.get_mut(slot as usize) {
                    *v = Value::Null;
                }
            }
        }
    }

    /// Re-reads one record and updates its entry. An unchanged vector is
    /// left in place, so writes to other fields do not churn the graph.
    pub(super) fn refresh(
        &mut self,
        tx: &mut RecordTx,
        id: &RecordId,
    ) -> Result<(), AtlantisError> {
        let record = tx.get(id)?.filter(|r| r.has_label(&self.def.label));
        let vector = record
            .as_ref()
            .and_then(|r| self.vector_of(r.get(&self.def.field)));
        if let (Some(v), Some(&slot)) = (&vector, self.slots.get(&id.to_string())) {
            if self.hnsw.get_vector(NodeId(slot)) == Some(v.as_slice()) {
                // Same vector: only filtered fields may have changed.
                if let Some(record) = &record {
                    self.set_columns(slot, record);
                }
                return Ok(());
            }
        }
        self.remove(id);
        if let (Some(vector), Some(record)) = (vector, record) {
            let slot = self.next;
            self.next += 1;
            self.hnsw.insert(NodeId(slot), vector)?;
            self.records.insert(slot, id.clone());
            self.slots.insert(id.to_string(), slot);
            self.set_columns(slot, &record);
        }
        Ok(())
    }

    /// `true` once tombstones outnumber live entries; searching such a
    /// graph loses recall, so it is rebuilt.
    pub(super) fn needs_rebuild(&self) -> bool {
        self.deleted > 1024.max(self.slots.len())
    }

    pub(super) fn score(&self, distance: f32) -> f64 {
        let d = distance as f64;
        match self.def.metric {
            DistanceMetricKind::Cosine => 1.0 - d,
            DistanceMetricKind::DotProduct => -d,
            DistanceMetricKind::Euclidean => 1.0 / (1.0 + d),
        }
    }
}
