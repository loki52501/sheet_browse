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
/// ANSI escape sequences, CR, bidi, zero-width and other default-ignorable marks) and
/// length-capped (D-110 (5)). A Send card's first value is the gate's own
/// `("new recipients (N)", list)` row (D-120).
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

/// Longest DNS label and name, in ASCII (A-label) form.
const MAX_LABEL: usize = 63;
const MAX_HOST: usize = 253;

/// D-110 (1), D-120, D-126: the canonical `gate.rules.domain`, the ASCII (A-label) form a
/// browser's URL host carries. The trimmed value goes through UTS-46 ToASCII (the `idna`
/// crate, nontransitional processing, as the WHATWG URL parser runs it: mapping, NFC,
/// lowercasing, the alternative full stops U+3002, U+FF0E, U+FF61 as `.`, Punycode `xn--`
/// labels); one trailing dot is then removed. `'*'` stays `'*'`.
/// Rejects (`MemoryError::Invalid`) what UTS-46 refuses (disallowed code points, bidi and
/// joiner rule failures, a bad `xn--` label, the forbidden host code points
/// `% # / : < > ? @ [ \ ] ^ |`, space and controls) and anything that is not a bare host
/// name after mapping: a character other than ASCII letters, digits, `-` and `_` in a label
/// (so `*` inside a name, `!`, `,` ...), an empty label (`..`, a leading dot, an empty
/// value), a label over 63 or a name over 253 characters.
/// `rule_domain(" Bank.COM. ") == Ok("bank.com")`,
/// `rule_domain("BÜCHER.DE") == Ok("xn--bcher-kva.de")`,
/// `rule_domain("straẞe.de") == Ok("xn--strae-oqa.de")`.
pub fn rule_domain(domain: &str) -> Result<String, MemoryError> {
    use idna::uts46::{AsciiDenyList, DnsLength, Hyphens, Uts46};
    let invalid = || {
        MemoryError::Invalid(format!(
            "gate rule domain {domain:?} is not a host name or '*'"
        ))
    };
    // Lengths are checked below on the result (after the trailing dot is removed), so idna
    // checks only what it alone knows: mapping, validity, bidi, joiners.
    let mapped = Uts46::new()
        .to_ascii(
            domain.trim().as_bytes(),
            AsciiDenyList::URL,
            Hyphens::Allow,
            DnsLength::Ignore,
        )
        .map_err(|_| invalid())?;
    if mapped == ANY_DOMAIN {
        return Ok(mapped.into_owned());
    }
    let host = mapped.strip_suffix('.').unwrap_or(&mapped);
    let label_ok = |l: &str| {
        !l.is_empty()
            && l.len() <= MAX_LABEL
            && l.bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
    };
    if host.len() > MAX_HOST || !host.split('.').all(label_ok) {
        return Err(invalid());
    }
    Ok(host.to_string())
}

/// D-110 (1), D-120: the canonical form of a classifier hint inside a rule key: trimmed and
/// ASCII-lowercased. Hints are classifier identifiers: only ASCII letters, digits, `_`, `-`
/// and `.` are accepted. Rejects (`MemoryError::Invalid`) an empty hint and any other
/// character (`:`, whitespace, control, invisible, full-width, combining, non-ASCII).
/// `rule_hint(" Job_Application ") == Ok("job_application")`.
pub fn rule_hint(hint: &str) -> Result<String, MemoryError> {
    let h = hint.trim().to_ascii_lowercase();
    if h.is_empty()
        || !h
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '_' | '-' | '.'))
    {
        return Err(MemoryError::Invalid(format!(
            "gate rule hint {hint:?} is empty or not ASCII letters, digits, '_', '-', '.'"
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
    /// A hint rule decided (or tied) the mode: an ask under it never times out to proceed
    /// (D-110 (4)). False when a lower-class hint lost to the class level (D-120).
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

/// A Send's recipients as the gate sees them (H-1.1, D-120).
struct Recipients<'a> {
    /// Non-blank recipients, as given.
    all: Vec<&'a str>,
    /// Those not in `memory.contacts`, one per [`contact_key`], first spelling kept.
    new: Vec<&'a str>,
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
    ///    the rule are not strict, no hint rule decided the mode, and the class is Account,
    ///    or Send with at least one recipient not in `memory.contacts` and every such
    ///    recipient shown in full and as written on the card (D-110 (4), D-120). So a
    ///    user's ask rule on Send to known contacts, a hinted ask, a Send with no
    ///    recipients and a Send whose new recipients the card cannot show exactly all skip.
    ///
    /// Recipients that are blank after trimming are not recipients. A Send card's first
    /// value is always `("new recipients (N)", list)` (D-120).
    pub async fn check(&self, call: &Call) -> Result<Decision, GateError> {
        if call.class == Class::Challenge {
            return Ok(Decision::Stop(StopReason {
                class: Class::Challenge,
                why: format!(
                    "challenge at {}: stopped, the user acts in the browser and says continue (D-5)",
                    card_text(&call.tool, TOOL_MAX_CHARS)
                ),
                element: element_of(&call.args),
            }));
        }

        let recipients = if call.class == Class::Send {
            let all: Vec<&str> = call
                .recipients
                .iter()
                .map(String::as_str)
                .filter(|r| !contact_key(r).is_empty())
                .collect();
            let new = self.new_recipients(&all).await?;
            Some(Recipients { all, new })
        } else {
            None
        };
        let known = recipients
            .as_ref()
            .is_some_and(|r| !r.all.is_empty() && r.new.is_empty());
        let r = self.resolve(call, known).await?;

        Ok(match r.mode {
            Mode::Allow => Decision::Allow,
            Mode::Ask => {
                let row = recipients.as_ref().map(|r| recipients_row(&r.new));
                let may_proceed = !r.from_hint
                    && match call.class {
                        Class::Account => true,
                        Class::Send => {
                            recipients.as_ref().is_some_and(|r| !r.new.is_empty())
                                && row.as_ref().is_some_and(|row| row.complete)
                        }
                        _ => false,
                    };
                let strict = call.strict || r.strict || !may_proceed;
                let shown = recipients.as_ref().map_or(&[][..], |r| r.all.as_slice());
                Decision::Ask(ask_card(
                    call,
                    shown,
                    row.map(|row| row.row),
                    Gate::on_timeout(call.class, strict),
                ))
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
    /// must not proceed on its own (D-109, D-110 (4), D-120): a strict run or rule, an ask a
    /// hint rule decided, and a Send whose new recipients are none or not all shown exactly.
    pub fn on_timeout(class: Class, strict: bool) -> OnTimeout {
        match class {
            Class::Send | Class::Account if !strict => OnTimeout::Proceed,
            _ => OnTimeout::Skip,
        }
    }

    /// Steps 2 and 3 of [`Gate::check`]. `known`: a Send whose recipients are all contacts.
    async fn resolve(&self, call: &Call, known: bool) -> Result<Resolved, GateError> {
        let domain = call
            .domain
            .as_deref()
            .and_then(|d| rule_domain(d).ok())
            .filter(|d| d != ANY_DOMAIN);
        let domain = domain.as_deref();

        let hits = self.hint_hits(call.class, domain, &call.hints).await?;
        if hits.is_empty() {
            return self.class_level(call.class, domain, known).await;
        }

        let mut out: Option<Resolved> = None;
        let mut base: Option<Resolved> = None;
        for hit in hits {
            let mut mode = hit.rule.mode;
            let mut strict = hit.rule.strict;
            let mut name = Some(hit.name);
            let mut from_hint = true;
            if hit.class < call.class {
                if base.is_none() {
                    base = Some(self.class_level(call.class, domain, known).await?);
                }
                if let Some(b) = &base {
                    // D-113 raise-only. When the class level is stricter it decided, not the
                    // hint, so the ask times out as a class-level ask would (D-120).
                    if severity(b.mode) > severity(mode) {
                        mode = b.mode;
                        name = b.name.clone();
                        from_hint = false;
                    }
                    strict |= b.strict;
                }
            }
            match &mut out {
                None => {
                    out = Some(Resolved {
                        mode,
                        strict,
                        from_hint,
                        name,
                    })
                }
                Some(o) => {
                    o.strict |= strict;
                    if severity(mode) > severity(o.mode) {
                        o.mode = mode;
                        o.name = name;
                        o.from_hint = from_hint;
                    } else if severity(mode) == severity(o.mode) {
                        o.from_hint |= from_hint;
                    }
                }
            }
        }
        match out {
            Some(o) => Ok(o),
            None => self.class_level(call.class, domain, known).await,
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
        class: Class,
        domain: Option<&str>,
        known: bool,
    ) -> Result<Resolved, GateError> {
        let key = class.as_str();
        for d in domain.into_iter().chain([ANY_DOMAIN]) {
            if let Some(rule) = self.store.gate_rule(d, key).await? {
                return Ok(Resolved {
                    mode: rule.mode,
                    strict: rule.strict,
                    from_hint: false,
                    name: Some(format!("({d}, {key})")),
                });
            }
        }
        Ok(Resolved {
            mode: Gate::default_mode(class, known),
            strict: false,
            from_hint: false,
            name: None,
        })
    }

    /// The recipients not in `memory.contacts` (H-1.1), one per [`contact_key`] (the first
    /// spelling is kept), in order. One lookup per distinct key.
    async fn new_recipients<'a>(&self, recipients: &[&'a str]) -> Result<Vec<&'a str>, GateError> {
        let mut seen: Vec<String> = Vec::new();
        let mut new = Vec::new();
        for r in recipients {
            let key = contact_key(r);
            if seen.contains(&key) {
                continue;
            }
            if self.store.contact(&key).await?.is_none() {
                new.push(*r);
            }
            seen.push(key);
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
/// Cap on the tool name inside a stop reason, in characters.
const TOOL_MAX_CHARS: usize = 128;
/// The Send card's new-recipients row (D-120): at most this many recipients, this many
/// characters each (longer ones keep their head and their `@domain`), and this many
/// characters in all; the rest are counted, never silently dropped.
const ROW_MAX_RECIPIENTS: usize = 32;
const RECIPIENT_MAX_CHARS: usize = 120;
const ROW_MAX_CHARS: usize = 2000;
/// The key prefix of that row; a classifier or arg value under it is dropped.
const NEW_RECIPIENTS: &str = "new recipients";

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

/// Invisible characters that can reorder or hide text on a terminal or in a notification:
/// Unicode's Default_Ignorable_Code_Point set (soft hyphen, combining grapheme joiner,
/// Hangul fillers, Mongolian and other variation selectors, zero-width and bidi marks, word
/// joiner and invisible operators, BOM, tags), the interlinear annotation marks, and the
/// blank-looking U+2800 BRAILLE PATTERN BLANK (D-120).
fn invisible(c: char) -> bool {
    matches!(
        c,
        '\u{00ad}'
            | '\u{034f}'
            | '\u{061c}'
            | '\u{115f}'..='\u{1160}'
            | '\u{17b4}'..='\u{17b5}'
            | '\u{180b}'..='\u{180f}'
            | '\u{200b}'..='\u{200f}'
            | '\u{202a}'..='\u{202e}'
            | '\u{2060}'..='\u{206f}'
            | '\u{2800}'
            | '\u{3164}'
            | '\u{fe00}'..='\u{fe0f}'
            | '\u{feff}'
            | '\u{ffa0}'
            | '\u{fff0}'..='\u{fffb}'
            | '\u{1bca0}'..='\u{1bca3}'
            | '\u{1d173}'..='\u{1d17a}'
            | '\u{e0000}'..='\u{e0fff}'
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
    let mut out = String::with_capacity(s.len().min(max.saturating_mul(4)));
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

/// `s` cleaned without a length cap (one component of a longer line).
fn clean_part(s: &str) -> String {
    card_text(s, usize::MAX)
}

/// "send to a@x.com: subject=Hi, body=...". Each component (tool, every recipient, every
/// arg key and value) is cleaned on its own before joining, so an escape sequence opened
/// in one cannot swallow the next (D-110 (5)); the caller caps the joined line.
fn what_line(call: &Call, recipients: &[&str]) -> String {
    let mut line = clean_part(&call.tool);
    if !recipients.is_empty() {
        let to: Vec<String> = recipients.iter().map(|r| clean_part(r)).collect();
        line.push_str(" to ");
        line.push_str(&to.join(", "));
    }
    let args = flatten_args(&call.args);
    if !args.is_empty() {
        let parts: Vec<String> = args
            .iter()
            .map(|(k, v)| format!("{}={}", clean_part(k), clean_part(v)))
            .collect();
        line.push_str(": ");
        line.push_str(&parts.join(", "));
    }
    line
}

/// One recipient as the new-recipients row shows it, and whether that is the recipient in
/// full and as written. Over [`RECIPIENT_MAX_CHARS`] it keeps its head and its `@domain`
/// (or its last characters) around a visible `…(+N chars)…` marker.
fn shown_recipient(r: &str) -> (String, bool) {
    let clean = clean_part(r);
    let exact = clean == r.trim() && !clean.is_empty();
    let chars: Vec<char> = clean.chars().collect();
    let n = chars.len();
    if clean.is_empty() {
        return ("(unprintable)".to_string(), false);
    }
    if n <= RECIPIENT_MAX_CHARS {
        return (clean, exact);
    }
    let max_tail = RECIPIENT_MAX_CHARS / 2;
    let at = chars
        .iter()
        .rposition(|&c| c == '@')
        .filter(|&i| n - i <= max_tail)
        .unwrap_or(n - max_tail);
    let tail: String = chars[at..].iter().collect();
    let head_len = RECIPIENT_MAX_CHARS - (n - at);
    let head: String = chars[..head_len].iter().collect();
    let omitted = n - RECIPIENT_MAX_CHARS;
    (format!("{head}…(+{omitted} chars)…{tail}"), false)
}

/// The Send card's first value (D-120).
struct RecipientsRow {
    row: (String, String),
    /// Every new recipient is listed, in full and as written.
    complete: bool,
}

/// `("new recipients (N)", "a@x.com, b@y.org")`, or `"none"`. Lists up to
/// [`ROW_MAX_RECIPIENTS`] within [`ROW_MAX_CHARS`] and ends with `…(+K more)` for the rest.
fn recipients_row(new: &[&str]) -> RecipientsRow {
    let mut parts: Vec<String> = Vec::new();
    let mut complete = true;
    let mut used = 0;
    for (i, r) in new.iter().enumerate() {
        let (shown, exact) = shown_recipient(r);
        let cost = shown.chars().count() + 2;
        if i == ROW_MAX_RECIPIENTS || used + cost > ROW_MAX_CHARS {
            complete = false;
            parts.push(format!("…(+{} more)", new.len() - i));
            break;
        }
        complete &= exact;
        used += cost;
        parts.push(shown);
    }
    let value = if new.is_empty() {
        "none".to_string()
    } else {
        parts.join(", ")
    };
    RecipientsRow {
        row: (format!("{NEW_RECIPIENTS} ({})", new.len()), value),
        complete,
    }
}

fn ask_card(
    call: &Call,
    recipients: &[&str],
    row: Option<(String, String)>,
    on_timeout: OnTimeout,
) -> AskCard {
    let what = call
        .what
        .as_deref()
        .map(|w| card_text(w, WHAT_MAX_CHARS))
        .filter(|w| !w.is_empty())
        .unwrap_or_else(|| card_text(&what_line(call, recipients), WHAT_MAX_CHARS));
    let raw = if call.values.is_empty() {
        flatten_args(&call.args)
    } else {
        call.values.clone()
    };
    let mut values: Vec<(String, String)> = Vec::new();
    let room = match row {
        Some(row) => {
            values.push(row);
            MAX_VALUES - 1
        }
        None => MAX_VALUES,
    };
    values.extend(clean_values(&raw, room));
    AskCard {
        class: call.class,
        what,
        site: call.domain.as_deref().map(|d| card_text(d, SITE_MAX_CHARS)),
        values,
        timeout: ASK_TIMEOUT,
        on_timeout,
    }
}

/// Each value's key and text cleaned and capped; at most `room` of them, then one summary
/// row. A value under the gate's own `new recipients` key is dropped: only the gate writes
/// that row (D-120).
fn clean_values(raw: &[(String, String)], room: usize) -> Vec<(String, String)> {
    let cleaned: Vec<(String, String)> = raw
        .iter()
        .map(|(k, v)| (card_text(k, KEY_MAX_CHARS), card_text(v, VALUE_MAX_CHARS)))
        .filter(|(k, _)| !k.to_lowercase().starts_with(NEW_RECIPIENTS))
        .collect();
    let more = cleaned.len().saturating_sub(room);
    let mut values: Vec<(String, String)> = cleaned.into_iter().take(room).collect();
    if more > 0 {
        values.push(("...".to_string(), format!("{more} more values not shown")));
    }
    values
}

/// D-70, D-110 (5): the gate's card sanitiser for a card built elsewhere (`ToolCtx::ask`,
/// diagnostic lines). `what`, `site` and every value key and text are control-stripped
/// (escape sequences, C0/C1 controls, CR/LF, bidi, zero-width and default-ignorable marks
/// become one space) and capped (200 characters for `what` and each value, 64 for a key,
/// 253 for the site; at most 32 values plus one summary row). A value under the gate's
/// `new recipients` key is dropped (D-120). `class`, `timeout` and `on_timeout` are kept.
pub fn clean_card(card: AskCard) -> AskCard {
    AskCard {
        what: card_text(&card.what, WHAT_MAX_CHARS),
        site: card.site.as_deref().map(|s| card_text(s, SITE_MAX_CHARS)),
        values: clean_values(&card.values, MAX_VALUES),
        ..card
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
        // ... nor an Account call. The class level (the seeded Account ask) decided, not the
        // hint, so it times out like any Account ask (review T-04 r2 item 1, D-120).
        c.class = Class::Account;
        assert_eq!(
            card(g.check(&c).await.unwrap()).on_timeout,
            OnTimeout::Proceed
        );
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

    // ---- Round-2 review regressions (each failed on commit c22ece0) ----

    fn row<'a>(c: &'a AskCard, key_prefix: &str) -> Option<&'a (String, String)> {
        c.values.iter().find(|(k, _)| k.starts_with(key_prefix))
    }

    #[tokio::test]
    async fn escapes_cannot_span_card_components() {
        // what_line joined recipients before cleaning: an OSC opened in one recipient and
        // closed (BEL) in the next ate the new address from a card that proceeds on timeout.
        let (s, g) = gate(MemStore::new());
        s.add_contact("boss@company.com");
        let mut c = send(&["\u{1b}]attacker@evil.example", "\u{7}boss@company.com"]);
        c.args = json!({"subject": "hi"});
        let sc = card(g.check(&c).await.unwrap());
        assert!(sc.what.contains("attacker@evil.example"), "{}", sc.what);
        assert!(sc.what.contains("boss@company.com"), "{}", sc.what);
        // The recipient is not shown as written, so silence must not send.
        assert_eq!(sc.on_timeout, OnTimeout::Skip);

        // The same across args and from the tool name into the args.
        let mut p = call(Class::Pay, Some("shop.example"));
        p.args = json!({"a": "\u{1b}]x", "b": "\u{7}to evil"});
        let pc = card(g.check(&p).await.unwrap());
        assert!(pc.what.contains("b=to evil"), "{}", pc.what);
        p.tool = "shop.pay\u{1b}]".into();
        p.args = json!({"to": "\u{7}evil"});
        let pc = card(g.check(&p).await.unwrap());
        assert!(pc.what.contains("to=evil"), "{}", pc.what);

        // A stop's reason carries the tool name cleaned.
        let mut ch = call(Class::Challenge, Some("x.example"));
        ch.tool = "page.click\u{1b}[2K\rok".into();
        let r = stop(g.check(&ch).await.unwrap());
        assert!(!r.why.chars().any(char::is_control), "{:?}", r.why);
    }

    #[tokio::test]
    async fn send_card_lists_every_new_recipient() {
        // 8 known + 1 new pushed the new address past the 200-char what cap.
        let (s, g) = gate(MemStore::new());
        let known: Vec<String> = (0..8)
            .map(|i| format!("colleague{i}@a-company-with-a-long-name.example"))
            .collect();
        for k in &known {
            s.add_contact(k);
        }
        let mut to: Vec<&str> = known.iter().map(String::as_str).collect();
        to.push("new@evil.example");
        let c = card(g.check(&send(&to)).await.unwrap());
        assert_eq!(
            c.values[0],
            (
                "new recipients (1)".to_string(),
                "new@evil.example".to_string()
            )
        );
        assert_eq!(c.on_timeout, OnTimeout::Proceed);

        // One 250-char recipient: capped with a visible marker, domain kept, card skips.
        let long = format!("{}@evil.example", "a".repeat(237));
        assert_eq!(long.chars().count(), 250);
        let c = card(g.check(&send(&[&long])).await.unwrap());
        let (k, v) = &c.values[0];
        assert_eq!(k, "new recipients (1)");
        assert!(v.contains("…(+130 chars)…"), "{v}");
        assert!(v.ends_with("@evil.example"), "{v}");
        assert_eq!(c.on_timeout, OnTimeout::Skip);

        // 40 new recipients and 40 classifier values: the row is first, never dropped by
        // the row cap, lists what fits and says how many more; the card skips.
        let many: Vec<String> = (0..40).map(|i| format!("n{i}@x.example")).collect();
        let refs: Vec<&str> = many.iter().map(String::as_str).collect();
        let mut m = send(&refs);
        m.values = (0..40)
            .map(|i| (format!("k{i}"), "v".to_string()))
            .collect();
        let c = card(g.check(&m).await.unwrap());
        let (k, v) = &c.values[0];
        assert_eq!(k, "new recipients (40)");
        assert!(
            v.contains("n0@x.example") && v.contains("n31@x.example"),
            "{v}"
        );
        assert!(!v.contains("n32@x.example"), "{v}");
        assert!(v.ends_with("…(+8 more)"), "{v}");
        assert_eq!(c.on_timeout, OnTimeout::Skip);
        assert!(c.values.len() <= MAX_VALUES + 1, "{}", c.values.len());

        // Duplicates (by contact key) count once; a classifier row cannot fake the row.
        let mut d = send(&["New@X.example", "new@x.example"]);
        d.values = vec![("new recipients (0)".into(), "none".into())];
        let c = card(g.check(&d).await.unwrap());
        assert_eq!(
            c.values,
            vec![(
                "new recipients (1)".to_string(),
                "New@X.example".to_string()
            )]
        );
        assert_eq!(c.on_timeout, OnTimeout::Proceed);
    }

    #[tokio::test]
    async fn blank_recipients_are_not_recipients() {
        // [""] or ["  "] counted as one new recipient and proceeded on timeout.
        let (s, g) = gate(MemStore::new());
        s.add_contact("known@example.com");
        for to in [&[""][..], &["  "], &["", " \t "]] {
            let c = card(g.check(&send(to)).await.unwrap());
            assert_eq!(c.on_timeout, OnTimeout::Skip, "{to:?}");
            assert_eq!(
                row(&c, "new recipients"),
                Some(&("new recipients (0)".to_string(), "none".to_string())),
                "{to:?}"
            );
        }
        // A blank beside a known contact is a Send to a known contact.
        assert_eq!(
            g.check(&send(&[" ", "known@example.com"])).await.unwrap(),
            Decision::Allow
        );
    }

    #[tokio::test]
    async fn a_hint_overruled_by_the_class_level_does_not_force_skip() {
        // D-113 raise-only: the class level decided, yet the ask was marked as from a hint
        // and a Send(new) / Account ask timed out to skip.
        let (s, g) = gate(MemStore::new());
        s.add_rule("*", "Read:newsletter", Mode::Allow, false)
            .unwrap();
        let mut sn = send(&["new@x.com"]);
        sn.hints = vec!["newsletter".into()];
        assert_eq!(
            card(g.check(&sn).await.unwrap()).on_timeout,
            OnTimeout::Proceed
        );
        let mut a = call(Class::Account, Some("shop.example"));
        a.hints = vec!["newsletter".into()];
        assert_eq!(
            card(g.check(&a).await.unwrap()).on_timeout,
            OnTimeout::Proceed
        );
        // A hint that matches the class level's mode still counts (D-110 (2)).
        a.hints = vec!["newsletter".into(), HINT_JOB_APPLICATION.into()];
        assert_eq!(card(g.check(&a).await.unwrap()).on_timeout, OnTimeout::Skip);
        // A hint that raises counts.
        s.add_rule("*", "Read:urgent", Mode::Ask, false).unwrap();
        s.add_rule("*", "Send", Mode::Allow, false).unwrap();
        sn.hints = vec!["urgent".into()];
        assert_eq!(
            card(g.check(&sn).await.unwrap()).on_timeout,
            OnTimeout::Skip
        );
    }

    #[tokio::test]
    async fn unicode_domains_and_hints_are_canonical_or_rejected() {
        // BÜCHER.DE was stored as "bücher.de" and never matched the A-label call domain.
        for (rule, calls) in [
            (
                "BÜCHER.DE",
                &["xn--bcher-kva.de", "bücher.de", "Bücher.de."][..],
            ),
            ("bücher\u{3002}de", &["xn--bcher-kva.de"]),
            ("xn--bcher-kva.de", &["BÜCHER.DE", "bücher\u{ff0e}de"]),
        ] {
            let (s, g) = gate(MemStore::empty());
            s.add_rule(rule, "Submit", Mode::Stop, false).unwrap();
            for d in calls {
                let c = call(Class::Submit, Some(d));
                assert_eq!(
                    mode_of(&g.check(&c).await.unwrap()),
                    Mode::Stop,
                    "rule {rule:?} call {d:?}"
                );
            }
        }
        assert_eq!(rule_domain("BÜCHER.DE").unwrap(), "xn--bcher-kva.de");
        for bad in ["bank\u{202e}.com", "bank\u{e0041}.com", "bank%2e.com"] {
            assert!(
                matches!(rule_domain(bad), Err(MemoryError::Invalid(_))),
                "{bad:?}"
            );
        }
        // D-126: UTS-46 ignores these, so the browser's host (and the stored key) has none.
        for ignored in [
            "bank\u{200b}.com",
            "\u{feff}bank.com",
            "bank.com\u{2060}",
            "ba\u{00ad}nk.com",
            "ban\u{034f}k.com",
            "bank\u{fe0f}.com",
            "bank\u{3164}.com",
            "bank\u{115f}.com",
        ] {
            assert_eq!(rule_domain(ignored).unwrap(), "bank.com", "{ignored:?}");
        }
        // U+2800 is a valid (if blank) symbol in UTS-46: its host is its own A-label.
        assert_eq!(
            rule_domain("bank\u{2800}.com").unwrap(),
            "xn--bank-yj5b.com"
        );
        for bad in [
            "job\u{200b}_application",
            "job_application\u{feff}",
            "\u{2060}signature",
            "job\u{e0041}",
            "ｊｏｂ_application",
            "signatu\u{0301}re",
        ] {
            assert!(
                matches!(rule_hint(bad), Err(MemoryError::Invalid(_))),
                "{bad:?}"
            );
        }
        // Cards drop the same invisible characters.
        let mut c = call(Class::Pay, Some("shop.example"));
        c.what = Some(
            "pay\u{2800}1\u{3164}0\u{e0041}0\u{034f}0\u{fe0f} EUR\u{115f}\u{1160}\u{ffa0}".into(),
        );
        let pc = card(decide(MemStore::new(), &c).await);
        assert_eq!(pc.what, "pay 1 0 0 0 EUR");
    }

    #[tokio::test]
    async fn d126_stored_key_is_the_idna_a_label() {
        // review-T-04 round 3 cases and B-190's, with the A-label a browser's URL host
        // carries for each (UTS-46 nontransitional), cross-checked against idna's own
        // WHATWG entry point.
        for (rule, host) in [
            ("\u{392}\u{391}\u{3a3}.gr", "xn--mxab6c.gr"), // ΒΑΣ: capital sigma -> σ, not ς
            ("example.\u{391}\u{3a3}", "example.xn--mxa0b"),
            ("stra\u{1e9e}e.de", "xn--strae-oqa.de"), // U+1E9E -> ß, kept (nontransitional)
            ("\u{f900}.kr", "xn--oh3a.kr"),           // CJK compatibility ideograph -> U+8C48
            ("\u{3131}.kr", "xn--ypd.kr"),            // Hangul compatibility jamo -> U+1100
            ("\u{17f}hop.example", "shop.example"),   // long s
            ("\u{133}ssel.nl", "ijssel.nl"),          // ligature ij
            ("b\u{1d2c}nk.example", "bank.example"),  // modifier capital A
            ("bu\u{308}cher.de", "xn--bcher-kva.de"), // decomposed: NFC first
        ] {
            let stored = rule_domain(rule).unwrap_or_else(|e| panic!("{rule:?}: {e}"));
            assert_eq!(stored, host, "{rule:?}");
            assert_eq!(idna::domain_to_ascii(rule).unwrap(), host, "{rule:?}");
            assert_eq!(rule_domain(&stored).unwrap(), stored, "idempotent");
            // A rule written either way stops the call whose host is the A-label.
            let (s, g) = gate(MemStore::empty());
            s.add_rule(rule, "Submit", Mode::Stop, false).unwrap();
            let c = call(Class::Submit, Some(host));
            assert_eq!(mode_of(&g.check(&c).await.unwrap()), Mode::Stop, "{rule:?}");
        }
        // What UTS-46 refuses is refused, never stored.
        for bad in [
            "ban\u{200d}k.com", // ZWJ outside a virama context (CheckJoiners)
            "bank\u{202e}.com", // bidi override: disallowed
            "xn--zz.com",       // not valid Punycode
            "bank\u{e0041}.com",
        ] {
            assert!(
                matches!(rule_domain(bad), Err(MemoryError::Invalid(_))),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn rule_domain_punycode_matches_rfc_3492() {
        for (u, a) in [
            ("bücher", "bcher-kva"),
            ("münchen", "mnchen-3ya"),
            ("mañana", "maana-pta"),
            ("例え", "r8jz45g"),
            ("テスト", "zckzah"),
        ] {
            assert_eq!(rule_domain(u).unwrap(), format!("xn--{a}"), "{u}");
        }
        // UTS-46 full-width and compatibility forms map (D-126); a forbidden host code point
        // and oversize names refuse.
        assert_eq!(rule_domain("Ｂａｎｋ．com").unwrap(), "bank.com");
        assert_eq!(
            rule_domain("例え.テスト").unwrap(),
            "xn--r8jz45g.xn--zckzah"
        );
        for (compat, host) in [
            ("\u{1d41b}ank.com", "bank.com"),
            ("\u{fb01}le.com", "file.com"),
            ("\u{2460}.com", "1.com"),
        ] {
            assert_eq!(rule_domain(compat).unwrap(), host, "{compat:?}");
        }
        let long_name = "a.".repeat(128);
        for bad in ["ｂａｎｋ／x.com", long_name.as_str()] {
            assert!(rule_domain(bad).is_err(), "{bad:?}");
        }
        assert!(rule_domain(&format!("{}.com", "a".repeat(64))).is_err());
        // KELVIN SIGN must not fold into an ASCII hint (D-62's look-alike rule).
        assert!(rule_hint("\u{212a}").is_err());
    }

    #[test]
    fn clean_card_is_the_gate_sanitiser() {
        // D-70: handler-built cards get the same strip and caps; the gate's row cannot be faked.
        let c = clean_card(AskCard {
            class: Class::Account,
            what: "sign up\r\u{1b}[2Kas admin\u{200b}".into(),
            site: Some("x.example\u{1b}[8m".into()),
            values: vec![
                ("New Recipients (0)".into(), "none".into()),
                ("name\n".into(), "v".repeat(500)),
            ],
            timeout: Duration::from_secs(5),
            on_timeout: OnTimeout::Proceed,
        });
        assert_eq!(c.what, "sign up as admin");
        assert_eq!(c.site.as_deref(), Some("x.example"));
        assert_eq!(c.values.len(), 1);
        assert_eq!(c.values[0].0, "name");
        assert_eq!(c.values[0].1.chars().count(), VALUE_MAX_CHARS);
        assert_eq!(
            (c.class, c.timeout, c.on_timeout),
            (Class::Account, Duration::from_secs(5), OnTimeout::Proceed)
        );
    }
}
