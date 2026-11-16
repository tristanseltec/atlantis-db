//! Index statements: `CREATE VECTOR INDEX` and `DROP INDEX`.

use atlantis_core::{AtlantisError, Value};
use atlantis_cypher::ast::CreateVectorIndex;
use atlantis_storage::VectorIndexDef;

use super::{eval::invalid, Exec, Row};

impl Exec<'_> {
    pub(super) fn create_vector_index(
        &mut self,
        def: &CreateVectorIndex,
    ) -> Result<(), AtlantisError> {
        if self.tx.vector_index(&def.name)?.is_some() {
            if def.if_not_exists {
                return Ok(());
            }
            return Err(invalid(format!("index {} already exists", def.name)));
        }
        let mut index = VectorIndexDef {
            name: def.name.clone(),
            label: def.label.clone(),
            field: def.property.clone(),
            dimension: self.vector_defaults.dimensions,
            metric: self.vector_defaults.distance_metric,
            m: self.vector_defaults.hnsw.m,
            ef_construction: self.vector_defaults.hnsw.ef_construction,
        };
        for (key, expr) in &def.options {
            let value = self.eval(expr, &Row::new())?;
            let int = || -> Result<usize, AtlantisError> {
                match value {
                    Value::Integer(i) if i > 0 => Ok(i as usize),
                    _ => Err(invalid(format!(
                        "vector index option {key} must be a positive integer"
                    ))),
                }
            };
            match key.as_str() {
                "dimension" | "dimensions" | "vector.dimensions" => index.dimension = int()?,
                "m" => index.m = int()?,
                "efConstruction" | "ef_construction" => index.ef_construction = int()?,
                "metric" | "similarity" | "vector.similarity_function" => {
                    let Value::String(name) = &value else {
                        return Err(invalid("metric must be a string"));
                    };
                    index.metric = atlantis_core::DistanceMetricKind::parse(name)?;
                }
                other => return Err(invalid(format!("unknown vector index option {other}"))),
            }
        }
        self.tx.define_vector_index(index)?;
        self.index_changes.push(def.name.clone());
        Ok(())
    }

    /// `DROP INDEX name [IF EXISTS]`.
    pub(super) fn drop_index(&mut self, name: &str, if_exists: bool) -> Result<(), AtlantisError> {
        if !self.tx.drop_index(name)? && !if_exists {
            return Err(invalid(format!("index {name} does not exist")));
        }
        self.index_changes.push(name.to_string());
        Ok(())
    }
}
