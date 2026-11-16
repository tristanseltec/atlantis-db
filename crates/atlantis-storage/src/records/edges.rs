//! Edges: relationships between records, with adjacency entries on both
//! endpoints so traversals are prefix scans.

use atlantis_core::{AtlantisError, Direction, Edge, Object, RecordId};
use atlantis_kv::keys::{self, EdgeDir};

use super::codec::{decode, Stored};
use super::{RecordTx, TableKind};

impl RecordTx {
    /// Reads an edge record.
    pub fn get_edge(&mut self, id: &RecordId) -> Result<Option<Edge>, AtlantisError> {
        match self.get_stored(id)? {
            Some(Stored::Edge(e)) => Ok(Some(e)),
            Some(Stored::Record(_)) => Err(AtlantisError::SchemaViolation(format!(
                "{id} is a record, not an edge"
            ))),
            None => Ok(None),
        }
    }

    // ---- edges -----------------------------------------------------------

    /// Creates an edge `from -[rel_type]-> to`. Both endpoints must exist.
    /// The edge table is created on first use; if it was defined with
    /// endpoint tables, the endpoints must belong to them. An `id` field in
    /// `data` sets the edge key.
    pub fn relate(
        &mut self,
        from: &RecordId,
        rel_type: &str,
        to: &RecordId,
        data: Object,
    ) -> Result<Edge, AtlantisError> {
        let mut data = data;
        let def = self.resolve_table(rel_type, true)?;
        if let TableKind::Edge {
            from: allowed_from,
            to: allowed_to,
        } = &def.kind
        {
            for (end, allowed, side) in [(from, allowed_from, "start"), (to, allowed_to, "end")] {
                if !allowed.is_empty() && !allowed.contains(&end.table) {
                    return Err(AtlantisError::SchemaViolation(format!(
                        "{rel_type} does not allow {} as its {side} node (allowed: {})",
                        end.table,
                        allowed.join(", ")
                    )));
                }
            }
        }
        for end in [from, to] {
            let key = keys::record(&self.db, end)?;
            let bytes = self.tx.get(&key)?.ok_or_else(|| Self::not_found(end))?;
            if matches!(decode::<Stored>(&bytes)?, Stored::Edge(_)) {
                return Err(AtlantisError::SchemaViolation(format!(
                    "{end} is an edge; edges cannot be endpoints of other edges"
                )));
            }
            // Rewrite the endpoint unchanged so that a concurrent delete of it
            // conflicts with this transaction instead of leaving a dangling edge.
            self.tx.set(key, bytes)?;
        }
        if data.contains_key("in") || data.contains_key("out") {
            return Err(AtlantisError::SchemaViolation(
                "in and out are set from the edge endpoints".into(),
            ));
        }
        let key = Self::take_key(rel_type, None, &mut data)?;
        let edge = Edge {
            id: RecordId::new(rel_type, key)?,
            from: from.clone(),
            to: to.clone(),
            data,
        };
        let rkey = keys::record(&self.db, &edge.id)?;
        if self.tx.get(&rkey)?.is_some() {
            return Err(AtlantisError::RecordExists(edge.id.to_string()));
        }
        self.tx.set(
            keys::adjacency(&self.db, from, EdgeDir::Out, &edge.id)?,
            Vec::new(),
        )?;
        self.tx.set(
            keys::adjacency(&self.db, to, EdgeDir::In, &edge.id)?,
            Vec::new(),
        )?;
        self.put_record(&edge.id, rkey, &Stored::Edge(edge.clone()))?;
        Ok(edge)
    }

    /// Deletes an edge and its adjacency entries. Returns whether it existed.
    pub fn delete_edge(&mut self, id: &RecordId) -> Result<bool, AtlantisError> {
        let Some(edge) = self.get_edge(id)? else {
            return Ok(false);
        };
        self.tx
            .delete(&keys::adjacency(&self.db, &edge.from, EdgeDir::Out, id)?)?;
        self.tx
            .delete(&keys::adjacency(&self.db, &edge.to, EdgeDir::In, id)?)?;
        self.delete_record_key(id)?;
        Ok(true)
    }

    fn dirs(direction: Direction) -> &'static [EdgeDir] {
        match direction {
            Direction::Outgoing => &[EdgeDir::Out],
            Direction::Incoming => &[EdgeDir::In],
            Direction::Both => &[EdgeDir::Out, EdgeDir::In],
        }
    }

    /// Ids of edges attached to `node`. `direction = None` means both.
    pub(super) fn edge_ids(
        &mut self,
        node: &RecordId,
        direction: Option<Direction>,
        rel_type: Option<&str>,
    ) -> Result<Vec<RecordId>, AtlantisError> {
        let mut ids: Vec<RecordId> = Vec::new();
        for dir in Self::dirs(direction.unwrap_or(Direction::Both)) {
            let range = keys::adjacency_range(&self.db, node, Some(*dir), rel_type)?;
            for (key, _) in self.tx.scan(&range, None)? {
                let (_, edge) = keys::parse_adjacency(&self.db, node, &key)?;
                // A self-loop appears in both directions; report it once.
                if !ids.contains(&edge) {
                    ids.push(edge);
                }
            }
        }
        Ok(ids)
    }

    /// Edges attached to `node` in `direction`, optionally of one type.
    pub fn edges(
        &mut self,
        node: &RecordId,
        direction: Direction,
        rel_type: Option<&str>,
    ) -> Result<Vec<Edge>, AtlantisError> {
        let ids = self.edge_ids(node, Some(direction), rel_type)?;
        let mut out = Vec::with_capacity(ids.len());
        for id in ids {
            let edge = self.get_edge(&id)?.ok_or_else(|| {
                AtlantisError::Storage(format!("adjacency entry points at missing edge {id}"))
            })?;
            out.push(edge);
        }
        Ok(out)
    }

    /// Ids of the nodes reached from `node` over edges in `direction`.
    pub fn neighbors(
        &mut self,
        node: &RecordId,
        direction: Direction,
        rel_type: Option<&str>,
    ) -> Result<Vec<RecordId>, AtlantisError> {
        Ok(self
            .edges(node, direction, rel_type)?
            .into_iter()
            .map(|e| if &e.from == node { e.to } else { e.from })
            .collect())
    }
}
