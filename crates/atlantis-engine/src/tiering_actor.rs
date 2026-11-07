//! Background tiering actor.
//!
//! Periodically checks if the hot tier exceeds its memory budget and
//! evicts cold data to the cold tier.

use std::time::Duration;

/// Runs the tiering loop. For v1, this is a placeholder that does nothing
/// since the GraphStore handles tiering inline.
pub async fn run_tiering_loop() {
    let mut interval = tokio::time::interval(Duration::from_secs(60));
    loop {
        interval.tick().await;
        // Tiering is handled inline by the store in v1.
        // Future: communicate with the core actor via channel.
    }
}
