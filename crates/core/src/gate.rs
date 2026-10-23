//! I-3 Gate (docs/01 H-1). CONTRACT: change only through `interface_changes`.
//!
//! `Gate::check(call) -> Allow | Ask(card) | Stop(reason)`. The gate never runs anything;
//! `mcp::serve` asks it, then runs, asks the user, or stops.
//!
//! Rule keys are canonical (D-110): every `MemoryStore::gate_rule_set` backend stores the
//! keys [`rule_key`] returns, and the gate looks rules up with the same canonicalisers
//! ([`rule_domain`], [`rule_class_key`], [`rule_hint`]).

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
/// Every string on a card built by [`Gate::check`] is control-stripped (C0/C1 controls,
/// ANSI escape sequences, CR, bidi and zero-width marks) and length-capped (D-110 (5)).
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

/// The `gate.rules.domain` value that matches every domain.
pub const ANY_DOMAIN: &str = "*";

/// D-110 (1): the canonical `gate.rules.domain`. Trimmed, lowercased, one trailing dot
/// removed; `'*'` (after trimming) stays `'*'`. Rejects (`MemoryError::Invalid`) an empty
/// value and anything that is not a bare host name: whitespace, control characters, a
/// `*` other than the whole value, `/ : @ ? # \`, a leading dot, an empty label (`..`).
/// `rule_domain(" Bank.COM. ") == Ok("bank.com")`.
pub fn rule_domain(domain: &str) -> Result<String, MemoryError> {
    let lower = domain.trim().to_lowercase();
    if lower == ANY_DOMAIN {
        return Ok(lower);
    }
    let d = lower.strip_suffix('.').unwrap_or(&lower);
    let bad = d.is_empty()
        || d.starts_with('.')
        || d.ends_with('.')
        || d.contains("..")
        || d.chars().any(|c| {
            c.is_whitespace()
                || c.is_control()
                || matches!(c, '*' | '/' | ':' | '@' | '?' | '#' | '\\')
        });
    if bad {
        return Err(MemoryError::Invalid(format!(
            "gate rule domain {domain:?} is not a host name or '*'"
        )));
    }
    Ok(d.to_string())
}

/// D-110 (1): the canonical form of a classifier hint inside a rule key: trimmed and
/// lowercased. Rejects (`MemoryError::Invalid`) an empty hint and one containing `:`,
/// whitespace or control characters. `rule_hint(" Job_Application ") == Ok("job_application")`.
pub fn rule_hint(hint: &str) -> Result<String, MemoryError> {
    let h = hint.trim().to_lowercase();
    if h.is_empty()
        || h.chars()
            .any(|c| c == ':' || c.is_whitespace() || c.is_control())
    {
        return Err(MemoryError::Invalid(format!(
            "gate rule hint {hint:?} is empty or has ':', whitespace or control characters"
        )));
    }
    Ok(h)
}

/// D-110 (1): the canonical `gate.rules.class` key: [`Class::as_str`], optionally followed
/// by `:` and [`rule_hint`]. The class name is matched case-insensitively after trimming.
/// Rejects (`MemoryError::Invalid`) an unknown class and a hint `rule_hint` rejects.
/// `rule_class_key(" submit : Wire ") == Ok("Submit:wire")`.
pub fn rule_class_key(class: &str) -> Result<String, MemoryError> {
    let (base, hint) = match class.split_once(':') {
        Some((b, h)) => (b, Some(h)),
        None => (class, None),
    };
    let base: Class = base
        .trim()
        .parse()
        .map_err(|e: crate::class::UnknownClass| MemoryError::Invalid(e.to_string()))?;
    Ok(match hint {
        None => base.as_str().to_string(),
        Some(h) => format!("{}:{}", base.as_str(), rule_hint(h)?),
    })
}

/// What every `MemoryStore::gate_rule_set` backend runs before writing: the canonical
/// `(domain, class)` key ([`rule_domain`], [`rule_class_key`]) and D-5 (a Challenge rule,
/// hinted or not, must be `stop`). Any failure is `MemoryError::Invalid`.
pub fn rule_key(
    domain: &str,
    class: &str,
    rule: &GateRule,
) -> Result<(String, String), MemoryError> {
    let domain = rule_domain(domain)?;
    let key = rule_class_key(class)?;
    let challenge = key == Class::Challenge.as_str()
        || key.starts_with(&format!("{}:", Class::Challenge.as_str()));
    if challenge && rule.mode != Mode::Stop {
        return Err(MemoryError::Invalid(
            "a Challenge rule must be stop (D-5)".into(),
        ));
    }
    Ok((domain, key))
}

/// The gate. Holds the store for `gate.rules` and `memory.contacts`.
pub struct Gate {
    store: Arc<dyn MemoryStore>,
}

/// How a call's mode was resolved, before the timeout rule.
struct Resolved {
    mode: Mode,
    strict: bool,
    /// A hint rule matched: an ask under it never times out to proceed (D-110 (4)).
    from_hint: bool,
    /// The deciding rule as `(domain, class)`; `None` = the H-1.1 default.
    name: Option<String>,
}

/// One hint rule that matched, at class `class` (the call's class or lower).
struct HintHit {
    class: Class,
    name: String,
    rule: GateRule,
}

impl Gate {
    pub fn new(store: Arc<dyn MemoryStore>) -> Gate {
        Gate { store }
    }

    /// I-3.
    /// 1. `Class::Challenge` → `Stop`, before any rule lookup: no rule can override D-5.
    /// 2. Hint rules (D-39, D-110 (2)(3)). Each hint (canonicalised by [`rule_hint`]) is
    ///    tried at the call's class C and then every lower class down to Read, as
    ///    `(domain, C:hint)` then `('*', C:hint)`; the first hit per hint counts. Across
    ///    the hints that hit, the most restrictive mode wins (stop > ask > allow) and
    ///    `strict` is OR-ed. Any hint hit beats the class-level lookup, with one guard: a
    ///    hit found at a class *below* the call's class can raise the class-level result
    ///    but never lower it (a `Read:x allow` rule cannot allow a Pay call hinted `x`).
    /// 3. Otherwise `(domain, Class)` → `('*', Class)` → [`Gate::default_mode`].
    ///    The domain is canonicalised by [`rule_domain`]; `None`, `'*'` or a value that does
    ///    not canonicalise skips the domain-scoped keys.
    /// 4. An ask times out per [`Gate::on_timeout`] with `strict = true` unless the run and
    ///    the rule are not strict, no hint rule hit, and the class is Account, or Send with
    ///    at least one recipient not in `memory.contacts` (D-110 (4)). So a user's ask rule
    ///    on Send to known contacts, a hinted ask, and a Send with no recipients all skip.
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

        let new_recipients = if call.class == Class::Send {
            Some(self.new_recipients(&call.recipients).await?)
        } else {
            None
        };
        let r = self.resolve(call, new_recipients).await?;

        Ok(match r.mode {
            Mode::Allow => Decision::Allow,
            Mode::Ask => {
                let may_proceed = !r.from_hint
                    && match call.class {
                        Class::Account => true,
                        Class::Send => new_recipients.is_some_and(|n| n > 0),
                        _ => false,
                    };
                let strict = call.strict || r.strict || !may_proceed;
                Decision::Ask(ask_card(call, Gate::on_timeout(call.class, strict)))
            }
            Mode::Stop => Decision::Stop(StopReason {
                class: call.class,
                why: match r.name {
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

    /// The H-1.3 table by class alone: Send and Account proceed, everything else skips;
    /// `strict` makes all skip. [`Gate::check`] passes `strict = true` for every ask that
    /// must not proceed on its own (D-109, D-110 (4)): a strict run or rule, an ask from a
    /// hint rule, and a Send whose recipients are all known (or empty).
    pub fn on_timeout(class: Class, strict: bool) -> OnTimeout {
        match class {
            Class::Send | Class::Account if !strict => OnTimeout::Proceed,
            _ => OnTimeout::Skip,
        }
    }

    /// Steps 2 and 3 of [`Gate::check`].
    async fn resolve(&self, call: &Call, new: Option<usize>) -> Result<Resolved, GateError> {
        let domain = call
            .domain
            .as_deref()
            .and_then(|d| rule_domain(d).ok())
            .filter(|d| d != ANY_DOMAIN);
        let domain = domain.as_deref();

        let hits = self.hint_hits(call.class, domain, &call.hints).await?;
        if hits.is_empty() {
            return self.class_level(call, domain, new).await;
        }

        let mut out: Option<Resolved> = None;
        let mut base: Option<Resolved> = None;
        for hit in hits {
            let mut mode = hit.rule.mode;
            let mut strict = hit.rule.strict;
            let mut name = Some(hit.name);
            if hit.class < call.class {
                if base.is_none() {
                    base = Some(self.class_level(call, domain, new).await?);
                }
                if let Some(b) = &base {
                    if severity(b.mode) > severity(mode) {
                        mode = b.mode;
                        name = b.name.clone();
                    }
                    strict |= b.strict;
                }
            }
            match &mut out {
                None => {
                    out = Some(Resolved {
                        mode,
                        strict,
                        from_hint: true,
                        name,
                    })
                }
                Some(o) => {
                    o.strict |= strict;
                    if severity(mode) > severity(o.mode) {
                        o.mode = mode;
                        o.name = name;
                    }
                }
            }
        }
        match out {
            Some(o) => Ok(o),
            None => self.class_level(call, domain, new).await,
        }
    }

    /// First hit per canonical hint, trying `call class` down to Read (D-110 (2)).
    async fn hint_hits(
        &self,
        class: Class,
        domain: Option<&str>,
        hints: &[String],
    ) -> Result<Vec<HintHit>, GateError> {
        let mut seen: Vec<String> = Vec::new();
        let mut hits = Vec::new();
        // A hint that does not canonicalise cannot match a stored (canonical) rule.
        for hint in hints.iter().filter_map(|h| rule_hint(h).ok()) {
            if seen.contains(&hint) {
                continue;
            }
            'hint: for c in Class::ALL.into_iter().rev().filter(|c| *c <= class) {
                let key = format!("{}:{hint}", c.as_str());
                for d in domain.into_iter().chain([ANY_DOMAIN]) {
                    if let Some(rule) = self.store.gate_rule(d, &key).await? {
                        hits.push(HintHit {
                            class: c,
                            name: format!("({d}, {key})"),
                            rule,
                        });
                        break 'hint;
                    }
                }
            }
            seen.push(hint);
        }
        Ok(hits)
    }

    /// `(domain, Class)` → `('*', Class)` → H-1.1 default.
    async fn class_level(
        &self,
        call: &Call,
        domain: Option<&str>,
        new: Option<usize>,
    ) -> Result<Resolved, GateError> {
        let class = call.class.as_str();
        for d in domain.into_iter().chain([ANY_DOMAIN]) {
            if let Some(rule) = self.store.gate_rule(d, class).await? {
                return Ok(Resolved {
                    mode: rule.mode,
                    strict: rule.strict,
                    from_hint: false,
                    name: Some(format!("({d}, {class})")),
                });
            }
        }
        let known = !call.recipients.is_empty() && new == Some(0);
        Ok(Resolved {
            mode: Gate::default_mode(call.class, known),
            strict: false,
            from_hint: false,
            name: None,
        })
    }

    /// How many recipients are not in `memory.contacts` (H-1.1).
    async fn new_recipients(&self, recipients: &[String]) -> Result<usize, GateError> {
        let mut new = 0;
        for r in recipients {
            if self.store.contact(&contact_key(r)).await?.is_none() {
                new += 1;
            }
        }
        Ok(new)
    }
}

/// stop > ask > allow (D-110 (3)).
fn severity(m: Mode) -> u8 {
    match m {
        Mode::Allow => 0,
        Mode::Ask => 1,
        Mode::Stop => 2,
    }
}

/// Cap on the card's "what will happen" line and on each shown value, in characters.
const WHAT_MAX_CHARS: usize = 200;
const VALUE_MAX_CHARS: usize = 200;
/// Cap on each value's field name and on the site, in characters.
const KEY_MAX_CHARS: usize = 64;
const SITE_MAX_CHARS: usize = 253;
/// At most this many values are shown; the rest are summarised in one extra row.
const MAX_VALUES: usize = 32;

/// `args["element"]` when it is a string (`#31`).
fn element_of(args: &Value) -> Option<String> {
    args.get("element")
        .and_then(Value::as_str)
        .map(|e| card_text(e, KEY_MAX_CHARS))
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

/// Invisible characters that can reorder or hide text on a terminal or in a notification.
fn invisible(c: char) -> bool {
    matches!(
        c,
        '\u{00ad}'
            | '\u{061c}'
            | '\u{180e}'
            | '\u{200b}'..='\u{200f}'
            | '\u{202a}'..='\u{202e}'
            | '\u{2060}'..='\u{2069}'
            | '\u{feff}'
            | '\u{fff9}'..='\u{fffb}'
    )
}

/// Skips an escape sequence after ESC (or after the 8-bit introducer `intro`).
/// CSI (`ESC [` / U+009B) runs to its final byte; OSC, DCS, SOS, PM, APC (`ESC ] P X ^ _`,
/// U+009D, U+0090, U+0098, U+009E, U+009F) run to BEL or ST (`ESC \` / U+009C) and are
/// kept as text when unterminated; any other `ESC x` is two characters.
fn skip_escape(it: &mut std::iter::Peekable<std::str::Chars<'_>>, intro: Option<char>) {
    let kind = match intro {
        Some(k) => k,
        None => match it.peek().copied() {
            Some(k @ ('[' | ']' | 'P' | 'X' | '^' | '_')) => {
                it.next();
                k
            }
            Some(k) if ('\u{20}'..='\u{7e}').contains(&k) => {
                it.next();
                return;
            }
            _ => return,
        },
    };
    match kind {
        '[' | '\u{9b}' => {
            while let Some(&c) = it.peek() {
                if ('\u{20}'..='\u{3f}').contains(&c) {
                    it.next();
                } else {
                    if ('\u{40}'..='\u{7e}').contains(&c) {
                        it.next();
                    }
                    return;
                }
            }
        }
        _ => {
            // Only a terminated string is dropped: an unterminated one keeps its text, so
            // a stray `ESC ]` cannot hide the rest of the line from the card.
            let mut probe = it.clone();
            while let Some(c) = probe.next() {
                match c {
                    '\u{07}' | '\u{9c}' => {
                        *it = probe;
                        return;
                    }
                    '\u{1b}' => {
                        if probe.peek() == Some(&'\\') {
                            probe.next();
                            *it = probe;
                        }
                        return;
                    }
                    _ => {}
                }
            }
        }
    }
}

/// D-110 (5): one clean line. Escape sequences, control characters (CR, LF, tab, C1) and
/// invisible marks become a single space; whitespace runs collapse; capped at `max`
/// characters with a trailing `...`.
fn card_text(s: &str, max: usize) -> String {
    let mut out = String::with_capacity(s.len().min(max * 4));
    let mut space = false;
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        let sep = match c {
            '\u{1b}' => {
                skip_escape(&mut it, None);
                true
            }
            '\u{9b}' | '\u{9d}' | '\u{90}' | '\u{98}' | '\u{9e}' | '\u{9f}' => {
                let intro = if c == '\u{9b}' { '\u{9b}' } else { ']' };
                skip_escape(&mut it, Some(intro));
                true
            }
            c => c.is_control() || c.is_whitespace() || invisible(c),
        };
        if sep {
            space = true;
            continue;
        }
        if space && !out.is_empty() {
            out.push(' ');
        }
        space = false;
        out.push(c);
    }
    if out.chars().count() > max {
        let mut cut: String = out.chars().take(max.saturating_sub(3)).collect();
        cut.push_str("...");
        cut
    } else {
        out
    }
}

/// "send to a@x.com: subject=Hi, body=..." before cleaning.
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
    line
}

fn ask_card(call: &Call, on_timeout: OnTimeout) -> AskCard {
    let what = call
        .what
        .as_deref()
        .map(|w| card_text(w, WHAT_MAX_CHARS))
        .filter(|w| !w.is_empty())
        .unwrap_or_else(|| card_text(&what_line(call), WHAT_MAX_CHARS));
    let raw = if call.values.is_empty() {
        flatten_args(&call.args)
    } else {
        call.values.clone()
    };
    let more = raw.len().saturating_sub(MAX_VALUES);
    let mut values: Vec<(String, String)> = raw
        .iter()
        .take(MAX_VALUES)
        .map(|(k, v)| (card_text(k, KEY_MAX_CHARS), card_text(v, VALUE_MAX_CHARS)))
        .collect();
    if more > 0 {
        values.push(("...".to_string(), format!("{more} more values not shown")));
    }
    AskCard {
        class: call.class,
        what,
        site: call.domain.as_deref().map(|d| card_text(d, SITE_MAX_CHARS)),
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

        // D-110 (4): no recipients means no new recipient, so the ask skips.
        let empty = card(g.check(&send(&[])).await.unwrap());
        assert_eq!(empty.on_timeout, OnTimeout::Skip);
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
    async fn rule_ask_on_send_to_a_known_contact_times_out_to_skip() {
        // D-110 (4): the user asked to be asked; silence must not send.
        let (s, g) = gate(MemStore::new());
        s.add_contact("known@example.com");
        s.add_rule("mail.example", "Send", Mode::Ask, false)
            .unwrap();
        let c = card(g.check(&send(&["known@example.com"])).await.unwrap());
        assert_eq!(c.on_timeout, OnTimeout::Skip);
        // The same rule with a new recipient still proceeds (Send(new), H-1.3).
        let c = card(g.check(&send(&["new@x.com"])).await.unwrap());
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
        assert!(s
            .add_rule("x.example", " challenge : turnstile", Mode::Ask, false)
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
        // D-109: classes H-1.3 does not name skip.
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

    // ---- D-110 regressions (each failed on commit 3cd304a) ----

    #[test]
    fn canonicalisers() {
        assert_eq!(rule_domain(" Bank.COM. ").unwrap(), "bank.com");
        assert_eq!(rule_domain(" * ").unwrap(), "*");
        for bad in [
            "",
            "  ",
            ".",
            "*.bank.com",
            "bank..com",
            ".bank.com",
            "bank.com..",
            "bank com",
            "bank.com/x",
            "bank.com:443",
            "u@bank.com",
            "bank\u{1b}.com",
        ] {
            assert!(
                matches!(rule_domain(bad), Err(MemoryError::Invalid(_))),
                "{bad:?}"
            );
        }
        assert_eq!(rule_class_key("submit").unwrap(), "Submit");
        assert_eq!(rule_class_key(" SUBMIT : Wire ").unwrap(), "Submit:wire");
        assert_eq!(
            rule_class_key("Submit:job_application").unwrap(),
            "Submit:job_application"
        );
        for bad in [
            "",
            "Shout",
            "Submit:",
            "Submit: ",
            "Submit:a:b",
            "Submit:a b",
            ":x",
        ] {
            assert!(
                matches!(rule_class_key(bad), Err(MemoryError::Invalid(_))),
                "{bad:?}"
            );
        }
        assert_eq!(rule_hint(" Job_Application ").unwrap(), "job_application");
        let stop = GateRule {
            mode: Mode::Stop,
            strict: false,
        };
        let ask = GateRule {
            mode: Mode::Ask,
            strict: false,
        };
        assert_eq!(
            rule_key("X.com.", "challenge:Turnstile", &stop).unwrap(),
            ("x.com".to_string(), "Challenge:turnstile".to_string())
        );
        assert!(rule_key("x.com", " challenge ", &ask).is_err());
    }

    #[tokio::test]
    async fn non_canonical_rules_are_stored_canonical_and_match() {
        // D-110 (1): each of these was stored verbatim and never matched, so the call allowed.
        for (domain, class, hint, call_domain) in [
            ("*", "submit", None, "shop.example"),
            (" shop2.com", "Submit", None, "shop2.com"),
            ("*", "Submit: wire", Some("wire"), "shop.example"),
            ("Bank.com", "Submit", None, "bank.com"),
            ("bank.com.", "SUBMIT", None, "Bank.Com"),
            ("bank.com", "Submit", None, " bank.com. "),
            ("*", "Submit:Wire", Some(" WIRE "), "shop.example"),
        ] {
            let (s, g) = gate(MemStore::empty());
            s.add_rule(domain, class, Mode::Ask, false).unwrap();
            let mut c = call(Class::Submit, Some(call_domain));
            c.hints = hint.map(|h| vec![h.to_string()]).unwrap_or_default();
            assert_eq!(
                mode_of(&g.check(&c).await.unwrap()),
                Mode::Ask,
                "rule ({domain:?}, {class:?}) on {call_domain:?}"
            );
        }
        let s = MemStore::empty();
        for (d, k) in [
            ("", "Submit"),
            ("*", "Shout"),
            ("*", "Submit:"),
            ("a b", "Submit"),
        ] {
            assert!(
                matches!(
                    s.add_rule(d, k, Mode::Ask, false),
                    Err(MemoryError::Invalid(_))
                ),
                "({d:?}, {k:?})"
            );
        }
    }

    #[tokio::test]
    async fn an_unrelated_hint_allow_cannot_undo_a_hint_ask() {
        // D-110 (3): ('*','Submit:newsletter','allow') undid the seeded job_application ask.
        let (s, g) = gate(MemStore::new());
        s.add_rule("*", "Submit:newsletter", Mode::Allow, false)
            .unwrap();
        s.add_rule("jobs.example", "Submit:newsletter", Mode::Allow, false)
            .unwrap();
        for hints in [
            ["newsletter", HINT_JOB_APPLICATION],
            [HINT_JOB_APPLICATION, "newsletter"],
        ] {
            let mut c = call(Class::Submit, Some("jobs.example"));
            c.hints = hints.iter().map(|h| h.to_string()).collect();
            let card = card(g.check(&c).await.unwrap());
            assert_eq!(card.on_timeout, OnTimeout::Skip, "{hints:?}");
        }
        // Stop beats ask across hints, and names the stopping rule.
        s.add_rule("*", "Submit:wire", Mode::Stop, false).unwrap();
        let mut c = call(Class::Submit, Some("jobs.example"));
        c.hints = vec![HINT_JOB_APPLICATION.into(), "wire".into()];
        let r = stop(g.check(&c).await.unwrap());
        assert!(r.why.contains("(*, Submit:wire)"), "{}", r.why);
        // Strict is OR-ed across hints.
        s.add_rule("*", "Submit:sig2", Mode::Ask, true).unwrap();
        s.add_rule("*", "Submit:wire", Mode::Ask, false).unwrap();
        c.hints = vec!["sig2".into(), "wire".into()];
        assert_eq!(card(g.check(&c).await.unwrap()).on_timeout, OnTimeout::Skip);
    }

    #[tokio::test]
    async fn a_hint_hit_beats_the_class_level_rule() {
        // D-110 (3): any hint match beats (domain, Class); here an ask over a domain allow
        // and an allow over a domain ask, both at the call's own class.
        let (s, g) = gate(MemStore::empty());
        s.add_rule("*", "Submit:job_application", Mode::Ask, false)
            .unwrap();
        s.add_rule("jobs.example", "Submit", Mode::Allow, false)
            .unwrap();
        let mut c = call(Class::Submit, Some("jobs.example"));
        c.hints = vec![HINT_JOB_APPLICATION.into()];
        assert_eq!(mode_of(&g.check(&c).await.unwrap()), Mode::Ask);

        s.add_rule("*", "Submit:receipt", Mode::Allow, false)
            .unwrap();
        s.add_rule("shop.example", "Submit", Mode::Ask, false)
            .unwrap();
        let mut r = call(Class::Submit, Some("shop.example"));
        r.hints = vec!["receipt".into()];
        assert_eq!(g.check(&r).await.unwrap(), Decision::Allow);
    }

    #[tokio::test]
    async fn hint_rules_apply_at_higher_classes() {
        // D-110 (2): a job application classed Account (password field) missed
        // ('*','Submit:job_application') and proceeded on timeout.
        let (_, g) = gate(MemStore::new());
        for class in [Class::Send, Class::Account, Class::Delete, Class::Pay] {
            let mut c = call(class, Some("jobs.example"));
            c.recipients = vec!["new@x.com".into()];
            c.hints = vec![HINT_JOB_APPLICATION.into()];
            let card = card(g.check(&c).await.unwrap());
            assert_eq!(card.class, class);
            assert_eq!(card.on_timeout, OnTimeout::Skip, "{class}");
        }
        // A higher-class hint rule does not reach down to a lower class.
        let (s, g) = gate(MemStore::empty());
        s.add_rule("*", "Pay:invoice", Mode::Stop, false).unwrap();
        let mut c = call(Class::Submit, Some("shop.example"));
        c.hints = vec!["invoice".into()];
        assert_eq!(g.check(&c).await.unwrap(), Decision::Allow);
        // The domain key at the call's class wins over a lower-class key.
        s.add_rule("*", "Submit:invoice", Mode::Ask, false).unwrap();
        s.add_rule("shop.example", "Account:invoice", Mode::Stop, false)
            .unwrap();
        let mut a = call(Class::Account, Some("shop.example"));
        a.hints = vec!["invoice".into()];
        assert_eq!(mode_of(&g.check(&a).await.unwrap()), Mode::Stop);
    }

    #[tokio::test]
    async fn a_lower_class_hint_allow_cannot_lower_a_higher_class() {
        // Guard on D-110 (2): ('*','Read:newsletter','allow') must not allow a Pay call.
        let (s, g) = gate(MemStore::new());
        s.add_rule("*", "Read:newsletter", Mode::Allow, false)
            .unwrap();
        let mut c = call(Class::Pay, Some("shop.example"));
        c.hints = vec!["newsletter".into()];
        let pay = card(g.check(&c).await.unwrap());
        assert_eq!(pay.on_timeout, OnTimeout::Skip);
        // ... nor an Account call; and from a hint, the ask skips.
        c.class = Class::Account;
        assert_eq!(card(g.check(&c).await.unwrap()).on_timeout, OnTimeout::Skip);
        // At its own class the allow applies.
        c.class = Class::Read;
        assert_eq!(g.check(&c).await.unwrap(), Decision::Allow);
    }

    #[tokio::test]
    async fn timeout_proceeds_only_for_account_and_send_new() {
        // D-110 (4).
        let (s, g) = gate(MemStore::new());
        s.add_contact("known@example.com");
        // Account and Send(new): proceed.
        let account = card(
            g.check(&call(Class::Account, Some("a.example")))
                .await
                .unwrap(),
        );
        assert_eq!(account.on_timeout, OnTimeout::Proceed);
        let new = card(g.check(&send(&["new@x.com"])).await.unwrap());
        assert_eq!(new.on_timeout, OnTimeout::Proceed);
        // A user rule asking on Send to a known contact: skip.
        s.add_rule("*", "Send", Mode::Ask, false).unwrap();
        let known = card(g.check(&send(&["known@example.com"])).await.unwrap());
        assert_eq!(known.on_timeout, OnTimeout::Skip);
        // An Account ask from a hint rule: skip.
        s.add_rule("*", "Account:signup", Mode::Ask, false).unwrap();
        let mut hinted = call(Class::Account, Some("a.example"));
        hinted.hints = vec!["signup".into()];
        assert_eq!(
            card(g.check(&hinted).await.unwrap()).on_timeout,
            OnTimeout::Skip
        );
        // A user rule asking on a lenient class: skip.
        s.add_rule("*", "Fill", Mode::Ask, false).unwrap();
        let fill = card(
            g.check(&call(Class::Fill, Some("a.example")))
                .await
                .unwrap(),
        );
        assert_eq!(fill.on_timeout, OnTimeout::Skip);
    }

    #[tokio::test]
    async fn every_card_string_is_control_stripped_and_capped() {
        // D-110 (5): page text with ANSI/CR made a card misreport the action.
        let mut c = call(Class::Pay, Some("shop.example"));
        c.what = Some(
            "pay 1 EUR\r\u{1b}[2K\u{1b}]0;title\u{07}pay \u{1b}[31m1000\u{1b}[0m EUR\u{202e}x"
                .into(),
        );
        c.values = vec![
            ("amount\u{1b}[8m".into(), "1\r1000\u{9b}2K\u{200b}".into()),
            ("k".repeat(500), "v\n".repeat(500)),
        ];
        let c1 = card(decide(MemStore::new(), &c).await);
        assert_eq!(c1.what, "pay 1 EUR pay 1000 EUR x");
        assert_eq!(c1.values[0], ("amount".to_string(), "1 1000".to_string()));
        assert_eq!(c1.values[1].0.chars().count(), KEY_MAX_CHARS);
        assert_eq!(c1.values[1].1.chars().count(), VALUE_MAX_CHARS);
        let all: Vec<&str> = std::iter::once(c1.what.as_str())
            .chain(c1.values.iter().flat_map(|(k, v)| [k.as_str(), v.as_str()]))
            .collect();
        for s in all {
            assert!(
                !s.chars().any(|ch| ch.is_control() || invisible(ch)),
                "{s:?}"
            );
        }

        // An unterminated OSC keeps its text: a stray `ESC ]` cannot hide the recipient.
        c.what = Some("send to a@x.com\u{1b}]0; and evil@x.com".into());
        let c0 = card(decide(MemStore::new(), &c).await);
        assert_eq!(c0.what, "send to a@x.com 0; and evil@x.com");

        // A what that is only control characters falls back to the generated line.
        c.what = Some("\u{1b}[2K\r\n".into());
        c.values = Vec::new();
        c.args = json!({"note": "a\u{1b}[1Ab"});
        let card2 = card(decide(MemStore::new(), &c).await);
        assert_eq!(card2.what, "echo.send: note=a b");
        assert_eq!(card2.values, vec![("note".to_string(), "a b".to_string())]);

        // Too many values are summarised.
        c.values = (0..40)
            .map(|i| (format!("k{i}"), "v".to_string()))
            .collect();
        let card3 = card(decide(MemStore::new(), &c).await);
        assert_eq!(card3.values.len(), MAX_VALUES + 1);
        assert_eq!(card3.values[MAX_VALUES].1, "8 more values not shown");
    }
}
