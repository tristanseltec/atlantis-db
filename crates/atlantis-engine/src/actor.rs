//! Core engine actor — owns storage, vector index, and executor.

use std::collections::HashMap;

use atlantis_core::{AtlantisError, EngineConfig, Label, LabelSet, Node, NodeId, PropertyMap, Value};
use atlantis_storage::GraphStore;
use atlantis_vector::{distance::from_kind, HnswIndex};
use tokio::sync::mpsc;

use crate::command::{Command, QueryResult};
use crate::executor::QueryExecutor;

/// The core engine actor that processes all database operations.
///
/// Owns the graph store and HNSW vector index. Processes one command at a
/// time, guaranteeing no data races. Background actors for tiering and
/// checkpointing are spawned separately.
pub struct CoreEngineActor {
    store: GraphStore,
    vector_index: Option<HnswIndex>,
    config: EngineConfig,
}

impl CoreEngineActor {
    /// Creates a new engine actor with the given configuration.
    pub fn new(config: EngineConfig) -> Result<Self, AtlantisError> {
        let store = if let Some(ref path) = config.storage.disk_path {
            GraphStore::open(path)?
        } else {
            GraphStore::new_in_memory()
        };

        let vector_index = HnswIndex::new(
            config.vector.hnsw.clone(),
            config.vector.dimensions,
            from_kind(config.vector.distance_metric),
        );

        Ok(CoreEngineActor {
            store,
            vector_index: Some(vector_index),
            config,
        })
    }

    /// Runs the actor loop, processing commands until the channel closes.
    pub async fn run(mut self, mut rx: mpsc::Receiver<Command>) {
        while let Some(cmd) = rx.recv().await {
            match cmd {
                Command::Query {
                    cypher,
                    params,
                    respond,
                } => {
                    let result = self.execute_query(&cypher, &params);
                    let _ = respond.send(result);
                }
                Command::CreateNode {
                    labels,
                    properties,
                    respond,
                } => {
                    let label_set: LabelSet = labels
                        .into_iter()
                        .map(Label::from)
                        .collect();
                    let prop_map: PropertyMap = properties.into_iter().collect();
                    let id = self.store.insert_node(label_set, prop_map);
                    let _ = respond.send(Ok(id));
                }
                Command::GetNode { id, respond } => {
                    let node = self.store.get_node_owned(id);
                    let _ = respond.send(Ok(node));
                }
                Command::Checkpoint { respond } => {
                    let result = self.store.checkpoint();
                    let _ = respond.send(result);
                }
                Command::Shutdown { respond } => {
                    // Flush to disk if configured
                    if self.config.storage.disk_path.is_some() {
                        let _ = self.store.checkpoint();
                    }
                    let _ = respond.send(Ok(()));
                    break;
                }
            }
        }
    }

    fn execute_query(
        &mut self,
        cypher: &str,
        params: &HashMap<String, Value>,
    ) -> Result<QueryResult, AtlantisError> {
        let mut executor = QueryExecutor::new(&mut self.store, self.vector_index.as_ref());
        executor.execute(cypher, params)
    }

    /// Returns a reference to the graph store (for testing).
    pub fn store(&self) -> &GraphStore {
        &self.store
    }

    /// Returns a reference to the vector index (for testing).
    pub fn vector_index(&self) -> Option<&HnswIndex> {
        self.vector_index.as_ref()
    }
}

/// Spawns the engine actor and returns a handle.
///
/// Creates the actor, spawns it on the tokio runtime, and returns
/// a clonable `EngineHandle` for sending commands.
pub fn spawn_engine(config: EngineConfig) -> Result<crate::handle::EngineHandle, AtlantisError> {
    let actor = CoreEngineActor::new(config)?;
    let (tx, rx) = mpsc::channel(128);
    tokio::spawn(actor.run(rx));
    Ok(crate::handle::EngineHandle::new(tx))
}
