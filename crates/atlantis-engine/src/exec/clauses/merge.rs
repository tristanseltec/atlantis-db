//! `MERGE`: match a path, or create it when nothing matches.

use atlantis_core::AtlantisError;
use atlantis_cypher::ast::{PathPattern, SetItem};

use crate::exec::{eval::invalid, Exec, Row};

impl Exec<'_> {
    /// `MERGE path [ON CREATE SET ...] [ON MATCH SET ...]`.
    pub(crate) fn merge_clause(
        &mut self,
        rows: Vec<Row>,
        path: &PathPattern,
        on_create: &[SetItem],
        on_match: &[SetItem],
    ) -> Result<Vec<Row>, AtlantisError> {
        let mut out = Vec::new();
        for row in rows {
            self.check_merge_properties(path, &row)?;
            let found = self.match_path(path, &row, &[])?;
            if found.is_empty() {
                let mut r = row;
                self.create_path(path, &mut r)?;
                self.apply_set(on_create, &mut r)?;
                out.push(r);
            } else {
                for (mut r, _) in found {
                    self.apply_set(on_match, &mut r)?;
                    out.push(r);
                }
            }
        }
        Ok(out)
    }

    /// `MERGE` cannot match on `null`, so a null property is an error
    /// instead of silently creating a duplicate every time.
    fn check_merge_properties(
        &mut self,
        path: &PathPattern,
        row: &Row,
    ) -> Result<(), AtlantisError> {
        let props = std::iter::once(&path.start.properties).chain(
            path.steps
                .iter()
                .flat_map(|(r, n)| [&r.properties, &n.properties]),
        );
        for list in props {
            for (key, expr) in list {
                if self.eval(expr, row)?.is_null() {
                    return Err(invalid(format!(
                        "MERGE cannot use a null value for property {key}"
                    )));
                }
            }
        }
        Ok(())
    }
}
