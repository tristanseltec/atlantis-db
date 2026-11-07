//! Clonable handle to the engine actor.

use std::collections::HashMap;

use atlantis_core::{AtlantisError, NodeId, Value};
use tokio::sync::{mpsc, oneshot};

use crate::command::{Command, QueryResult, Node};

/// A clonable, cheap handle to the core engine actor.
///
/// Holds an `mpsc::Sender` to the engine actor's command channel.
/// Safe to clone and share across async tasks.
#[derive(Clone)]
pub struct EngineHandle {
    tx: mpsc::Sender<Command>,
}

impl EngineHandle {
    /// Creates a new engine handle from the command channel sender.
    pub fn new(tx: mpsc::Sender<Command>) -> Self {
        EngineHandle { tx }
    }

    /// Executes a Cypher query with parameters.
    pub async fn query(
        &self,
        cypher: &str,
        params: HashMap<String, Value>,
    ) -> Result<QueryResult, AtlantisError> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(Command::Query {
                cypher: cypher.to_string(),
                params,
                respond: tx,
            })
            .await
            .map_err(|_| AtlantisError::Shutdown)?;
        rx.await.map_err(|_| AtlantisError::Shutdown)?
    }

    /// Creates a node directly.
    pub async fn create_node(
        &self,
        labels: Vec<String>,
        properties: HashMap<String, Value>,
    ) -> Result<NodeId, AtlantisError> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(Command::CreateNode {
                labels,
                properties,
                respond: tx,
            })
            .await
            .map_err(|_| AtlantisError::Shutdown)?;
        rx.await.map_err(|_| AtlantisError::Shutdown)?
    }

    /// Gets a node by ID.
    pub async fn get_node(&self, id: NodeId) -> Result<Option<Node>, AtlantisError> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(Command::GetNode {
                id,
                respond: tx,
            })
            .await
            .map_err(|_| AtlantisError::Shutdown)?;
        rx.await.map_err(|_| AtlantisError::Shutdown)?
    }

    /// Triggers a checkpoint.
    pub async fn checkpoint(&self) -> Result<(), AtlantisError> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(Command::Checkpoint { respond: tx })
            .await
            .map_err(|_| AtlantisError::Shutdown)?;
        rx.await.map_err(|_| AtlantisError::Shutdown)?
    }

    /// Shuts down the engine.
    pub async fn shutdown(&self) -> Result<(), AtlantisError> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(Command::Shutdown { respond: tx })
            .await
            .map_err(|_| AtlantisError::Shutdown)?;
        rx.await.map_err(|_| AtlantisError::Shutdown)?
    }
}
