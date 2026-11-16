//! Encoding of stored values: a format byte followed by a bincode body.

use atlantis_core::{AtlantisError, Edge, Record};
use serde::{de::DeserializeOwned, Deserialize, Serialize};

/// Leading byte of every stored value, so the format can evolve.
const FORMAT_V1: u8 = 1;

#[derive(Serialize, Deserialize)]
pub(super) enum Stored {
    Record(Record),
    Edge(Edge),
}

impl Stored {
    /// The record view: edges are returned as documents with `in` and `out`
    /// fields.
    pub(super) fn into_record(self) -> Record {
        match self {
            Stored::Record(r) => r,
            Stored::Edge(e) => edge_as_record(e),
        }
    }
}

pub(super) fn encode<T: Serialize>(value: &T) -> Result<Vec<u8>, AtlantisError> {
    let mut out = vec![FORMAT_V1];
    bincode::serialize_into(&mut out, value)
        .map_err(|e| AtlantisError::Serialization(e.to_string()))?;
    Ok(out)
}

pub(super) fn decode<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, AtlantisError> {
    match bytes.split_first() {
        Some((&FORMAT_V1, body)) => {
            bincode::deserialize(body).map_err(|e| AtlantisError::Serialization(e.to_string()))
        }
        _ => Err(AtlantisError::Serialization(
            "unknown stored value format".into(),
        )),
    }
}

pub(super) fn edge_as_record(edge: Edge) -> Record {
    let mut data = edge.to_object();
    data.remove("id");
    Record {
        id: edge.id,
        labels: Vec::new(),
        data,
    }
}
