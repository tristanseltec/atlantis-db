//! Query result types: `ResultSet` and `Row`.

use atlantis_core::Value;

/// The result of a Cypher query execution.
#[derive(Debug, Clone)]
pub struct ResultSet {
    /// Column names from the RETURN clause.
    columns: Vec<String>,
    /// Rows of values, one per result row.
    rows: Vec<Vec<Value>>,
}

impl ResultSet {
    /// Creates a new result set from columns and rows.
    pub(crate) fn new(columns: Vec<String>, rows: Vec<Vec<Value>>) -> Self {
        ResultSet { columns, rows }
    }

    /// Returns the column names.
    pub fn columns(&self) -> &[String] {
        &self.columns
    }

    /// Returns the number of rows.
    pub fn row_count(&self) -> usize {
        self.rows.len()
    }

    /// Returns `true` if the result set has no rows.
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// Returns an iterator over the rows.
    pub fn rows(&self) -> impl Iterator<Item = &Row> {
        self.rows.iter().map(|vals| Row {
            columns: &self.columns,
            values: vals,
        })
    }
}

/// A single row in a query result.
#[derive(Debug, Clone)]
pub struct Row<'a> {
    columns: &'a [String],
    values: &'a [Value],
}

impl<'a> Row<'a> {
    /// Returns the value for a column by name.
    pub fn get(&self, column: &str) -> Option<&Value> {
        self.columns
            .iter()
            .position(|c| c == column)
            .and_then(|i| self.values.get(i))
    }

    /// Returns the value as a string for the given column.
    pub fn get_string(&self, column: &str) -> Option<&str> {
        self.get(column).and_then(|v| v.as_string())
    }

    /// Returns the value as an integer for the given column.
    pub fn get_int(&self, column: &str) -> Option<i64> {
        self.get(column).and_then(|v| v.as_int())
    }

    /// Returns the value as a float for the given column.
    pub fn get_float(&self, column: &str) -> Option<f64> {
        self.get(column).and_then(|v| v.as_float())
    }

    /// Returns the value as a bool for the given column.
    pub fn get_bool(&self, column: &str) -> Option<bool> {
        self.get(column).and_then(|v| v.as_bool())
    }

    /// Returns the value as a vector slice for the given column.
    pub fn get_vector(&self, column: &str) -> Option<&[f32]> {
        self.get(column).and_then(|v| v.as_vector())
    }

    /// Returns the raw values in this row.
    pub fn values(&self) -> &[Value] {
        self.values
    }

    /// Returns the number of columns in this row.
    pub fn column_count(&self) -> usize {
        self.values.len()
    }
}
