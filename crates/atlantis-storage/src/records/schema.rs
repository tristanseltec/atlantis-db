//! Property indexes and constraints.
//!
//! Definitions live in the catalog under `/sc {name}`. Their entries are
//! ordinary keys written in the same transaction as the record they
//! describe, so an index is never stale for any snapshot:
//!
//! - a **property index** has one entry per indexed value and record
//!   (`/px {index} {values} {record}`). A single-field index is *multikey*:
//!   an array field gets one entry per distinct element. A composite index
//!   stores whole values and skips records that miss any field.
//! - a **unique** or **node key** constraint has one entry per value
//!   (`/ux {constraint} {values}` → record id). Two transactions that claim
//!   the same value write the same key, so the second to commit fails with
//!   a write-write conflict at every isolation level.
//! - **not null** and **node key** constraints are checked on every write.
//!
//! `null` and missing values are never indexed. Values that cannot be
//! indexed (objects, vectors, geometries, NaN) are skipped by property
//! indexes and rejected by unique constraints, which could not check them.
//!
//! Schema changes and concurrent writers always conflict, at every
//! isolation level, so no writer can commit a record without the entries
//! of an index created after its snapshot:
//!
//! - every schema change writes a new schema version
//!   ([`keys::schema_version`]), and every transaction that writes a record
//!   watches that key ([`KvTransaction::watch`]): a writer whose snapshot
//!   predates a committed schema change fails to commit;
//! - an index or constraint build watches the records it read: a schema
//!   change whose snapshot predates a committed write fails to commit.
//!
//! The losing transaction gets [`AtlantisError::TxConflict`] and can retry.
//! Writers do not write anything for this, so they never conflict with
//! each other over it.
//!
//! [`KvTransaction::watch`]: atlantis_kv::KvTransaction::watch

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::sync::{Arc, OnceLock};

use atlantis_core::{validate_ident, AtlantisError, Object, Record, RecordId, Ulid, Value};
use atlantis_kv::index_encoding::{
    append_index_value, append_index_value_or_opaque, eq_seekable, index_value, range_bounds,
    string_prefix_bounds, RangeClass,
};
use atlantis_kv::{keys, KeyRange};
use serde::{Deserialize, Serialize};

use super::codec::{decode, encode};
use super::RecordTx;

/// The schema of a database in which no schema change ever ran.
static EMPTY_SCHEMA: OnceLock<Arc<Vec<SchemaDef>>> = OnceLock::new();

/// A path to a possibly nested field, e.g. `["address", "city"]`.
pub type FieldPath = Vec<String>;

/// Formats a field path as `a.b.c`.
pub fn display_path(path: &[String]) -> String {
    path.join(".")
}

/// A property index on nodes carrying `label`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PropertyIndexDef {
    pub name: String,
    pub label: String,
    /// One field for a single-field index, several for a composite one.
    pub fields: Vec<FieldPath>,
}

/// What a constraint requires.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConstraintKind {
    /// No two nodes share the same (non-null) values.
    Unique,
    /// The field is present and not null.
    NotNull,
    /// Every field is present and not null, and the values are unique.
    NodeKey,
}

impl ConstraintKind {
    fn checks_unique(self) -> bool {
        matches!(self, ConstraintKind::Unique | ConstraintKind::NodeKey)
    }

    fn checks_not_null(self) -> bool {
        matches!(self, ConstraintKind::NotNull | ConstraintKind::NodeKey)
    }
}

impl fmt::Display for ConstraintKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            ConstraintKind::Unique => "UNIQUE",
            ConstraintKind::NotNull => "NOT NULL",
            ConstraintKind::NodeKey => "NODE KEY",
        })
    }
}

/// A constraint on nodes carrying `label`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConstraintDef {
    pub name: String,
    pub label: String,
    pub kind: ConstraintKind,
    pub fields: Vec<FieldPath>,
}

/// A property index or a constraint, as stored in the catalog.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SchemaDef {
    Index(PropertyIndexDef),
    Constraint(ConstraintDef),
}

impl SchemaDef {
    pub fn name(&self) -> &str {
        match self {
            SchemaDef::Index(d) => &d.name,
            SchemaDef::Constraint(d) => &d.name,
        }
    }

    pub fn label(&self) -> &str {
        match self {
            SchemaDef::Index(d) => &d.label,
            SchemaDef::Constraint(d) => &d.label,
        }
    }

    pub fn fields(&self) -> &[FieldPath] {
        match self {
            SchemaDef::Index(d) => &d.fields,
            SchemaDef::Constraint(d) => &d.fields,
        }
    }

    /// The index this definition provides for lookups, if any.
    pub fn index_info(&self) -> Option<IndexInfo> {
        match self {
            SchemaDef::Index(d) => Some(IndexInfo {
                name: d.name.clone(),
                label: d.label.clone(),
                fields: d.fields.clone(),
                unique: false,
                multikey: d.fields.len() == 1,
                complete: false,
            }),
            SchemaDef::Constraint(c) if c.kind.checks_unique() => Some(IndexInfo {
                name: c.name.clone(),
                label: c.label.clone(),
                fields: c.fields.clone(),
                unique: true,
                multikey: false,
                complete: c.kind == ConstraintKind::NodeKey,
            }),
            SchemaDef::Constraint(_) => None,
        }
    }
}

/// An index that lookups can use: a property index, or the entries of a
/// unique or node key constraint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexInfo {
    pub name: String,
    pub label: String,
    pub fields: Vec<FieldPath>,
    /// Backed by a unique constraint (one record per value).
    pub unique: bool,
    /// Array fields are indexed element by element.
    pub multikey: bool,
    /// Every node with the label has an entry (a node key). Otherwise a
    /// node has an entry only when all fields are non-null, so a composite
    /// index answers a query only if it constrains every field.
    pub complete: bool,
}

/// What to look up in an index. Results are *candidates*: a superset of
/// the matching records, which the caller re-checks.
#[derive(Debug, Clone, PartialEq)]
pub enum IndexLookup {
    /// Equality on the first `values.len()` fields.
    Eq(Vec<Value>),
    /// Values of the first field between the bounds, inclusive. Both bounds
    /// must be of the same class (numbers, strings, datetimes, durations).
    Range {
        lower: Option<Value>,
        upper: Option<Value>,
    },
    /// String values of the first field starting with a prefix.
    Prefix(String),
}

/// Reads the value at `path` inside a document.
pub fn field_at<'a>(data: &'a Object, path: &[String]) -> Option<&'a Value> {
    let (first, rest) = path.split_first()?;
    rest.iter()
        .try_fold(data.get(first)?, |v, key| v.as_object()?.get(key))
}

fn present<'a>(data: &'a Object, path: &[String]) -> Option<&'a Value> {
    field_at(data, path).filter(|v| !v.is_null())
}

/// Encoded values of `record` for a property index: one entry per distinct
/// element for an array in a single-field index, else one entry for the
/// concatenated whole values (none if any field is missing or null).
fn index_parts(def: &PropertyIndexDef, record: &Record) -> Vec<Vec<u8>> {
    if let [path] = def.fields.as_slice() {
        return match present(&record.data, path) {
            None => Vec::new(),
            Some(Value::Array(items)) => {
                let mut parts: Vec<Vec<u8>> = items
                    .iter()
                    .filter(|v| !matches!(v, Value::Array(_)))
                    .filter_map(index_value)
                    .collect();
                parts.sort();
                parts.dedup();
                parts
            }
            Some(v) => index_value(v).into_iter().collect(),
        };
    }
    // Composite: an entry exactly when every field is non-null, so that a
    // query with a condition on every field can rely on the index.
    let mut out = Vec::new();
    for path in &def.fields {
        match present(&record.data, path) {
            Some(v) => {
                append_index_value_or_opaque(&mut out, v);
            }
            None => return Vec::new(),
        }
    }
    vec![out]
}

/// The encoded values of `record` for a unique constraint; `None` if a
/// field is missing. With `strict`, a value that cannot be indexed is an
/// error (the constraint could not check it); otherwise it yields `None`.
fn unique_part(
    def: &ConstraintDef,
    record: &Record,
    strict: bool,
) -> Result<Option<Vec<u8>>, AtlantisError> {
    let mut out = Vec::new();
    for path in &def.fields {
        let Some(v) = present(&record.data, path) else {
            return Ok(None);
        };
        if !append_index_value(&mut out, v) {
            if !strict {
                return Ok(None);
            }
            return Err(AtlantisError::ConstraintViolation(format!(
                "constraint {} cannot check a {} value in {}.{}",
                def.name,
                v.type_name(),
                record.id,
                display_path(path)
            )));
        }
    }
    Ok(Some(out))
}

fn check_not_null(def: &ConstraintDef, record: &Record) -> Result<(), AtlantisError> {
    for path in &def.fields {
        if present(&record.data, path).is_none() {
            return Err(AtlantisError::ConstraintViolation(format!(
                "{} must have {} (constraint {}: {} IS {})",
                record.id,
                display_path(path),
                def.name,
                display_path(path),
                if def.kind == ConstraintKind::NodeKey {
                    "NODE KEY"
                } else {
                    "NOT NULL"
                }
            )));
        }
    }
    Ok(())
}

fn duplicate(def: &ConstraintDef, record: &Record, other: &RecordId) -> AtlantisError {
    let values: Vec<String> = def
        .fields
        .iter()
        .map(|p| {
            let v = field_at(&record.data, p).cloned().unwrap_or(Value::Null);
            format!("{} = {v}", display_path(p))
        })
        .collect();
    AtlantisError::ConstraintViolation(format!(
        "{} already has {} (constraint {}), so {} cannot",
        other,
        values.join(", "),
        def.name,
        record.id
    ))
}

fn validate_fields(fields: &[FieldPath]) -> Result<(), AtlantisError> {
    if fields.is_empty() {
        return Err(AtlantisError::SchemaViolation(
            "an index or constraint needs at least one property".into(),
        ));
    }
    for path in fields {
        if path.is_empty() {
            return Err(AtlantisError::SchemaViolation("empty property path".into()));
        }
        path.iter().try_for_each(|k| validate_ident("field", k))?;
        if path.first().is_some_and(|k| k == "id") {
            return Err(AtlantisError::SchemaViolation(
                "id is the record key, which is always unique and indexed".into(),
            ));
        }
    }
    let distinct: HashSet<&FieldPath> = fields.iter().collect();
    if distinct.len() != fields.len() {
        return Err(AtlantisError::SchemaViolation(
            "a property is listed twice".into(),
        ));
    }
    Ok(())
}

impl RecordTx {
    // ---- catalog ---------------------------------------------------------

    /// All property index and constraint definitions, ordered by name.
    /// Cached for the life of the transaction.
    pub fn schema(&mut self) -> Result<Arc<Vec<SchemaDef>>, AtlantisError> {
        if let Some(schema) = &self.schema {
            return Ok(schema.clone());
        }
        let key = self
            .schema_version_key
            .get_or_insert_with(|| keys::schema_version(&self.db));
        let Some(version) = self.tx.get(key)? else {
            // No schema change ever ran in this database: nothing to read.
            let empty = EMPTY_SCHEMA.get_or_init(Arc::default).clone();
            self.schema = Some(empty.clone());
            return Ok(empty);
        };
        let cached = self.schemas.lock().ok().and_then(|c| {
            c.get(&self.db)
                .filter(|(v, _)| *v == version)
                .map(|(_, s)| s.clone())
        });
        let defs = match cached {
            Some(defs) => defs,
            None => {
                let defs: Vec<SchemaDef> = self
                    .tx
                    .scan(&keys::schema_defs(&self.db), None)?
                    .iter()
                    .map(|(_, v)| decode(v))
                    .collect::<Result<_, _>>()?;
                let defs = Arc::new(defs);
                if let Ok(mut cache) = self.schemas.lock() {
                    cache.insert(self.db.clone(), (version, defs.clone()));
                }
                defs
            }
        };
        self.schema = Some(defs.clone());
        Ok(defs)
    }

    /// Returns a property index or constraint definition by name.
    pub fn schema_def(&mut self, name: &str) -> Result<Option<SchemaDef>, AtlantisError> {
        Ok(self.schema()?.iter().find(|d| d.name() == name).cloned())
    }

    /// The indexes lookups can use for nodes carrying `label`.
    pub fn indexes_for(&mut self, label: &str) -> Result<Vec<IndexInfo>, AtlantisError> {
        Ok(self
            .schema()?
            .iter()
            .filter(|d| d.label() == label)
            .filter_map(SchemaDef::index_info)
            .collect())
    }

    /// Watches the schema version once per transaction (see the module
    /// documentation) and returns the schema writes must maintain.
    fn schema_for_write(&mut self) -> Result<Arc<Vec<SchemaDef>>, AtlantisError> {
        if !self.schema_watched {
            let key = self.schema_version_key();
            self.tx.watch(keys::single_key(key))?;
            self.schema_watched = true;
        }
        self.schema()
    }

    /// Writes a new schema version, so the change conflicts with every
    /// concurrent writer, and drops the transaction's cached schema.
    fn bump_schema_version(&mut self) -> Result<(), AtlantisError> {
        let version = Ulid::new().0.to_be_bytes().to_vec();
        let key = self.schema_version_key();
        self.tx.set(key, version)?;
        self.schema_watched = true;
        self.schema = None;
        Ok(())
    }

    /// The key of this database's schema version, built once per
    /// transaction.
    fn schema_version_key(&mut self) -> Vec<u8> {
        self.schema_version_key
            .get_or_insert_with(|| keys::schema_version(&self.db))
            .clone()
    }

    /// Makes this schema change conflict with any concurrent write to the
    /// nodes it reads: records of table `label` and nodes carrying `label`
    /// as a secondary label.
    fn watch_label(&mut self, label: &str) -> Result<(), AtlantisError> {
        self.tx.watch(keys::records(&self.db, label))?;
        self.tx.watch(keys::label_range(&self.db, label))
    }

    /// Fails if `name` is taken by any index or constraint.
    pub(super) fn check_schema_name_free(&mut self, name: &str) -> Result<(), AtlantisError> {
        validate_ident("index", name)?;
        if self.schema_def(name)?.is_some() || self.vector_index(name)?.is_some() {
            return Err(AtlantisError::SchemaViolation(format!(
                "an index or constraint named {name} already exists"
            )));
        }
        Ok(())
    }

    fn store_def(&mut self, def: &SchemaDef) -> Result<(), AtlantisError> {
        self.ensure_database()?;
        self.tx
            .set(keys::schema_def(&self.db, def.name()), encode(def)?)?;
        self.bump_schema_version()
    }

    /// Defines a property index and indexes the existing nodes. Fails if
    /// the name is taken or an index on the same label and fields exists.
    pub fn define_property_index(&mut self, def: PropertyIndexDef) -> Result<(), AtlantisError> {
        validate_ident("label", &def.label)?;
        validate_fields(&def.fields)?;
        if let Some(same) = self.equivalent_index(&def.label, &def.fields)? {
            return Err(AtlantisError::SchemaViolation(format!(
                "index {same} already covers these properties"
            )));
        }
        self.check_schema_name_free(&def.name)?;
        self.watch_label(&def.label)?;
        let mut entries = Vec::new();
        for record in self.with_label(&def.label)? {
            for part in index_parts(&def, &record) {
                let key = keys::index_entry(&self.db, &def.name, &part, &record.id)?;
                entries.push((key, encode(&record.id)?));
            }
        }
        for (key, value) in entries {
            self.tx.set(key, value)?;
        }
        self.store_def(&SchemaDef::Index(def))
    }

    /// The name of a property index on exactly `label` and `fields`.
    pub fn equivalent_index(
        &mut self,
        label: &str,
        fields: &[FieldPath],
    ) -> Result<Option<String>, AtlantisError> {
        Ok(self.schema()?.iter().find_map(|d| match d {
            SchemaDef::Index(i) if i.label == label && i.fields == fields => Some(i.name.clone()),
            _ => None,
        }))
    }

    /// The name of a constraint of `kind` on exactly `label` and `fields`.
    pub fn equivalent_constraint(
        &mut self,
        label: &str,
        kind: ConstraintKind,
        fields: &[FieldPath],
    ) -> Result<Option<String>, AtlantisError> {
        Ok(self.schema()?.iter().find_map(|d| match d {
            SchemaDef::Constraint(c)
                if c.label == label && c.kind == kind && c.fields == fields =>
            {
                Some(c.name.clone())
            }
            _ => None,
        }))
    }

    /// Defines a constraint after checking every existing node against it.
    /// Fails with [`AtlantisError::ConstraintViolation`] naming the first
    /// offending node.
    pub fn define_constraint(&mut self, def: ConstraintDef) -> Result<(), AtlantisError> {
        validate_ident("label", &def.label)?;
        validate_fields(&def.fields)?;
        if def.kind == ConstraintKind::NotNull && def.fields.len() != 1 {
            return Err(AtlantisError::SchemaViolation(
                "a NOT NULL constraint covers exactly one property".into(),
            ));
        }
        if let Some(same) = self.equivalent_constraint(&def.label, def.kind, &def.fields)? {
            return Err(AtlantisError::SchemaViolation(format!(
                "constraint {same} already requires this"
            )));
        }
        self.check_schema_name_free(&def.name)?;
        self.watch_label(&def.label)?;
        let mut claimed: HashMap<Vec<u8>, RecordId> = HashMap::new();
        let mut entries = Vec::new();
        for record in self.with_label(&def.label)? {
            if def.kind.checks_not_null() {
                check_not_null(&def, &record)?;
            }
            if def.kind.checks_unique() {
                if let Some(part) = unique_part(&def, &record, true)? {
                    if let Some(other) = claimed.get(&part) {
                        return Err(duplicate(&def, &record, other));
                    }
                    entries.push((
                        keys::unique_entry(&self.db, &def.name, &part),
                        encode(&record.id)?,
                    ));
                    claimed.insert(part, record.id.clone());
                }
            }
        }
        for (k, v) in entries {
            self.tx.set(k, v)?;
        }
        self.store_def(&SchemaDef::Constraint(def))
    }

    /// Drops a property index or constraint and all of its entries.
    /// Returns the dropped definition, or `None` if none had that name.
    pub fn drop_schema(&mut self, name: &str) -> Result<Option<SchemaDef>, AtlantisError> {
        let Some(def) = self.schema_def(name)? else {
            return Ok(None);
        };
        let prefix = match &def {
            SchemaDef::Index(_) => keys::index_entries(&self.db, name),
            SchemaDef::Constraint(c) if c.kind.checks_unique() => {
                keys::unique_entries(&self.db, name)
            }
            SchemaDef::Constraint(_) => Vec::new(),
        };
        if !prefix.is_empty() {
            let range = KeyRange::prefix(prefix);
            self.tx.watch(range.clone())?;
            for (key, _) in self.tx.scan(&range, None)? {
                self.tx.delete(&key)?;
            }
        }
        self.tx.delete(&keys::schema_def(&self.db, name))?;
        self.bump_schema_version()?;
        Ok(Some(def))
    }

    // ---- maintenance -----------------------------------------------------

    /// `true` if writes must maintain indexes or check constraints. Watches
    /// the schema version, so call it before writing any record.
    pub(super) fn has_schema(&mut self) -> Result<bool, AtlantisError> {
        Ok(!self.schema_for_write()?.is_empty())
    }

    /// Brings index entries up to date for a record changing from `old` to
    /// `new` (`None` for a create or a delete) and checks constraints on
    /// `new`. All checks run before the first write, so a failure leaves
    /// no partial entries behind.
    pub(super) fn maintain(
        &mut self,
        id: &RecordId,
        old: Option<&Record>,
        new: Option<&Record>,
    ) -> Result<(), AtlantisError> {
        let schema = self.schema_for_write()?;
        if schema.is_empty() {
            return Ok(());
        }
        let id_bytes = encode(id)?;
        let mut deletes = Vec::new();
        let mut sets = Vec::new();
        for def in schema.iter() {
            match def {
                SchemaDef::Index(ix) => {
                    let parts = |r: Option<&Record>| {
                        r.filter(|r| r.has_label(&ix.label))
                            .map(|r| index_parts(ix, r))
                            .unwrap_or_default()
                    };
                    let (before, after) = (parts(old), parts(new));
                    for p in before.iter().filter(|p| !after.contains(p)) {
                        deletes.push(keys::index_entry(&self.db, &ix.name, p, id)?);
                    }
                    for p in after.iter().filter(|p| !before.contains(p)) {
                        sets.push((
                            keys::index_entry(&self.db, &ix.name, p, id)?,
                            id_bytes.clone(),
                        ));
                    }
                }
                SchemaDef::Constraint(c) => {
                    let new = new.filter(|r| r.has_label(&c.label));
                    if let (true, Some(r)) = (c.kind.checks_not_null(), new) {
                        check_not_null(c, r)?;
                    }
                    if !c.kind.checks_unique() {
                        continue;
                    }
                    let before = match old.filter(|r| r.has_label(&c.label)) {
                        Some(r) => unique_part(c, r, false)?,
                        None => None,
                    };
                    let after = match new {
                        Some(r) => unique_part(c, r, true)?,
                        None => None,
                    };
                    if before == after {
                        continue;
                    }
                    if let Some(p) = before {
                        deletes.push(keys::unique_entry(&self.db, &c.name, &p));
                    }
                    if let Some(p) = after {
                        let key = keys::unique_entry(&self.db, &c.name, &p);
                        if let Some(owner) = self.tx.get(&key)? {
                            let owner: RecordId = decode(&owner)?;
                            if &owner != id {
                                return Err(duplicate(c, new.expect("after implies new"), &owner));
                            }
                        }
                        sets.push((key, id_bytes.clone()));
                    }
                }
            }
        }
        for key in deletes {
            self.tx.delete(&key)?;
        }
        for (key, value) in sets {
            self.tx.set(key, value)?;
        }
        Ok(())
    }

    // ---- lookups ---------------------------------------------------------

    /// Looks up candidate records in an index, in index order without
    /// duplicates. Returns `None` when the index cannot answer this lookup
    /// exactly enough (for example an integer beyond ±2^53, or a value that
    /// is never indexed); the caller then scans instead. The candidates are
    /// a superset of the records that match: re-check the predicate.
    pub fn index_lookup(
        &mut self,
        index: &IndexInfo,
        lookup: &IndexLookup,
    ) -> Result<Option<Vec<RecordId>>, AtlantisError> {
        let prefix = if index.unique {
            keys::unique_entries(&self.db, &index.name)
        } else {
            keys::index_entries(&self.db, &index.name)
        };
        let range = match lookup {
            IndexLookup::Eq(values) => {
                if values.is_empty() || values.len() > index.fields.len() {
                    return Ok(None);
                }
                // `x = null` is never true.
                if values.iter().any(Value::is_null) {
                    return Ok(Some(Vec::new()));
                }
                if !values.iter().all(eq_seekable)
                    || (index.multikey && matches!(values[0], Value::Array(_)))
                {
                    return Ok(None);
                }
                let mut key = prefix;
                for v in values {
                    if !append_index_value(&mut key, v) {
                        return Ok(None);
                    }
                }
                KeyRange::prefix(key)
            }
            IndexLookup::Range { lower, upper } => {
                if lower.as_ref().is_some_and(Value::is_null)
                    || upper.as_ref().is_some_and(Value::is_null)
                {
                    return Ok(Some(Vec::new()));
                }
                let Some(class) = lower.as_ref().or(upper.as_ref()).and_then(RangeClass::of) else {
                    return Ok(None);
                };
                let Some((start, end)) = range_bounds(class, lower.as_ref(), upper.as_ref()) else {
                    return Ok(None);
                };
                KeyRange {
                    start: [prefix.as_slice(), &start].concat(),
                    end: [prefix.as_slice(), &end].concat(),
                }
            }
            IndexLookup::Prefix(p) => {
                let Some((start, end)) = string_prefix_bounds(p) else {
                    return Ok(None);
                };
                KeyRange {
                    start: [prefix.as_slice(), &start].concat(),
                    end: [prefix.as_slice(), &end].concat(),
                }
            }
        };
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        for (_, value) in self.tx.scan(&range, None)? {
            if seen.insert(value.clone()) {
                out.push(decode(&value)?);
            }
        }
        Ok(Some(out))
    }
}

#[cfg(test)]
mod tests;
