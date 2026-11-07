//! Matching and creating graph patterns against the record store.
//!
//! Nodes and relationships are bound to `Value::RecordId`s. Each path is
//! matched left to right from its start node, expanding along adjacency
//! entries. Within one `MATCH`, a relationship is used at most once
//! (Cypher's relationship-uniqueness rule).

use atlantis_core::{AtlantisError, Direction, Edge, Object, RecordId, RecordKey, Value};
use atlantis_cypher::ast::{NodePattern, PathPattern, Pattern, RelPattern};

use super::eval::invalid;
use super::{Exec, Row};

/// Path value: `{nodes: [...], relationships: [...]}`.
fn path_value(nodes: Vec<Value>, rels: Vec<Value>) -> Value {
    let mut obj = Object::new();
    obj.insert("nodes".into(), Value::Array(nodes));
    obj.insert("relationships".into(), Value::Array(rels));
    Value::Object(obj)
}

/// Variables a pattern would bind.
pub(crate) fn pattern_variables(pattern: &Pattern) -> Vec<String> {
    let mut out = Vec::new();
    for path in &pattern.paths {
        out.extend(path.variable.clone());
        out.extend(path.start.variable.clone());
        for (rel, node) in &path.steps {
            out.extend(rel.variable.clone());
            out.extend(node.variable.clone());
        }
    }
    out
}

/// State threaded through matching of one path.
struct Partial {
    row: Row,
    nodes: Vec<Value>,
    rels: Vec<Value>,
}

impl Exec<'_> {
    /// Evaluates inline `{key: expr}` properties of a pattern element.
    fn eval_props(
        &mut self,
        props: &[(String, atlantis_cypher::ast::Expression)],
        row: &Row,
    ) -> Result<Vec<(String, Value)>, AtlantisError> {
        props
            .iter()
            .map(|(k, e)| Ok((k.clone(), self.eval(e, row)?)))
            .collect()
    }

    /// Checks labels and properties of an existing node.
    fn node_fits(
        &mut self,
        id: &RecordId,
        np: &NodePattern,
        props: &[(String, Value)],
    ) -> Result<bool, AtlantisError> {
        if self.tx.table(&id.table)?.is_some_and(|t| t.is_edge()) {
            return Ok(false); // relationships are not nodes
        }
        let Some(record) = self.load(id)? else {
            return Ok(false);
        };
        if !np.labels.iter().all(|l| record.has_label(l)) {
            return Ok(false);
        }
        Ok(props.iter().all(|(k, v)| {
            if k == "id" {
                id_matches(id, v)
            } else {
                record
                    .data
                    .get(k)
                    .is_some_and(|have| !v.is_null() && have == v)
            }
        }))
    }

    /// Candidate nodes for a node pattern that is not yet bound.
    fn node_candidates(
        &mut self,
        np: &NodePattern,
        props: &[(String, Value)],
    ) -> Result<Vec<RecordId>, AtlantisError> {
        // Direct lookup by id when the pattern pins it.
        if let Some((_, v)) = props.iter().find(|(k, _)| k == "id") {
            // Candidate ids: the id itself, or the key in each pattern label
            // that names a node table (a label may also be secondary).
            let ids: Option<Vec<RecordId>> = match v {
                Value::RecordId(id) => Some(vec![id.clone()]),
                other => match RecordKey::from_value(other) {
                    Ok(key) => {
                        let mut ids = Vec::new();
                        for label in &np.labels {
                            if self.tx.table(label)?.is_some_and(|t| !t.is_edge()) {
                                ids.extend(RecordId::new(label.clone(), key.clone()).ok());
                            }
                        }
                        // No label is a node table: fall back to scanning.
                        (!ids.is_empty()).then_some(ids)
                    }
                    Err(_) => Some(Vec::new()),
                },
            };
            if let Some(ids) = ids {
                let mut out = Vec::new();
                for id in ids {
                    if self.node_fits(&id, np, props)? {
                        out.push(id);
                    }
                }
                return Ok(out);
            }
        }
        let records = match np.labels.first() {
            Some(label) => self.tx.with_label(label)?,
            None => {
                let mut all = Vec::new();
                for table in self.tx.tables()? {
                    if !table.is_edge() {
                        all.extend(self.tx.scan(&table.name, None)?);
                    }
                }
                all
            }
        };
        let mut out = Vec::new();
        for record in records {
            if np.labels.iter().all(|l| record.has_label(l))
                && props.iter().all(|(k, v)| {
                    if k == "id" {
                        id_matches(&record.id, v)
                    } else {
                        record
                            .data
                            .get(k)
                            .is_some_and(|have| !v.is_null() && have == v)
                    }
                })
            {
                out.push(record.id);
            }
        }
        Ok(out)
    }

    /// Nodes matching `np` given the current row (bound or scanned).
    fn match_node(&mut self, np: &NodePattern, row: &Row) -> Result<Vec<RecordId>, AtlantisError> {
        let props = self.eval_props(&np.properties, row)?;
        if let Some(var) = &np.variable {
            if let Some(bound) = row.get(var) {
                return match bound {
                    Value::Null => Ok(Vec::new()),
                    Value::RecordId(id) => Ok(if self.node_fits(id, np, &props)? {
                        vec![id.clone()]
                    } else {
                        Vec::new()
                    }),
                    other => Err(invalid(format!(
                        "`{var}` is a {}, not a node",
                        other.type_name()
                    ))),
                };
            }
        }
        self.node_candidates(np, &props)
    }

    /// Edges leaving `node` that fit the relationship pattern (types and
    /// properties), with the node at their other end.
    fn step_edges(
        &mut self,
        node: &RecordId,
        rp: &RelPattern,
        props: &[(String, Value)],
    ) -> Result<Vec<(Edge, RecordId)>, AtlantisError> {
        let edges = if rp.types.is_empty() {
            self.tx.edges(node, rp.direction, None)?
        } else {
            let mut all = Vec::new();
            for t in &rp.types {
                all.extend(self.tx.edges(node, rp.direction, Some(t))?);
            }
            all
        };
        let mut out = Vec::new();
        for edge in edges {
            let fits = props.iter().all(|(k, v)| {
                if k == "id" {
                    id_matches(&edge.id, v)
                } else {
                    edge.data
                        .get(k)
                        .is_some_and(|have| !v.is_null() && have == v)
                }
            });
            if !fits {
                continue;
            }
            let other = match rp.direction {
                Direction::Outgoing => edge.to.clone(),
                Direction::Incoming => edge.from.clone(),
                Direction::Both => {
                    if &edge.from == node {
                        edge.to.clone()
                    } else {
                        edge.from.clone()
                    }
                }
            };
            out.push((edge, other));
        }
        Ok(out)
    }

    /// Matches one path, extending `row`. `used` holds relationship ids
    /// already used by earlier paths of the same `MATCH`.
    pub(crate) fn match_path(
        &mut self,
        path: &PathPattern,
        row: &Row,
        used: &[String],
    ) -> Result<Vec<(Row, Vec<String>)>, AtlantisError> {
        let mut results = Vec::new();
        for start in self.match_node(&path.start, row)? {
            let mut r = row.clone();
            if let Some(v) = &path.start.variable {
                r.insert(v.clone(), Value::RecordId(start.clone()));
            }
            let partial = Partial {
                row: r,
                nodes: vec![Value::RecordId(start.clone())],
                rels: Vec::new(),
            };
            let mut used = used.to_vec();
            self.match_steps(path, 0, &start, partial, &mut used, &mut results)?;
        }
        Ok(results)
    }

    fn match_steps(
        &mut self,
        path: &PathPattern,
        step: usize,
        current: &RecordId,
        partial: Partial,
        used: &mut Vec<String>,
        out: &mut Vec<(Row, Vec<String>)>,
    ) -> Result<(), AtlantisError> {
        if step == path.steps.len() {
            let mut row = partial.row;
            if let Some(v) = &path.variable {
                row.insert(v.clone(), path_value(partial.nodes, partial.rels));
            }
            out.push((row, used.clone()));
            return Ok(());
        }
        let (rp, np) = &path.steps[step];
        let rel_props = self.eval_props(&rp.properties, &partial.row)?;
        let node_props = self.eval_props(&np.properties, &partial.row)?;

        // Each candidate: the edges walked, the nodes passed through after
        // `current` (ending with the node reached), and the node reached.
        let mut walks: Vec<(Vec<Edge>, Vec<RecordId>, RecordId)> = Vec::new();
        match rp.length {
            None => {
                for (edge, other) in self.step_edges(current, rp, &rel_props)? {
                    if !used.contains(&edge.id.to_string()) {
                        walks.push((vec![edge], vec![other.clone()], other));
                    }
                }
            }
            Some(len) => {
                let min = len.min.unwrap_or(1);
                let max = len.max;
                if max.is_some_and(|m| m < min) {
                    return Ok(());
                }
                let mut stack: Vec<(RecordId, Vec<Edge>, Vec<RecordId>)> =
                    vec![(current.clone(), Vec::new(), Vec::new())];
                while let Some((at, walked, passed)) = stack.pop() {
                    if walked.len() >= min {
                        walks.push((walked.clone(), passed.clone(), at.clone()));
                    }
                    if max.is_some_and(|m| walked.len() >= m) {
                        continue;
                    }
                    for (edge, other) in self.step_edges(&at, rp, &rel_props)? {
                        let id = edge.id.to_string();
                        if used.contains(&id) || walked.iter().any(|e| e.id.to_string() == id) {
                            continue;
                        }
                        let mut next = walked.clone();
                        next.push(edge);
                        let mut through = passed.clone();
                        through.push(other.clone());
                        stack.push((other, next, through));
                    }
                }
            }
        }

        for (edges, passed, other) in walks {
            // The far node must fit its pattern (and binding, if bound).
            if let Some(var) = &np.variable {
                if let Some(bound) = partial.row.get(var) {
                    if bound != &Value::RecordId(other.clone()) {
                        continue;
                    }
                }
            }
            if !self.node_fits(&other, np, &node_props)? {
                continue;
            }
            let rel_value = if rp.length.is_some() {
                Value::Array(
                    edges
                        .iter()
                        .map(|e| Value::RecordId(e.id.clone()))
                        .collect(),
                )
            } else {
                Value::RecordId(edges[0].id.clone())
            };
            if let Some(var) = &rp.variable {
                if let Some(bound) = partial.row.get(var) {
                    if bound != &rel_value {
                        continue;
                    }
                }
            }
            let mut row = partial.row.clone();
            if let Some(var) = &rp.variable {
                row.insert(var.clone(), rel_value);
            }
            if let Some(var) = &np.variable {
                row.insert(var.clone(), Value::RecordId(other.clone()));
            }
            let mut nodes = partial.nodes.clone();
            let mut rels = partial.rels.clone();
            for edge in &edges {
                rels.push(Value::RecordId(edge.id.clone()));
            }
            nodes.extend(passed.into_iter().map(Value::RecordId));
            let before = used.len();
            used.extend(edges.iter().map(|e| e.id.to_string()));
            self.match_steps(
                path,
                step + 1,
                &other,
                Partial { row, nodes, rels },
                used,
                out,
            )?;
            used.truncate(before);
        }
        Ok(())
    }

    /// Matches a whole comma-separated pattern.
    pub(crate) fn match_pattern(
        &mut self,
        pattern: &Pattern,
        row: &Row,
    ) -> Result<Vec<Row>, AtlantisError> {
        let mut partials: Vec<(Row, Vec<String>)> = vec![(row.clone(), Vec::new())];
        for path in &pattern.paths {
            let mut next = Vec::new();
            for (r, used) in &partials {
                next.extend(self.match_path(path, r, used)?);
            }
            partials = next;
        }
        Ok(partials.into_iter().map(|(r, _)| r).collect())
    }

    /// Creates the nodes and relationships of a path, reusing bound
    /// variables, and binds the new elements in `row`.
    pub(crate) fn create_path(
        &mut self,
        path: &PathPattern,
        row: &mut Row,
    ) -> Result<(), AtlantisError> {
        let start = self.create_node(&path.start, row)?;
        let mut nodes = vec![Value::RecordId(start.clone())];
        let mut rels = Vec::new();
        let mut current = start;
        for (rp, np) in &path.steps {
            let next = self.create_node(np, row)?;
            if rp.length.is_some() {
                return Err(invalid("variable-length relationships cannot be created"));
            }
            let rel_type = match rp.types.as_slice() {
                [t] => t,
                _ => return Err(invalid("a created relationship needs exactly one type")),
            };
            let (from, to) = match rp.direction {
                Direction::Outgoing => (&current, &next),
                Direction::Incoming => (&next, &current),
                Direction::Both => return Err(invalid("a created relationship needs a direction")),
            };
            if let Some(var) = &rp.variable {
                if row.contains_key(var) {
                    return Err(invalid(format!(
                        "variable `{var}` is already bound; CREATE needs a new relationship"
                    )));
                }
            }
            let props = self.eval_props(&rp.properties, row)?;
            let data: Object = props.into_iter().filter(|(_, v)| !v.is_null()).collect();
            let edge = self.tx.relate(from, rel_type, to, data)?;
            if let Some(var) = &rp.variable {
                row.insert(var.clone(), Value::RecordId(edge.id.clone()));
            }
            rels.push(Value::RecordId(edge.id));
            nodes.push(Value::RecordId(next.clone()));
            current = next;
        }
        if let Some(v) = &path.variable {
            row.insert(v.clone(), path_value(nodes, rels));
        }
        Ok(())
    }

    fn create_node(&mut self, np: &NodePattern, row: &mut Row) -> Result<RecordId, AtlantisError> {
        if let Some(var) = &np.variable {
            if let Some(bound) = row.get(var) {
                if !np.labels.is_empty() || !np.properties.is_empty() {
                    return Err(invalid(format!("variable `{var}` is already bound; it cannot get new labels or properties in CREATE")));
                }
                return match bound {
                    Value::RecordId(id) => Ok(id.clone()),
                    other => Err(invalid(format!(
                        "`{var}` is a {}, not a node",
                        other.type_name()
                    ))),
                };
            }
        }
        let props = self.eval_props(&np.properties, row)?;
        let data: Object = props.into_iter().filter(|(_, v)| !v.is_null()).collect();
        let labels: Vec<&str> = np.labels.iter().map(String::as_str).collect();
        let record = self.tx.create_node(&labels, data)?;
        if let Some(var) = &np.variable {
            row.insert(var.clone(), Value::RecordId(record.id.clone()));
        }
        Ok(record.id)
    }
}

/// `true` if a pattern's `id` property value names this record.
fn id_matches(id: &RecordId, v: &Value) -> bool {
    match v {
        Value::RecordId(other) => other == id,
        other => RecordKey::from_value(other).is_ok_and(|k| k == id.key),
    }
}
