//! Query parameters for parameterized Cypher queries.

use std::collections::HashMap;

use atlantis_core::Value;

/// A map of query parameters for parameterized Cypher queries.
///
/// Use `Params::from(&[("name", Value::String("Alice".into()))])` for
/// ergonomic construction from a slice of tuples.
#[derive(Debug, Clone, Default)]
pub struct Params(HashMap<String, Value>);

impl Params {
    /// Creates a new empty parameter map.
    pub fn new() -> Self {
        Params(HashMap::new())
    }

    /// Inserts a parameter.
    pub fn insert(&mut self, key: impl Into<String>, value: impl Into<Value>) {
        self.0.insert(key.into(), value.into());
    }

    /// Returns a reference to the inner map.
    pub fn as_map(&self) -> &HashMap<String, Value> {
        &self.0
    }

    /// Consumes and returns the inner map.
    pub fn into_map(self) -> HashMap<String, Value> {
        self.0
    }
}

impl From<&[(&str, Value)]> for Params {
    fn from(slice: &[(&str, Value)]) -> Self {
        let mut params = Params::new();
        for (key, value) in slice {
            params.0.insert((*key).to_string(), value.clone());
        }
        params
    }
}
