//! Harness core (docs/01, docs/02). The docs call this crate `core`; it is packaged as
//! `harness-core` / `harness_core` because `core` is Rust's built-in crate (D-35).
//! Read `core::x` in the docs as `harness_core::x`.
//!
//! CONTRACT modules (I-1..I-4, agent-memory/interfaces.yaml): `class`, `memory`, `gate`,
//! `vault`, `mcp`, and `run::RunId`. Change them only through `interface_changes`.
//! Owned by area core (not contract): `ask` (FileAsker, D-105), `ledger`, `run::RunFolder`,
//! `sched` (skeleton).

pub mod ask;
pub mod class;
pub mod gate;
pub mod ledger;
pub mod mcp;
pub mod memory;
pub mod run;
pub mod sched;
#[cfg(any(test, feature = "testing"))]
pub mod testing;
pub mod vault;

pub use ask::FileAsker;
pub use class::{Class, Mode};
pub use gate::{AskCard, AskOutcome, Asker, Call, Decision, Gate, OnTimeout, StopReason};
pub use ledger::Ledger;
pub use mcp::{
    dispatch, serve, serve_io, Classified, Harness, ToolCtx, ToolError, ToolHandler, ToolOutput,
    ToolRegistry, ToolSpec,
};
pub use memory::MemoryStore;
pub use run::{RunFolder, RunId};
pub use vault::{scrub, Vault};
