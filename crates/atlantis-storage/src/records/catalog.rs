//! Catalog operations: namespace, database and table definitions.

use atlantis_core::{validate_ident, validate_user_table, AtlantisError, DEFAULT_TABLE};
use atlantis_kv::keys;
use serde::de::DeserializeOwned;

use super::codec::{decode, encode};
use super::{RecordTx, TableDef, TableKind};

impl RecordTx {
    pub(super) fn get_decoded<T: DeserializeOwned>(
        &mut self,
        key: &[u8],
    ) -> Result<Option<T>, AtlantisError> {
        self.tx.get(key)?.map(|bytes| decode(&bytes)).transpose()
    }

    /// Registers the namespace and database in the catalog on first write.
    pub(super) fn ensure_database(&mut self) -> Result<(), AtlantisError> {
        if self.catalog_checked {
            return Ok(());
        }
        // Idempotent: concurrent first writes register identical entries
        // without conflicting.
        let ns_key = keys::namespace_def(&self.db.ns);
        if self.tx.get(&ns_key)?.is_none() {
            self.tx.set_idempotent(ns_key, encode(&self.db.ns)?)?;
        }
        let db_key = keys::database_def(&self.db);
        if self.tx.get(&db_key)?.is_none() {
            self.tx.set_idempotent(db_key, encode(&self.db)?)?;
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
    pub(super) fn resolve_table(
        &mut self,
        name: &str,
        edge: bool,
    ) -> Result<TableDef, AtlantisError> {
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
        // An implicit definition is deterministic, so concurrent first
        // writes to a new table do not conflict over it.
        self.tx
            .set_idempotent(keys::table_def(&self.db, name), encode(&def)?)?;
        Ok(def)
    }
}
