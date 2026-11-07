//! Atlantis DB — embedded graph + vector database for AI applications.
//!
//! A high-performance embedded database engine that unifies graph traversal
//! and vector similarity search into a single query layer with a
//! Cypher-compatible query language.
//!
//! # Quick Start
//!
//! ```no_run
//! use atlantis_db::{Engine, Value};
//!
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! let engine = Engine::open("./my_db").await?;
//!
//! engine.execute(
//!     "CREATE (a:Person {name: 'Alice'})-[:KNOWS]->(b:Person {name: 'Bob'})",
//!     &[],
//! ).await?;
//!
//! let result = engine.execute(
//!     "MATCH (a:Person)-[:KNOWS]->(b:Person) WHERE a.name = $name RETURN b.name AS friend",
//!     &[("name", Value::String("Alice".into()))],
//! ).await?;
//!
//! for row in result.rows() {
//!     println!("Friend: {}", row.get_string("friend").unwrap_or(""));
//! }
//! # Ok(())
//! # }
//! ```

pub mod config;
pub mod engine;
pub mod params;
pub mod result_set;

pub use atlantis_core::Value;
pub use config::Config;
pub use engine::Engine;
pub use params::Params;
pub use result_set::{ResultSet, Row};
