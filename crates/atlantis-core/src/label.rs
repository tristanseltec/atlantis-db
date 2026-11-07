//! Label types for node categorization.

use std::collections::BTreeSet;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

/// A node label (e.g., `:Person`, `:Embedding`, `:Document`).
///
/// Uses `Arc<str>` for cheap cloning — labels are frequently shared
/// across nodes and passed between actors.
#[derive(Debug, Clone, Eq, PartialEq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Label(Arc<str>);

impl Label {
    /// Creates a new label from a string-like value.
    pub fn new(s: impl AsRef<str>) -> Self {
        Label(Arc::from(s.as_ref()))
    }

    /// Returns the label as a `&str`.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&str> for Label {
    fn from(s: &str) -> Self {
        Label::new(s)
    }

}

impl From<String> for Label {
    fn from(s: String) -> Self {
        Label::new(s)
    }
}

impl std::fmt::Display for Label {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, ":{}", self.0)
    }
}

/// An ordered set of labels for a node.
///
/// Uses `BTreeSet` for deterministic iteration order, which is important
/// for reproducible query results and consistent serialization.
#[derive(Debug, Clone, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct LabelSet(BTreeSet<Label>);

impl LabelSet {
    /// Creates a new empty label set.
    pub fn new() -> Self {
        LabelSet(BTreeSet::new())
    }

    /// Inserts a label into the set.
    pub fn insert(&mut self, label: Label) -> bool {
        self.0.insert(label)
    }

    /// Removes a label from the set.
    pub fn remove(&mut self, label: &Label) -> bool {
        self.0.remove(label)
    }

    /// Returns `true` if the set contains the given label.
    pub fn contains(&self, label: &Label) -> bool {
        self.0.contains(label)
    }

    /// Returns the number of labels in the set.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Returns `true` if the set is empty.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Returns an iterator over the labels.
    pub fn iter(&self) -> impl Iterator<Item = &Label> {
        self.0.iter()
    }

    /// Returns `true` if this set contains all labels in `other`.
    pub fn is_superset(&self, other: &LabelSet) -> bool {
        other.0.iter().all(|l| self.0.contains(l))
    }
}

impl FromIterator<Label> for LabelSet {
    fn from_iter<I: IntoIterator<Item = Label>>(iter: I) -> Self {
        LabelSet(iter.into_iter().collect())
    }
}

impl IntoIterator for LabelSet {
    type Item = Label;
    type IntoIter = std::collections::btree_set::IntoIter<Label>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_label_creation() {
        let l1 = Label::from("Person");
        let l2 = Label::new("Person");
        assert_eq!(l1, l2);
        assert_eq!(l1.as_str(), "Person");
    }

    #[test]
    fn test_label_display() {
        let l = Label::from("Movie");
        assert_eq!(l.to_string(), ":Movie");
    }

    #[test]
    fn test_label_ordering() {
        let a = Label::from("Apple");
        let b = Label::from("Banana");
        assert!(a < b);
    }

    #[test]
    fn test_label_set_basic() {
        let mut set = LabelSet::new();
        assert!(set.is_empty());

        set.insert(Label::from("Person"));
        set.insert(Label::from("Adult"));
        assert_eq!(set.len(), 2);
        assert!(set.contains(&Label::from("Person")));
        assert!(set.contains(&Label::from("Adult")));

        // Duplicate insert is a no-op
        set.insert(Label::from("Person"));
        assert_eq!(set.len(), 2);
    }

    #[test]
    fn test_label_set_remove() {
        let mut set = LabelSet::new();
        set.insert(Label::from("Person"));
        assert!(set.remove(&Label::from("Person")));
        assert!(!set.contains(&Label::from("Person")));
        assert!(set.is_empty());
    }

    #[test]
    fn test_label_set_superset() {
        let mut a = LabelSet::new();
        a.insert(Label::from("Person"));
        a.insert(Label::from("Adult"));

        let mut b = LabelSet::new();
        b.insert(Label::from("Person"));

        assert!(a.is_superset(&b));
        assert!(!b.is_superset(&a));
    }

    #[test]
    fn test_label_set_iteration_order() {
        let mut set = LabelSet::new();
        set.insert(Label::from("Zebra"));
        set.insert(Label::from("Apple"));
        set.insert(Label::from("Mango"));

        let labels: Vec<_> = set.iter().map(|l| l.as_str()).collect();
        assert_eq!(labels, vec!["Apple", "Mango", "Zebra"]);
    }

    #[test]
    fn test_label_cheap_clone() {
        let l1 = Label::from("Test");
        let l2 = l1.clone();
        // Both point to the same underlying allocation
        assert_eq!(l1.as_str(), l2.as_str());
    }
}
