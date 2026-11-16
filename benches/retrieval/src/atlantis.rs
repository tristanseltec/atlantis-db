//! The Atlantis DB adapter: embedded engine, Cypher for queries, the record
//! API for bulk loading.
//!
//! Today's engine has no filter inside the vector search, so the filtered
//! workloads over-fetch `k × overfetch` hits and filter them with `WHERE`.
//! That is what an application must do today, and the report shows the
//! recall it gets.

use std::cell::RefCell;
use std::path::PathBuf;
use std::sync::RwLock;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context};
use atlantis_db::{AtlantisError, Engine, Object, RecordId, RecordKey, Value};

use crate::adapter::{Adapter, Capabilities, LoadStats, Memory};
use crate::dataset::{Dataset, Query};

const INDEX: &str = "chunk_vec";
const BATCH: usize = 1_000;
const ROLLBACK: &str = "__rbench_rollback__";

pub struct Atlantis {
    engine: RwLock<Option<Engine>>,
    dir: Option<PathBuf>,
    dim: usize,
    overfetch: usize,
    /// `efSearch` for every search; `None` uses the index default.
    ef_search: Option<usize>,
}

thread_local! {
    static RT: RefCell<Option<tokio::runtime::Runtime>> = const { RefCell::new(None) };
}

/// Drives the engine's async API from plain threads.
fn block_on<F: std::future::Future>(f: F) -> F::Output {
    RT.with(|rt| {
        let mut rt = rt.borrow_mut();
        let rt = rt.get_or_insert_with(|| {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("tokio runtime")
        });
        rt.block_on(f)
    })
}

impl Atlantis {
    pub fn open(
        dir: Option<PathBuf>,
        dim: usize,
        overfetch: usize,
        ef_search: Option<usize>,
    ) -> anyhow::Result<Self> {
        let engine = match &dir {
            Some(d) => block_on(Engine::open(d))?,
            None => block_on(Engine::open_in_memory())?,
        };
        block_on(engine.execute(
            &format!(
                "CREATE VECTOR INDEX {INDEX} IF NOT EXISTS FOR (c:Chunk) ON (c.embedding) \
                 OPTIONS {{dimension: {dim}, metric: 'cosine'}}"
            ),
            &[],
        ))?;
        Ok(Atlantis {
            engine: RwLock::new(Some(engine)),
            dir,
            dim,
            overfetch: overfetch.max(1),
            ef_search,
        })
    }

    fn engine(&self) -> Engine {
        self.engine
            .read()
            .expect("engine lock")
            .clone()
            .expect("engine is open")
    }

    /// `CALL db.vector.search(...)` for `n` hits, with `efSearch` when set.
    fn search_call(&self, n: usize) -> String {
        match self.ef_search {
            // efSearch below the number of hits would truncate the result.
            Some(ef) => format!(
                "CALL db.vector.search('{INDEX}', $q, {n}, {{efSearch: {}}})",
                ef.max(n)
            ),
            None => format!("CALL db.vector.search('{INDEX}', $q, {n})"),
        }
    }

    fn filtered_search(&self, k: usize) -> String {
        format!(
            "{} YIELD node, score \
             WITH node, score WHERE node.user = $u \
             RETURN id(node) AS id, score ORDER BY score DESC LIMIT {k}",
            self.search_call(k * self.overfetch)
        )
    }
}

fn rid(table: &str, key: u64) -> RecordId {
    RecordId::new(table, RecordKey::Integer(key as i64)).expect("valid table")
}

fn ids(rs: &atlantis_db::ResultSet) -> anyhow::Result<Vec<u64>> {
    rs.rows()
        .map(|row| match row.get("id") {
            Some(Value::RecordId(RecordId {
                key: RecordKey::Integer(i),
                ..
            })) => Ok(*i as u64),
            other => Err(anyhow!("unexpected id value {other:?}")),
        })
        .collect()
}

fn chunk_object(user: u32, vector: &[f32], text: &str) -> Object {
    let mut o = Object::new();
    o.insert("user".into(), Value::Integer(user as i64));
    o.insert("text".into(), Value::from(text));
    o.insert("embedding".into(), Value::Vector(vector.to_vec()));
    o
}

fn params(q: &Query) -> [(&'static str, Value); 2] {
    [
        ("q", Value::Vector(q.vector.clone())),
        ("u", Value::Integer(q.user as i64)),
    ]
}

impl Adapter for Atlantis {
    fn name(&self) -> String {
        match self.dir {
            Some(_) => "atlantis (embedded, directory)".into(),
            None => "atlantis (embedded, in-memory)".into(),
        }
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            filter_in_search: false,
            hybrid_text: false,
            search_reads_own_writes: false,
            graph_in_query: true,
            embedded: true,
        }
    }

    fn load(&self, ds: &Dataset) -> anyhow::Result<LoadStats> {
        let engine = self.engine();
        let start = Instant::now();
        let mut items = 0;

        for range in (0..ds.spec.docs()).collect::<Vec<_>>().chunks(BATCH) {
            block_on(engine.transaction(|tx| {
                for &d in range {
                    let mut o = Object::new();
                    o.insert("title".into(), Value::from(format!("document {d}")));
                    tx.create("Doc", Some(RecordKey::Integer(d as i64)), vec![], o)?;
                }
                Ok(())
            }))?;
            items += range.len();
        }
        for range in (0..ds.spec.entities()).collect::<Vec<_>>().chunks(BATCH) {
            block_on(engine.transaction(|tx| {
                for &e in range {
                    let mut o = Object::new();
                    o.insert("name".into(), Value::from(format!("entity {e}")));
                    tx.create("Entity", Some(RecordKey::Integer(e as i64)), vec![], o)?;
                }
                Ok(())
            }))?;
            items += range.len();
        }
        for range in (0..ds.spec.chunks).collect::<Vec<_>>().chunks(BATCH) {
            block_on(engine.transaction(|tx| {
                for &c in range {
                    let o = chunk_object(ds.user[c], ds.vector(c), &ds.text[c]);
                    let chunk =
                        tx.create("Chunk", Some(RecordKey::Integer(c as i64)), vec![], o)?;
                    tx.relate(
                        &chunk.id,
                        "PART_OF",
                        &rid("Doc", ds.doc_of[c] as u64),
                        Object::new(),
                    )?;
                    tx.relate(
                        &chunk.id,
                        "MENTIONS",
                        &rid("Entity", ds.mentions[c] as u64),
                        Object::new(),
                    )?;
                }
                Ok(())
            }))?;
            items += range.len() * 3;
        }
        for pairs in ds.related.chunks(BATCH) {
            block_on(engine.transaction(|tx| {
                for &(f, t) in pairs {
                    tx.relate(
                        &rid("Entity", f as u64),
                        "RELATED",
                        &rid("Entity", t as u64),
                        Object::new(),
                    )?;
                }
                Ok(())
            }))?;
            items += pairs.len();
        }
        let load = start.elapsed();

        let build = Instant::now();
        self.vector_topk(&ds.queries[0], 1, false)
            .context("first search (index build)")?;
        Ok(LoadStats {
            items,
            load,
            index_build: build.elapsed(),
        })
    }

    fn vector_topk(&self, q: &Query, k: usize, filtered: bool) -> anyhow::Result<Vec<u64>> {
        let engine = self.engine();
        let rs = if filtered {
            block_on(engine.execute(&self.filtered_search(k), &params(q)))?
        } else {
            block_on(engine.execute(
                &format!("{} YIELD node RETURN id(node) AS id", self.search_call(k)),
                &[("q", Value::Vector(q.vector.clone()))],
            ))?
        };
        ids(&rs)
    }

    fn graph_rag(&self, q: &Query, k: usize) -> anyhow::Result<usize> {
        // No BM25 yet: the text half of the hybrid query is skipped (reported
        // through `hybrid_text: false`).
        let cypher = format!(
            "{} YIELD node, score \
             WITH node, score WHERE node.user = $u \
             WITH node, score ORDER BY score DESC LIMIT {k} \
             MATCH (node)-[:PART_OF]->(d:Doc) \
             OPTIONAL MATCH (node)-[:MENTIONS]->(:Entity)-[:RELATED]->(r:Entity) \
             RETURN id(node) AS id, d.title AS title, node.text AS text, score, \
                    collect(DISTINCT r.name) AS context",
            self.search_call(k * self.overfetch)
        );
        let rs = block_on(self.engine().execute(&cypher, &params(q)))?;
        Ok(rs.row_count())
    }

    fn write_memory(&self, m: &Memory) -> anyhow::Result<()> {
        block_on(self.engine().transaction(|tx| {
            let chunk = tx.create(
                "Chunk",
                Some(RecordKey::Integer(m.id as i64)),
                vec![],
                chunk_object(m.user, m.vector, m.text),
            )?;
            tx.relate(
                &chunk.id,
                "MENTIONS",
                &rid("Entity", m.entity as u64),
                Object::new(),
            )?;
            Ok(())
        }))?;
        Ok(())
    }

    fn reads_own_write(&self, m: &Memory) -> anyhow::Result<bool> {
        let mut found = false;
        let query = Query {
            vector: m.vector.to_vec(),
            user: m.user,
            text: m.text.to_string(),
        };
        let cypher = self.filtered_search(1);
        let result = block_on(self.engine().transaction(|tx| {
            tx.create(
                "Chunk",
                Some(RecordKey::Integer(m.id as i64)),
                vec![],
                chunk_object(m.user, m.vector, m.text),
            )?;
            let rs = tx.execute(&cypher, &params(&query))?;
            found = ids(&rs).map_err(|e| AtlantisError::InvalidQuery(e.to_string()))? == vec![m.id];
            Err::<(), _>(AtlantisError::InvalidQuery(ROLLBACK.into()))
        }));
        match result {
            Err(AtlantisError::InvalidQuery(msg)) if msg == ROLLBACK => Ok(found),
            Err(e) => Err(e.into()),
            Ok(()) => unreachable!("the probe always rolls back"),
        }
    }

    fn reopen(&self) -> anyhow::Result<Option<Duration>> {
        let Some(dir) = &self.dir else {
            return Ok(None);
        };
        let mut slot = self.engine.write().expect("engine lock");
        if let Some(old) = slot.take() {
            block_on(old.shutdown())?;
        }
        let start = Instant::now();
        let engine = block_on(Engine::open(dir))?;
        let probe = vec![1.0 / (self.dim as f32).sqrt(); self.dim];
        block_on(engine.execute(
            &format!("CALL db.vector.search('{INDEX}', $q, 1) YIELD node RETURN id(node) AS id"),
            &[("q", Value::Vector(probe))],
        ))?;
        let elapsed = start.elapsed();
        *slot = Some(engine);
        Ok(Some(elapsed))
    }
}
