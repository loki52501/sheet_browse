//! Scheduler (H-5). SKELETON owned by area core (T-08, T-16): change freely.
//! The row types are contract and live in `memory` (D-48).

use crate::memory::MemoryStore;
pub use crate::memory::{Schedule, ScheduleKind};
use std::sync::Arc;

/// The `harnessd` loop: fire due schedules as fresh Claude Code sessions with a run folder.
pub async fn run_loop(store: Arc<dyn MemoryStore>) -> crate::memory::Result<()> {
    let _ = store;
    todo!("T-08: H-5 loop")
}
