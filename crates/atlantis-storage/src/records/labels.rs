//! Secondary labels: label index entries and label lookups.

use atlantis_core::{AtlantisError, Record, RecordId};
use atlantis_kv::keys;

use super::codec::Stored;
use super::RecordTx;

impl RecordTx {
    /// Returns all records carrying `label`, either as their table or as a
    /// secondary label.
    pub fn with_label(&mut self, label: &str) -> Result<Vec<Record>, AtlantisError> {
        let mut out = match self.table(label)? {
            Some(def) if !def.is_edge() => self.scan(label, None)?,
            _ => Vec::new(),
        };
        let range = keys::label_range(&self.db, label);
        for (key, _) in self.tx.scan(&range, None)? {
            let id = keys::parse_label_entry(&self.db, label, &key)?;
            if let Some(record) = self.get(&id)? {
                out.push(record);
            }
        }
        Ok(out)
    }

    /// Adds and removes secondary labels of a record. Adding the record's
    /// own table as a label is a no-op; it cannot be removed.
    pub fn set_labels(
        &mut self,
        id: &RecordId,
        add: &[String],
        remove: &[String],
    ) -> Result<Record, AtlantisError> {
        let mut record = match self.get_stored(id)?.ok_or_else(|| Self::not_found(id))? {
            Stored::Record(r) => r,
            Stored::Edge(_) => {
                return Err(AtlantisError::SchemaViolation(format!(
                    "edge {id} cannot carry labels"
                )))
            }
        };
        if remove.contains(&id.table) {
            return Err(AtlantisError::SchemaViolation(format!(
                "{} is the table of {id} and cannot be removed",
                id.table
            )));
        }
        // The old label list is replaced below, so take it instead of cloning.
        let before = std::mem::take(&mut record.labels);
        let mut labels: Vec<String> = before
            .iter()
            .filter(|l| !remove.contains(l))
            .cloned()
            .collect();
        labels.extend(add.iter().cloned());
        record = Record::new(record.id, labels, record.data)?;
        for gone in before.iter().filter(|l| !record.labels.contains(l)) {
            self.tx.delete(&keys::label_entry(&self.db, gone, id)?)?;
        }
        for new in record.labels.iter().filter(|l| !before.contains(l)) {
            self.tx
                .set(keys::label_entry(&self.db, new, id)?, Vec::new())?;
        }
        self.write_back(Stored::Record(record))
    }
}
