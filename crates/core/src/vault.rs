//! I-4 Vault (docs/01 H-2). CONTRACT: change only through `interface_changes`.
//!
//! The one rule: a resolved value goes into a sink (the `insertText` message, the HTTP
//! header) and nowhere else. It is never returned to a tool caller, logged, put in the
//! ledger, a panic message or a fixture. Every string that leaves the process passes
//! through [`scrub`], which turns any vault value back into `@vault(name)`.
//!
//! Backend (D-101): `vault.toml.age`, an age x25519-encrypted TOML map
//! `name = { value = "...", kind = "secret" | "personal" }`, decrypted with an identity
//! from the OS keychain (default) or an identity file outside the repo. A missing vault
//! file opens as an empty vault; the identity is generated on the first [`Vault::set`].
//! `set` holds an exclusive OS lock on `<vault file>.lock` for its whole
//! read-modify-write, so concurrent sets (threads or processes) never orphan the vault
//! with a second key or drop each other's entries.
//!
//! Scrubbing (D-102): an ASCII-case-insensitive aho-corasick automaton over every value
//! and its encoded spellings (JSON, Rust Debug, every common percent-encoding including
//! the browser's WHATWG URL sets, double percent-encoding, Unicode case forms, and
//! base64 and base64url at every byte alignment, B-151; see [`spellings`]). All overlapping matches are found and the union of their spans is
//! replaced, so two entries that overlap in the text leave no fragment behind. A match
//! lying wholly inside an `@vault(name)` literal of an existing entry (or inside the
//! `@vault(` of any opener) is ignored; a match that only overlaps one is kept and widened
//! to cover the whole literal. Scrub repeats until a pass changes nothing (at most
//! 8 passes, after which the whole output is withheld), so a literal
//! it wrote cannot complete another value and `scrub(scrub(s)) == scrub(s)`. The
//! `@vault(name)` literals of existing entries in the input stay parseable by
//! [`vault_refs`]; a well-formed reference to an unknown name whose name holds a value
//! does not (`@vault(xx<v>yy)` scrubs to `@vault(xx@vault(n)yy)`), which leaks nothing
//! (D-118).
//!
//! Value rules (D-118): `set` refuses, and `open` refuses a file holding, a non-empty
//! value shorter than [`MIN_VALUE_CHARS`] characters (`ValueTooShort`), or one that
//! occurs, in any ASCII case, inside `@vault(`, inside `@vault(n)` for any entry name `n`,
//! or inside either withheld-output marker scrub can return (`ValueInLiteral`). Matched case-insensitively, a very short value (a middle
//! initial, a state code) hits ordinary words everywhere and protects almost nothing,
//! and a value inside a literal could not be told from it. Empty values are allowed and
//! scrub nothing. Both errors name the entry, never the value, and tell the user to run
//! `harness vault set <name>`; [`Vault::open_for_set`] skips these rules so that command
//! can repair such a file. It returns a [`VaultEditor`], not a `Vault` (D-84): the editor
//! sets, removes and lists entries but cannot resolve a value, scrub, or be installed as
//! the process-wide scrubber. `set` on the editor (and on [`Vault::in_memory`]) checks
//! only the rules involving the entry being set; `set` on a vault from `open` checks
//! every entry of the re-read file. The withheld marker is the fail-closed output of an `in_memory` vault whose
//! automaton could not be built, and of any scrub that hits the pass cap.
//!
//! Plaintext hygiene: values, decrypted plaintext, the serialized TOML and every
//! scrub spelling live in `Zeroizing` buffers sized up front so they never reallocate
//! (a reallocation leaves an unzeroed copy). Not zeroized, and not fixable without
//! forking dependencies: age's `StreamWriter` plaintext chunk buffer while encrypting,
//! `toml`'s parser internals while decrypting (strings with escapes are unescaped into
//! its own buffers before they move into an entry), std's `to_lowercase` growth for
//! non-ASCII values, and the aho-corasick automaton's pattern store, which holds every
//! spelling for the scrubber's lifetime.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fmt::Write as _;
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Once, RwLock};

use age::secrecy::ExposeSecret;
use aho_corasick::{AhoCorasick, MatchKind};
use zeroize::Zeroizing;

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum VaultError {
    /// The name only; never the value.
    #[error("no vault entry named {0:?}")]
    Unknown(String),
    #[error("malformed @vault(...) reference at byte {0}")]
    BadReference(usize),
    #[error("vault key unavailable: {0}")]
    Key(String),
    #[error("vault file: {0}")]
    Io(#[from] std::io::Error),
    #[error("vault decrypt or parse failed")]
    Decrypt,
    /// A non-empty value shorter than `min` characters (D-118). The name only.
    #[error(
        "vault entry {name:?} is shorter than {min} characters; a value that short cannot be scrubbed without destroying ordinary text; give it a longer value with `harness vault set {name}`"
    )]
    ValueTooShort { name: String, min: usize },
    /// The value of entry `name` occurs, in some ASCII case, inside `@vault(`, inside
    /// the `@vault(n)` of an entry `n`, where scrub could not tell it from the literal, or
    /// inside a withheld-output marker, which scrub would then rewrite (D-118 (3b)). The
    /// name only; `n` is not named either.
    #[error(
        "the value of vault entry {name:?} occurs inside `@vault(`, inside an entry's @vault(name) reference or inside the scrubber's `[output withheld: ...]` notice, where scrub cannot find it; give it another value with `harness vault set {name}`, or give the new entry another name"
    )]
    ValueInLiteral { name: String },
    /// The scrub automaton could not be built; `set` wrote nothing (D-118).
    #[error("vault scrubber could not be built; the vault was not changed")]
    ScrubUnavailable,
}

/// The shortest non-empty value `set` and `open` accept, in characters (D-118).
pub const MIN_VALUE_CHARS: usize = 4;

/// `ValueTooShort` when `value` is non-empty and under [`MIN_VALUE_CHARS`] characters.
fn check_value(name: &str, value: &str) -> Result<(), VaultError> {
    if !value.is_empty() && value.chars().nth(MIN_VALUE_CHARS - 1).is_none() {
        return Err(VaultError::ValueTooShort {
            name: name.to_owned(),
            min: MIN_VALUE_CHARS,
        });
    }
    Ok(())
}

/// Which entries [`check_entries`] checks.
#[derive(Clone, Copy)]
enum Check<'a> {
    /// Every entry: `open`, and `set` on a vault from `open`.
    All,
    /// Only what involves this entry: its own value, and every value against its
    /// `@vault(name)`. `set` on an `in_memory` vault or a `VaultEditor`: problems the
    /// set did not make are left for `open` to report, so they can be fixed one at a time.
    Only(&'a str),
}

/// True when `needle` occurs in `hay` ignoring ASCII case (the scrub automaton's rule).
fn contains_ascii_ci(hay: &str, needle: &str) -> bool {
    let n = needle.as_bytes();
    !n.is_empty()
        && hay
            .as_bytes()
            .windows(n.len())
            .any(|w| w.eq_ignore_ascii_case(n))
}

/// The D-118 value rules: `ValueTooShort` for a value under the floor, then
/// `ValueInLiteral` for a value any of whose scrub spellings lies, in some ASCII case,
/// inside `@vault(` or inside `@vault(n)` for an entry name `n` (every name, with a value
/// or not), or inside either withheld marker ([`SCRUB_WITHHELD`], [`SCRUB_UNSETTLED`]).
/// Scrub drops a match inside a literal as part of it, so the value could hide there; a
/// value inside a marker would be scrubbed out of the marker itself, so the fail-closed
/// output would change on every pass. The first offending entry is named.
fn check_entries(entries: &BTreeMap<String, Entry>, check: Check<'_>) -> Result<(), VaultError> {
    let involves = |n: &str| match check {
        Check::All => true,
        Check::Only(only) => only == n,
    };
    for (name, e) in entries {
        if involves(name) {
            check_value(name, &e.value)?;
        }
    }
    let mut every: Vec<String> = [REF_OPEN, SCRUB_WITHHELD, SCRUB_UNSETTLED]
        .map(str::to_owned)
        .into();
    every.extend(entries.keys().map(|n| vault_literal(n)));
    let own: Vec<String> = match check {
        Check::All => Vec::new(),
        Check::Only(only) => vec![vault_literal(only)],
    };
    for (name, e) in entries {
        if e.value.is_empty() {
            continue;
        }
        let involved = involves(name);
        let literals = if involved { &every } else { &own };
        if literals.is_empty() {
            continue;
        }
        let sps = spellings(&e.value);
        let hides_in = |l: &str| sps.iter().any(|sp| contains_ascii_ci(l, sp));
        // Another entry already hidden in `@vault(` is hidden in every literal: a problem
        // this set did not make, which `open` reports and its own set fixes.
        if !involved && hides_in(REF_OPEN) {
            continue;
        }
        if literals.iter().any(|l| hides_in(l)) {
            return Err(VaultError::ValueInLiteral { name: name.clone() });
        }
    }
    Ok(())
}

/// `kind` of an entry (H-2): credentials vs. the user's form data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EntryKind {
    Secret,
    Personal,
}

impl EntryKind {
    fn as_str(self) -> &'static str {
        match self {
            EntryKind::Secret => "secret",
            EntryKind::Personal => "personal",
        }
    }
}

/// Where the age identity comes from. Never the repo, never an env var (D-1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeySource {
    /// OS keychain entry (Windows Credential Manager, macOS Keychain, Secret Service).
    Keychain { service: String, account: String },
    /// An age identity file outside the repo (e.g. `%APPDATA%/sheetbrowse/vault.key`).
    IdentityFile(PathBuf),
}

/// Default vault path (D-101): `SB_VAULT_PATH` if set, else
/// `<config_dir>/sheetbrowse/vault.toml.age`. The path is not secret; the key is never
/// read from the environment.
pub fn default_path() -> Result<PathBuf, VaultError> {
    if let Some(p) = std::env::var_os("SB_VAULT_PATH").filter(|p| !p.is_empty()) {
        return Ok(PathBuf::from(p));
    }
    dirs::config_dir()
        .map(|d| d.join("sheetbrowse").join("vault.toml.age"))
        .ok_or_else(|| VaultError::Key("no config directory for the default vault path".into()))
}

/// Default key source (D-101): OS keychain, service `sheetbrowse`, account `vault`.
pub fn default_key() -> KeySource {
    KeySource::Keychain {
        service: "sheetbrowse".into(),
        account: "vault".into(),
    }
}

/// A resolved template, alive only for the duration of a sink call.
/// No `Clone`, no `Display`, no `Serialize`; `Debug` prints the unresolved template.
/// The buffer is zeroed on drop (`zeroize`).
pub struct Resolved<'a> {
    template: &'a str,
    value: Zeroizing<String>,
}

impl Resolved<'_> {
    /// The resolved string. Use it only to build the sink's payload.
    pub fn expose(&self) -> &str {
        &self.value
    }
}

impl fmt::Debug for Resolved<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Resolved({:?})", self.template)
    }
}

/// One vault entry. Deliberately no `Debug`, `Clone` or `Serialize`.
struct Entry {
    value: Zeroizing<String>,
    kind: EntryKind,
}

fn clone_entries(entries: &BTreeMap<String, Entry>) -> BTreeMap<String, Entry> {
    entries
        .iter()
        .map(|(k, e)| {
            (
                k.clone(),
                Entry {
                    value: e.value.clone(),
                    kind: e.kind,
                },
            )
        })
        .collect()
}

/// Where a file-backed vault lives; `None` for [`Vault::in_memory`].
struct Backing {
    path: PathBuf,
    key: KeySource,
}

/// The vault (H-2): `vault.toml.age` decrypted with a key from [`KeySource`].
pub struct Vault {
    // Values are reachable only through `resolve_into`; `Debug` prints nothing of them.
    entries: BTreeMap<String, Entry>,
    backing: Option<Backing>,
    scrubber: Arc<Scrubber>,
    /// `set` checks every entry ([`Check::All`]): a vault from `open`. An `in_memory`
    /// vault checks only the entry being set (as does a [`VaultEditor`]).
    strict: bool,
}

impl fmt::Debug for Vault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Vault(..)")
    }
}

impl Vault {
    /// Opens and decrypts `path` (`vault.toml.age`).
    /// A missing file opens as an empty vault without touching the key (D-101).
    /// A file holding a value under [`MIN_VALUE_CHARS`] characters is `ValueTooShort`, one
    /// holding a value that occurs inside a vault literal or a withheld marker is
    /// `ValueInLiteral` (both name
    /// the entry and tell the user to run `harness vault set <name>`), and a scrubber that
    /// cannot be built is `ScrubUnavailable` (D-118).
    pub fn open(path: &Path, key: &KeySource) -> Result<Vault, VaultError> {
        let entries = read_entries(path, key)?;
        check_entries(&entries, Check::All)?;
        Vault::with_backing(entries, path, key, true)
    }

    /// For `harness vault set` only (D-84): opens `path` without the D-118 value rules, so
    /// a file `open` refuses (an entry under the floor or inside a literal, written by an
    /// older build or by hand) can be repaired. It returns a [`VaultEditor`], not a
    /// `Vault`: the editor can set, remove and list entries but cannot resolve a value,
    /// scrub or be installed as the process-wide scrubber, because a vault that skipped
    /// the value rules may hold values scrub cannot see. `set` on the editor checks only
    /// the rules involving the entry it sets; the other entries are kept as they are, and `open` keeps
    /// refusing the file, naming the next entry to fix, until every one is fixed. A
    /// missing file opens as an empty editor without touching the key (D-101).
    pub fn open_for_set(path: &Path, key: &KeySource) -> Result<VaultEditor, VaultError> {
        let entries = read_entries(path, key)?;
        Ok(VaultEditor {
            entries,
            backing: Backing {
                path: path.to_path_buf(),
                key: key.clone(),
            },
        })
    }

    fn with_backing(
        entries: BTreeMap<String, Entry>,
        path: &Path,
        key: &KeySource,
        strict: bool,
    ) -> Result<Vault, VaultError> {
        let scrubber = Arc::new(Scrubber::try_build(&entries)?);
        Ok(Vault {
            entries,
            backing: Some(Backing {
                path: path.to_path_buf(),
                key: key.clone(),
            }),
            scrubber,
            strict,
        })
    }

    /// In-memory vault for tests. Fixtures must use a planted canary (`SB_VAULT_CANARY`), never a real value.
    /// A later entry with the same name replaces an earlier one. `set` on it writes no file.
    /// Unlike `open`, it accepts values under [`MIN_VALUE_CHARS`] (fixtures pick their
    /// values), and if the scrubber cannot be built every scrub withholds its whole input
    /// as `@vault(?) [output withheld: ...]` (fail closed).
    pub fn in_memory(entries: impl IntoIterator<Item = (String, String, EntryKind)>) -> Vault {
        let entries: BTreeMap<String, Entry> = entries
            .into_iter()
            .map(|(name, value, kind)| {
                (
                    name,
                    Entry {
                        value: Zeroizing::new(value),
                        kind,
                    },
                )
            })
            .collect();
        let scrubber = Arc::new(Scrubber::build(&entries));
        Vault {
            entries,
            backing: None,
            scrubber,
            strict: false,
        }
    }

    /// `harness vault set <name> [--personal]`: add or replace an entry and re-encrypt.
    ///
    /// `name` must be `[A-Za-z0-9_.-]+` so that `@vault(name)` can reference it; otherwise
    /// `BadReference(byte offset of the first bad char in name)`. A non-empty `value`
    /// under [`MIN_VALUE_CHARS`] characters is `ValueTooShort`, and a value that occurs
    /// inside a vault literal or a withheld marker (or a name whose literal holds another
    /// entry's value) is
    /// `ValueInLiteral`; both are refused before the file or key is touched when this
    /// handle's entries already show it (D-118). On a vault from `open` every entry of the
    /// re-read file is checked, except that the entry being set replaces a same-name entry
    /// that broke a rule; on an `in_memory` vault (and on a [`VaultEditor`]) only the
    /// entry being set is checked, so a file `open` refuses can be repaired one entry at a
    /// time. For a file-backed vault
    /// the whole read-modify-write runs under an exclusive lock on `<vault file>.lock`:
    /// the current file is re-read and decrypted (so entries set by other processes since
    /// `open` are kept and picked up), the change applied, and the file rewritten
    /// atomically (temp file + rename). When neither the vault file nor the identity
    /// exists yet, a fresh identity is generated and stored in the key source first,
    /// also under the lock. The new scrubber is built before the file is written: if it
    /// cannot be built, `ScrubUnavailable` and nothing changes. If the vault is the
    /// installed global one, the global scrub follows.
    pub fn set(&mut self, name: &str, value: &str, kind: EntryKind) -> Result<(), VaultError> {
        let check = if self.strict {
            Check::All
        } else {
            Check::Only(name)
        };
        let (next, scrubber) = set_entry(
            &self.entries,
            self.backing.as_ref(),
            check,
            name,
            value,
            kind,
        )?;
        let scrubber = Arc::new(scrubber);
        replace_global_if_installed(&self.scrubber, &scrubber);
        self.entries = next;
        self.scrubber = scrubber;
        Ok(())
    }

    /// Entry names and kinds; never values.
    pub fn names(&self) -> Vec<(String, EntryKind)> {
        self.entries
            .iter()
            .map(|(k, e)| (k.clone(), e.kind))
            .collect()
    }

    /// Resolves every `@vault(name)` literal in `template` and hands the result to `sink`.
    /// The resolved string exists only inside the call; `R` is the sink's payload (a
    /// header value, a serialized `Input.insertText` message) and must not be a plain
    /// copy of the value returned to a caller.
    pub fn resolve_into<R>(
        &self,
        template: &str,
        sink: impl FnOnce(&Resolved<'_>) -> R,
    ) -> Result<R, VaultError> {
        let refs = parse_refs(template)?;
        // Size the buffer exactly so it never reallocates (a realloc would leave an
        // unzeroed copy behind).
        let mut len = template.len();
        for r in &refs {
            let e = self
                .entries
                .get(r.name)
                .ok_or_else(|| VaultError::Unknown(r.name.to_owned()))?;
            len = len - (r.end - r.start) + e.value.len();
        }
        let mut value = Zeroizing::new(String::with_capacity(len));
        let mut at = 0;
        for r in &refs {
            value.push_str(&template[at..r.start]);
            value.push_str(&self.entries[r.name].value);
            at = r.end;
        }
        value.push_str(&template[at..]);
        let resolved = Resolved { template, value };
        Ok(sink(&resolved))
    }

    /// Replaces every vault value found in `s` with `@vault(name)`.
    pub fn scrub(&self, s: &str) -> String {
        self.scrubber.scrub(s)
    }

    /// Makes this vault the one the process-wide [`scrub`] uses (logs, panics, ledger).
    /// Also installs (once per process) a panic hook that scrubs the panic message.
    /// Call it after any other `std::panic::set_hook`: a hook set later replaces this
    /// one and would print dirty panic messages raw.
    pub fn install_global(&self) {
        *GLOBAL.write().unwrap_or_else(|e| e.into_inner()) = Some(self.scrubber.clone());
        PANIC_HOOK.call_once(install_panic_hook);
    }
}

/// The shared `set` of [`Vault`] and [`VaultEditor`]: validates `name` and `value`, then
/// (for a file-backed handle, under the lock) re-reads the file, applies the change,
/// checks per `check`, builds the scrubber and writes the file. Returns the new entries
/// and their scrubber; the caller swaps them in.
fn set_entry(
    entries: &BTreeMap<String, Entry>,
    backing: Option<&Backing>,
    check: Check<'_>,
    name: &str,
    value: &str,
    kind: EntryKind,
) -> Result<(BTreeMap<String, Entry>, Scrubber), VaultError> {
    if let Some(bad) = invalid_name_at(name) {
        return Err(VaultError::BadReference(bad));
    }
    check_value(name, value)?;
    let entry = || Entry {
        value: Zeroizing::new(value.to_owned()),
        kind,
    };
    // Against this handle's entries first, before any lock, file or key is touched.
    let mut next = clone_entries(entries);
    next.insert(name.to_owned(), entry());
    check_entries(&next, Check::Only(name))?;
    match backing {
        None => {
            let scrubber = Scrubber::try_build(&next)?;
            Ok((next, scrubber))
        }
        Some(backing) => {
            drop(next);
            let lock = lock_exclusive(&backing.path)?;
            let (identity, mut next) = current_locked(backing, entries)?;
            next.insert(name.to_owned(), entry());
            check_entries(&next, check)?;
            let scrubber = Scrubber::try_build(&next)?;
            let ciphertext = encrypt_entries(&identity, &next)?;
            write_atomic(&backing.path, &ciphertext)?;
            // Closing the handle releases the lock.
            drop(lock);
            Ok((next, scrubber))
        }
    }
}

/// The handle [`Vault::open_for_set`] returns (D-84, I-4): edits a vault file that
/// [`Vault::open`] may refuse, for `harness vault set` only. It has `set`, `remove`,
/// `names` and `save`, and nothing that reads a value back: no `resolve_into`, no
/// `scrub`, no `install_global`, no `Deref` to or conversion into a [`Vault`]. A vault
/// opened without the D-118 value rules may hold values scrub cannot see (short ones,
/// ones inside a literal), so this type can neither resolve them nor become the
/// process-wide scrubber. `Debug` prints nothing of the entries.
///
/// `set` and `remove` write through: each runs its whole read-modify-write under the
/// exclusive `<vault file>.lock`, as [`Vault::set`] does, so concurrent writers never
/// drop each other's entries. `save` closes the editor after checking the file decrypts.
///
/// ```no_run
/// use harness_core::vault::{EntryKind, KeySource, Vault, VaultEditor, VaultError};
/// # fn f(path: &std::path::Path, key: &KeySource) -> Result<(), VaultError> {
/// let mut editor: VaultEditor = Vault::open_for_set(path, key)?;
/// editor.set("site_pw", "a value of 4 or more chars", EntryKind::Secret)?;
/// let _names: Vec<(String, EntryKind)> = editor.names();
/// editor.remove("old_entry")?;
/// editor.save()?;
/// # Ok(()) }
/// ```
///
/// It cannot resolve a value:
///
/// ```compile_fail,E0599
/// # use harness_core::vault::{KeySource, Vault};
/// # fn f(path: &std::path::Path, key: &KeySource) {
/// let editor = Vault::open_for_set(path, key).unwrap();
/// let _ = editor.resolve_into("@vault(k)", |r| r.expose().len());
/// # }
/// ```
///
/// It cannot be installed as the process-wide scrubber:
///
/// ```compile_fail,E0599
/// # use harness_core::vault::{KeySource, Vault};
/// # fn f(path: &std::path::Path, key: &KeySource) {
/// let editor = Vault::open_for_set(path, key).unwrap();
/// editor.install_global();
/// # }
/// ```
///
/// It cannot scrub:
///
/// ```compile_fail,E0599
/// # use harness_core::vault::{KeySource, Vault};
/// # fn f(path: &std::path::Path, key: &KeySource) {
/// let editor = Vault::open_for_set(path, key).unwrap();
/// let _ = editor.scrub("text");
/// # }
/// ```
///
/// It is not a `Vault` and does not deref to one:
///
/// ```compile_fail,E0308
/// # use harness_core::vault::{KeySource, Vault};
/// # fn f(path: &std::path::Path, key: &KeySource) {
/// let editor = Vault::open_for_set(path, key).unwrap();
/// let _: &Vault = &editor;
/// # }
/// ```
pub struct VaultEditor {
    // Values are kept only for the checks in `set` and are never handed out.
    entries: BTreeMap<String, Entry>,
    backing: Backing,
}

impl fmt::Debug for VaultEditor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("VaultEditor(..)")
    }
}

impl VaultEditor {
    /// Adds or replaces an entry and re-encrypts the file, as [`Vault::set`] does, except
    /// that only the entry being set is checked against the D-118 value rules: its own
    /// value (`ValueTooShort`, `ValueInLiteral`) and every value against its
    /// `@vault(name)`. Problems in other entries are left for `open` to report, so a
    /// refused file can be repaired one entry at a time. `ScrubUnavailable` writes nothing.
    pub fn set(&mut self, name: &str, value: &str, kind: EntryKind) -> Result<(), VaultError> {
        let (next, _scrubber) = set_entry(
            &self.entries,
            Some(&self.backing),
            Check::Only(name),
            name,
            value,
            kind,
        )?;
        self.entries = next;
        Ok(())
    }

    /// Removes entry `name` from the file under the lock and re-encrypts it. A name the
    /// file does not hold (or a missing file) is `Unknown(name)`, and nothing is written
    /// and no key is generated. The name is not checked against `[A-Za-z0-9_.-]+`, so an
    /// entry written by hand under a bad name can be removed. `ScrubUnavailable` writes
    /// nothing.
    pub fn remove(&mut self, name: &str) -> Result<(), VaultError> {
        let unknown = || VaultError::Unknown(name.to_owned());
        let path = &self.backing.path;
        if !path.try_exists()? {
            return Err(unknown());
        }
        let lock = lock_exclusive(path)?;
        let ciphertext = match std::fs::read(path) {
            Ok(c) => c,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(unknown()),
            Err(e) => return Err(VaultError::Io(e)),
        };
        let identity = load_identity(&self.backing.key)?.ok_or_else(|| {
            VaultError::Key("vault file exists but its age identity is missing".into())
        })?;
        let mut next = decrypt_entries(&identity, &ciphertext)?;
        if next.remove(name).is_none() {
            return Err(unknown());
        }
        // Removing an entry cannot break a D-118 rule; the scrubber is built only so that,
        // as with `set`, a file no scrubber could cover is never written.
        Scrubber::try_build(&next)?;
        let ciphertext = encrypt_entries(&identity, &next)?;
        write_atomic(path, &ciphertext)?;
        // Closing the handle releases the lock.
        drop(lock);
        self.entries = next;
        Ok(())
    }

    /// Entry names and kinds as of the last `open_for_set`, `set` or `remove`; never values.
    pub fn names(&self) -> Vec<(String, EntryKind)> {
        self.entries
            .iter()
            .map(|(k, e)| (k.clone(), e.kind))
            .collect()
    }

    /// Closes the editor. `set` and `remove` have already written the file (write-through),
    /// so `save` writes nothing: it takes the lock and checks that the file on disk
    /// decrypts with the key, so a caller that wants confirmation gets an error instead of
    /// a silent bad write. A missing file is fine only when the editor holds no entries
    /// (nothing was ever written); otherwise it is `Io(NotFound)`. The existence check and
    /// the decrypt both happen under the lock, so a file deleted while `save` waits for
    /// the lock is reported, not passed. The values held in memory are zeroed on drop.
    pub fn save(self) -> Result<(), VaultError> {
        let path = &self.backing.path;
        // An empty editor on a missing file is ok whatever happens next, so it returns
        // without creating the lock file (or the directory) for nothing.
        if self.entries.is_empty() && !path.try_exists()? {
            return Ok(());
        }
        #[cfg(test)]
        tests::before_save_lock(path);
        let lock = lock_exclusive(path)?;
        let result = match std::fs::read(path) {
            Ok(ciphertext) => load_identity(&self.backing.key)?
                .ok_or_else(|| {
                    VaultError::Key("vault file exists but its age identity is missing".into())
                })
                .and_then(|identity| decrypt_entries(&identity, &ciphertext))
                .map(drop),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound && self.entries.is_empty() => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(VaultError::Io(
                std::io::Error::new(std::io::ErrorKind::NotFound, "vault file is missing"),
            )),
            Err(e) => Err(VaultError::Io(e)),
        };
        // Closing the handle releases the lock.
        drop(lock);
        result
    }
}

/// `core::scrub(s)` (H-2): scrub with the process-wide vault installed by
/// [`Vault::install_global`]. Before a vault is installed it returns `s` unchanged.
pub fn scrub(s: &str) -> String {
    match global_scrubber() {
        Some(sc) => sc.scrub(s),
        None => s.to_owned(),
    }
}

/// The `@vault(name)` literals in `s`, in order, by name. Used by classifiers and the
/// toolforge invariant "a repair cannot add a vault key" (G-4).
/// Every occurrence is listed (duplicates included).
pub fn vault_refs(s: &str) -> Result<Vec<&str>, VaultError> {
    Ok(parse_refs(s)?.into_iter().map(|r| r.name).collect())
}

/// The literal a value is shown as everywhere outside a sink.
pub fn vault_literal(name: &str) -> String {
    format!("@vault({name})")
}

// ---------------------------------------------------------------------------------------
// References

const REF_OPEN: &str = "@vault(";

struct Ref<'a> {
    /// Byte range of the whole `@vault(name)` literal.
    start: usize,
    end: usize,
    name: &'a str,
}

fn is_name_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.')
}

fn invalid_name_at(name: &str) -> Option<usize> {
    if name.is_empty() {
        return Some(0);
    }
    name.bytes().position(|b| !is_name_byte(b))
}

/// Every `@vault(name)` in `s`. A `@vault(` not followed by `name)` is
/// `BadReference(byte offset of the '@')`.
fn parse_refs(s: &str) -> Result<Vec<Ref<'_>>, VaultError> {
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(i) = s[from..].find(REF_OPEN) {
        let start = from + i;
        let name_start = start + REF_OPEN.len();
        let name_len = s.as_bytes()[name_start..]
            .iter()
            .position(|&b| !is_name_byte(b))
            .unwrap_or(s.len() - name_start);
        let close = name_start + name_len;
        if name_len == 0 || s.as_bytes().get(close) != Some(&b')') {
            return Err(VaultError::BadReference(start));
        }
        out.push(Ref {
            start,
            end: close + 1,
            name: &s[name_start..close],
        });
        from = close + 1;
    }
    Ok(out)
}

// ---------------------------------------------------------------------------------------
// Scrubbing (D-102)

/// What [`Scrubber::scrub`] returns when the automaton could not be built: the whole
/// string is withheld (fail closed) rather than passed through unscrubbed. Only an
/// [`Vault::in_memory`] vault can get here; `open` and `set` return `ScrubUnavailable`
/// instead (D-118). `@vault(?)` is itself a malformed reference, so `vault_refs` and
/// `resolve_into` on text holding the marker give `BadReference`: still fail closed.
const SCRUB_WITHHELD: &str = "@vault(?) [output withheld: vault scrubber unavailable]";

/// What [`Scrubber::scrub`] returns when [`MAX_SCRUB_PASSES`] passes did not reach a
/// fixed point (text built to make every pass change one more span): the whole string
/// is withheld, fail closed, with the same malformed `@vault(?)` reference (D-118).
const SCRUB_UNSETTLED: &str = "@vault(?) [output withheld: scrub did not settle]";

enum Matcher {
    /// No non-empty value: nothing to scrub.
    Empty,
    Ready(AhoCorasick),
    /// Building failed (size limits far beyond any vault); scrub fails closed.
    Failed,
}

struct Scrubber {
    matcher: Matcher,
    /// Replacement literal per pattern id.
    literals: Vec<String>,
    /// Names of the entries with a value, for [`literal_ranges`].
    names: BTreeSet<String>,
}

/// Byte ranges scrub never matches inside, sorted and disjoint: every `@vault(name)`
/// literal whose `name` is in `names` (what scrub itself writes), and the fixed
/// `@vault(` text of any other opener. The name inside an unknown or malformed literal
/// is not protected: a value someone else wrapped in `@vault(...)` is still scrubbed.
fn literal_ranges(s: &str, names: &BTreeSet<String>) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(i) = s[from..].find(REF_OPEN) {
        let start = from + i;
        let name_start = start + REF_OPEN.len();
        let name_len = s.as_bytes()[name_start..]
            .iter()
            .position(|&b| !is_name_byte(b))
            .unwrap_or(s.len() - name_start);
        let close = name_start + name_len;
        if name_len > 0
            && s.as_bytes().get(close) == Some(&b')')
            && names.contains(&s[name_start..close])
        {
            out.push((start, close + 1));
            from = close + 1;
        } else {
            out.push((start, name_start));
            from = name_start;
        }
    }
    out
}

/// True when `[start, end)` lies wholly inside one of `ranges` (sorted, disjoint).
fn inside_literal(ranges: &[(usize, usize)], start: usize, end: usize) -> bool {
    let i = ranges.partition_point(|&(s, _)| s <= start);
    i > 0 && end <= ranges[i - 1].1
}

/// A redacted byte range of the scrubbed text and the match that names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Span {
    start: usize,
    end: usize,
    /// End of the span's leading match (leftmost, then longest, then lowest pattern id).
    lead_end: usize,
    pid: usize,
}

impl Span {
    fn new(start: usize, end: usize, pid: usize) -> Span {
        Span {
            start,
            end,
            lead_end: end,
            pid,
        }
    }

    /// Unions `other` into `self` (the caller checked that they overlap or touch).
    fn absorb(&mut self, other: Span) {
        let other_leads = other.start < self.start
            || (other.start == self.start
                && (other.lead_end > self.lead_end
                    || (other.lead_end == self.lead_end && other.pid < self.pid)));
        if other_leads {
            self.start = other.start;
            self.lead_end = other.lead_end;
            self.pid = other.pid;
        }
        self.end = self.end.max(other.end);
    }
}

/// Merges matches into disjoint, sorted spans: overlapping or adjacent matches become
/// one span named by its leftmost match.
///
/// Overlapping aho-corasick matches arrive in order of end offset, so a new match can
/// only overlap or touch a suffix of the spans built so far: a stack merge, linear in
/// the number of matches. A match that breaks that order goes to `late` and is merged by
/// sorting (not expected; it keeps the result independent of the iterator's order).
fn merge_spans(matches: impl Iterator<Item = Span>) -> Vec<Span> {
    let mut spans: Vec<Span> = Vec::new();
    let mut late: Vec<Span> = Vec::new();
    let mut last_end = 0;
    for mut cur in matches {
        if cur.end < last_end {
            late.push(cur);
            continue;
        }
        last_end = cur.end;
        while let Some(&top) = spans.last() {
            if top.end < cur.start {
                break;
            }
            spans.pop();
            let mut merged = top;
            merged.absorb(cur);
            cur = merged;
        }
        spans.push(cur);
    }
    if late.is_empty() {
        return spans;
    }
    spans.append(&mut late);
    spans.sort_unstable_by_key(|sp| sp.start);
    let mut merged: Vec<Span> = Vec::with_capacity(spans.len());
    for sp in spans {
        match merged.last_mut() {
            Some(top) if sp.start <= top.end => top.absorb(sp),
            _ => merged.push(sp),
        }
    }
    merged
}

impl Scrubber {
    /// [`Scrubber::build`] for `open` and `set`: a failed build is an error (D-118).
    fn try_build(entries: &BTreeMap<String, Entry>) -> Result<Scrubber, VaultError> {
        Scrubber::build(entries).checked()
    }

    fn checked(self) -> Result<Scrubber, VaultError> {
        match self.matcher {
            Matcher::Failed => Err(VaultError::ScrubUnavailable),
            _ => Ok(self),
        }
    }

    fn build(entries: &BTreeMap<String, Entry>) -> Scrubber {
        let mut patterns: Vec<Zeroizing<String>> = Vec::new();
        let mut literals = Vec::new();
        let mut names = BTreeSet::new();
        for (name, e) in entries {
            if e.value.is_empty() {
                continue;
            }
            names.insert(name.clone());
            let literal = vault_literal(name);
            for sp in spellings(&e.value) {
                patterns.push(sp);
                literals.push(literal.clone());
            }
        }
        if patterns.is_empty() {
            return Scrubber {
                matcher: Matcher::Empty,
                literals,
                names,
            };
        }
        // Standard semantics for the overlapping search. ASCII case-insensitive because
        // sites echo values lower- or upper-cased (over-scrubbing is harmless); this also
        // makes upper- and lower-hex percent and `\u` escapes the same pattern.
        let matcher = match AhoCorasick::builder()
            .match_kind(MatchKind::Standard)
            .ascii_case_insensitive(true)
            .build(patterns.iter().map(|p| p.as_bytes()))
        {
            Ok(ac) => Matcher::Ready(ac),
            Err(_) => Matcher::Failed,
        };
        Scrubber {
            matcher,
            literals,
            names,
        }
    }

    /// Repeats [`Scrubber::scrub_pass`] until it changes nothing, so the output is a fixed
    /// point (D-118): a literal scrub wrote cannot complete another value that touches it
    /// (`<v>@` before `t`'s value gives `<v>@vault(t)`, which the next pass catches). Text
    /// with no value takes one pass. Each later pass swallows at least one literal or one
    /// character of text into a literal, so the loop ends; the cap only bounds the work an
    /// adversarial string can cause, and hitting it withholds the output (fail closed).
    fn scrub(&self, s: &str) -> String {
        let ac = match &self.matcher {
            Matcher::Empty => return s.to_owned(),
            Matcher::Failed => return SCRUB_WITHHELD.to_owned(),
            Matcher::Ready(ac) => ac,
        };
        let Some(mut cur) = self.scrub_pass(ac, s) else {
            return s.to_owned();
        };
        for _ in 1..MAX_SCRUB_PASSES {
            match self.scrub_pass(ac, &cur) {
                Some(next) if next != cur => cur = next,
                _ => return cur,
            }
        }
        SCRUB_UNSETTLED.to_owned()
    }

    /// One pass: redacts the union of all (overlapping) match spans, see
    /// [`merge_spans`]; `None` when nothing matched. A match wholly inside an existing
    /// entry's `@vault(name)` literal, or inside the `@vault(` of any opener, is not a
    /// value: the literal is what scrub itself writes, and it stays intact (D-118,
    /// [`literal_ranges`]). A match that only overlaps such a range is kept and widened
    /// to cover the whole range, so a value can never hide behind a literal and no
    /// fragment of a literal is left to break `vault_refs`.
    fn scrub_pass(&self, ac: &AhoCorasick, s: &str) -> Option<String> {
        let protected = literal_ranges(s, &self.names);
        let spans = merge_spans(ac.find_overlapping_iter(s).filter_map(|m| {
            if inside_literal(&protected, m.start(), m.end()) {
                return None;
            }
            let (start, end) = widen_over_literals(&protected, m.start(), m.end());
            Some(Span::new(start, end, m.pattern().as_usize()))
        }));
        if spans.is_empty() {
            return None;
        }
        let mut out = String::with_capacity(s.len());
        let mut at = 0;
        for sp in &spans {
            // Patterns are whole UTF-8 strings and ASCII case folding keeps char
            // boundaries, so every span starts and ends on one.
            out.push_str(s.get(at..sp.start).unwrap_or_default());
            out.push_str(&self.literals[sp.pid]);
            at = sp.end;
        }
        out.push_str(s.get(at..).unwrap_or_default());
        Some(out)
    }
}

/// Bound on [`Scrubber::scrub`]'s passes, counting the confirming pass that changes
/// nothing. Ordinary text takes 1, a value completed by a literal scrub wrote takes 2 or
/// 3; each pass changes at least one span. Hitting the bound withholds the output as
/// `@vault(?) [output withheld: scrub did not settle]` (D-118).
const MAX_SCRUB_PASSES: usize = 8;

/// `[start, end)` grown to cover every range of `ranges` (sorted, disjoint) it overlaps.
/// Ranges start at `@` and end after `(` or `)`, so the result stays on char boundaries.
fn widen_over_literals(ranges: &[(usize, usize)], start: usize, end: usize) -> (usize, usize) {
    let (mut lo, mut hi) = (start, end);
    let mut i = ranges.partition_point(|&(_, e)| e <= start);
    while let Some(&(rs, re)) = ranges.get(i) {
        if rs >= end {
            break;
        }
        lo = lo.min(rs);
        hi = hi.max(re);
        i += 1;
    }
    (lo, hi)
}

/// A percent-encode set over ASCII bytes; non-ASCII bytes are always encoded.
#[derive(Clone, Copy)]
enum PctSet {
    /// Encode every ASCII byte except alphanumerics and these.
    Keep(&'static [u8]),
    /// Encode C0 controls, DEL and these (the WHATWG URL standard's sets).
    Encode(&'static [u8]),
}

impl PctSet {
    fn encodes(self, b: u8) -> bool {
        if !b.is_ascii() {
            return true;
        }
        match self {
            PctSet::Keep(keep) => !(b.is_ascii_alphanumeric() || keep.contains(&b)),
            PctSet::Encode(enc) => b < 0x20 || b == 0x7f || enc.contains(&b),
        }
    }
}

/// Every percent-encoding a value is likely to be echoed in.
const PCT_SETS: &[PctSet] = &[
    // Every non-alphanumeric byte.
    PctSet::Keep(b""),
    // encodeURIComponent.
    PctSet::Keep(b"-_.!~*'()"),
    // encodeURI.
    PctSet::Keep(b";,/?:@&=+$-_.!~*'()#"),
    // application/x-www-form-urlencoded (URLSearchParams, HTML forms).
    PctSet::Keep(b"*-._"),
    // RFC 3986 unreserved (Python quote(safe=''), most HTTP clients).
    PctSet::Keep(b"-._~"),
    // Python quote() default.
    PctSet::Keep(b"-._~/"),
    // WHATWG fragment.
    PctSet::Encode(b" \"<>`"),
    // WHATWG query.
    PctSet::Encode(b" \"#<>"),
    // WHATWG special-query.
    PctSet::Encode(b" \"#<>'"),
    // WHATWG path, before and after `^` joined it.
    PctSet::Encode(b" \"#<>?`{}"),
    PctSet::Encode(b" \"#<>?^`{}"),
    // WHATWG userinfo. (The WHATWG component set equals encodeURIComponent's.)
    PctSet::Encode(b" \"#<>?^`{}/:;=@[\\]|"),
];

/// The second level of double encoding (a URL or query inside a `continue=` parameter):
/// the encoders that turn `%` into `%25`. The WHATWG sets above leave `%` alone, so they
/// add nothing at this level.
const OUTER_PCT_SETS: &[PctSet] = &[
    PctSet::Keep(b""),
    PctSet::Keep(b"-_.!~*'()"),
    PctSet::Keep(b"*-._"),
    PctSet::Keep(b"-._~"),
    PctSet::Keep(b"-._~/"),
];

/// The spellings of `v` that scrub replaces. Base texts: `v`, plus for a value with
/// non-ASCII characters its lower- and upper-cased forms (ASCII case is handled by the
/// case-insensitive automaton). For each base text: raw; JSON-escaped (serde style,
/// with `\/`, ASCII-only, HTML-safe as Go/Rails and System.Text.Json write it, and
/// escaped twice for JSON inside a JSON string); Rust `{:?}` without quotes,
/// `escape_debug` and `escape_default`; `{:?}` inside JSON, and `{:?}` or JSON inside
/// `{:?}` (an error string formatted twice); percent-encoded in every [`PCT_SETS`] set
/// (space as `%20` and as `+`), and each of those percent-encoded once more in every
/// [`OUTER_PCT_SETS`] set (double encoding, `%25XX`). Duplicates up to ASCII case are
/// dropped, so upper and lower hex are one pattern.
///
/// Each base text, and its JSON forms (serde and ASCII-only) where they differ, is also
/// spelled in base64 and base64url ([`push_base64_spellings`], B-151, D-77): HTTP Basic
/// `user:<value>`, JWT payloads, `data:` URLs.
///
/// Not covered (known limits, tracked with deadlines): input masks for phone, card and
/// date numbers, values split across DOM nodes or ellipsis-truncated, and mixed Unicode
/// case such as CSS `capitalize` (B-152, before step 3).
///
/// Every temporary is a `Zeroizing<String>` sized up front so it never reallocates.
fn spellings(v: &str) -> Vec<Zeroizing<String>> {
    let mut bases = vec![Zeroizing::new(v.to_owned())];
    if !v.is_ascii() {
        bases.push(case_map(v, |c, out| out.extend(c.to_lowercase())));
        bases.push(case_map(v, |c, out| out.extend(c.to_uppercase())));
        // std's to_lowercase also applies the final-sigma rule. It may reallocate
        // internally (unzeroed residue); the result itself is zeroized.
        bases.push(Zeroizing::new(v.to_lowercase()));
        dedup_ascii_ci(&mut bases);
    }
    let mut out = Vec::new();
    for t in &bases {
        push_spellings(t, &mut out);
        // base64 is case-sensitive over bytes: each case base is encoded on its own,
        // and the JSON forms only where they differ byte for byte from `t`.
        push_base64_spellings(t.as_bytes(), &mut out);
        let json = json_escape(t, false);
        if json.as_str() != t.as_str() {
            push_base64_spellings(json.as_bytes(), &mut out);
        }
        let ascii = json_escape(t, true);
        if ascii.as_str() != t.as_str() && ascii.as_str() != json.as_str() {
            push_base64_spellings(ascii.as_bytes(), &mut out);
        }
    }
    out.extend(bases);
    dedup_ascii_ci(&mut out);
    out
}

fn push_spellings(t: &str, out: &mut Vec<Zeroizing<String>>) {
    let json = json_escape(t, false);
    if t.contains('/') {
        out.push(json_escape_slash(&json));
    }
    out.push(json_escape(&json, false));
    out.push(debug_inner(&json));
    out.push(json);
    out.push(json_escape(t, true));
    out.push(json_escape_with(t, JSON_HTML));
    out.push(json_escape_with(t, JSON_DOTNET));
    let debug = debug_inner(t);
    out.push(json_escape(&debug, false));
    out.push(debug_inner(&debug));
    out.push(debug);
    out.push(collect_chars(t, t.escape_debug()));
    out.push(collect_chars(t, t.escape_default()));
    let mut single = Vec::new();
    for &set in PCT_SETS {
        single.push(pct_encode(t, set, false));
        if t.contains(' ') {
            single.push(pct_encode(t, set, true));
        }
    }
    dedup_ascii_ci(&mut single);
    for p in &single {
        if !p.contains('%') {
            continue;
        }
        for &outer in OUTER_PCT_SETS {
            out.push(pct_encode(p, outer, false));
        }
    }
    out.extend(single);
}

/// The shortest base64 spelling scrub adds, counted without `=` padding (B-151, D-77).
/// The trimmed core of a short value is a handful of characters that, matched ignoring
/// ASCII case, would hit unrelated base64 blobs and ordinary words: the D-118 floor for
/// the encoded form. A 4-character value gets no base64 spelling, a 6-character value
/// gets some alignments, and from 8 characters on every alignment is covered.
const MIN_BASE64_CHARS: usize = 8;

const BASE64_STD: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
const BASE64_URL: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

/// Appends base64 of `bytes` in `alphabet` to `out`, without padding; a final partial
/// character has its missing low bits zero, as at the end of a real encoding.
fn base64_into(bytes: &[u8], alphabet: &[u8; 64], out: &mut String) {
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        // 2, 3 or 4 characters carry the chunk's 8, 16 or 24 bits.
        for i in 0..=chunk.len() {
            let idx = (n >> (18 - 6 * i)) & 0x3f;
            out.push(char::from(alphabet[idx as usize]));
        }
    }
}

/// The base64 and base64url spellings of `t` inside a larger encoded blob (B-151, D-77).
///
/// The value may start at any byte offset of the blob, so for each alignment `k` in
/// 0, 1, 2 (offset mod 3) it is encoded after `k` placeholder bytes, and the leading
/// characters that carry placeholder bits (`ceil(8k / 6)`) are dropped. Three forms per
/// alignment and alphabet: the core, also cut before the last character that carries
/// bits of whatever follows the value (the value mid-blob); the tail, which keeps that
/// character with its fill bits zero (the value ends the blob, unpadded as in JWTs);
/// and the tail with its `=` padding. Overlapping matches are unioned, so where the
/// value ends the blob the tail forms widen the redaction over the last character and
/// the padding. Only the edge characters that depend on neighbouring bytes survive a
/// scrub, and they hold at most 4 bits of the value each. A form with fewer than
/// [`MIN_BASE64_CHARS`] characters (padding not counted) is skipped.
///
/// Cost: at most 18 patterns per encoded text (3 alignments x 2 alphabets x 3 forms),
/// fewer after the case-insensitive dedup (the alphabets agree where no character is
/// 62 or 63, and an aligned tail is the core), each about 4/3 of the text's length. The
/// automaton is built once per `open` or `set` and its size is linear in the total
/// pattern bytes, so this grows the build roughly in proportion and stays far below
/// aho-corasick's limits for any real vault.
fn push_base64_spellings(t: &[u8], out: &mut Vec<Zeroizing<String>>) {
    let n = t.len();
    for k in 0..3 {
        let mut blob = Zeroizing::new(Vec::with_capacity(k + n));
        blob.resize(k, 0);
        blob.extend_from_slice(t);
        let lead = (8 * k).div_ceil(6);
        let core_end = 8 * (k + n) / 6;
        let tail_end = (8 * (k + n)).div_ceil(6);
        let pad = (3 - (k + n) % 3) % 3;
        for alphabet in [BASE64_STD, BASE64_URL] {
            let mut enc = Zeroizing::new(String::with_capacity(tail_end + pad));
            base64_into(&blob, alphabet, &mut enc);
            if core_end >= lead + MIN_BASE64_CHARS {
                out.push(Zeroizing::new(enc[lead..core_end].to_owned()));
            }
            if tail_end >= lead + MIN_BASE64_CHARS {
                if tail_end > core_end {
                    out.push(Zeroizing::new(enc[lead..tail_end].to_owned()));
                }
                if pad > 0 {
                    for _ in 0..pad {
                        enc.push('=');
                    }
                    out.push(Zeroizing::new(enc[lead..].to_owned()));
                }
            }
        }
    }
}

/// Sorts and drops spellings equal up to ASCII case (the automaton ignores it anyway).
fn dedup_ascii_ci(v: &mut Vec<Zeroizing<String>>) {
    v.sort_unstable_by(|a, b| {
        a.bytes()
            .map(|x| x.to_ascii_lowercase())
            .cmp(b.bytes().map(|x| x.to_ascii_lowercase()))
    });
    v.dedup_by(|a, b| a.eq_ignore_ascii_case(b));
}

/// Char-wise case mapping into a buffer sized for the worst expansion (3 bytes per byte).
fn case_map(v: &str, map: impl Fn(char, &mut String)) -> Zeroizing<String> {
    let mut out = Zeroizing::new(String::with_capacity(v.len() * 3));
    for c in v.chars() {
        map(c, &mut out);
    }
    out
}

/// Percent-encodes `t` with upper-case hex. `plus` writes space as `+`.
fn pct_encode(t: &str, set: PctSet, plus: bool) -> Zeroizing<String> {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = Zeroizing::new(String::with_capacity(t.len() * 3));
    for b in t.bytes() {
        if plus && b == b' ' {
            out.push('+');
        } else if set.encodes(b) {
            out.push('%');
            out.push(char::from(HEX[usize::from(b >> 4)]));
            out.push(char::from(HEX[usize::from(b & 0xf)]));
        } else {
            out.push(char::from(b));
        }
    }
    out
}

/// How a JSON encoder escapes a string, beyond what every encoder escapes.
#[derive(Clone, Copy)]
struct JsonStyle {
    /// Every non-ASCII char as `\uXXXX` (Python `ensure_ascii`, System.Text.Json).
    ascii: bool,
    /// `<`, `>`, `&`, U+2028 and U+2029 as `\uXXXX` (Go `encoding/json`, Rails).
    html: bool,
    /// Also `"`, `'`, `+` and `` ` `` as `\u00XX` (System.Text.Json's default encoder).
    dotnet: bool,
}

const JSON_HTML: JsonStyle = JsonStyle {
    ascii: false,
    html: true,
    dotnet: false,
};
const JSON_DOTNET: JsonStyle = JsonStyle {
    ascii: true,
    html: true,
    dotnet: true,
};

/// JSON string escaping without the surrounding quotes. With `ascii`, every non-ASCII
/// char is `\uXXXX` (Python `ensure_ascii`); without, this is exactly `serde_json`.
fn json_escape(t: &str, ascii: bool) -> Zeroizing<String> {
    json_escape_with(
        t,
        JsonStyle {
            ascii,
            html: false,
            dotnet: false,
        },
    )
}

/// JSON string escaping in `style`, without the surrounding quotes.
fn json_escape_with(t: &str, style: JsonStyle) -> Zeroizing<String> {
    // Worst case 6 bytes out per byte in (`\u001f`; a 4-byte char is 12 as a pair).
    let mut out = Zeroizing::new(String::with_capacity(t.len() * 6));
    for c in t.chars() {
        let html = style.html && matches!(c, '<' | '>' | '&' | '\u{2028}' | '\u{2029}');
        let dotnet = style.dotnet && matches!(c, '"' | '\'' | '+' | '`');
        match c {
            _ if html || dotnet => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 || (style.ascii && !c.is_ascii()) => {
                let mut buf = [0u16; 2];
                for unit in c.encode_utf16(&mut buf) {
                    let _ = write!(out, "\\u{unit:04x}");
                }
            }
            c => out.push(c),
        }
    }
    out
}

/// A JSON spelling with `/` escaped as `\/` (PHP, some Java encoders).
fn json_escape_slash(json: &str) -> Zeroizing<String> {
    let mut out = Zeroizing::new(String::with_capacity(json.len() * 2));
    for c in json.chars() {
        if c == '/' {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// `format!("{t:?}")` without the surrounding quotes (assert_eq!, unwrap, tracing `?`).
fn debug_inner(t: &str) -> Zeroizing<String> {
    // Worst case 6 bytes out per byte in (`\u{1f}`), plus the quotes.
    let mut out = Zeroizing::new(String::with_capacity(t.len() * 6 + 2));
    let _ = write!(out, "{t:?}");
    out.pop();
    if !out.is_empty() {
        out.remove(0);
    }
    out
}

/// Collects `str::escape_debug` / `escape_default` output (at most 6 bytes per byte in).
fn collect_chars(t: &str, chars: impl Iterator<Item = char>) -> Zeroizing<String> {
    let mut out = Zeroizing::new(String::with_capacity(t.len() * 6));
    out.extend(chars);
    out
}

// ---------------------------------------------------------------------------------------
// Process-wide scrub and panic hook

static GLOBAL: RwLock<Option<Arc<Scrubber>>> = RwLock::new(None);
static PANIC_HOOK: Once = Once::new();

fn global_scrubber() -> Option<Arc<Scrubber>> {
    GLOBAL.read().unwrap_or_else(|e| e.into_inner()).clone()
}

fn replace_global_if_installed(old: &Arc<Scrubber>, new: &Arc<Scrubber>) {
    let mut slot = GLOBAL.write().unwrap_or_else(|e| e.into_inner());
    if slot.as_ref().is_some_and(|cur| Arc::ptr_eq(cur, old)) {
        *slot = Some(new.clone());
    }
}

/// std cannot rewrite a panic payload, so when the message contains a vault value the
/// hook prints the scrubbed message itself (in the default hook's format) instead of
/// handing the raw one to the previous hook. Clean messages go to the previous hook.
fn install_panic_hook() {
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let payload = info.payload();
        let msg = payload
            .downcast_ref::<&str>()
            .copied()
            .or_else(|| payload.downcast_ref::<String>().map(String::as_str));
        let Some(msg) = msg else {
            return prev(info);
        };
        let scrubbed = scrub(msg);
        if scrubbed == msg {
            return prev(info);
        }
        let thread = std::thread::current();
        let name = thread.name().unwrap_or("<unnamed>");
        let at = info
            .location()
            .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
            .unwrap_or_default();
        let bt = std::backtrace::Backtrace::capture();
        let mut text = format!("\nthread '{name}' panicked at {at}:\n{scrubbed}");
        if bt.status() == std::backtrace::BacktraceStatus::Captured {
            let _ = write!(text, "\nstack backtrace:\n{bt}");
        }
        // A raw, locked stderr write whose error is ignored: `eprint!` panics when
        // stderr is broken, and a panic inside the panic hook aborts the process. (It
        // also bypasses libtest's output capture; the text is scrubbed either way.)
        let _ = writeln!(std::io::stderr().lock(), "{text}");
    }));
}

// ---------------------------------------------------------------------------------------
// age file + key source (D-101)

#[derive(serde::Deserialize)]
struct FileEntry {
    value: String,
    kind: EntryKind,
}

fn decrypt_entries(
    identity: &age::x25519::Identity,
    ciphertext: &[u8],
) -> Result<BTreeMap<String, Entry>, VaultError> {
    let decryptor = age::Decryptor::new_buffered(ciphertext).map_err(|_| VaultError::Decrypt)?;
    let mut reader = decryptor
        .decrypt(std::iter::once(identity as &dyn age::Identity))
        .map_err(|_| VaultError::Decrypt)?;
    // The plaintext is shorter than the ciphertext (header and tags), so a buffer of the
    // ciphertext's length never has to grow (growth would leave an unzeroed copy).
    let mut plain = Zeroizing::new(vec![0u8; ciphertext.len()]);
    let mut len = 0;
    loop {
        if len == plain.len() {
            // Impossible for a well-formed age file; refuse rather than grow.
            return Err(VaultError::Decrypt);
        }
        match reader.read(&mut plain[len..]) {
            Ok(0) => break,
            Ok(n) => len += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => return Err(VaultError::Decrypt),
        }
    }
    let text = std::str::from_utf8(&plain[..len]).map_err(|_| VaultError::Decrypt)?;
    // The parse error is dropped unread: its message can quote the offending line.
    let raw: BTreeMap<String, FileEntry> = toml::from_str(text).map_err(|_| VaultError::Decrypt)?;
    let entries: BTreeMap<String, Entry> = raw
        .into_iter()
        .map(|(name, e)| {
            (
                name,
                Entry {
                    value: Zeroizing::new(e.value),
                    kind: e.kind,
                },
            )
        })
        .collect();
    // A file written by hand or by an older build may break a D-118 value rule; the
    // callers decide (`open` refuses it, `set` checks per its mode, `open_for_set` repairs).
    Ok(entries)
}

/// Serializes the entries as TOML by hand into one `Zeroizing` buffer sized for the
/// worst case, so no intermediate copy of a value is left unzeroed. One line per entry:
/// `"name" = { value = "...", kind = "secret" }` (names quoted: `.` is a TOML dotted key).
fn encode_toml(entries: &BTreeMap<String, Entry>) -> Zeroizing<String> {
    // Escapes are at most 6 bytes per byte; the fixed text per line is under 48 bytes.
    let cap: usize = entries
        .iter()
        .map(|(k, e)| 6 * k.len() + 6 * e.value.len() + 48)
        .sum();
    let mut out = Zeroizing::new(String::with_capacity(cap));
    for (name, e) in entries {
        out.push('"');
        toml_escape_into(name, &mut out);
        out.push_str("\" = { value = \"");
        toml_escape_into(&e.value, &mut out);
        out.push_str("\", kind = \"");
        out.push_str(e.kind.as_str());
        out.push_str("\" }\n");
    }
    out
}

/// TOML basic-string escaping (control characters and DEL must be escaped).
fn toml_escape_into(s: &str, out: &mut String) {
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{8}' => out.push_str("\\b"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\u{c}' => out.push_str("\\f"),
            '\r' => out.push_str("\\r"),
            c if (c as u32) < 0x20 || c == '\u{7f}' => {
                let _ = write!(out, "\\u{:04X}", c as u32);
            }
            c => out.push(c),
        }
    }
}

fn encrypt_entries(
    identity: &age::x25519::Identity,
    entries: &BTreeMap<String, Entry>,
) -> Result<Vec<u8>, VaultError> {
    let text = encode_toml(entries);
    age::encrypt(&identity.to_public(), text.as_bytes())
        .map_err(|_| VaultError::Key("age encryption failed".into()))
}

/// The decrypted entries of `path`, unchecked; a missing file is an empty map and does
/// not touch the key (D-101).
fn read_entries(path: &Path, key: &KeySource) -> Result<BTreeMap<String, Entry>, VaultError> {
    match std::fs::read(path) {
        Ok(ciphertext) => {
            let identity = load_identity(key)?
                .ok_or_else(|| VaultError::Key("no age identity for this vault".into()))?;
            decrypt_entries(&identity, &ciphertext)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(BTreeMap::new()),
        Err(e) => Err(VaultError::Io(e)),
    }
}

/// Opens (creating if needed) `<path>.lock` and takes an exclusive OS lock on it,
/// blocking until other `set`s release theirs. Dropping the file releases the lock. The
/// lock file is left in place: deleting it would let two lockers hold different files.
fn lock_exclusive(path: &Path) -> Result<std::fs::File, VaultError> {
    let dir = parent_dir(path);
    std::fs::create_dir_all(dir)?;
    let mut name = path
        .file_name()
        .map(|n| n.to_os_string())
        .unwrap_or_else(|| "vault".into());
    name.push(".lock");
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(dir.join(name))?;
    file.lock()?;
    Ok(file)
}

/// Under the `set` lock: the identity and the entries currently on disk. With no vault
/// file, the entries are `fallback` (this handle's) and the identity is loaded, or
/// generated and stored when the key source has none.
fn current_locked(
    backing: &Backing,
    fallback: &BTreeMap<String, Entry>,
) -> Result<(age::x25519::Identity, BTreeMap<String, Entry>), VaultError> {
    let identity = load_identity(&backing.key)?;
    match std::fs::read(&backing.path) {
        Ok(ciphertext) => {
            // An existing file is never orphaned by a fresh key.
            let identity = identity.ok_or_else(|| {
                VaultError::Key("vault file exists but its age identity is missing".into())
            })?;
            let entries = decrypt_entries(&identity, &ciphertext)?;
            Ok((identity, entries))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let identity = match identity {
                Some(id) => id,
                None => generate_identity(&backing.key)?,
            };
            Ok((identity, clone_entries(fallback)))
        }
        Err(e) => Err(VaultError::Io(e)),
    }
}

/// The identity, or `None` when the key source holds none yet.
fn load_identity(key: &KeySource) -> Result<Option<age::x25519::Identity>, VaultError> {
    let text: Zeroizing<String> = match key {
        KeySource::Keychain { service, account } => {
            let entry = keyring::Entry::new(service, account).map_err(keychain_err)?;
            match entry.get_password() {
                Ok(s) => Zeroizing::new(s),
                Err(keyring::Error::NoEntry) => return Ok(None),
                Err(e) => return Err(keychain_err(e)),
            }
        }
        KeySource::IdentityFile(p) => match std::fs::read_to_string(p) {
            Ok(s) => Zeroizing::new(s),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(VaultError::Io(e)),
        },
    };
    parse_identity(&text).map(Some)
}

/// First non-comment line of an age identity file (or a bare identity string).
fn parse_identity(text: &str) -> Result<age::x25519::Identity, VaultError> {
    let line = text
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && !l.starts_with('#'))
        .ok_or_else(|| VaultError::Key("identity is empty".into()))?;
    line.parse::<age::x25519::Identity>()
        .map_err(|why| VaultError::Key(format!("identity is not an age x25519 key ({why})")))
}

/// Generates a fresh identity and stores it in the key source. Called only under the
/// `set` lock when neither the vault file nor an identity exists.
fn generate_identity(key: &KeySource) -> Result<age::x25519::Identity, VaultError> {
    let id = age::x25519::Identity::generate();
    let secret = id.to_string();
    match key {
        KeySource::Keychain { service, account } => {
            keyring::Entry::new(service, account)
                .and_then(|e| e.set_password(secret.expose_secret()))
                .map_err(keychain_err)?;
        }
        KeySource::IdentityFile(p) => {
            let body = Zeroizing::new(format!(
                "# sheetbrowse vault identity (age x25519). Keep outside the repo.\n# public key: {}\n{}\n",
                id.to_public(),
                secret.expose_secret()
            ));
            write_atomic(p, body.as_bytes())?;
        }
    }
    Ok(id)
}

/// Keychain errors without any secret bytes the error may carry.
fn keychain_err(e: keyring::Error) -> VaultError {
    let msg = match e {
        keyring::Error::NoEntry => "no identity in the keychain".to_owned(),
        keyring::Error::BadEncoding(_) | keyring::Error::BadDataFormat(..) => {
            "keychain entry is malformed".to_owned()
        }
        keyring::Error::NoStorageAccess(p) => format!("keychain not accessible: {p}"),
        keyring::Error::PlatformFailure(p) => format!("keychain failure: {p}"),
        keyring::Error::NoDefaultStore => "no OS keychain on this platform".to_owned(),
        _ => "keychain error".to_owned(),
    };
    VaultError::Key(msg)
}

fn parent_dir(path: &Path) -> &Path {
    path.parent()
        .filter(|d| !d.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

/// Write `bytes` to a temp file next to `path`, fsync, then rename over `path`.
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), VaultError> {
    static TMP_SEQ: AtomicUsize = AtomicUsize::new(0);
    let dir = parent_dir(path);
    std::fs::create_dir_all(dir)?;
    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "vault".into());
    // Unique per process and call, so two writers never share a temp file.
    let tmp = dir.join(format!(
        ".{file_name}.{}.{}.tmp",
        std::process::id(),
        TMP_SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let result = (|| {
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut f = opts.open(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        drop(f);
        std::fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result.map_err(VaultError::Io)
}

#[cfg(test)]
mod tests {
    use super::*;
    use percent_encoding::{utf8_percent_encode, AsciiSet, NON_ALPHANUMERIC};

    /// Every test value is the canary (never a realistic secret).
    fn canary() -> String {
        std::env::var("SB_VAULT_CANARY")
            .ok()
            .filter(|c| !c.is_empty())
            .unwrap_or_else(|| "canary-7f3a-not-a-secret".to_owned())
    }

    fn vault_with(entries: &[(&str, String)]) -> Vault {
        Vault::in_memory(
            entries
                .iter()
                .map(|(n, v)| ((*n).to_owned(), v.clone(), EntryKind::Secret)),
        )
    }

    /// encodeURIComponent keeps `A-Z a-z 0-9 - _ . ! ~ * ' ( )`.
    const URI_COMPONENT: &AsciiSet = &NON_ALPHANUMERIC
        .remove(b'-')
        .remove(b'_')
        .remove(b'.')
        .remove(b'!')
        .remove(b'~')
        .remove(b'*')
        .remove(b'\'')
        .remove(b'(')
        .remove(b')');

    /// application/x-www-form-urlencoded keeps `A-Z a-z 0-9 * - . _`.
    const FORM: &AsciiSet = &NON_ALPHANUMERIC
        .remove(b'*')
        .remove(b'-')
        .remove(b'.')
        .remove(b'_');

    /// `%2F` -> `%2f`.
    fn lower_hex_escapes(p: &str) -> String {
        let mut out = String::with_capacity(p.len());
        let mut hex_left = 0;
        for c in p.chars() {
            if hex_left > 0 {
                out.push(c.to_ascii_lowercase());
                hex_left -= 1;
            } else {
                if c == '%' {
                    hex_left = 2;
                }
                out.push(c);
            }
        }
        out
    }

    /// A unique temp dir removed on drop; never the user's real vault path.
    struct TempDir(PathBuf);
    impl TempDir {
        fn new() -> TempDir {
            static N: AtomicUsize = AtomicUsize::new(0);
            let p = std::env::temp_dir().join(format!(
                "sb-vault-test-{}-{}",
                std::process::id(),
                N.fetch_add(1, Ordering::Relaxed)
            ));
            let _ = std::fs::remove_dir_all(&p);
            std::fs::create_dir_all(&p).unwrap();
            TempDir(p)
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn resolve_into_sink() {
        let c = canary();
        let v = vault_with(&[("test", c.clone())]);
        let payload = v
            .resolve_into("user=@vault(test);", |r| r.expose() == format!("user={c};"))
            .unwrap();
        assert!(payload);
        let plain = v
            .resolve_into("no refs here", |r| r.expose().len())
            .unwrap();
        assert_eq!(plain, "no refs here".len());
    }

    #[test]
    fn resolve_unknown_name() {
        let v = vault_with(&[("test", canary())]);
        match v.resolve_into("@vault(test) @vault(nope)", |_| ()) {
            Err(VaultError::Unknown(n)) => assert_eq!(n, "nope"),
            other => panic!("expected Unknown, got {other:?}"),
        }
    }

    #[test]
    fn resolve_bad_reference() {
        let v = vault_with(&[("test", canary())]);
        for (t, at) in [
            ("ab @vault(test", 3),
            ("@vault()", 0),
            ("x@vault(te st)", 1),
            ("@vault(test) @vault(", 13),
        ] {
            match v.resolve_into(t, |_| ()) {
                Err(VaultError::BadReference(i)) => assert_eq!(i, at, "{t}"),
                other => panic!("expected BadReference for {t:?}, got {other:?}"),
            }
        }
    }

    #[test]
    fn vault_refs_in_order() {
        let refs = vault_refs("a @vault(b.x) c @vault(a_1)@vault(b.x) vault(z)").unwrap();
        assert_eq!(refs, vec!["b.x", "a_1", "b.x"]);
        assert!(vault_refs("plain").unwrap().is_empty());
        assert!(matches!(
            vault_refs("@vault(x"),
            Err(VaultError::BadReference(0))
        ));
    }

    #[test]
    fn scrub_raw_json_and_percent_forms() {
        let c = format!("{} \"q\\/é", canary());
        let v = vault_with(&[("test", c.clone())]);
        let json = serde_json::to_string(&serde_json::json!({ "v": c })).unwrap();
        let json_twice = serde_json::to_string(&json).unwrap();
        let ascii = json_escape(&c, true);
        let pct = utf8_percent_encode(&c, NON_ALPHANUMERIC).to_string();
        let comp = utf8_percent_encode(&c, URI_COMPONENT).to_string();
        let form = utf8_percent_encode(&c, FORM)
            .to_string()
            .replace("%20", "+");
        let lower = lower_hex_escapes(&pct);
        for s in [
            format!("raw {c} end"),
            json,
            json_twice,
            format!("[{}]", ascii.as_str()),
            format!("https://x.test/?a={pct}"),
            format!("?a={comp}"),
            format!("a={form}&b=1"),
            format!("?a={lower}"),
        ] {
            let out = v.scrub(&s);
            assert!(!out.contains(&canary()), "value survived");
            assert!(out.contains("@vault(test)"));
        }
        assert_eq!(v.scrub("nothing here"), "nothing here");
    }

    #[test]
    fn json_escape_matches_serde_json() {
        let c = canary();
        let s = format!("{c}\"\\/\n\r\t\u{8}\u{c}\u{1}\u{1f}\u{7f}\u{e9}\u{200b}\u{1f600}");
        let serde = serde_json::to_string(&s).unwrap();
        assert!(*json_escape(&s, false) == serde[1..serde.len() - 1]);
        let ascii = json_escape(&s, true);
        assert!(ascii.is_ascii());
        let back: String = serde_json::from_str(&format!("\"{}\"", ascii.as_str())).unwrap();
        assert!(back == s, "ASCII-only JSON does not decode to the value");
    }

    #[test]
    fn debug_inner_is_debug_without_quotes() {
        let s = format!("{}\u{1}\u{200b}e\u{301}\"'\\", canary());
        let full = format!("{s:?}");
        assert!(*debug_inner(&s) == full[1..full.len() - 1]);
    }

    use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD, URL_SAFE, URL_SAFE_NO_PAD};
    use base64::engine::GeneralPurpose;
    use base64::Engine as _;

    /// Both alphabets, padded and unpadded (B-151).
    const ENGINES: [&GeneralPurpose; 4] =
        [&STANDARD, &STANDARD_NO_PAD, &URL_SAFE, &URL_SAFE_NO_PAD];

    /// Asserts that `out` is `before`, then `enc` with one `@vault(name)` in place of
    /// everything but its edge characters, then `after`. The value is the `n` bytes at
    /// byte `off` of the blob `enc` encodes: only the characters that carry bits of the
    /// bytes before it (`ceil(8 off / 6)`) or after it may survive. Prints no value.
    #[allow(clippy::too_many_arguments)]
    fn assert_only_edges(
        what: &str,
        out: &str,
        before: &str,
        enc: &str,
        after: &str,
        name: &str,
        off: usize,
        n: usize,
    ) {
        assert!(
            out.len() >= before.len() + after.len()
                && out.starts_with(before)
                && out.ends_with(after),
            "{what}: the text around the blob changed"
        );
        let mid = &out[before.len()..out.len() - after.len()];
        let lit = vault_literal(name);
        let Some(i) = mid.find(&lit) else {
            panic!("{what}: no {lit} in the scrubbed blob (off {off})");
        };
        let (left, right) = (&mid[..i], &mid[i + lit.len()..]);
        assert!(
            enc.starts_with(left) && enc.ends_with(right),
            "{what}: the kept edges are not the blob's own (off {off})"
        );
        assert!(
            left.len() <= (8 * off).div_ceil(6),
            "{what}: {} characters kept before the literal (off {off})",
            left.len()
        );
        assert!(
            enc.len() - right.len() >= 8 * (off + n) / 6,
            "{what}: {} characters kept after the literal (off {off})",
            right.len()
        );
    }

    #[test]
    fn base64_into_matches_the_base64_crate() {
        let bytes: Vec<u8> = canary()
            .bytes()
            .chain([0x00, 0xfb, 0xff, 0x3e, 0x7f, 0x80])
            .collect();
        for len in 0..=bytes.len() {
            let b = &bytes[..len];
            for (alphabet, engine) in [(BASE64_STD, STANDARD_NO_PAD), (BASE64_URL, URL_SAFE_NO_PAD)]
            {
                let mut s = String::new();
                base64_into(b, alphabet, &mut s);
                assert!(s == engine.encode(b), "length {len}");
            }
        }
    }

    #[test]
    fn scrub_basic_auth_header() {
        let c = canary();
        let v = vault_with(&[("pw", c.clone())]);
        let creds = format!("user:{c}");
        let before = "Authorization: Basic ";
        for engine in ENGINES {
            let enc = engine.encode(&creds);
            let out = v.scrub(&format!("{before}{enc}"));
            // `user:` is 5 bytes: its bits fill the first 7 characters (ceil(40 / 6)), and
            // the value ends the blob, so the last character and the padding go too.
            assert!(
                out == format!("{before}{}@vault(pw)", &enc[..7]),
                "Basic credentials not fully scrubbed"
            );
        }
    }

    #[test]
    fn scrub_jwt_payload() {
        let c = canary();
        let header = URL_SAFE_NO_PAD.encode(r#"{"alg":"HS256","typ":"JWT"}"#);
        let sig = URL_SAFE_NO_PAD.encode([0xfb_u8; 32]);
        let before = format!("Bearer {header}.");
        let after = format!(".{sig}");
        // The second value is JSON-escaped in the payload, so only its JSON form's
        // base64 is in the token.
        for value in [c.clone(), format!("{c}\"/q")] {
            let v = vault_with(&[("pw", value.clone())]);
            let payload =
                serde_json::json!({ "sub": "user", "pw": value, "iat": 1_700_000_000 }).to_string();
            let quoted = serde_json::to_string(&value).unwrap();
            let inner = &quoted[1..quoted.len() - 1];
            let off = payload.find(inner).unwrap();
            let enc = URL_SAFE_NO_PAD.encode(&payload);
            let out = v.scrub(&format!("{before}{enc}{after}"));
            assert_only_edges("JWT", &out, &before, &enc, &after, "pw", off, inner.len());
        }
    }

    #[test]
    fn scrub_base64_at_every_alignment() {
        let c = canary();
        let prefix = [0xa5_u8, 0xff, 0x00, 0x7e, 0xfb, 0x3f];
        let suffix = [0xff_u8, 0xfe];
        let mut alphabets_differ = false;
        // `~~~???` puts characters 62 and 63, where the alphabets differ, in the value's
        // encoding at every alignment.
        for value in [c.clone(), format!("{c}~~~???")] {
            let v = vault_with(&[("c", value.clone())]);
            for off in 0..prefix.len() {
                for tail in 0..=suffix.len() {
                    let mut blob = prefix[..off].to_vec();
                    blob.extend_from_slice(value.as_bytes());
                    blob.extend_from_slice(&suffix[..tail]);
                    let encs = ENGINES.map(|e| e.encode(&blob));
                    alphabets_differ |= encs[0] != encs[2];
                    for enc in &encs {
                        let out = v.scrub(&format!("x {enc} y"));
                        assert_only_edges("aligned", &out, "x ", enc, " y", "c", off, value.len());
                    }
                }
            }
        }
        assert!(alphabets_differ, "no blob exercised the url alphabet");
    }

    #[test]
    fn base64_spellings_respect_the_floor() {
        let c = canary();
        for len in 1..=24 {
            let t: Vec<u8> = c.bytes().cycle().take(len).collect();
            let mut out = Vec::new();
            push_base64_spellings(&t, &mut out);
            assert!(
                out.iter()
                    .all(|p| p.trim_end_matches('=').len() >= MIN_BASE64_CHARS),
                "length {len}: a base64 spelling under the floor"
            );
            if len <= 5 {
                assert!(out.is_empty(), "length {len}: short value got base64");
            }
            if len >= 7 {
                // A core per alignment and alphabet.
                assert!(out.len() >= 6, "length {len}: an alignment is missing");
            }
        }
        // No over-scrubbing: the base64 of a 4-character value is left alone.
        let short = "q7Zk";
        let v = vault_with(&[("s", short.to_owned())]);
        for off in 0..3 {
            let mut blob = [0xa5_u8, 0xff][..off].to_vec();
            blob.extend_from_slice(short.as_bytes());
            blob.push(0xfe);
            for engine in ENGINES {
                let s = format!("x {} y", engine.encode(&blob));
                assert_eq!(v.scrub(&s), s);
            }
        }
    }

    #[test]
    fn merge_spans_any_order() {
        let sp = |s, e, p| Span::new(s, e, p);
        // In end order: overlap chain, then a separate span, then an adjacent one.
        let ordered = [sp(0, 5, 1), sp(3, 8, 0), sp(10, 12, 2), sp(12, 14, 3)];
        let want = vec![
            Span {
                start: 0,
                end: 8,
                lead_end: 5,
                pid: 1,
            },
            Span {
                start: 10,
                end: 14,
                lead_end: 12,
                pid: 2,
            },
        ];
        assert_eq!(merge_spans(ordered.into_iter()), want);
        // Same matches out of order take the `late` path and give the same spans.
        let shuffled = [sp(12, 14, 3), sp(3, 8, 0), sp(10, 12, 2), sp(0, 5, 1)];
        assert_eq!(merge_spans(shuffled.into_iter()), want);
        // A longer match at the same start leads; a long match bridging two spans
        // merges them.
        let bridge = [sp(2, 4, 0), sp(6, 7, 1), sp(2, 9, 2)];
        assert_eq!(
            merge_spans(bridge.into_iter()),
            vec![Span {
                start: 2,
                end: 9,
                lead_end: 9,
                pid: 2
            }]
        );
    }

    #[test]
    fn failed_build_withholds_output() {
        let failed = || Scrubber {
            matcher: Matcher::Failed,
            literals: Vec::new(),
            names: BTreeSet::new(),
        };
        // in_memory's backstop: everything withheld, and the marker is a malformed ref.
        assert_eq!(failed().scrub("anything"), SCRUB_WITHHELD);
        assert!(matches!(
            vault_refs(SCRUB_WITHHELD),
            Err(VaultError::BadReference(0))
        ));
        // open and set: a failed build is reported, never silently withheld.
        assert!(matches!(
            failed().checked(),
            Err(VaultError::ScrubUnavailable)
        ));
        let ok = Scrubber::try_build(&BTreeMap::new());
        assert!(ok.is_ok_and(|sc| matches!(sc.matcher, Matcher::Empty)));
    }

    #[test]
    fn scrub_pass_cap_withholds_unsettled_output() {
        // Every `xyz1` after the canary costs one pass: `)xyz1` (entry `k`) overlaps the
        // `)` of the literal written before it, is widened over that literal, and writes
        // `@vault(k)`, whose `)` completes the next repeat (review-T-05 round 4).
        let c = canary();
        let k = ")xyz1";
        let v = vault_with(&[("test", c.clone()), ("k", k.to_owned())]);
        let text = |repeats: usize| format!("{c}{}", "xyz1".repeat(repeats));
        // One pass for the canary, one per repeat and one confirming pass: the most
        // repeats that still settle within the cap.
        let settles = MAX_SCRUB_PASSES - 2;
        assert_eq!(v.scrub(&text(settles)), "@vault(k)");
        assert_eq!(v.scrub(&format!("<{}>", text(settles))), "<@vault(k)>");
        for repeats in [settles + 1, settles + 2, 64] {
            let out = v.scrub(&text(repeats));
            // Fail closed: the whole output is withheld, says why, and holds no value.
            assert_eq!(out, SCRUB_UNSETTLED);
            assert!(out.contains("did not settle"));
            assert!(!out.contains(c.as_str()) && !out.contains(k));
            assert!(matches!(vault_refs(&out), Err(VaultError::BadReference(0))));
            let mut sunk = false;
            assert!(matches!(
                v.resolve_into(&out, |_| sunk = true),
                Err(VaultError::BadReference(0))
            ));
            assert!(!sunk);
        }
        // The marker is a fixed point and differs from the failed-build marker.
        assert_eq!(v.scrub(SCRUB_UNSETTLED), SCRUB_UNSETTLED);
        assert_ne!(v.scrub(&text(settles + 1)), SCRUB_WITHHELD);
    }

    #[test]
    fn set_refuses_values_under_the_floor() {
        let c = canary();
        let mut v = vault_with(&[("test", c.clone())]);
        // 3 characters (8 bytes) is still under the floor: the floor counts characters.
        for short in ["a", "a1z", "\u{e9}\u{65e5}\u{672c}"] {
            match v.set("st", short, EntryKind::Personal) {
                Err(VaultError::ValueTooShort { name, min }) => {
                    assert_eq!(name, "st");
                    assert_eq!(min, MIN_VALUE_CHARS);
                }
                other => panic!("expected ValueTooShort, got {other:?}"),
            }
        }
        assert_eq!(v.names().len(), 1, "a refused set changed the vault");
        v.set("four", "a1z9", EntryKind::Personal).unwrap();
        v.set("blank", "", EntryKind::Personal).unwrap();
        assert_eq!(v.scrub("x a1z9 y"), "x @vault(four) y");
        // A bad name is reported before the value is looked at.
        assert!(matches!(
            v.set("bad name", "a", EntryKind::Secret),
            Err(VaultError::BadReference(3))
        ));
        let msg = VaultError::ValueTooShort {
            name: "st".into(),
            min: MIN_VALUE_CHARS,
        }
        .to_string();
        assert!(msg.contains("\"st\"") && msg.contains('4'), "{msg}");
    }

    #[test]
    fn open_refuses_a_file_holding_a_short_value() {
        let dir = TempDir::new();
        let path = dir.0.join("vault.toml.age");
        let key = KeySource::IdentityFile(dir.0.join("vault.key"));
        let mut v = Vault::open(&path, &key).unwrap();
        v.set("test", &canary(), EntryKind::Secret).unwrap();
        // Rewrite the file as a hand edit or an older build could have left it.
        let id = load_identity(&key).unwrap().unwrap();
        let mut entries = clone_entries(&v.entries);
        entries.insert(
            "st".into(),
            Entry {
                value: Zeroizing::new("a1z".into()),
                kind: EntryKind::Personal,
            },
        );
        write_atomic(&path, &encrypt_entries(&id, &entries).unwrap()).unwrap();
        assert!(matches!(
            Vault::open(&path, &key),
            Err(VaultError::ValueTooShort { name, .. }) if name == "st"
        ));
        // set re-reads the file under its lock and refuses it too, writing nothing.
        let before = std::fs::read(&path).unwrap();
        assert!(matches!(
            v.set("other", &canary(), EntryKind::Secret),
            Err(VaultError::ValueTooShort { .. })
        ));
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }

    #[test]
    fn short_values_never_touch_vault_literals() {
        // in_memory accepts values under the floor; literals must still survive them.
        let v = Vault::in_memory([
            ("x".to_owned(), "a".to_owned(), EntryKind::Personal),
            ("st".to_owned(), "ca".to_owned(), EntryKind::Personal),
            ("v".to_owned(), "vault".to_owned(), EntryKind::Personal),
            ("name".to_owned(), canary(), EntryKind::Secret),
        ]);
        let once = v.scrub("fill @vault(name) then @vault(st) for @vault(x)");
        assert_eq!(once, "fill @vault(name) then @vault(st) for @vault(x)");
        assert_eq!(vault_refs(&once).unwrap(), ["name", "st", "x"]);
        // Unknown names keep their `@vault(` intact; the name itself is still scrubbed.
        let unknown = v.scrub("@vault(zzz) @vault(cab)");
        assert_eq!(unknown, "@vault(zzz) @vault(@vault(st)b)");
        // Idempotent: scrubbing scrub output changes nothing.
        let s = format!("Call the cache in CA, {} vault", canary());
        let once = v.scrub(&s);
        assert!(!once.contains(&canary()));
        assert_eq!(v.scrub(&once), once);
        assert!(vault_refs(&once).is_ok());
        // A value that only overlaps a literal is still scrubbed (it cannot hide), and the
        // literal it overlaps goes with it, so no fragment `(name)` is left behind.
        let w = Vault::in_memory([
            ("k".to_owned(), "zz@vault".to_owned(), EntryKind::Secret),
            ("name".to_owned(), canary(), EntryKind::Secret),
        ]);
        assert_eq!(w.scrub("zz@vault(name)"), "@vault(k)");
    }

    #[test]
    fn widen_over_literals_covers_every_overlapped_range() {
        let r = [(2, 12), (13, 20), (20, 27)];
        assert_eq!(widen_over_literals(&r, 0, 2), (0, 2), "touching only");
        assert_eq!(widen_over_literals(&r, 0, 3), (0, 12));
        assert_eq!(widen_over_literals(&r, 11, 14), (2, 20));
        assert_eq!(widen_over_literals(&r, 19, 21), (13, 27));
        assert_eq!(widen_over_literals(&r, 27, 30), (27, 30));
        assert_eq!(widen_over_literals(&[], 1, 4), (1, 4));
    }

    #[test]
    fn scrub_is_a_fixed_point_when_a_literal_completes_a_value() {
        let c = canary();
        // Review round 3: `)xyz1` follows the literal scrub writes for `name`.
        let v = vault_with(&[("name", c.clone()), ("k", ")xyz1".to_owned())]);
        let once = v.scrub(&format!("{c}xyz1"));
        assert_eq!(once, "@vault(k)");
        assert_eq!(v.scrub(&once), once);
        assert_eq!(vault_refs(&once).unwrap(), ["k"]);
        // The same text arriving already half scrubbed.
        assert_eq!(v.scrub("@vault(name)xyz1"), "@vault(k)");
        // A value ending in `@` before another value, and one starting with `)` after it.
        let tail = "Zq7-tail-value";
        for (p, s) in [
            (format!("{c}@"), format!("{c}{tail}")),
            (format!("){c}"), format!("{tail}{c}")),
        ] {
            let v = vault_with(&[("p", p.clone()), ("t", tail.to_owned())]);
            let once = v.scrub(&s);
            assert!(!contains_ascii_ci(&once, &p), "a whole value survived");
            assert_eq!(once, "@vault(p)");
            assert_eq!(v.scrub(&once), once);
        }
        // Clean text takes one pass and comes back as is.
        assert_eq!(v.scrub("no value (here) @ all"), "no value (here) @ all");
    }

    #[test]
    fn set_refuses_values_inside_literals() {
        let c = canary();
        let mut v = vault_with(&[("test", c.clone())]);
        let in_literal = |r: Result<(), VaultError>, want: &str| match r {
            Err(VaultError::ValueInLiteral { name }) => assert_eq!(name, want),
            other => panic!("expected ValueInLiteral for {want}, got {other:?}"),
        };
        // Inside `@vault(`, in any ASCII case, and inside an entry's own or another's literal.
        in_literal(v.set("pw", "vault", EntryKind::Secret), "pw");
        in_literal(v.set("pw", "AULT(", EntryKind::Secret), "pw");
        in_literal(v.set("pw", "(test)", EntryKind::Secret), "pw");
        in_literal(
            v.set("zip_94110", "94110", EntryKind::Personal),
            "zip_94110",
        );
        // A non-ASCII value whose Unicode lower case is ASCII (KELVIN SIGN) hides too.
        in_literal(v.set("key1", "\u{212a}ey1", EntryKind::Secret), "key1");
        // A new name whose literal would hold an existing value names the value's entry.
        let mut z = vault_with(&[("zip", "94110".to_owned())]);
        in_literal(z.set("zip_94110", &c, EntryKind::Personal), "zip");
        assert_eq!(z.names().len(), 1, "a refused set changed the vault");
        assert_eq!(v.names().len(), 1, "a refused set changed the vault");
        // Values that only touch or overlap a literal are fine; they cannot hide.
        v.set("k", ")xyz1", EntryKind::Secret).unwrap();
        v.set("m", "vault(x", EntryKind::Secret).unwrap();
        let msg = VaultError::ValueInLiteral { name: "pw".into() }.to_string();
        assert!(
            msg.contains("\"pw\"") && msg.contains("harness vault set pw"),
            "{msg}"
        );
    }

    #[test]
    fn set_and_open_refuse_values_inside_the_withheld_markers() {
        // D-118 (3b), review-T-05 round 5: a value inside a marker would be scrubbed out of
        // the fail-closed output itself.
        let mut v = vault_with(&[("test", canary())]);
        for value in [
            "withheld",
            "OUTPUT WITHHELD",
            "put withh",
            "Scrubber Unavailable",
            "did not settle",
            "settle]",
            "[output",
        ] {
            match v.set("w", value, EntryKind::Personal) {
                Err(VaultError::ValueInLiteral { name }) => assert_eq!(name, "w", "{value:?}"),
                other => panic!("{value:?}: expected ValueInLiteral, got {other:?}"),
            }
        }
        assert_eq!(v.names().len(), 1, "a refused set changed the vault");
        // Words that only resemble the markers are fine, and the markers survive scrub.
        v.set("w", "settled", EntryKind::Personal).unwrap();
        v.set("u", "outputs", EntryKind::Personal).unwrap();
        for marker in [SCRUB_WITHHELD, SCRUB_UNSETTLED] {
            assert_eq!(v.scrub(marker), marker);
        }
        // open refuses a file holding one and names its entry.
        let dir = TempDir::new();
        let path = dir.0.join("vault.toml.age");
        let key = KeySource::IdentityFile(dir.0.join("vault.key"));
        Vault::open(&path, &key)
            .unwrap()
            .set("test", &canary(), EntryKind::Secret)
            .unwrap();
        write_raw(&path, &key, &[("test", &canary()), ("note", "Not Settle")]);
        match Vault::open(&path, &key) {
            Err(e @ VaultError::ValueInLiteral { .. }) => {
                assert!(e.to_string().contains("harness vault set note"), "{e}");
            }
            other => panic!("expected ValueInLiteral, got {other:?}"),
        }
    }

    /// Writes `entries` to `path` as an older build or a hand edit could have.
    fn write_raw(path: &Path, key: &KeySource, entries: &[(&str, &str)]) {
        let id = load_identity(key).unwrap().unwrap();
        let map: BTreeMap<String, Entry> = entries
            .iter()
            .map(|(n, v)| {
                (
                    (*n).to_owned(),
                    Entry {
                        value: Zeroizing::new((*v).to_owned()),
                        kind: EntryKind::Personal,
                    },
                )
            })
            .collect();
        write_atomic(path, &encrypt_entries(&id, &map).unwrap()).unwrap();
    }

    #[test]
    fn open_for_set_repairs_a_file_open_refuses() {
        let c = canary();
        let dir = TempDir::new();
        let path = dir.0.join("vault.toml.age");
        let key = KeySource::IdentityFile(dir.0.join("vault.key"));
        let mut strict = Vault::open(&path, &key).unwrap();
        strict.set("test", &c, EntryKind::Secret).unwrap();
        write_raw(&path, &key, &[("test", &c), ("st", "a1z"), ("v", "vault")]);
        // open names the first entry to fix and the command that fixes it.
        match Vault::open(&path, &key) {
            Err(e @ VaultError::ValueTooShort { .. }) => {
                assert!(e.to_string().contains("harness vault set st"), "{e}");
            }
            other => panic!("expected ValueTooShort, got {other:?}"),
        }
        // A handle from open still checks every entry of the re-read file.
        assert!(matches!(
            strict.set("other", &format!("{c}-o"), EntryKind::Secret),
            Err(VaultError::ValueTooShort { name, .. }) if name == "st"
        ));
        // open_for_set opens it as a VaultEditor (D-84); set fixes one entry while another
        // is still broken.
        let mut fix: VaultEditor = Vault::open_for_set(&path, &key).unwrap();
        fix.set("st", "a1z9", EntryKind::Personal).unwrap();
        match Vault::open(&path, &key) {
            Err(e @ VaultError::ValueInLiteral { .. }) => {
                assert!(e.to_string().contains("harness vault set v"), "{e}");
            }
            other => panic!("expected ValueInLiteral, got {other:?}"),
        }
        // The new value is still checked on the editor.
        assert!(matches!(
            fix.set("v", "a1", EntryKind::Personal),
            Err(VaultError::ValueTooShort { .. })
        ));
        assert!(matches!(
            fix.set("v", "Vault", EntryKind::Personal),
            Err(VaultError::ValueInLiteral { .. })
        ));
        // A handle from open may replace a same-name entry that breaks a rule.
        strict.set("v", "Zq7-v-fixed", EntryKind::Personal).unwrap();
        let ok = Vault::open(&path, &key).unwrap();
        assert_eq!(ok.names().len(), 3);
        assert_eq!(ok.scrub("x a1z9 y"), "x @vault(st) y");
        assert!(ok
            .resolve_into("@vault(test)", |r| r.expose() == c)
            .unwrap());
    }

    /// File names in `dir`, sorted (for "nothing was created" checks).
    fn file_names(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn editor_remove_drops_the_entry_from_the_file() {
        let c = canary();
        let keep = format!("{c}-keep");
        let dir = TempDir::new();
        let path = dir.0.join("vault.toml.age");
        let key = KeySource::IdentityFile(dir.0.join("vault.key"));
        Vault::open(&path, &key)
            .unwrap()
            .set("test", &c, EntryKind::Secret)
            .unwrap();
        // A file `open` refuses (a short entry): the editor can remove the broken entry.
        write_raw(&path, &key, &[("test", &c), ("keep", &keep), ("st", "a1z")]);
        assert!(matches!(
            Vault::open(&path, &key),
            Err(VaultError::ValueTooShort { name, .. }) if name == "st"
        ));
        let mut ed = Vault::open_for_set(&path, &key).unwrap();
        ed.remove("st").unwrap();
        ed.remove("test").unwrap();
        assert_eq!(ed.names(), vec![("keep".to_owned(), EntryKind::Personal)]);
        ed.save().unwrap();
        // Re-opened from disk: only `keep` is left, and the file opens again.
        let after = Vault::open(&path, &key).unwrap();
        assert_eq!(
            after.names(),
            vec![("keep".to_owned(), EntryKind::Personal)]
        );
        assert!(matches!(
            after.resolve_into("@vault(test)", |_| ()),
            Err(VaultError::Unknown(n)) if n == "test"
        ));
        assert!(
            after
                .resolve_into("@vault(keep)", |r| r.expose() == keep)
                .unwrap(),
            "remove lost the entry it was not asked to remove"
        );
        let ct = std::fs::read(&path).unwrap();
        assert!(
            !ct.windows(c.len()).any(|w| w == c.as_bytes()),
            "value stored in plaintext"
        );
    }

    #[test]
    fn editor_remove_of_a_missing_name_or_file_writes_nothing() {
        let c = canary();
        let dir = TempDir::new();
        let path = dir.0.join("vault.toml.age");
        let key = KeySource::IdentityFile(dir.0.join("vault.key"));
        // Missing file: Unknown, and no vault file, lock file or key is created.
        let mut ed = Vault::open_for_set(&path, &key).unwrap();
        assert!(ed.names().is_empty());
        assert!(matches!(
            ed.remove("test"),
            Err(VaultError::Unknown(n)) if n == "test"
        ));
        assert!(
            file_names(&dir.0).is_empty(),
            "remove on a missing vault file created a file"
        );
        // Existing file, name it does not hold: Unknown and the file is left byte for
        // byte (age encryption is randomized, so any rewrite would change the bytes).
        Vault::open(&path, &key)
            .unwrap()
            .set("test", &c, EntryKind::Secret)
            .unwrap();
        let mut ed = Vault::open_for_set(&path, &key).unwrap();
        let before = std::fs::read(&path).unwrap();
        assert!(matches!(
            ed.remove("nope"),
            Err(VaultError::Unknown(n)) if n == "nope"
        ));
        assert!(
            std::fs::read(&path).unwrap() == before,
            "remove of an unknown name rewrote the vault file"
        );
        assert_eq!(ed.names(), vec![("test".to_owned(), EntryKind::Secret)]);
        // File deleted since open_for_set: Unknown, and nothing is written back.
        std::fs::remove_file(&path).unwrap();
        assert!(matches!(
            ed.remove("test"),
            Err(VaultError::Unknown(n)) if n == "test"
        ));
        assert!(!path.exists(), "remove re-created a deleted vault file");
    }

    #[test]
    fn editor_save_checks_the_file_decrypts() {
        let c = canary();
        let dir = TempDir::new();
        let path = dir.0.join("vault.toml.age");
        let key = KeySource::IdentityFile(dir.0.join("vault.key"));
        // Nothing set on a missing file: ok, and no vault file, lock file or key appears.
        Vault::open_for_set(&path, &key).unwrap().save().unwrap();
        assert!(
            file_names(&dir.0).is_empty(),
            "save of an empty editor created a file"
        );
        // A good file: set wrote it, save confirms it.
        let mut ed = Vault::open_for_set(&path, &key).unwrap();
        ed.set("test", &c, EntryKind::Secret).unwrap();
        ed.save().unwrap();
        assert!(
            Vault::open(&path, &key)
                .unwrap()
                .resolve_into("@vault(test)", |r| r.expose() == c)
                .unwrap(),
            "the set did not reach the file"
        );
        // The file no longer decrypts: garbage, or a file encrypted to another key.
        let mut garbage = Vault::open_for_set(&path, &key).unwrap();
        garbage
            .set("other", &format!("{c}-o"), EntryKind::Secret)
            .unwrap();
        let foreign = Vault::open_for_set(&path, &key).unwrap();
        std::fs::write(&path, b"not an age file").unwrap();
        assert!(matches!(garbage.save(), Err(VaultError::Decrypt)));
        let other = TempDir::new();
        let other_key = KeySource::IdentityFile(other.0.join("k"));
        Vault::open(&other.0.join("v.age"), &other_key)
            .unwrap()
            .set("x", &c, EntryKind::Secret)
            .unwrap();
        std::fs::copy(other.0.join("v.age"), &path).unwrap();
        assert!(matches!(foreign.save(), Err(VaultError::Decrypt)));
        // Entries written, then the file is gone: an error, not a silent ok.
        let lost = Vault::open_for_set(&other.0.join("v.age"), &other_key).unwrap();
        std::fs::remove_file(other.0.join("v.age")).unwrap();
        assert!(matches!(
            lost.save(),
            Err(VaultError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound
        ));
    }

    thread_local! {
        /// Run by `VaultEditor::save` just before it takes the lock (this thread only).
        static BEFORE_SAVE_LOCK: std::cell::Cell<Option<fn(&Path)>> =
            const { std::cell::Cell::new(None) };
    }

    pub(super) fn before_save_lock(path: &Path) {
        if let Some(hook) = BEFORE_SAVE_LOCK.with(|h| h.get()) {
            hook(path);
        }
    }

    #[test]
    fn editor_save_reports_a_file_deleted_while_it_waits_for_the_lock() {
        let c = canary();
        let dir = TempDir::new();
        let path = dir.0.join("vault.toml.age");
        let key = KeySource::IdentityFile(dir.0.join("vault.key"));
        let mut ed = Vault::open_for_set(&path, &key).unwrap();
        ed.set("test", &c, EntryKind::Secret).unwrap();
        // Another process deletes the file after `save` started and before it holds the
        // lock: any look at the file taken before the lock is stale by then.
        let delete: fn(&Path) = |p| std::fs::remove_file(p).unwrap();
        BEFORE_SAVE_LOCK.with(|h| h.set(Some(delete)));
        let saved = ed.save();
        BEFORE_SAVE_LOCK.with(|h| h.set(None));
        assert!(!path.exists());
        assert!(
            matches!(
                &saved,
                Err(VaultError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound
            ),
            "save passed an editor holding entries whose file is gone"
        );
    }

    #[test]
    fn editor_remove_fails_closed_on_a_file_it_cannot_read() {
        let c = canary();
        let dir = TempDir::new();
        let path = dir.0.join("vault.toml.age");
        let key_file = dir.0.join("vault.key");
        let key = KeySource::IdentityFile(key_file.clone());
        Vault::open(&path, &key)
            .unwrap()
            .set("test", &c, EntryKind::Secret)
            .unwrap();
        let good = std::fs::read(&path).unwrap();
        let mut ed = Vault::open_for_set(&path, &key).unwrap();
        // Garbage bytes: Decrypt, and the file is left byte for byte.
        std::fs::write(&path, b"not an age file").unwrap();
        assert!(matches!(ed.remove("test"), Err(VaultError::Decrypt)));
        assert!(
            std::fs::read(&path).unwrap() == b"not an age file",
            "remove rewrote a vault file it could not decrypt"
        );
        // A file encrypted to another key: Decrypt, and the file is left byte for byte.
        let other = TempDir::new();
        let other_key = KeySource::IdentityFile(other.0.join("k"));
        Vault::open(&other.0.join("v.age"), &other_key)
            .unwrap()
            .set("test", &c, EntryKind::Secret)
            .unwrap();
        std::fs::copy(other.0.join("v.age"), &path).unwrap();
        let foreign = std::fs::read(&path).unwrap();
        assert!(matches!(ed.remove("test"), Err(VaultError::Decrypt)));
        assert!(
            std::fs::read(&path).unwrap() == foreign,
            "remove rewrote a vault file encrypted to another key"
        );
        // The file's identity is missing: Key, the file is left byte for byte and no new
        // key is generated (it would orphan the file).
        std::fs::write(&path, &good).unwrap();
        std::fs::remove_file(&key_file).unwrap();
        assert!(matches!(ed.remove("test"), Err(VaultError::Key(_))));
        assert!(
            std::fs::read(&path).unwrap() == good,
            "remove rewrote a vault file whose key is missing"
        );
        assert!(
            !key_file.exists(),
            "remove generated a key for an existing file"
        );
        // The editor still holds what it held.
        assert_eq!(ed.names(), vec![("test".to_owned(), EntryKind::Secret)]);
    }

    #[test]
    fn editor_debug_prints_no_value() {
        let c = canary();
        let dir = TempDir::new();
        let path = dir.0.join("vault.toml.age");
        let key = KeySource::IdentityFile(dir.0.join("vault.key"));
        let mut ed = Vault::open_for_set(&path, &key).unwrap();
        ed.set("test", &c, EntryKind::Secret).unwrap();
        let shown = format!("{ed:?} {:?}", Vault::open_for_set(&path, &key));
        // Neither assertion formats `shown`: on failure it would print the value.
        assert!(
            !shown.to_ascii_lowercase().contains(&c.to_ascii_lowercase()),
            "Debug of a VaultEditor holds a vault value"
        );
        assert!(
            shown == "VaultEditor(..) Ok(VaultEditor(..))",
            "Debug of a VaultEditor prints more than its type name"
        );
    }

    #[test]
    fn literal_ranges_and_inside() {
        let names: BTreeSet<String> = ["ab".to_owned()].into();
        let s = "x @vault(ab) @vault(cd) @vault( @vault(ab";
        let r = literal_ranges(s, &names);
        assert_eq!(r, vec![(2, 12), (13, 20), (24, 31), (32, 39)]);
        assert!(inside_literal(&r, 2, 12));
        assert!(inside_literal(&r, 4, 8));
        assert!(!inside_literal(&r, 1, 5));
        assert!(!inside_literal(&r, 10, 14));
        assert!(!inside_literal(&r, 20, 22), "name of an unknown literal");
        assert!(!inside_literal(&r, 0, 1));
    }

    #[test]
    fn html_safe_and_nested_json_debug_spellings() {
        let c = canary();
        let d = format!("{c} A&B <x> 'q' +1 \u{e9}\u{2028}\"\u{1}");
        let v = vault_with(&[("d", d.clone())]);
        // Go encoding/json and Rails: <, >, & and U+2028 as \u00XX /  .
        let go = format!("{c} A\\u0026B \\u003cx\\u003e 'q' +1 \u{e9}\\u2028\\\"\\u0001");
        // System.Text.Json default: also ' + " as \u00XX, every non-ASCII char escaped.
        let dotnet = format!(
            "{c} A\\u0026B \\u003Cx\\u003E \\u0027q\\u0027 \\u002B1 \\u00E9\\u2028\\u0022\\u0001"
        );
        let dbg = format!("{d:?}");
        let dbg_in_json = serde_json::to_string(&dbg).unwrap();
        let dbg_in_dbg = format!("{dbg:?}");
        let json = serde_json::to_string(&d).unwrap();
        let json_in_dbg = format!("{json:?}");
        for (label, s) in [
            ("go", format!("{{\"a\":\"{go}\"}}")),
            ("dotnet", format!("{{\"a\":\"{dotnet}\"}}")),
            ("debug in json", dbg_in_json),
            ("debug in debug", dbg_in_dbg),
            ("json in debug", json_in_dbg),
        ] {
            let out = v.scrub(&s);
            assert!(!out.contains(&c), "{label}: value survived");
            assert!(out.contains("@vault(d)"), "{label}: no literal");
        }
    }

    #[test]
    fn scrub_overlapping_picks_longest() {
        let c = canary();
        let long = format!("{c}-extended");
        let v = vault_with(&[("short", c.clone()), ("long", long.clone())]);
        assert_eq!(v.scrub(&format!("<{long}>")), "<@vault(long)>");
        assert_eq!(v.scrub(&format!("<{c}>")), "<@vault(short)>");
    }

    #[test]
    fn empty_value_is_not_scrubbed() {
        let v = vault_with(&[("empty", String::new())]);
        assert_eq!(v.scrub("abc"), "abc");
        assert_eq!(
            v.resolve_into("[@vault(empty)]", |r| r.expose().len())
                .unwrap(),
            2
        );
    }

    // The only test touching the process-wide slot, so before/after is deterministic.
    #[test]
    fn global_scrub_before_and_after_install() {
        let c = canary();
        let s = format!("log {c}");
        assert_eq!(scrub(&s), s);
        let mut v = vault_with(&[("test", c.clone())]);
        v.install_global();
        assert_eq!(scrub(&s), "log @vault(test)");
        // `set` on the installed vault keeps the global scrub current.
        let other = format!("{c}-second");
        v.set("other", &other, EntryKind::Personal).unwrap();
        assert_eq!(scrub(&other), "@vault(other)");
        // The hook's scrubbed output is checked in a child process by
        // tests/vault_accept_t05.rs row 7 and tests/vault_scrub_t05_fix.rs.
        let r = std::panic::catch_unwind(|| panic!("boom {}", canary()));
        assert!(r.is_err());
    }

    #[test]
    fn debug_shows_no_value() {
        let c = canary();
        let v = vault_with(&[("test", c.clone())]);
        assert!(!format!("{v:?}").contains(&c));
        v.resolve_into("@vault(test)", |r| {
            let d = format!("{r:?}");
            assert!(!d.contains(&c));
            assert!(d.contains("@vault(test)"));
        })
        .unwrap();
        let e = v.resolve_into("@vault(missing)", |_| ()).unwrap_err();
        assert!(!e.to_string().contains(&c));
    }

    #[test]
    fn names_never_values() {
        let mut v = vault_with(&[("b", canary())]);
        v.set("a", &canary(), EntryKind::Personal).unwrap();
        assert_eq!(
            v.names(),
            vec![
                ("a".to_owned(), EntryKind::Personal),
                ("b".to_owned(), EntryKind::Secret)
            ]
        );
        assert!(matches!(
            v.set("bad name", "x", EntryKind::Secret),
            Err(VaultError::BadReference(3))
        ));
    }

    #[test]
    fn age_round_trip_through_identity_file() {
        let dir = TempDir::new();
        let path = dir.0.join("vault.toml.age");
        let key = KeySource::IdentityFile(dir.0.join("keys").join("vault.key"));
        let c = canary();

        let mut v = Vault::open(&path, &key).unwrap();
        assert!(v.names().is_empty(), "missing file opens empty");
        v.set("test", &c, EntryKind::Secret).unwrap();
        v.set("name", &format!("{c} \"quoted\""), EntryKind::Personal)
            .unwrap();
        assert!(path.exists());

        let ciphertext = std::fs::read(&path).unwrap();
        let needle = c.as_bytes();
        assert!(
            !ciphertext.windows(needle.len()).any(|w| w == needle),
            "value stored in plaintext"
        );
        assert!(!dir.0.read_dir().unwrap().any(|e| e
            .unwrap()
            .file_name()
            .to_string_lossy()
            .ends_with(".tmp")));

        let v2 = Vault::open(&path, &key).unwrap();
        assert_eq!(
            v2.names(),
            vec![
                ("name".to_owned(), EntryKind::Personal),
                ("test".to_owned(), EntryKind::Secret)
            ]
        );
        let ok = v2
            .resolve_into("@vault(test)|@vault(name)", |r| {
                r.expose() == format!("{c}|{c} \"quoted\"")
            })
            .unwrap();
        assert!(ok);

        // Wrong identity: decrypt fails without echoing anything.
        let other = TempDir::new();
        let other_key = KeySource::IdentityFile(other.0.join("k"));
        let mut seed = Vault::open(&other.0.join("v.age"), &other_key).unwrap();
        seed.set("x", &c, EntryKind::Secret).unwrap();
        assert!(matches!(
            Vault::open(&path, &other_key),
            Err(VaultError::Decrypt)
        ));

        // Existing file, missing identity: an error, never a fresh key.
        let missing = KeySource::IdentityFile(dir.0.join("absent.key"));
        assert!(matches!(
            Vault::open(&path, &missing),
            Err(VaultError::Key(_))
        ));
        let mut stale = Vault::open(&dir.0.join("none.age"), &missing).unwrap();
        stale.backing = Some(Backing {
            path: path.clone(),
            key: missing.clone(),
        });
        assert!(matches!(
            stale.set("y", &c, EntryKind::Secret),
            Err(VaultError::Key(_))
        ));
        assert!(!dir.0.join("absent.key").exists(), "fresh key generated");
    }

    #[test]
    fn set_picks_up_entries_written_since_open() {
        let dir = TempDir::new();
        let path = dir.0.join("vault.toml.age");
        let key = KeySource::IdentityFile(dir.0.join("vault.key"));
        let c = canary();
        let mut a = Vault::open(&path, &key).unwrap();
        let mut b = Vault::open(&path, &key).unwrap();
        a.set("a", &format!("{c}-a"), EntryKind::Secret).unwrap();
        b.set("b", &format!("{c}-b"), EntryKind::Secret).unwrap();
        let names: Vec<String> = Vault::open(&path, &key)
            .unwrap()
            .names()
            .into_iter()
            .map(|(n, _)| n)
            .collect();
        assert_eq!(names, ["a", "b"]);
        // `b` now knows `a` too, and scrubs it.
        assert_eq!(b.scrub(&format!("{c}-a")), "@vault(a)");
    }
}
