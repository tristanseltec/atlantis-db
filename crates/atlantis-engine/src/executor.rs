//! Query executor — walks the logical plan and produces result rows.

use std::collections::HashMap;

use atlantis_core::{
    AtlantisError, Direction, Label, LabelSet, NodeId, PropertyMap, RelType, Value,
};
use atlantis_cypher::parser::ast::{BinaryOp, Expression, Statement};
use atlantis_cypher::planner::LogicalOperator;
use atlantis_cypher::{optimize, plan};
use atlantis_storage::GraphStore;
use atlantis_vector::HnswIndex;

use crate::command::QueryResult;

/// A row in the query result: maps variable names to values.
#[derive(Debug, Clone, Default)]
pub struct Row {
    pub values: HashMap<String, Value>,
}

impl Row {
    /// Creates a new empty row.
    pub fn new() -> Self {
        Row {
            values: HashMap::new(),
        }
    }

    /// Gets a value by variable name.
    pub fn get(&self, name: &str) -> Option<&Value> {
        self.values.get(name)
    }

    /// Sets a variable to a value.
    pub fn set(&mut self, name: impl Into<String>, value: Value) {
        self.values.insert(name.into(), value);
    }
}

/// The query executor. Operates on the graph store and vector index.
pub struct QueryExecutor<'a> {
    store: &'a mut GraphStore,
    vector_index: Option<&'a HnswIndex>,
}

impl<'a> QueryExecutor<'a> {
    /// Creates a new executor with the given store and optional vector index.
    pub fn new(store: &'a mut GraphStore, vector_index: Option<&'a HnswIndex>) -> Self {
        QueryExecutor { store, vector_index }
    }

    /// Parses, plans, optimizes, and executes a Cypher query.
    pub fn execute(
        &mut self,
        cypher: &str,
        params: &HashMap<String, Value>,
    ) -> Result<QueryResult, AtlantisError> {
        let statement = atlantis_cypher::parse(cypher)?;
        let logical_plan = plan(&statement)?;
        let optimized = optimize(logical_plan);

        let (columns, rows) = self.execute_plan(&optimized, params)?;

        // Convert rows to value vectors
        let result_rows: Vec<Vec<Value>> = rows
            .iter()
            .map(|row| {
                columns
                    .iter()
                    .map(|col| row.get(col).cloned().unwrap_or(Value::Null))
                    .collect()
            })
            .collect();

        Ok(QueryResult {
            columns,
            rows: result_rows,
        })
    }

    fn execute_plan(
        &mut self,
        op: &LogicalOperator,
        params: &HashMap<String, Value>,
    ) -> Result<(Vec<String>, Vec<Row>), AtlantisError> {
        match op {
            LogicalOperator::Empty => Ok((vec![], vec![])),

            LogicalOperator::NodeByLabelScan { var, label, .. } => {
                let node_ids = self.store.get_nodes_by_label(label);
                let rows: Vec<Row> = node_ids
                    .into_iter()
                    .map(|id| {
                        let mut row = Row::new();
                        row.set(var.clone(), Value::Integer(id.0 as i64));
                        row
                    })
                    .collect();
                Ok((vec![], rows))
            }

            LogicalOperator::AllNodesScan { var, .. } => {
                let node_ids = self.store.get_all_nodes();
                let rows: Vec<Row> = node_ids
                    .into_iter()
                    .map(|id| {
                        let mut row = Row::new();
                        row.set(var.clone(), Value::Integer(id.0 as i64));
                        row
                    })
                    .collect();
                Ok((vec![], rows))
            }

            LogicalOperator::Expand {
                from,
                to,
                rel_type,
                direction,
                input,
                ..
            } => {
                let (_, input_rows) = self.execute_plan(input, params)?;
                let mut result_rows = Vec::new();

                for row in &input_rows {
                    let from_id = match row.get(from) {
                        Some(Value::Integer(id)) => NodeId(*id as u64),
                        _ => continue,
                    };

                    if let Some(rt) = rel_type {
                        let neighbors = self.store.get_neighbors(from_id, rt);
                        for (neighbor_id, _edge_id) in neighbors {
                            let mut new_row = row.clone();
                            new_row.set(to.clone(), Value::Integer(neighbor_id.0 as i64));
                            result_rows.push(new_row);
                        }
                    } else {
                        // No rel_type specified — expand all edges
                        // For v1, we skip this case
                    }
                }

                Ok((vec![], result_rows))
            }

            LogicalOperator::Filter { input, predicate } => {
                let (cols, rows) = self.execute_plan(input, params)?;
                let filtered: Vec<Row> = rows
                    .into_iter()
                    .filter(|row| {
                        let val = self.eval_expr(predicate, row, params);
                        matches!(val, Some(Value::Bool(true)))
                    })
                    .collect();
                Ok((cols, filtered))
            }

            LogicalOperator::Project { input, items } => {
                let (_, rows) = self.execute_plan(input, params)?;
                let mut result_rows = Vec::new();

                let columns: Vec<String> = items
                    .iter()
                    .map(|(expr, alias)| {
                        alias
                            .clone()
                            .or_else(|| {
                                if let Expression::Variable(name) = expr {
                                    Some(name.clone())
                                } else if let Expression::Property(inner, prop) = expr {
                                    if let Expression::Variable(name) = inner.as_ref() {
                                        Some(format!("{name}.{prop}"))
                                    } else {
                                        None
                                    }
                                } else {
                                    None
                                }
                            })
                            .unwrap_or_else(|| "expr".to_string())
                    })
                    .collect();

                for row in &rows {
                    let mut new_row = Row::new();
                    for ((expr, _), col) in items.iter().zip(columns.iter()) {
                        let val = self.eval_expr(expr, row, params).unwrap_or(Value::Null);
                        new_row.set(col.clone(), val);
                    }
                    result_rows.push(new_row);
                }

                Ok((columns, result_rows))
            }

            LogicalOperator::Sort { input, keys } => {
                let (cols, mut rows) = self.execute_plan(input, params)?;
                rows.sort_by(|a, b| {
                    for (expr, desc) in keys {
                        let va = self.eval_expr(expr, a, params).unwrap_or(Value::Null);
                        let vb = self.eval_expr(expr, b, params).unwrap_or(Value::Null);
                        let cmp = va.partial_cmp(&vb).unwrap_or(std::cmp::Ordering::Equal);
                        if *desc {
                            return cmp.reverse();
                        }
                        if cmp != std::cmp::Ordering::Equal {
                            return cmp;
                        }
                    }
                    std::cmp::Ordering::Equal
                });
                Ok((cols, rows))
            }

            LogicalOperator::Limit { input, count } => {
                let (cols, rows) = self.execute_plan(input, params)?;
                let limit = match self.eval_expr(count, &Row::new(), params) {
                    Some(Value::Integer(n)) => n as usize,
                    _ => rows.len(),
                };
                Ok((cols, rows.into_iter().take(limit).collect()))
            }

            LogicalOperator::Skip { input, count } => {
                let (cols, rows) = self.execute_plan(input, params)?;
                let skip = match self.eval_expr(count, &Row::new(), params) {
                    Some(Value::Integer(n)) => n as usize,
                    _ => 0,
                };
                Ok((cols, rows.into_iter().skip(skip).collect()))
            }

            LogicalOperator::CreateNode {
                var,
                labels,
                properties,
            } => {
                let label_set: LabelSet = labels.iter().cloned().collect();
                let mut prop_map = PropertyMap::new();
                for (key, expr) in properties {
                    let val = self.eval_expr(expr, &Row::new(), params).unwrap_or(Value::Null);
                    prop_map.insert(key.clone(), val);
                }

                let id = self.store.insert_node(label_set, prop_map);
                let mut row = Row::new();
                row.set(var.clone(), Value::Integer(id.0 as i64));
                Ok((vec![var.clone()], vec![row]))
            }

            LogicalOperator::SetProperty { input, target, key, value } => {
                let (cols, rows) = self.execute_plan(input, params)?;
                for row in &rows {
                    if let Some(Value::Integer(id)) = self.eval_expr(target, row, params) {
                        let val = self.eval_expr(value, row, params).unwrap_or(Value::Null);
                        self.store.set_property(NodeId(id as u64), key.clone(), val);
                    }
                }
                Ok((cols, rows))
            }

            LogicalOperator::Delete { input, targets, detach } => {
                let (cols, rows) = self.execute_plan(input, params)?;
                for row in &rows {
                    for target in targets {
                        if let Some(Value::Integer(id)) = self.eval_expr(target, row, params) {
                            let node_id = NodeId(id as u64);
                            if *detach {
                                // DETACH DELETE: remove all edges first
                                // For v1, delete_node handles this
                            }
                            self.store.delete_node(node_id);
                        }
                    }
                }
                Ok((cols, rows))
            }

            LogicalOperator::VectorSearch {
                label,
                query,
                k,
                threshold,
                node_var,
                score_var,
            } => {
                let index = self.vector_index.ok_or_else(|| {
                    AtlantisError::Vector("no vector index available".into())
                })?;

                let query_vec = match self.eval_expr(query, &Row::new(), params) {
                    Some(Value::Vector(v)) => v,
                    _ => return Err(AtlantisError::Vector("query must be a vector".into())),
                };

                let results = if let Some(thr) = threshold {
                    index
                        .search_with_params(
                            &query_vec,
                            &atlantis_vector::SearchParams::top_k(*k).with_threshold(*thr),
                        )?
                } else {
                    index.search(&query_vec, *k, None)?
                };

                // Filter by label — for v1 we return all results
                // (proper label filtering requires tracking which nodes have which labels in the index)
                let _ = label;

                let rows: Vec<Row> = results
                    .into_iter()
                    .map(|(id, dist)| {
                        let mut row = Row::new();
                        row.set(node_var.clone(), Value::Integer(id.0 as i64));
                        // Convert distance to similarity score (1 - distance for cosine)
                        row.set(score_var.clone(), Value::Float(1.0 - dist as f64));
                        row
                    })
                    .collect();

                Ok((vec![], rows))
            }

            LogicalOperator::Sequence(ops) => {
                let mut last_result = (vec![], vec![]);
                for op in ops {
                    last_result = self.execute_plan(op, params)?;
                }
                Ok(last_result)
            }

            LogicalOperator::Unwind {
                input,
                expression,
                variable,
            } => {
                let (cols, input_rows) = self.execute_plan(input, params)?;
                let mut result_rows = Vec::new();

                for row in &input_rows {
                    if let Some(Value::Array(items)) = self.eval_expr(expression, row, params) {
                        for item in items {
                            let mut new_row = row.clone();
                            new_row.set(variable.clone(), item);
                            result_rows.push(new_row);
                        }
                    }
                }

                Ok((cols, result_rows))
            }

            LogicalOperator::VarLenExpand { .. } => {
                // TODO: implement variable-length expansion
                Ok((vec![], vec![]))
            }

            LogicalOperator::CreateRelationship { .. } => {
                // TODO: implement relationship creation
                Ok((vec![], vec![]))
            }
        }
    }

    /// Evaluates an expression in the context of a row and parameters.
    fn eval_expr(
        &self,
        expr: &Expression,
        row: &Row,
        params: &HashMap<String, Value>,
    ) -> Option<Value> {
        match expr {
            Expression::Literal(v) => Some(v.clone()),
            Expression::Variable(name) => {
                if let Some(v) = row.get(name) {
                    Some(v.clone())
                } else {
                    params.get(name).cloned()
                }
            }
            Expression::Property(inner, prop) => {
                if let Some(Value::Integer(id)) = self.eval_expr(inner, row, params) {
                    if let Some(node) = self.store.get_node_hot(NodeId(id as u64)) {
                        return node.get_property(prop).cloned();
                    }
                }
                None
            }
            Expression::Parameter(name) => params.get(name).cloned(),
            Expression::BinaryOp { left, op, right } => {
                let lv = self.eval_expr(left, row, params)?;
                let rv = self.eval_expr(right, row, params)?;

                Some(match op {
                    BinaryOp::Eq => Value::Bool(lv == rv),
                    BinaryOp::NotEq => Value::Bool(lv != rv),
                    BinaryOp::Lt => Value::Bool(lv.partial_cmp(&rv) == Some(std::cmp::Ordering::Less)),
                    BinaryOp::Gt => Value::Bool(
                        lv.partial_cmp(&rv) == Some(std::cmp::Ordering::Greater),
                    ),
                    BinaryOp::LtEq => Value::Bool(
                        lv.partial_cmp(&rv).map(|o| o != std::cmp::Ordering::Greater)
                            .unwrap_or(false),
                    ),
                    BinaryOp::GtEq => Value::Bool(
                        lv.partial_cmp(&rv).map(|o| o != std::cmp::Ordering::Less)
                            .unwrap_or(false),
                    ),
                    BinaryOp::And => {
                        let lb = matches!(lv, Value::Bool(true));
                        let rb = matches!(rv, Value::Bool(true));
                        Value::Bool(lb && rb)
                    }
                    BinaryOp::Or => {
                        let lb = matches!(lv, Value::Bool(true));
                        let rb = matches!(rv, Value::Bool(true));
                        Value::Bool(lb || rb)
                    }
                    BinaryOp::Add => match (lv, rv) {
                        (Value::Integer(a), Value::Integer(b)) => Value::Integer(a + b),
                        (Value::Float(a), Value::Float(b)) => Value::Float(a + b),
                        (Value::String(a), Value::String(b)) => Value::String(format!("{a}{b}")),
                        _ => Value::Null,
                    },
                    BinaryOp::Sub => match (lv, rv) {
                        (Value::Integer(a), Value::Integer(b)) => Value::Integer(a - b),
                        (Value::Float(a), Value::Float(b)) => Value::Float(a - b),
                        _ => Value::Null,
                    },
                    BinaryOp::Mul => match (lv, rv) {
                        (Value::Integer(a), Value::Integer(b)) => Value::Integer(a * b),
                        (Value::Float(a), Value::Float(b)) => Value::Float(a * b),
                        _ => Value::Null,
                    },
                    BinaryOp::Div => match (lv, rv) {
                        (Value::Integer(a), Value::Integer(b)) if b != 0 => Value::Integer(a / b),
                        (Value::Float(a), Value::Float(b)) => Value::Float(a / b),
                        _ => Value::Null,
                    },
                    BinaryOp::In => match rv {
                        Value::Array(items) => Value::Bool(items.contains(&lv)),
                        _ => Value::Bool(false),
                    },
                    _ => Value::Null,
                })
            }
            Expression::UnaryOp { op, operand } => {
                let val = self.eval_expr(operand, row, params)?;
                match op {
                    atlantis_cypher::parser::ast::UnaryOp::Neg => match val {
                        Value::Integer(i) => Some(Value::Integer(-i)),
                        Value::Float(f) => Some(Value::Float(-f)),
                        _ => Some(Value::Null),
                    },
                    atlantis_cypher::parser::ast::UnaryOp::Not => match val {
                        Value::Bool(b) => Some(Value::Bool(!b)),
                        _ => Some(Value::Null),
                    },
                }
            }
            Expression::FunctionCall(name, args) => {
                if name == "db.vector.similarity" && args.len() == 2 {
                    let a = self.eval_expr(&args[0], row, params)?;
                    let b = self.eval_expr(&args[1], row, params)?;
                    if let (Value::Vector(va), Value::Vector(vb)) = (a, b) {
                        let dot: f32 = va.iter().zip(vb.iter()).map(|(x, y)| x * y).sum();
                        let norm_a: f32 = va.iter().map(|x| x * x).sum::<f32>().sqrt();
                        let norm_b: f32 = vb.iter().map(|x| x * x).sum::<f32>().sqrt();
                        if norm_a > 0.0 && norm_b > 0.0 {
                            return Some(Value::Float((dot / (norm_a * norm_b)) as f64));
                        }
                    }
                    return Some(Value::Float(0.0));
                }
                None
            }
            Expression::List(items) => {
                let values: Vec<Value> = items
                    .iter()
                    .filter_map(|item| self.eval_expr(item, row, params))
                    .collect();
                Some(Value::Array(values))
            }
            _ => None,
        }
    }
}
