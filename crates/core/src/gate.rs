//! I-3 Gate (docs/01 H-1). CONTRACT: change only through `interface_changes`.
//!
//! `Gate::check(call) -> Allow | Ask(card) | Stop(reason)`. The gate never runs anything;
//! `mcp::serve` asks it, then runs, asks the user, or stops.

use crate::class::{Class, Mode};
use crate::memory::{contact_key, GateRule, MemoryError, MemoryStore};
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
        if call.class == Class::Challenge {
            return Ok(Decision::Stop(StopReason {
                class: Class::Challenge,
                why: format!(
                    "challenge at {}: stopped, the user acts in the browser and says continue (D-5)",
                    call.tool
                ),
                element: element_of(&call.args),
            }));
        }

        let (mode, rule_strict, rule_name) = match self.lookup(call).await? {
            Some((name, rule)) => (rule.mode, rule.strict, Some(name)),
            None => {
                let known =
                    call.class == Class::Send && self.known_recipients(&call.recipients).await?;
                (Gate::default_mode(call.class, known), false, None)
            }
        };

        Ok(match mode {
            Mode::Allow => Decision::Allow,
            Mode::Ask => Decision::Ask(ask_card(
                call,
                Gate::on_timeout(call.class, call.strict || rule_strict),
            )),
            Mode::Stop => Decision::Stop(StopReason {
                class: call.class,
                why: match rule_name {
                    Some(name) => format!("gate rule {name} says stop"),
                    None => format!("{} stops by default (H-1.1)", call.class),
                },
                element: element_of(&call.args),
            }),
        })
    }

    /// H-1.1 defaults. `known_recipients` is true only when `recipients` is non-empty and
    /// every one is in `memory.contacts`: a Send with no recipients is Send(new) and asks.
    pub fn default_mode(class: Class, known_recipients: bool) -> Mode {
        match class {
            Class::Read | Class::Fill | Class::Navigate | Class::Submit => Mode::Allow,
            Class::Send if known_recipients => Mode::Allow,
            Class::Send | Class::Pay | Class::Account | Class::Delete => Mode::Ask,
            Class::Challenge => Mode::Stop,
        }
    }

    /// H-1.3: Send(new) and Account proceed, Pay and Delete skip; `strict` makes all skip.
    /// Classes H-1.3 does not name (an ask raised by a rule on Read..Submit, e.g. the
    /// job-application rule) skip: only the two classes the spec lists ever proceed
    /// without the user (B-100).
    pub fn on_timeout(class: Class, strict: bool) -> OnTimeout {
        match class {
            Class::Send | Class::Account if !strict => OnTimeout::Proceed,
            _ => OnTimeout::Skip,
        }
    }

    /// D-39 lookup; returns the first matching rule and its key as `(domain, class)`.
    /// With several hints, every domain-scoped hint key is tried before any `'*'` hint key.
    async fn lookup(&self, call: &Call) -> Result<Option<(String, GateRule)>, GateError> {
        let domain = call
            .domain
            .as_deref()
            .map(str::trim)
            .filter(|d| !d.is_empty() && *d != ANY_DOMAIN);
        let class = call.class.as_str();
        let hint_keys: Vec<String> = call
            .hints
            .iter()
            .map(|h| h.trim())
            .filter(|h| !h.is_empty())
            .map(|h| format!("{class}:{h}"))
            .collect();

        let mut keys: Vec<(&str, &str)> = Vec::with_capacity(2 * hint_keys.len() + 2);
        if let Some(d) = domain {
            keys.extend(hint_keys.iter().map(|k| (d, k.as_str())));
        }
        keys.extend(hint_keys.iter().map(|k| (ANY_DOMAIN, k.as_str())));
        if let Some(d) = domain {
            keys.push((d, class));
        }
        keys.push((ANY_DOMAIN, class));

        for (d, k) in keys {
            if let Some(rule) = self.store.gate_rule(d, k).await? {
                return Ok(Some((format!("({d}, {k})"), rule)));
            }
        }
        Ok(None)
    }

    /// Non-empty, and every recipient is a known contact (H-1.1).
    async fn known_recipients(&self, recipients: &[String]) -> Result<bool, GateError> {
        if recipients.is_empty() {
            return Ok(false);
        }
        for r in recipients {
            if self.store.contact(&contact_key(r)).await?.is_none() {
                return Ok(false);
            }
        }
        Ok(true)
    }
}

/// The `gate.rules.domain` value that matches every domain.
const ANY_DOMAIN: &str = "*";

/// Cap on a generated "what will happen" line, in characters.
const WHAT_MAX_CHARS: usize = 200;

/// `args["element"]` when it is a string (`#31`).
fn element_of(args: &Value) -> Option<String> {
    args.get("element")
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// Top-level args as (key, shown value). Args are already scrubbed (see [`Call::args`]).
fn flatten_args(args: &Value) -> Vec<(String, String)> {
    match args {
        Value::Null => Vec::new(),
        Value::Object(map) => map.iter().map(|(k, v)| (k.clone(), shown(v))).collect(),
        other => vec![("args".to_string(), shown(other))],
    }
}

fn shown(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// "send to a@x.com: subject=Hi, body=..." on one line, capped at [`WHAT_MAX_CHARS`].
fn what_line(call: &Call) -> String {
    let mut line = call.tool.clone();
    if !call.recipients.is_empty() {
        line.push_str(" to ");
        line.push_str(&call.recipients.join(", "));
    }
    let args = flatten_args(&call.args);
    if !args.is_empty() {
        let parts: Vec<String> = args.iter().map(|(k, v)| format!("{k}={v}")).collect();
        line.push_str(": ");
        line.push_str(&parts.join(", "));
    }
    let line: String = line
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    if line.chars().count() > WHAT_MAX_CHARS {
        let mut cut: String = line.chars().take(WHAT_MAX_CHARS - 3).collect();
        cut.push_str("...");
        cut
    } else {
        line
    }
}

fn ask_card(call: &Call, on_timeout: OnTimeout) -> AskCard {
    let what = call
        .what
        .as_deref()
        .filter(|w| !w.trim().is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| what_line(call));
    let values = if call.values.is_empty() {
        flatten_args(&call.args)
    } else {
        call.values.clone()
    };
    AskCard {
        class: call.class,
        what,
        site: call.domain.clone(),
        values,
        timeout: ASK_TIMEOUT,
        on_timeout,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::MemStore;
    use serde_json::json;

    fn call(class: Class, domain: Option<&str>) -> Call {
        Call {
            run_id: None,
            server: "echo".into(),
            tool: "echo.send".into(),
            class,
            domain: domain.map(str::to_string),
            recipients: Vec::new(),
            hints: Vec::new(),
            args: json!({}),
            what: None,
            values: Vec::new(),
            strict: false,
        }
    }

    fn send(to: &[&str]) -> Call {
        let mut c = call(Class::Send, Some("mail.example"));
        c.recipients = to.iter().map(|s| s.to_string()).collect();
        c.args = json!({"to": to.join(","), "body": "hi"});
        c
    }

    fn gate(store: MemStore) -> (Arc<MemStore>, Gate) {
        let store = Arc::new(store);
        (store.clone(), Gate::new(store))
    }

    async fn decide(store: MemStore, c: &Call) -> Decision {
        gate(store).1.check(c).await.unwrap()
    }

    fn mode_of(d: &Decision) -> Mode {
        match d {
            Decision::Allow => Mode::Allow,
            Decision::Ask(_) => Mode::Ask,
            Decision::Stop(_) => Mode::Stop,
        }
    }

    fn card(d: Decision) -> AskCard {
        match d {
            Decision::Ask(c) => c,
            other => panic!("expected an ask, got {other:?}"),
        }
    }

    fn stop(d: Decision) -> StopReason {
        match d {
            Decision::Stop(r) => r,
            other => panic!("expected a stop, got {other:?}"),
        }
    }

    #[test]
    fn h1_1_default_table() {
        use Class::*;
        for c in [Read, Fill, Navigate, Submit] {
            assert_eq!(Gate::default_mode(c, false), Mode::Allow, "{c}");
            assert_eq!(Gate::default_mode(c, true), Mode::Allow, "{c}");
        }
        assert_eq!(Gate::default_mode(Send, true), Mode::Allow);
        assert_eq!(Gate::default_mode(Send, false), Mode::Ask);
        for c in [Pay, Account, Delete] {
            assert_eq!(Gate::default_mode(c, true), Mode::Ask, "{c}");
            assert_eq!(Gate::default_mode(c, false), Mode::Ask, "{c}");
        }
        assert_eq!(Gate::default_mode(Challenge, true), Mode::Stop);
        assert_eq!(Gate::default_mode(Challenge, false), Mode::Stop);
    }

    #[tokio::test]
    async fn h1_1_defaults_through_check() {
        // Same table through check(), with and without migration 0001's seed rules.
        for store in [MemStore::new, MemStore::empty] {
            for (class, want) in [
                (Class::Read, Mode::Allow),
                (Class::Fill, Mode::Allow),
                (Class::Navigate, Mode::Allow),
                (Class::Submit, Mode::Allow),
                (Class::Pay, Mode::Ask),
                (Class::Account, Mode::Ask),
                (Class::Delete, Mode::Ask),
                (Class::Challenge, Mode::Stop),
            ] {
                let d = decide(store(), &call(class, Some("shop.example"))).await;
                assert_eq!(mode_of(&d), want, "{class}");
            }
        }
    }

    #[tokio::test]
    async fn send_known_new_and_empty_recipients() {
        let store = MemStore::new();
        store.add_contact("Known@Example.com");
        store.add_contact("other@example.com");
        let (_, g) = gate(store);

        let known = send(&[" known@example.COM", "other@example.com"]);
        assert_eq!(g.check(&known).await.unwrap(), Decision::Allow);

        let mixed = card(
            g.check(&send(&["known@example.com", "new@x.com"]))
                .await
                .unwrap(),
        );
        assert_eq!(mixed.class, Class::Send);
        assert_eq!(mixed.on_timeout, OnTimeout::Proceed);
        assert_eq!(mixed.timeout, ASK_TIMEOUT);
        assert_eq!(mixed.site.as_deref(), Some("mail.example"));
        assert!(mixed.what.contains("new@x.com"), "{}", mixed.what);

        let empty = card(g.check(&send(&[])).await.unwrap());
        assert_eq!(empty.on_timeout, OnTimeout::Proceed);
    }

    #[tokio::test]
    async fn each_lookup_level_beats_the_next() {
        let d = "bank.example";
        let (s, g) = gate(MemStore::empty());
        let mut c = call(Class::Submit, Some(d));
        c.hints = vec!["wire".into()];

        // default: Submit allows
        assert_eq!(g.check(&c).await.unwrap(), Decision::Allow);
        // ('*', Class) beats the default
        s.add_rule("*", "Submit", Mode::Ask, false).unwrap();
        assert_eq!(mode_of(&g.check(&c).await.unwrap()), Mode::Ask);
        // (domain, Class) beats ('*', Class)
        s.add_rule(d, "Submit", Mode::Stop, false).unwrap();
        assert_eq!(mode_of(&g.check(&c).await.unwrap()), Mode::Stop);
        // ('*', Class:hint) beats (domain, Class)
        s.add_rule("*", "Submit:wire", Mode::Allow, false).unwrap();
        assert_eq!(g.check(&c).await.unwrap(), Decision::Allow);
        // (domain, Class:hint) beats ('*', Class:hint)
        s.add_rule(d, "Submit:wire", Mode::Ask, false).unwrap();
        assert_eq!(mode_of(&g.check(&c).await.unwrap()), Mode::Ask);

        // domain = None skips both domain-scoped keys
        let mut local = c.clone();
        local.domain = None;
        assert_eq!(g.check(&local).await.unwrap(), Decision::Allow);
        local.hints.clear();
        assert_eq!(mode_of(&g.check(&local).await.unwrap()), Mode::Ask);
    }

    #[tokio::test]
    async fn rule_overrides_the_send_contact_check() {
        let (s, g) = gate(MemStore::new());
        s.add_contact("known@example.com");
        s.add_rule("mail.example", "Send", Mode::Ask, false)
            .unwrap();
        let c = card(g.check(&send(&["known@example.com"])).await.unwrap());
        assert_eq!(c.on_timeout, OnTimeout::Proceed);
    }

    #[tokio::test]
    async fn hint_rule_beats_a_domain_allow() {
        // H-1.2: "job submissions and signatures are mine", seeded by migration 0001.
        let (s, g) = gate(MemStore::new());
        s.add_rule("jobs.example", "Submit", Mode::Allow, false)
            .unwrap();
        let mut c = call(Class::Submit, Some("jobs.example"));
        assert_eq!(g.check(&c).await.unwrap(), Decision::Allow);
        for hint in [HINT_JOB_APPLICATION, HINT_SIGNATURE] {
            c.hints = vec![hint.to_string()];
            let card = card(g.check(&c).await.unwrap());
            assert_eq!(card.class, Class::Submit);
            assert_eq!(card.on_timeout, OnTimeout::Skip, "{hint}");
        }
    }

    #[tokio::test]
    async fn challenge_stops_even_with_an_allow_rule() {
        let (s, g) = gate(MemStore::empty());
        let allow = GateRule {
            mode: Mode::Allow,
            strict: false,
        };
        // gate_rule_set refuses a non-stop Challenge rule (D-5) ...
        assert!(s
            .add_rule("x.example", "Challenge", Mode::Allow, false)
            .is_err());
        // ... and a hand-edited table still cannot win.
        for (d, k) in [
            ("x.example", "Challenge"),
            ("*", "Challenge"),
            ("x.example", "Challenge:turnstile"),
            ("*", "Challenge:turnstile"),
        ] {
            s.force_rule(d, k, allow);
        }
        let mut c = call(Class::Challenge, Some("x.example"));
        c.hints = vec!["turnstile".into()];
        c.args = json!({"element": "#31"});
        let r = stop(g.check(&c).await.unwrap());
        assert_eq!(r.class, Class::Challenge);
        assert_eq!(r.element.as_deref(), Some("#31"));
        assert!(r.why.contains("D-5"), "{}", r.why);
    }

    #[tokio::test]
    async fn strict_from_run_and_from_rule() {
        let (s, g) = gate(MemStore::new());
        s.add_rule("strict.example", "Account", Mode::Ask, true)
            .unwrap();

        let lenient = card(g.check(&send(&["new@x.com"])).await.unwrap());
        assert_eq!(lenient.on_timeout, OnTimeout::Proceed);
        let mut run_strict = send(&["new@x.com"]);
        run_strict.strict = true;
        let run_strict = card(g.check(&run_strict).await.unwrap());
        assert_eq!(run_strict.on_timeout, OnTimeout::Skip);

        let account = call(Class::Account, Some("other.example"));
        let account = card(g.check(&account).await.unwrap());
        assert_eq!(account.on_timeout, OnTimeout::Proceed);
        let rule_strict = call(Class::Account, Some("strict.example"));
        let rule_strict = card(g.check(&rule_strict).await.unwrap());
        assert_eq!(rule_strict.on_timeout, OnTimeout::Skip);
    }

    #[test]
    fn h1_3_timeout_table() {
        assert_eq!(Gate::on_timeout(Class::Send, false), OnTimeout::Proceed);
        assert_eq!(Gate::on_timeout(Class::Account, false), OnTimeout::Proceed);
        assert_eq!(Gate::on_timeout(Class::Pay, false), OnTimeout::Skip);
        assert_eq!(Gate::on_timeout(Class::Delete, false), OnTimeout::Skip);
        for c in Class::ALL {
            assert_eq!(Gate::on_timeout(c, true), OnTimeout::Skip, "{c}");
        }
        // B-100: classes H-1.3 does not name skip.
        for c in [
            Class::Read,
            Class::Fill,
            Class::Navigate,
            Class::Submit,
            Class::Challenge,
        ] {
            assert_eq!(Gate::on_timeout(c, false), OnTimeout::Skip, "{c}");
        }
    }

    #[tokio::test]
    async fn rule_stop_names_the_rule() {
        let store = MemStore::empty();
        store
            .add_rule("bank.example", "Submit", Mode::Stop, false)
            .unwrap();
        let mut c = call(Class::Submit, Some("bank.example"));
        c.args = json!({"element": "#7"});
        let r = stop(decide(store, &c).await);
        assert_eq!(r.class, Class::Submit);
        assert!(r.why.contains("(bank.example, Submit)"), "{}", r.why);
        assert_eq!(r.element.as_deref(), Some("#7"));
    }

    #[tokio::test]
    async fn card_uses_classifier_what_and_values_or_builds_them() {
        let mut c = call(Class::Pay, Some("shop.example"));
        c.args = json!({"card": "@vault(card)", "amount": 12, "note": null});
        let built = card(decide(MemStore::new(), &c).await);
        assert_eq!(
            built.values,
            vec![
                ("amount".to_string(), "12".to_string()),
                ("card".to_string(), "@vault(card)".to_string()),
                ("note".to_string(), String::new()),
            ]
        );
        assert!(built.what.starts_with("echo.send: "), "{}", built.what);
        assert!(built.what.contains("card=@vault(card)"), "{}", built.what);
        assert_eq!(built.on_timeout, OnTimeout::Skip);

        c.what = Some("confirm order on shop.example".into());
        c.values = vec![("total".into(), "12 EUR".into())];
        let given = card(decide(MemStore::new(), &c).await);
        assert_eq!(given.what, "confirm order on shop.example");
        assert_eq!(given.values, c.values);

        c.what = None;
        c.args = json!({"body": format!("line one\n{}", "x".repeat(500))});
        let long = card(decide(MemStore::new(), &c).await);
        assert_eq!(long.what.chars().count(), WHAT_MAX_CHARS);
        assert!(long.what.ends_with("..."));
        assert!(!long.what.contains('\n'));
    }
}
