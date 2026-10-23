//! Ledger (H-3) and `report.md` (Q-4). SKELETON owned by area core (T-06): change freely.
//! The row type is `memory::LedgerEvent` (contract); this module writes it twice:
//! DB first through `MemoryStore::append`, then one line in `runs/<run>/ledger.jsonl`.

use crate::memory::{LedgerEvent, MemoryStore, Result};
use crate::run::RunFolder;
use std::sync::Arc;

pub struct Ledger {
    store: Arc<dyn MemoryStore>,
    run: RunFolder,
}

impl Ledger {
    pub fn new(store: Arc<dyn MemoryStore>, run: RunFolder) -> Ledger {
        Ledger { store, run }
    }

    /// Insert, then mirror to jsonl. The event must already be scrubbed.
    pub async fn write(&self, e: LedgerEvent) -> Result<()> {
        let _ = (&self.store, &self.run, e);
        todo!("T-06: H-3 table + jsonl mirror")
    }
}

/// `report.md` from a run's events (H-3, Q-4 format).
pub fn report(prompt: &str, events: &[LedgerEvent]) -> String {
    let _ = (prompt, events);
    todo!("T-06: Q-4 report")
}
