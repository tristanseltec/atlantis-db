//! Vector index definitions in the catalog. The indexes themselves are
//! derived data, rebuilt in memory from the records they cover.

use atlantis_core::{validate_ident, AtlantisError};
use atlantis_kv::keys;

use super::codec::{decode, encode};
use super::{RecordTx, VectorIndexDef};

impl RecordTx {
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
}
