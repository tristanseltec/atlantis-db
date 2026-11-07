//! The Cypher executor: interprets the AST clause by clause against a
//! record transaction.
//!
//! Every clause maps a list of rows (variable → value) to a new list. Nodes
//! and relationships are bound as `Value::RecordId`; reading a property of
//! one loads the record inside the same transaction, so a query sees its own
//! writes. `RETURN` turns variables bound to records into documents.

mod eval;
mod pattern;

use std::collections::{HashMap, HashSet};

use atlantis_core::{AtlantisError, Object, RecordId, Value};
use atlantis_cypher::ast::{
    Clause, CreateVectorIndex, Expression, Projection, Query, QueryUnion, RemoveItem, SetItem,
    Statement, YieldItem,
};
use atlantis_storage::{RecordStore, RecordTx, VectorIndexDef};

use crate::vector_index::VectorIndexes;
use eval::{as_vector, group_key, invalid, is_true, order_cmp};
use pattern::pattern_variables;

/// Variable bindings of one row.
pub(crate) type Row = HashMap<String, Value>;

/// Query parameters (`$name` → value).
pub type Params = HashMap<String, Value>;

/// Columns and rows produced by a statement.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct QueryResult {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Value>>,
}

/// Executes statements inside one record transaction.
pub(crate) struct Exec<'a> {
    pub(crate) tx: &'a mut RecordTx,
    pub(crate) params: &'a Params,
    pub(crate) store: &'a RecordStore,
    pub(crate) vectors: &'a VectorIndexes,
    pub(crate) vector_defaults: &'a atlantis_core::VectorConfig,
    /// Index definitions created or dropped, to invalidate after commit.
    pub(crate) index_changes: Vec<String>,
    /// Compiled `=~` patterns, reused across rows.
    pub(crate) regexes: HashMap<String, regex::Regex>,
}

/// Output of a projection: column names and rows (as bindings, in order).
struct Projected {
    columns: Vec<String>,
    rows: Vec<Row>,
}

impl<'a> Exec<'a> {
    pub(crate) fn run(&mut self, statement: &Statement) -> Result<QueryResult, AtlantisError> {
        match statement {
            Statement::Query(union) => self.run_union(union),
            Statement::CreateVectorIndex(def) => {
                self.create_vector_index(def)?;
                Ok(QueryResult::default())
            }
            Statement::DropIndex { name, if_exists } => {
                if !self.tx.drop_index(name)? && !if_exists {
                    return Err(invalid(format!("index {name} does not exist")));
                }
                self.index_changes.push(name.clone());
                Ok(QueryResult::default())
            }
            Statement::Begin | Statement::Commit | Statement::Rollback => Err(invalid(
                "transaction control statements are handled by the session",
            )),
        }
    }

    fn create_vector_index(&mut self, def: &CreateVectorIndex) -> Result<(), AtlantisError> {
        if self.tx.vector_index(&def.name)?.is_some() {
            if def.if_not_exists {
                return Ok(());
            }
            return Err(invalid(format!("index {} already exists", def.name)));
        }
        let mut index = VectorIndexDef {
            name: def.name.clone(),
            label: def.label.clone(),
            field: def.property.clone(),
            dimension: self.vector_defaults.dimensions,
            metric: self.vector_defaults.distance_metric,
            m: self.vector_defaults.hnsw.m,
            ef_construction: self.vector_defaults.hnsw.ef_construction,
        };
        for (key, expr) in &def.options {
            let value = self.eval(expr, &Row::new())?;
            let int = || -> Result<usize, AtlantisError> {
                match value {
                    Value::Integer(i) if i > 0 => Ok(i as usize),
                    _ => Err(invalid(format!(
                        "vector index option {key} must be a positive integer"
                    ))),
                }
            };
            match key.as_str() {
                "dimension" | "dimensions" | "vector.dimensions" => index.dimension = int()?,
                "m" => index.m = int()?,
                "efConstruction" | "ef_construction" => index.ef_construction = int()?,
                "metric" | "similarity" | "vector.similarity_function" => {
                    let Value::String(name) = &value else {
                        return Err(invalid("metric must be a string"));
                    };
                    index.metric = atlantis_core::DistanceMetricKind::parse(name)?;
                }
                other => return Err(invalid(format!("unknown vector index option {other}"))),
            }
        }
        self.tx.define_vector_index(index)?;
        self.index_changes.push(def.name.clone());
        Ok(())
    }

    fn run_union(&mut self, union: &QueryUnion) -> Result<QueryResult, AtlantisError> {
        let mut result = self.run_query(&union.first)?;
        let mut dedupe = false;
        for (all, query) in &union.rest {
            let next = self.run_query(query)?;
            if next.columns != result.columns {
                return Err(invalid(
                    "all queries in a UNION must return the same columns",
                ));
            }
            result.rows.extend(next.rows);
            dedupe |= !all;
        }
        if dedupe {
            let mut seen = HashSet::new();
            result.rows.retain(|row| seen.insert(group_key(row)));
        }
        Ok(result)
    }

    fn run_query(&mut self, query: &Query) -> Result<QueryResult, AtlantisError> {
        let mut rows = vec![Row::new()];
        let mut call_columns: Option<Vec<String>> = None;
        for clause in &query.clauses {
            call_columns = None;
            rows = match clause {
                Clause::Match {
                    optional,
                    pattern,
                    where_clause,
                } => {
                    let new_vars: Vec<String> = pattern_variables(pattern);
                    let mut out = Vec::new();
                    for row in &rows {
                        let mut matched = Vec::new();
                        for m in self.match_pattern(pattern, row)? {
                            if match where_clause {
                                Some(w) => is_true(&self.eval(w, &m)?),
                                None => true,
                            } {
                                matched.push(m);
                            }
                        }
                        if matched.is_empty() && *optional {
                            let mut r = row.clone();
                            for v in &new_vars {
                                r.entry(v.clone()).or_insert(Value::Null);
                            }
                            matched.push(r);
                        }
                        out.extend(matched);
                    }
                    out
                }
                Clause::Create(pattern) => {
                    for row in &mut rows {
                        for path in &pattern.paths {
                            self.create_path(path, row)?;
                        }
                    }
                    rows
                }
                Clause::Merge {
                    path,
                    on_create,
                    on_match,
                } => {
                    let mut out = Vec::new();
                    for row in &rows {
                        self.check_merge_properties(path, row)?;
                        let found = self.match_path(path, row, &[])?;
                        if found.is_empty() {
                            let mut r = row.clone();
                            self.create_path(path, &mut r)?;
                            self.apply_set(on_create, &mut r)?;
                            out.push(r);
                        } else {
                            for (mut r, _) in found {
                                self.apply_set(on_match, &mut r)?;
                                out.push(r);
                            }
                        }
                    }
                    out
                }
                Clause::Set(items) => {
                    for row in &mut rows {
                        self.apply_set(items, row)?;
                    }
                    rows
                }
                Clause::Remove(items) => {
                    for row in &rows {
                        self.apply_remove(items, row)?;
                    }
                    rows
                }
                Clause::Delete { detach, exprs } => {
                    // Collect every target first, delete relationships, then
                    // nodes: `DELETE n, r` must not fail on edges that the
                    // same clause deletes.
                    let mut targets = Vec::new();
                    for row in &rows {
                        for expr in exprs {
                            collect_delete_targets(self.eval(expr, row)?, &mut targets)?;
                        }
                    }
                    let mut nodes = Vec::new();
                    for id in targets {
                        if self.tx.table(&id.table)?.is_some_and(|t| t.is_edge()) {
                            self.tx.delete_edge(&id)?;
                        } else {
                            nodes.push(id);
                        }
                    }
                    for id in nodes {
                        self.tx.delete(&id, *detach)?;
                    }
                    rows
                }
                Clause::With {
                    projection,
                    where_clause,
                } => {
                    let projected = self.project(projection, rows)?;
                    let mut out = Vec::new();
                    for row in projected.rows {
                        if match where_clause {
                            Some(w) => is_true(&self.eval(w, &row)?),
                            None => true,
                        } {
                            out.push(row);
                        }
                    }
                    out
                }
                Clause::Unwind { expr, variable } => {
                    let mut out = Vec::new();
                    for row in &rows {
                        let items = match self.eval(expr, row)? {
                            Value::Null => Vec::new(),
                            Value::Array(items) => items,
                            Value::Vector(v) => {
                                v.into_iter().map(|x| Value::Float(x as f64)).collect()
                            }
                            other => vec![other],
                        };
                        for item in items {
                            let mut r = row.clone();
                            r.insert(variable.clone(), item);
                            out.push(r);
                        }
                    }
                    out
                }
                Clause::Call {
                    procedure,
                    args,
                    yields,
                    where_clause,
                } => {
                    let mut out = Vec::new();
                    let mut columns_seen = None;
                    for row in &rows {
                        let arg_values: Vec<Value> = args
                            .iter()
                            .map(|a| self.eval(a, row))
                            .collect::<Result<_, _>>()?;
                        let (columns, results) = self.call_procedure(procedure, arg_values)?;
                        let bindings = yield_bindings(&columns, yields.as_ref())?;
                        columns_seen = Some(
                            bindings
                                .iter()
                                .map(|(_, name)| name.clone())
                                .collect::<Vec<_>>(),
                        );
                        for result in results {
                            let mut r = row.clone();
                            for (index, name) in &bindings {
                                r.insert(name.clone(), result[*index].clone());
                            }
                            if match where_clause {
                                Some(w) => is_true(&self.eval(w, &r)?),
                                None => true,
                            } {
                                out.push(r);
                            }
                        }
                    }
                    call_columns = columns_seen;
                    out
                }
            };
        }
        match &query.ret {
            Some(projection) => {
                let projected = self.project(projection, rows)?;
                // Columns from `*` are variables; then one flag per item.
                let star_columns = projected.columns.len() - projection.items.len();
                let expand: Vec<bool> = std::iter::repeat_n(true, star_columns)
                    .chain(projection.items.iter().map(|i| returns_records(&i.expr)))
                    .collect();
                let mut out = Vec::with_capacity(projected.rows.len());
                for row in &projected.rows {
                    let mut values = Vec::with_capacity(projected.columns.len());
                    for (i, col) in projected.columns.iter().enumerate() {
                        let v = row.get(col).cloned().unwrap_or(Value::Null);
                        values.push(if expand.get(i).copied().unwrap_or(false) {
                            self.expand(v)?
                        } else {
                            v
                        });
                    }
                    out.push(values);
                }
                Ok(QueryResult {
                    columns: projected.columns,
                    rows: out,
                })
            }
            // A query ending in CALL returns the yielded columns.
            None => match call_columns {
                Some(columns) => {
                    let out = rows
                        .iter()
                        .map(|r| {
                            columns
                                .iter()
                                .map(|c| r.get(c).cloned().unwrap_or(Value::Null))
                                .collect()
                        })
                        .collect();
                    Ok(QueryResult { columns, rows: out })
                }
                None => Ok(QueryResult::default()),
            },
        }
    }

    /// Turns a record id into its document (recursively inside lists).
    fn expand(&mut self, v: Value) -> Result<Value, AtlantisError> {
        Ok(match v {
            Value::RecordId(id) => match self.load(&id)? {
                Some(record) => {
                    let mut obj = record.data;
                    obj.insert("id".into(), Value::RecordId(record.id));
                    Value::Object(obj)
                }
                None => Value::Null,
            },
            Value::Array(items) => Value::Array(
                items
                    .into_iter()
                    .map(|i| self.expand(i))
                    .collect::<Result<_, _>>()?,
            ),
            other => other,
        })
    }

    // ---- writes ----------------------------------------------------------

    fn target(row: &Row, variable: &str) -> Result<Option<RecordId>, AtlantisError> {
        match row.get(variable) {
            None => Err(invalid(format!("variable `{variable}` is not defined"))),
            Some(Value::Null) => Ok(None),
            Some(Value::RecordId(id)) => Ok(Some(id.clone())),
            Some(other) => Err(invalid(format!(
                "`{variable}` is a {}, not a node or relationship",
                other.type_name()
            ))),
        }
    }

    fn apply_set(&mut self, items: &[SetItem], row: &mut Row) -> Result<(), AtlantisError> {
        for item in items {
            match item {
                SetItem::Property {
                    variable,
                    path,
                    value,
                } => {
                    let value = self.eval(value, row)?;
                    let Some(id) = Self::target(row, variable)? else {
                        continue;
                    };
                    let mut data = self
                        .load(&id)?
                        .ok_or_else(|| AtlantisError::RecordNotFound(id.to_string()))?
                        .data;
                    set_path(&mut data, path, value)?;
                    self.strip_edge_fields(&id, &mut data)?;
                    self.tx.update(&id, data)?;
                }
                SetItem::Replace { variable, value } | SetItem::Merge { variable, value } => {
                    let value = self.eval(value, row)?;
                    let Some(id) = Self::target(row, variable)? else {
                        continue;
                    };
                    let map = match value {
                        Value::Object(map) => map,
                        Value::RecordId(other) => {
                            self.load(&other)?.map(|r| r.data).unwrap_or_default()
                        }
                        Value::Null => Object::new(),
                        other => {
                            return Err(invalid(format!(
                                "SET {variable} needs a map, got {}",
                                other.type_name()
                            )))
                        }
                    };
                    if matches!(item, SetItem::Replace { .. }) {
                        let mut map: Object =
                            map.into_iter().filter(|(_, v)| !v.is_null()).collect();
                        self.strip_edge_fields(&id, &mut map)?;
                        map.remove("id");
                        self.tx.update(&id, map)?;
                    } else {
                        let mut map = map;
                        self.strip_edge_fields(&id, &mut map)?;
                        map.remove("id");
                        self.tx.merge(&id, map)?;
                    }
                }
                SetItem::Labels { variable, labels } => {
                    let Some(id) = Self::target(row, variable)? else {
                        continue;
                    };
                    self.tx.set_labels(&id, labels, &[])?;
                }
            }
        }
        Ok(())
    }

    fn apply_remove(&mut self, items: &[RemoveItem], row: &Row) -> Result<(), AtlantisError> {
        for item in items {
            match item {
                RemoveItem::Property { variable, path } => {
                    let Some(id) = Self::target(row, variable)? else {
                        continue;
                    };
                    let mut data = self
                        .load(&id)?
                        .ok_or_else(|| AtlantisError::RecordNotFound(id.to_string()))?
                        .data;
                    set_path(&mut data, path, Value::Null)?;
                    self.strip_edge_fields(&id, &mut data)?;
                    self.tx.update(&id, data)?;
                }
                RemoveItem::Labels { variable, labels } => {
                    let Some(id) = Self::target(row, variable)? else {
                        continue;
                    };
                    self.tx.set_labels(&id, &[], labels)?;
                }
            }
        }
        Ok(())
    }

    /// `MERGE` cannot match on `null`, so a null property is an error
    /// instead of silently creating a duplicate every time.
    fn check_merge_properties(
        &mut self,
        path: &atlantis_cypher::ast::PathPattern,
        row: &Row,
    ) -> Result<(), AtlantisError> {
        let props = std::iter::once(&path.start.properties).chain(
            path.steps
                .iter()
                .flat_map(|(r, n)| [&r.properties, &n.properties]),
        );
        for list in props {
            for (key, expr) in list {
                if self.eval(expr, row)?.is_null() {
                    return Err(invalid(format!(
                        "MERGE cannot use a null value for property {key}"
                    )));
                }
            }
        }
        Ok(())
    }

    /// Edge documents expose `in`/`out`, which are not writable fields.
    fn strip_edge_fields(&mut self, id: &RecordId, data: &mut Object) -> Result<(), AtlantisError> {
        if self.tx.table(&id.table)?.is_some_and(|t| t.is_edge()) {
            data.remove("in");
            data.remove("out");
        }
        Ok(())
    }

    // ---- projection --------------------------------------------------------

    fn eval_count(
        &mut self,
        expr: Option<&Expression>,
        row: &Row,
        what: &str,
    ) -> Result<Option<usize>, AtlantisError> {
        let Some(expr) = expr else { return Ok(None) };
        match self.eval(expr, row)? {
            Value::Integer(i) if i >= 0 => Ok(Some(i as usize)),
            other => Err(invalid(format!(
                "{what} must be a non-negative integer, got {other}"
            ))),
        }
    }

    fn project(
        &mut self,
        projection: &Projection,
        rows: Vec<Row>,
    ) -> Result<Projected, AtlantisError> {
        let mut columns: Vec<String> = Vec::new();
        // Variables kept by `*`; with aggregates they are grouping keys.
        let mut star_vars: Vec<String> = Vec::new();
        if projection.star {
            star_vars = rows
                .first()
                .map(|r| r.keys().cloned().collect())
                .unwrap_or_default();
            star_vars.sort();
            columns.extend(star_vars.iter().cloned());
        }
        for item in &projection.items {
            columns.push(item.name().to_string());
        }
        let aggregating = projection.items.iter().any(|i| i.expr.contains_aggregate());

        // Each output row keeps a context for ORDER BY: the projected values,
        // plus (when not aggregating) the variables of the input row.
        let mut out: Vec<(Row, Row)> = Vec::new();
        if aggregating {
            let keys: Vec<usize> = (0..projection.items.len())
                .filter(|i| !projection.items[*i].expr.contains_aggregate())
                .collect();
            let mut groups: Vec<(Vec<Value>, Vec<Row>)> = Vec::new();
            let mut index: HashMap<String, usize> = HashMap::new();
            for row in rows {
                let mut key_values: Vec<Value> = keys
                    .iter()
                    .map(|i| self.eval(&projection.items[*i].expr, &row))
                    .collect::<Result<_, _>>()?;
                key_values.extend(
                    star_vars
                        .iter()
                        .map(|v| row.get(v).cloned().unwrap_or(Value::Null)),
                );
                let k = group_key(&key_values);
                match index.get(&k) {
                    Some(&g) => groups[g].1.push(row),
                    None => {
                        index.insert(k, groups.len());
                        groups.push((key_values, vec![row]));
                    }
                }
            }
            if groups.is_empty() && keys.is_empty() && star_vars.is_empty() {
                groups.push((Vec::new(), Vec::new()));
            }
            for (_, group) in groups {
                let first = group.first().cloned().unwrap_or_default();
                let mut projected = Row::new();
                for var in &star_vars {
                    projected.insert(var.clone(), first.get(var).cloned().unwrap_or(Value::Null));
                }
                for item in &projection.items {
                    let value = if item.expr.contains_aggregate() {
                        let substituted = self.substitute_aggregates(&item.expr, &group)?;
                        self.eval(&substituted, &first)?
                    } else {
                        self.eval(&item.expr, &first)?
                    };
                    projected.insert(item.name().to_string(), value);
                }
                out.push((projected.clone(), projected));
            }
        } else {
            for row in rows {
                let mut projected = Row::new();
                if projection.star {
                    projected.extend(row.clone());
                }
                for item in &projection.items {
                    projected.insert(item.name().to_string(), self.eval(&item.expr, &row)?);
                }
                let mut context = row;
                context.extend(projected.clone());
                out.push((projected, context));
            }
        }

        if projection.distinct {
            let mut seen = HashSet::new();
            out.retain(|(p, _)| {
                let values: Vec<Value> = columns
                    .iter()
                    .map(|c| p.get(c).cloned().unwrap_or(Value::Null))
                    .collect();
                seen.insert(group_key(&values))
            });
        }

        if !projection.order_by.is_empty() {
            let mut keyed = Vec::with_capacity(out.len());
            for (projected, context) in out {
                let mut keys = Vec::with_capacity(projection.order_by.len());
                for order in &projection.order_by {
                    // An ORDER BY expression equal to a projected item reuses its value.
                    let same = projection.items.iter().find(|i| i.expr == order.expr);
                    keys.push(match same {
                        Some(item) => projected.get(item.name()).cloned().unwrap_or(Value::Null),
                        None => self.eval(&order.expr, &context)?,
                    });
                }
                keyed.push((keys, projected, context));
            }
            keyed.sort_by(|a, b| {
                for (i, order) in projection.order_by.iter().enumerate() {
                    let mut c = order_cmp(&a.0[i], &b.0[i]);
                    if order.descending {
                        c = c.reverse();
                    }
                    if c != std::cmp::Ordering::Equal {
                        return c;
                    }
                }
                std::cmp::Ordering::Equal
            });
            out = keyed.into_iter().map(|(_, p, c)| (p, c)).collect();
        }

        let skip = self
            .eval_count(projection.skip.as_ref(), &Row::new(), "SKIP")?
            .unwrap_or(0);
        let limit = self.eval_count(projection.limit.as_ref(), &Row::new(), "LIMIT")?;
        let rows = out
            .into_iter()
            .skip(skip)
            .take(limit.unwrap_or(usize::MAX))
            .map(|(p, _)| p)
            .collect();
        Ok(Projected { columns, rows })
    }

    /// Replaces every aggregate call in `expr` with its value over `group`.
    fn substitute_aggregates(
        &mut self,
        expr: &Expression,
        group: &[Row],
    ) -> Result<Expression, AtlantisError> {
        use Expression as E;
        Ok(match expr {
            E::CountStar => E::Literal(Value::Integer(group.len() as i64)),
            E::Function {
                name,
                distinct,
                args,
            } if atlantis_cypher::ast::is_aggregate(name) => {
                E::Literal(self.aggregate(name, *distinct, args, group)?)
            }
            E::Function {
                name,
                distinct,
                args,
            } => E::Function {
                name: name.clone(),
                distinct: *distinct,
                args: args
                    .iter()
                    .map(|a| self.substitute_aggregates(a, group))
                    .collect::<Result<_, _>>()?,
            },
            E::Property(b, k) => {
                E::Property(Box::new(self.substitute_aggregates(b, group)?), k.clone())
            }
            E::Index(a, b) => E::Index(
                Box::new(self.substitute_aggregates(a, group)?),
                Box::new(self.substitute_aggregates(b, group)?),
            ),
            E::Unary(op, e) => E::Unary(*op, Box::new(self.substitute_aggregates(e, group)?)),
            E::Binary(a, op, b) => E::Binary(
                Box::new(self.substitute_aggregates(a, group)?),
                *op,
                Box::new(self.substitute_aggregates(b, group)?),
            ),
            E::IsNull(e, n) => E::IsNull(Box::new(self.substitute_aggregates(e, group)?), *n),
            E::List(items) => E::List(
                items
                    .iter()
                    .map(|i| self.substitute_aggregates(i, group))
                    .collect::<Result<_, _>>()?,
            ),
            E::Map(items) => E::Map(
                items
                    .iter()
                    .map(|(k, v)| Ok((k.clone(), self.substitute_aggregates(v, group)?)))
                    .collect::<Result<_, AtlantisError>>()?,
            ),
            E::Case {
                operand,
                whens,
                default,
            } => E::Case {
                operand: operand
                    .as_ref()
                    .map(|o| self.substitute_aggregates(o, group).map(Box::new))
                    .transpose()?,
                whens: whens
                    .iter()
                    .map(|(a, b)| {
                        Ok((
                            self.substitute_aggregates(a, group)?,
                            self.substitute_aggregates(b, group)?,
                        ))
                    })
                    .collect::<Result<_, AtlantisError>>()?,
                default: default
                    .as_ref()
                    .map(|d| self.substitute_aggregates(d, group).map(Box::new))
                    .transpose()?,
            },
            other => {
                if other.contains_aggregate() {
                    return Err(invalid("aggregates are not supported in this position"));
                }
                other.clone()
            }
        })
    }

    fn aggregate(
        &mut self,
        name: &str,
        distinct: bool,
        args: &[Expression],
        group: &[Row],
    ) -> Result<Value, AtlantisError> {
        let [arg] = args else {
            return Err(invalid(format!("{name}() takes exactly one argument")));
        };
        if arg.contains_aggregate() {
            return Err(invalid("aggregates cannot be nested"));
        }
        let mut values = Vec::new();
        let mut seen = HashSet::new();
        for row in group {
            let v = self.eval(arg, row)?;
            if v.is_null() {
                continue;
            }
            if distinct && !seen.insert(group_key(std::slice::from_ref(&v))) {
                continue;
            }
            values.push(v);
        }
        let lname = name.to_ascii_lowercase();
        Ok(match lname.as_str() {
            "count" => Value::Integer(values.len() as i64),
            "collect" => Value::Array(values),
            "min" => values.into_iter().min_by(order_cmp).unwrap_or(Value::Null),
            "max" => values.into_iter().max_by(order_cmp).unwrap_or(Value::Null),
            "sum" | "avg" | "stdev" => {
                if values.is_empty() {
                    return Ok(if lname == "sum" {
                        Value::Integer(0)
                    } else {
                        Value::Null
                    });
                }
                if lname == "sum" && values.iter().all(|v| matches!(v, Value::Integer(_))) {
                    let mut total: i64 = 0;
                    for v in &values {
                        total = total
                            .checked_add(v.as_int().expect("integer"))
                            .ok_or_else(|| invalid("integer overflow in sum()"))?;
                    }
                    return Ok(Value::Integer(total));
                }
                let nums: Vec<f64> = values
                    .iter()
                    .map(|v| {
                        v.as_float().ok_or_else(|| {
                            invalid(format!("{name}() expects numbers, got {}", v.type_name()))
                        })
                    })
                    .collect::<Result<_, _>>()?;
                let sum: f64 = nums.iter().sum();
                let mean = sum / nums.len() as f64;
                match lname.as_str() {
                    "sum" => Value::Float(sum),
                    "avg" => Value::Float(mean),
                    _ if nums.len() < 2 => Value::Float(0.0),
                    _ => Value::Float(
                        (nums.iter().map(|x| (x - mean).powi(2)).sum::<f64>()
                            / (nums.len() - 1) as f64)
                            .sqrt(),
                    ),
                }
            }
            _ => return Err(invalid(format!("unknown aggregate {name}()"))),
        })
    }

    // ---- procedures --------------------------------------------------------

    fn call_procedure(
        &mut self,
        name: &str,
        args: Vec<Value>,
    ) -> Result<(Vec<String>, Vec<Vec<Value>>), AtlantisError> {
        let cols = |names: &[&str]| names.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        match name.to_ascii_lowercase().as_str() {
            "db.vector.search" => {
                if !(3..=4).contains(&args.len()) {
                    return Err(invalid(
                        "db.vector.search(index, vector, k[, threshold | {threshold, efSearch}])",
                    ));
                }
                let Value::String(target) = &args[0] else {
                    return Err(invalid(
                        "db.vector.search: the first argument is an index name",
                    ));
                };
                let def = match self.tx.vector_index(target)? {
                    Some(def) => def,
                    // A label is accepted when exactly one index covers it.
                    None => {
                        let matching: Vec<VectorIndexDef> = self
                            .tx
                            .vector_indexes()?
                            .into_iter()
                            .filter(|d| &d.label == target)
                            .collect();
                        match matching.as_slice() {
                            [one] => one.clone(),
                            [] => return Err(invalid(format!("no vector index named {target} (create one with CREATE VECTOR INDEX)"))),
                            _ => return Err(invalid(format!("several vector indexes cover label {target}; use the index name"))),
                        }
                    }
                };
                let query = as_vector(&args[1]).ok_or_else(|| {
                    invalid("db.vector.search: the second argument must be a vector")
                })?;
                let k = match &args[2] {
                    Value::Integer(k) if *k > 0 => *k as usize,
                    other => {
                        return Err(invalid(format!(
                            "db.vector.search: k must be a positive integer, got {other}"
                        )))
                    }
                };
                let (threshold, ef) = match args.get(3) {
                    None | Some(Value::Null) => (None, None),
                    Some(Value::Object(opts)) => (
                        opts.get("threshold").and_then(Value::as_float),
                        opts.get("efSearch")
                            .and_then(Value::as_int)
                            .map(|e| e.max(1) as usize),
                    ),
                    Some(v) => (
                        Some(
                            v.as_float()
                                .ok_or_else(|| invalid("threshold must be a number"))?,
                        ),
                        None,
                    ),
                };
                let db = self.tx.database().clone();
                let request = crate::vector_index::VectorQuery {
                    vector: query,
                    k,
                    ef_search: ef,
                    threshold,
                };
                let hits = self.vectors.search(self.store, &db, &def, &request)?;
                Ok((
                    cols(&["node", "score"]),
                    hits.into_iter()
                        .map(|h| vec![Value::RecordId(h.id), Value::Float(h.score)])
                        .collect(),
                ))
            }
            "db.labels" => {
                let tables = self.tx.tables()?;
                Ok((
                    cols(&["label"]),
                    tables
                        .into_iter()
                        .filter(|t| !t.is_edge())
                        .map(|t| vec![Value::String(t.name)])
                        .collect(),
                ))
            }
            "db.relationshiptypes" => {
                let tables = self.tx.tables()?;
                Ok((
                    cols(&["relationshipType"]),
                    tables
                        .into_iter()
                        .filter(|t| t.is_edge())
                        .map(|t| vec![Value::String(t.name)])
                        .collect(),
                ))
            }
            "db.indexes" => {
                let defs = self.tx.vector_indexes()?;
                Ok((
                    cols(&["name", "type", "label", "property", "dimension", "metric"]),
                    defs.into_iter()
                        .map(|d| {
                            vec![
                                Value::String(d.name),
                                Value::String("VECTOR".into()),
                                Value::String(d.label),
                                Value::String(d.field),
                                Value::Integer(d.dimension as i64),
                                Value::String(format!("{:?}", d.metric).to_lowercase()),
                            ]
                        })
                        .collect(),
                ))
            }
            other => Err(invalid(format!("unknown procedure {other}"))),
        }
    }
}

/// Flattens a `DELETE` operand into record ids (lists and paths expand).
fn collect_delete_targets(target: Value, out: &mut Vec<RecordId>) -> Result<(), AtlantisError> {
    match target {
        Value::Null => Ok(()),
        Value::RecordId(id) => {
            out.push(id);
            Ok(())
        }
        Value::Array(items) => items
            .into_iter()
            .try_for_each(|v| collect_delete_targets(v, out)),
        Value::Object(mut path) if path.contains_key("relationships") => {
            if let Some(rels) = path.remove("relationships") {
                collect_delete_targets(rels, out)?;
            }
            match path.remove("nodes") {
                Some(nodes) => collect_delete_targets(nodes, out),
                None => Ok(()),
            }
        }
        other => Err(invalid(format!("cannot delete a {}", other.type_name()))),
    }
}

/// Maps yielded columns to (column index, bound name).
fn yield_bindings(
    columns: &[String],
    yields: Option<&Vec<YieldItem>>,
) -> Result<Vec<(usize, String)>, AtlantisError> {
    match yields {
        None => Ok(columns.iter().cloned().enumerate().collect()),
        Some(items) => items
            .iter()
            .map(|y| {
                let index = columns.iter().position(|c| c == &y.column).ok_or_else(|| {
                    invalid(format!(
                        "procedure has no output column {} (has: {})",
                        y.column,
                        columns.join(", ")
                    ))
                })?;
                Ok((index, y.alias.clone().unwrap_or_else(|| y.column.clone())))
            })
            .collect(),
    }
}

/// `true` when a RETURN item yields records that should be returned as
/// documents: a bare variable, or `collect(variable)`.
fn returns_records(expr: &Expression) -> bool {
    match expr {
        Expression::Variable(_) => true,
        Expression::Function { name, args, .. } if name.eq_ignore_ascii_case("collect") => {
            matches!(args.as_slice(), [Expression::Variable(_)])
        }
        _ => false,
    }
}

/// Sets (or, with `Null`, removes) a nested field.
fn set_path(data: &mut Object, path: &[String], value: Value) -> Result<(), AtlantisError> {
    let (last, parents) = path
        .split_last()
        .ok_or_else(|| invalid("empty property path"))?;
    let mut current = data;
    for key in parents {
        let entry = current
            .entry(key.clone())
            .or_insert_with(|| Value::Object(Object::new()));
        if entry.is_null() {
            *entry = Value::Object(Object::new());
        }
        current = match entry {
            Value::Object(obj) => obj,
            other => {
                return Err(invalid(format!(
                    "cannot set a field inside a {}",
                    other.type_name()
                )))
            }
        };
    }
    if value.is_null() {
        current.remove(last);
    } else {
        current.insert(last.clone(), value);
    }
    Ok(())
}
