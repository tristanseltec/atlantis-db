//! Schema statements: `CREATE VECTOR INDEX`, `CREATE INDEX`, `CREATE
//! CONSTRAINT`, `DROP INDEX`, `DROP CONSTRAINT`, `SHOW INDEXES` and
//! `SHOW CONSTRAINTS`.

use atlantis_core::{AtlantisError, Value};
use atlantis_cypher::ast::{
    ConstraintRequirement, CreateConstraint, CreateIndex, CreateVectorIndex,
};
use atlantis_storage::{
    display_path, ConstraintDef, ConstraintKind, FieldPath, PropertyIndexDef, SchemaDef,
    VectorIndexDef,
};

use super::{eval::invalid, Exec, QueryResult, Row};

/// A generated name: `{prefix}_{Label}_{a}_{b_c}[_{suffix}]`.
fn generated_name(prefix: &str, label: &str, fields: &[FieldPath], suffix: &str) -> String {
    let mut name = format!("{prefix}_{label}");
    for f in fields {
        name.push('_');
        name.push_str(&f.join("_"));
    }
    if !suffix.is_empty() {
        name.push('_');
        name.push_str(suffix);
    }
    name
}

fn strings(items: impl IntoIterator<Item = String>) -> Value {
    Value::Array(items.into_iter().map(Value::String).collect())
}

fn paths(fields: &[FieldPath]) -> Value {
    strings(fields.iter().map(|f| display_path(f)))
}

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

    /// `CREATE [RANGE] INDEX`: a property index, built from existing nodes.
    pub(super) fn create_index(&mut self, def: &CreateIndex) -> Result<(), AtlantisError> {
        let name = def
            .name
            .clone()
            .unwrap_or_else(|| generated_name("index", &def.label, &def.properties, ""));
        if def.if_not_exists
            && (self.name_taken(&name)?
                || self
                    .tx
                    .equivalent_index(&def.label, &def.properties)?
                    .is_some())
        {
            return Ok(());
        }
        self.tx.define_property_index(PropertyIndexDef {
            name,
            label: def.label.clone(),
            fields: def.properties.clone(),
        })
    }

    /// `CREATE CONSTRAINT`: checks existing nodes, then enforces it on
    /// every write.
    pub(super) fn create_constraint(
        &mut self,
        def: &CreateConstraint,
    ) -> Result<(), AtlantisError> {
        let (kind, suffix) = match def.requirement {
            ConstraintRequirement::Unique => (ConstraintKind::Unique, "unique"),
            ConstraintRequirement::NotNull => (ConstraintKind::NotNull, "not_null"),
            ConstraintRequirement::NodeKey => (ConstraintKind::NodeKey, "node_key"),
        };
        let name = def
            .name
            .clone()
            .unwrap_or_else(|| generated_name("constraint", &def.label, &def.properties, suffix));
        if def.if_not_exists
            && (self.name_taken(&name)?
                || self
                    .tx
                    .equivalent_constraint(&def.label, kind, &def.properties)?
                    .is_some())
        {
            return Ok(());
        }
        self.tx.define_constraint(ConstraintDef {
            name,
            label: def.label.clone(),
            kind,
            fields: def.properties.clone(),
        })
    }

    fn name_taken(&mut self, name: &str) -> Result<bool, AtlantisError> {
        Ok(self.tx.schema_def(name)?.is_some() || self.tx.vector_index(name)?.is_some())
    }

    /// `DROP INDEX name [IF EXISTS]`: a vector or property index. The index
    /// behind a constraint goes with `DROP CONSTRAINT`.
    pub(super) fn drop_index(&mut self, name: &str, if_exists: bool) -> Result<(), AtlantisError> {
        if self.tx.drop_index(name)? {
            self.index_changes.push(name.to_string());
            return Ok(());
        }
        match self.tx.schema_def(name)? {
            Some(SchemaDef::Index(_)) => {
                self.tx.drop_schema(name)?;
                Ok(())
            }
            Some(SchemaDef::Constraint(_)) => Err(invalid(format!(
                "{name} is a constraint; drop it with DROP CONSTRAINT {name}"
            ))),
            None if if_exists => Ok(()),
            None => Err(invalid(format!("index {name} does not exist"))),
        }
    }

    /// `DROP CONSTRAINT name [IF EXISTS]`.
    pub(super) fn drop_constraint(
        &mut self,
        name: &str,
        if_exists: bool,
    ) -> Result<(), AtlantisError> {
        match self.tx.schema_def(name)? {
            Some(SchemaDef::Constraint(_)) => {
                self.tx.drop_schema(name)?;
                Ok(())
            }
            Some(SchemaDef::Index(_)) => Err(invalid(format!(
                "{name} is an index; drop it with DROP INDEX {name}"
            ))),
            None if if_exists => Ok(()),
            None => Err(invalid(format!("constraint {name} does not exist"))),
        }
    }

    /// `SHOW INDEXES`: property, constraint-backed and vector indexes.
    pub(super) fn show_indexes(&mut self) -> Result<QueryResult, AtlantisError> {
        let mut rows = Vec::new();
        for def in self.tx.schema()?.iter() {
            let owner = match def {
                SchemaDef::Index(_) => Value::Null,
                SchemaDef::Constraint(c) if c.kind != ConstraintKind::NotNull => {
                    Value::String(c.name.clone())
                }
                SchemaDef::Constraint(_) => continue,
            };
            rows.push(vec![
                Value::String(def.name().to_string()),
                Value::String("RANGE".into()),
                strings([def.label().to_string()]),
                paths(def.fields()),
                owner,
            ]);
        }
        for v in self.tx.vector_indexes()? {
            rows.push(vec![
                Value::String(v.name),
                Value::String("VECTOR".into()),
                strings([v.label]),
                strings([v.field]),
                Value::Null,
            ]);
        }
        rows.sort_by(|a, b| a[0].to_string().cmp(&b[0].to_string()));
        Ok(QueryResult {
            columns: [
                "name",
                "type",
                "labelsOrTypes",
                "properties",
                "owningConstraint",
            ]
            .map(String::from)
            .to_vec(),
            rows,
        })
    }

    /// `SHOW CONSTRAINTS`.
    pub(super) fn show_constraints(&mut self) -> Result<QueryResult, AtlantisError> {
        let mut rows = Vec::new();
        for def in self.tx.schema()?.iter() {
            let SchemaDef::Constraint(c) = def else {
                continue;
            };
            let kind = match c.kind {
                ConstraintKind::Unique => "UNIQUENESS",
                ConstraintKind::NotNull => "NODE_PROPERTY_EXISTENCE",
                ConstraintKind::NodeKey => "NODE_KEY",
            };
            rows.push(vec![
                Value::String(c.name.clone()),
                Value::String(kind.into()),
                strings([c.label.clone()]),
                paths(&c.fields),
            ]);
        }
        Ok(QueryResult {
            columns: ["name", "type", "labelsOrTypes", "properties"]
                .map(String::from)
                .to_vec(),
            rows,
        })
    }
}
