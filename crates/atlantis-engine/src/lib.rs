//! Query execution, transactions and derived indexes for Atlantis DB.
//!
//! [`Datastore`] owns the durable key-value store, the record store on top
//! of it, and the vector indexes derived from records. Cypher statements
//! are parsed by `atlantis-cypher` and interpreted clause by clause against
//! a record transaction, so graph queries, documents and the Rust record
//! API all read and write the same data.

pub mod datastore;
mod exec;
pub mod vector_index;

pub use datastore::{Datastore, Transaction, LOCK_FILE};
pub use exec::{Params, QueryResult};
pub use vector_index::{VectorHit, VectorIndexes, VectorQuery};
