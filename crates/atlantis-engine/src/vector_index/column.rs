//! Per-slot field columns and the evaluation of pushed-down filters.

use std::collections::HashMap;

use atlantis_core::Value;
use atlantis_cypher::ast::BinaryOp;
use atlantis_vector::AllowSet;

use super::FieldFilter;
use crate::exec::eval::{compare_op, group_key, is_true};

impl FieldFilter {
    fn admits(&self, field_value: &Value) -> bool {
        let r = if self.field_on_left {
            compare_op(field_value, self.op, &self.value)
        } else {
            compare_op(&self.value, self.op, field_value)
        };
        r.is_ok_and(|v| is_true(&v))
    }

    /// The equality-map keys that can satisfy this condition, when it is an
    /// equality or `field IN [...]` (`None`: scan the column instead).
    fn lookup_keys(&self) -> Option<Vec<String>> {
        match (self.op, self.field_on_left, &self.value) {
            (BinaryOp::Eq, _, v) => Some(vec![group_key(std::slice::from_ref(v))]),
            (BinaryOp::In, true, Value::Array(items)) => Some(
                items
                    .iter()
                    .map(|v| group_key(std::slice::from_ref(v)))
                    .collect(),
            ),
            _ => None,
        }
    }
}

/// A per-slot copy of one record field, kept for filtered search.
#[derive(Default)]
pub(super) struct Column {
    /// Slot → field value (`Null` when absent or the slot was removed).
    pub(super) values: Vec<Value>,
    /// Value key → slots that had that value when written. Entries of
    /// removed or changed slots go stale; candidates are re-checked against
    /// `values`.
    eq: HashMap<String, Vec<u32>>,
}

impl Column {
    pub(super) fn set(&mut self, slot: u64, value: Value) {
        let i = slot as usize;
        if self.values.len() <= i {
            self.values.resize(i + 1, Value::Null);
        }
        if !value.is_null() {
            self.eq
                .entry(group_key(std::slice::from_ref(&value)))
                .or_default()
                .push(slot as u32);
        }
        self.values[i] = value;
    }

    fn get(&self, slot: u32) -> &Value {
        self.values.get(slot as usize).unwrap_or(&Value::Null)
    }

    /// Slots whose value meets `filter`.
    pub(super) fn matching(&self, filter: &FieldFilter) -> AllowSet {
        let mut set = AllowSet::with_capacity(self.values.len());
        match filter.lookup_keys() {
            Some(keys) => {
                for key in keys {
                    for &slot in self.eq.get(&key).into_iter().flatten() {
                        if filter.admits(self.get(slot)) {
                            set.insert(slot);
                        }
                    }
                }
            }
            None => {
                for (slot, v) in self.values.iter().enumerate() {
                    if filter.admits(v) {
                        set.insert(slot as u32);
                    }
                }
            }
        }
        set
    }
}
