//! I-3 Gate (docs/01 H-1). CONTRACT: change only through `interface_changes`.
//!
//! `Gate::check(call) -> Allow | Ask(card) | Stop(reason)`. The gate never runs anything;
//! `mcp::serve` asks it, then runs, asks the user, or stops.

use crate::class::{Class, Mode};
use crate::memory::{MemoryError, MemoryStore};
use crate::run::RunId;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;
use std::time::Duration;

/// H-1.3: ask cards wait this long before applying [`OnTimeout`].
pub const ASK_TIMEOUT: Duration = Duration::from_secs(60);

/// Classifier hints for the user's standing rule "job submissions and signatures are mine"
/// (H-1.2). Looked up as the rule keys `Submit:job_application` / `Submit:signature` (D-39).
pub const HINT_JOB_APPLICATION: &str = "job_application";
pub const HINT_SIGNATURE: &str = "signature";

/// One tool call as the gate sees it, after classification.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Call {
    pub run_id: Option<RunId>,
    pub server: String,
    pub tool: String,
    /// `max(declared class, tool floor, classifier result)`; never lowered after this point.
    pub class: Class,
    /// eTLD+1 of the target, `None` for local effects (files, shell).
    pub domain: Option<String>,
    /// For Send: who receives it. Each is checked against `memory.contacts` (H-1.1).
    pub recipients: Vec<String>,
    /// Classifier hints such as [`HINT_JOB_APPLICATION`]; select hint-scoped rules (D-39).
    pub hints: Vec<String>,
    /// Scrubbed arguments, vault values only as `@vault(name)`.
    pub args: Value,
    /// The ask card's "what will happen" line, from the classifier; `None` = built from args.
    pub what: Option<String>,
    /// The card's values, already scrubbed (vault as `@vault(name)`); empty = built from args.
    pub values: Vec<(String, String)>,
    /// The run asked for `strict` (`runs.strict`). A strict rule is OR-ed in by `check` (H-1.3).
    pub strict: bool,
}

/// I-3 result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Decision {
    Allow,
    Ask(AskCard),
    Stop(StopReason),
}

/// What happens when an ask card times out (H-1.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OnTimeout {
    Proceed,
    Skip,
}

/// An ask (H-1.3): what will happen, where, with which values, and the timer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AskCard {
    pub class: Class,
    /// One line: "send mail to new@x.com: Subject ...".
    pub what: String,
    pub site: Option<String>,
    /// Field, shown value. Vault values are shown as `@vault(name)`, never resolved.
    pub values: Vec<(String, String)>,
    pub timeout: Duration,
    pub on_timeout: OnTimeout,
}

/// A stop (H-1.4). For Challenge: the program stops at this step, the tab is marked.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StopReason {
    pub class: Class,
    pub why: String,
    /// The element that triggered it, e.g. `#31`.
    pub element: Option<String>,
}

/// How an ask ended; becomes a ledger event (`LedgerMode::Answered*` / `Timeout*`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AskOutcome {
    AnsweredProceed,
    AnsweredSkip,
    TimeoutProceed,
    TimeoutSkip,
}

impl AskOutcome {
    pub fn proceeds(self) -> bool {
        matches!(
            self,
            AskOutcome::AnsweredProceed | AskOutcome::TimeoutProceed
        )
    }
}

/// Who answers ask cards: the CLI prompt, a notification, a test double.
/// Implementations must honour `card.timeout` and `card.on_timeout`.
#[async_trait]
pub trait Asker: Send + Sync {
    async fn ask(&self, card: &AskCard) -> AskOutcome;
}

#[derive(Debug, thiserror::Error)]
pub enum GateError {
    #[error(transparent)]
    Memory(#[from] MemoryError),
}

/// The gate. Holds the store for `gate.rules` and `memory.contacts`.
pub struct Gate {
    store: Arc<dyn MemoryStore>,
}

impl Gate {
    pub fn new(store: Arc<dyn MemoryStore>) -> Gate {
        Gate { store }
    }

    /// I-3.
    /// 1. `Class::Challenge` → `Stop`, before any rule lookup: no rule can override D-5.
    /// 2. Rule lookup (H-1.2 + D-39), first hit wins:
    ///    `(domain, Class:hint)` → `('*', Class:hint)` → `(domain, Class)` → `('*', Class)` →
    ///    [`Gate::default_mode`].
    /// 3. Ask cards time out per [`Gate::on_timeout`] with `strict = call.strict || rule.strict`.
    pub async fn check(&self, call: &Call) -> Result<Decision, GateError> {
        let _ = (&self.store, call);
        todo!("T-04: H-1 classes, H-1.2 rules, H-1.3 cards")
    }

    /// H-1.1 defaults. `known_recipients` is true only when `recipients` is non-empty and
    /// every one is in `memory.contacts`: a Send with no recipients is Send(new) and asks.
    pub fn default_mode(class: Class, known_recipients: bool) -> Mode {
        let _ = (class, known_recipients);
        todo!("T-04: H-1.1 table")
    }

    /// H-1.3: Send(new) and Account proceed, Pay and Delete skip; `strict` makes all skip.
    pub fn on_timeout(class: Class, strict: bool) -> OnTimeout {
        let _ = (class, strict);
        todo!("T-04: H-1.3")
    }
}
