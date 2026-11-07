//! The public `Engine` type — the main entry point for Atlantis DB.

use std::path::Path;

use atlantis_core::{AtlantisError, EngineConfig, Value};
use atlantis_engine::{spawn_engine, EngineHandle};

use crate::params::Params;
use crate::result_set::ResultSet;

/// The main entry point for Atlantis DB.
///
/// An `Engine` wraps a handle to the core actor. It is cheaply clonable
/// and safe to share across async tasks.
///
/// # Example
///
/// ```no_run
/// use atlantis_db::{Engine, Value};
///
/// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
/// let engine = Engine::open("./my_db").await?;
/// engine.execute("CREATE (n:Person {name: 'Alice'}) RETURN n", &[]).await?;
/// # Ok(())
/// # }
/// ```
#[derive(Clone)]
pub struct Engine {
    handle: EngineHandle,
}

impl Engine {
    /// Opens or creates a database at the given path.
    ///
    /// If a checkpoint exists at the path, the database state is restored.
    /// Otherwise, a new empty database is created.
    pub async fn open(path: impl AsRef<Path>) -> Result<Self, AtlantisError> {
        let config = EngineConfig::builder()
            .disk_path(path.as_ref().to_path_buf())
            .build()?;
        Self::open_with_config(config).await
    }

    /// Opens or creates a database with a custom configuration.
    pub async fn open_with_config(config: EngineConfig) -> Result<Self, AtlantisError> {
        let handle = spawn_engine(config)?;
        Ok(Engine { handle })
    }

    /// Opens an in-memory database (no disk persistence).
    pub async fn open_in_memory() -> Result<Self, AtlantisError> {
        let config = EngineConfig::default();
        Self::open_with_config(config).await
    }

    /// Executes a Cypher query with parameters.
    ///
    /// The `params` slice contains `(name, value)` pairs that are
    /// substituted for `$name` references in the query.
    pub async fn execute(
        &self,
        cypher: &str,
        params: &[(&str, Value)],
    ) -> Result<ResultSet, AtlantisError> {
        let params = Params::from(params).into_map();
        let result = self.handle.query(cypher, params).await?;
        Ok(ResultSet::new(result.columns, result.rows))
    }

    /// Triggers a checkpoint to disk.
    pub async fn checkpoint(&self) -> Result<(), AtlantisError> {
        self.handle.checkpoint().await
    }

    /// Shuts down the engine, flushing data to disk.
    pub async fn shutdown(&self) -> Result<(), AtlantisError> {
        self.handle.shutdown().await
    }
}
