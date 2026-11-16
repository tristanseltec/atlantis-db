//! The clauses of a query, one module per family: reading (`MATCH`,
//! `OPTIONAL MATCH`, `WITH`, `UNWIND`), writing (`CREATE`, `SET`, `REMOVE`,
//! `DELETE`), `MERGE`, and `CALL`. Each clause maps the incoming rows to the
//! rows the next clause sees.

mod call;
mod merge;
mod reading;
mod writing;

use atlantis_core::AtlantisError;
use atlantis_cypher::ast::Expression;

pub(super) use call::yielded_result;

use super::{eval::is_true, Exec, Row};

impl Exec<'_> {
    /// `true` if `row` passes the optional `WHERE` (only `true` passes).
    fn passes(
        &mut self,
        where_clause: Option<&Expression>,
        row: &Row,
    ) -> Result<bool, AtlantisError> {
        Ok(match where_clause {
            Some(w) => is_true(&self.eval(w, row)?),
            None => true,
        })
    }
}
