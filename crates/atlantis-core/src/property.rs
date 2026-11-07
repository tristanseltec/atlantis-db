//! Property map for storing key-value pairs on nodes and relationships.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::Value;

/// A map of property key-value pairs.
///
/// Backed by `HashMap<String, Value>`. Keys are case-sensitive strings.
#[derive(Debug, Clone, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct PropertyMap(HashMap<String, Value>);

impl PropertyMap {
    /// Creates a new empty property map.
    pub fn new() -> Self {
        PropertyMap(HashMap::new())
    }

    /// Creates a property map with the given capacity pre-allocated.
    pub fn with_capacity(cap: usize) -> Self {
        PropertyMap(HashMap::with_capacity(cap))
    }

    /// Inserts a key-value pair, returning the previous value if any.
    pub fn insert(&mut self, key: impl Into<String>, value: impl Into<Value>) -> Option<Value> {
        self.0.insert(key.into(), value.into())
    }

    /// Returns a reference to the value for the given key.
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.0.get(key)
    }

    /// Returns a mutable reference to the value for the given key.
    pub fn get_mut(&mut self, key: &str) -> Option<&mut Value> {
        self.0.get_mut(key)
    }

    /// Removes a key-value pair, returning the value if it was present.
    pub fn remove(&mut self, key: &str) -> Option<Value> {
        self.0.remove(key)
    }

    /// Returns `true` if the map contains the given key.
    pub fn contains(&self, key: &str) -> bool {
        self.0.contains_key(key)
    }

    /// Returns the number of properties.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Returns `true` if the map is empty.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Returns an iterator over (key, value) pairs.
    pub fn iter(&self) -> impl Iterator<Item = (&String, &Value)> {
        self.0.iter()
    }

    /// Clears all properties.
    pub fn clear(&mut self) {
        self.0.clear();
    }

    /// Extends this map with entries from another map.
    pub fn extend(&mut self, other: PropertyMap) {
        self.0.extend(other.0);
    }
}

impl FromIterator<(String, Value)> for PropertyMap {
    fn from_iter<I: IntoIterator<Item = (String, Value)>>(iter: I) -> Self {
        PropertyMap(iter.into_iter().collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_insert_and_get() {
        let mut map = PropertyMap::new();
        map.insert("name", "Alice");
        map.insert("age", 30i64);

        assert_eq!(map.get("name"), Some(&Value::String("Alice".into())));
        assert_eq!(map.get("age"), Some(&Value::Integer(30)));
        assert_eq!(map.get("missing"), None);
    }

    #[test]
    fn test_insert_overwrite() {
        let mut map = PropertyMap::new();
        let old = map.insert("x", 1i64);
        assert_eq!(old, None);

        let old = map.insert("x", 2i64);
        assert_eq!(old, Some(Value::Integer(1)));
        assert_eq!(map.get("x"), Some(&Value::Integer(2)));
    }

    #[test]
    fn test_remove() {
        let mut map = PropertyMap::new();
        map.insert("key", "value");
        assert!(map.contains("key"));

        let removed = map.remove("key");
        assert_eq!(removed, Some(Value::String("value".into())));
        assert!(!map.contains("key"));
        assert_eq!(map.remove("key"), None);
    }

    #[test]
    fn test_len_and_empty() {
        let mut map = PropertyMap::new();
        assert!(map.is_empty());
        assert_eq!(map.len(), 0);

        map.insert("a", 1i64);
        map.insert("b", 2i64);
        assert!(!map.is_empty());
        assert_eq!(map.len(), 2);

        map.clear();
        assert!(map.is_empty());
    }

    #[test]
    fn test_extend() {
        let mut map1 = PropertyMap::new();
        map1.insert("a", 1i64);

        let mut map2 = PropertyMap::new();
        map2.insert("b", 2i64);
        map2.insert("c", 3i64);

        map1.extend(map2);
        assert_eq!(map1.len(), 3);
        assert!(map1.contains("a"));
        assert!(map1.contains("b"));
        assert!(map1.contains("c"));
    }

    #[test]
    fn test_from_iter() {
        let map = PropertyMap::from_iter([
            ("x".to_string(), Value::Integer(1)),
            ("y".to_string(), Value::Bool(true)),
        ]);
        assert_eq!(map.len(), 2);
    }

    #[test]
    fn test_serde_roundtrip() {
        let mut map = PropertyMap::new();
        map.insert("name", "Bob");
        map.insert("score", 0.95f64);
        map.insert("active", true);

        let serialized = bincode::serialize(&map).unwrap();
        let deserialized: PropertyMap = bincode::deserialize(&serialized).unwrap();
        assert_eq!(map, deserialized);
    }
}
