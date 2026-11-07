//! Background checkpoint actor.
//!
//! Periodically checkpoints the graph store to disk.

use std::time::Duration;

/// Runs the checkpoint loop. For v1, this is a placeholder that does nothing
/// since checkpointing is triggered on shutdown and explicitly by the user.
pub async fn run_checkpoint_loop() {
    let mut interval = tokio::time::interval(Duration::from_secs(300));
    loop {
        interval.tick().await;
        // Checkpointing is handled explicitly by the user or on shutdown in v1.
        // Future: communicate with the core actor via channel.
    }
}
