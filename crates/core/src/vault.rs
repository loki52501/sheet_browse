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
//! Scrubbing (D-102): aho-corasick leftmost-longest over every value and its JSON-escaped
//! and percent-encoded spellings.

use std::collections::{BTreeMap, HashSet};
use std::fmt;
use std::fmt::Write as _;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Once, RwLock};

use age::secrecy::ExposeSecret;
use aho_corasick::{AhoCorasick, MatchKind};
use percent_encoding::{utf8_percent_encode, AsciiSet, NON_ALPHANUMERIC};
use zeroize::Zeroizing;

#[derive(Debug, thiserror::Error)]
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
}

/// `kind` of an entry (H-2): credentials vs. the user's form data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EntryKind {
    Secret,
    Personal,
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
}

impl fmt::Debug for Vault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Vault(..)")
    }
}

impl Vault {
    /// Opens and decrypts `path` (`vault.toml.age`).
    /// A missing file opens as an empty vault without touching the key (D-101).
    pub fn open(path: &Path, key: &KeySource) -> Result<Vault, VaultError> {
        let entries = match std::fs::read(path) {
            Ok(ciphertext) => {
                let identity = load_identity(key)?
                    .ok_or_else(|| VaultError::Key("no age identity for this vault".into()))?;
                decrypt_entries(&identity, &ciphertext)?
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
            Err(e) => return Err(VaultError::Io(e)),
        };
        let scrubber = Arc::new(Scrubber::build(&entries));
        Ok(Vault {
            entries,
            backing: Some(Backing {
                path: path.to_path_buf(),
                key: key.clone(),
            }),
            scrubber,
        })
    }

    /// In-memory vault for tests. Fixtures must use a planted canary (`SB_VAULT_CANARY`), never a real value.
    /// A later entry with the same name replaces an earlier one. `set` on it writes no file.
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
        }
    }

    /// `harness vault set <name> [--personal]`: add or replace an entry and re-encrypt.
    ///
    /// `name` must be `[A-Za-z0-9_.-]+` so that `@vault(name)` can reference it; otherwise
    /// `BadReference(byte offset of the first bad char in name)`. For a file-backed vault
    /// the file is rewritten atomically (temp file + rename); when neither the vault file
    /// nor the identity exists yet, a fresh identity is generated and stored in the key
    /// source first. If the vault is the installed global one, the global scrub follows.
    pub fn set(&mut self, name: &str, value: &str, kind: EntryKind) -> Result<(), VaultError> {
        if let Some(bad) = invalid_name_at(name) {
            return Err(VaultError::BadReference(bad));
        }
        let mut next: BTreeMap<String, Entry> = self
            .entries
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
            .collect();
        next.insert(
            name.to_owned(),
            Entry {
                value: Zeroizing::new(value.to_owned()),
                kind,
            },
        );
        if let Some(backing) = &self.backing {
            let identity = identity_for_write(backing)?;
            let ciphertext = encrypt_entries(&identity, &next)?;
            write_atomic(&backing.path, &ciphertext)?;
        }
        let scrubber = Arc::new(Scrubber::build(&next));
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
    pub fn install_global(&self) {
        *GLOBAL.write().unwrap_or_else(|e| e.into_inner()) = Some(self.scrubber.clone());
        PANIC_HOOK.call_once(install_panic_hook);
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

struct Scrubber {
    /// `None` when there is no non-empty value.
    ac: Option<AhoCorasick>,
    /// Replacement literal per pattern id.
    literals: Vec<String>,
}

impl Scrubber {
    fn build(entries: &BTreeMap<String, Entry>) -> Scrubber {
        let mut patterns: Vec<Zeroizing<String>> = Vec::new();
        let mut literals = Vec::new();
        for (name, e) in entries {
            if e.value.is_empty() {
                continue;
            }
            let literal = vault_literal(name);
            let mut seen: HashSet<&str> = HashSet::new();
            let spellings = spellings(&e.value);
            for sp in &spellings {
                if seen.insert(sp.as_str()) {
                    patterns.push(sp.clone());
                    literals.push(literal.clone());
                }
            }
        }
        if patterns.is_empty() {
            return Scrubber { ac: None, literals };
        }
        let ac = AhoCorasick::builder()
            .match_kind(MatchKind::LeftmostLongest)
            .build(patterns.iter().map(|p| p.as_bytes()))
            // Building only fails on size limits far beyond any vault.
            .ok();
        Scrubber { ac, literals }
    }

    fn scrub(&self, s: &str) -> String {
        let Some(ac) = &self.ac else {
            return s.to_owned();
        };
        let mut out = String::with_capacity(s.len());
        ac.replace_all_with(s, &mut out, |m, _, dst| {
            dst.push_str(&self.literals[m.pattern().as_usize()]);
            true
        });
        out
    }
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

/// application/x-www-form-urlencoded keeps `A-Z a-z 0-9 * - . _` and writes space as `+`.
const FORM: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'*')
    .remove(b'-')
    .remove(b'.')
    .remove(b'_');

/// The spellings of `v` that scrub replaces: raw, JSON-escaped (serde style, ASCII-only
/// style, and escaped twice for JSON inside a JSON string), and percent-encoded
/// (all non-alphanumerics, encodeURIComponent, form encoding; upper and lower hex).
fn spellings(v: &str) -> Vec<Zeroizing<String>> {
    let mut out = vec![Zeroizing::new(v.to_owned())];
    let json = Zeroizing::new(json_escape(v));
    out.push(Zeroizing::new(json_escape(&json)));
    out.push(json);
    out.push(Zeroizing::new(json_escape_ascii(v)));
    let pct = [
        Zeroizing::new(utf8_percent_encode(v, NON_ALPHANUMERIC).to_string()),
        Zeroizing::new(utf8_percent_encode(v, URI_COMPONENT).to_string()),
        Zeroizing::new(utf8_percent_encode(v, FORM).to_string().replace("%20", "+")),
    ];
    for p in pct {
        out.push(Zeroizing::new(lower_hex_escapes(&p)));
        out.push(p);
    }
    out
}

/// `serde_json` string escaping without the surrounding quotes.
fn json_escape(v: &str) -> String {
    let quoted = Zeroizing::new(serde_json::to_string(v).unwrap_or_default());
    quoted
        .get(1..quoted.len().saturating_sub(1))
        .unwrap_or_default()
        .to_owned()
}

/// JSON escaping with every non-ASCII char as `\uXXXX` (Python `ensure_ascii`).
fn json_escape_ascii(v: &str) -> String {
    let mut out = String::with_capacity(v.len() * 2);
    for c in v.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 || !c.is_ascii() => {
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
        let mut text = format!("\nthread '{name}' panicked at {at}:\n{scrubbed}\n");
        if bt.status() == std::backtrace::BacktraceStatus::Captured {
            let _ = writeln!(text, "stack backtrace:\n{bt}");
        }
        // One `eprint!` (not a raw stderr handle) so test output capture still applies.
        eprint!("{text}");
    }));
}

// ---------------------------------------------------------------------------------------
// age file + key source (D-101)

#[derive(serde::Deserialize)]
struct FileEntry {
    value: String,
    kind: EntryKind,
}

#[derive(serde::Serialize)]
struct FileEntryRef<'a> {
    value: &'a str,
    kind: EntryKind,
}

fn decrypt_entries(
    identity: &age::x25519::Identity,
    ciphertext: &[u8],
) -> Result<BTreeMap<String, Entry>, VaultError> {
    let plain =
        Zeroizing::new(age::decrypt(identity, ciphertext).map_err(|_| VaultError::Decrypt)?);
    let text = std::str::from_utf8(&plain).map_err(|_| VaultError::Decrypt)?;
    // The parse error is dropped unread: its message can quote the offending line.
    let raw: BTreeMap<String, FileEntry> = toml::from_str(text).map_err(|_| VaultError::Decrypt)?;
    Ok(raw
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
        .collect())
}

fn encrypt_entries(
    identity: &age::x25519::Identity,
    entries: &BTreeMap<String, Entry>,
) -> Result<Vec<u8>, VaultError> {
    let view: BTreeMap<&str, FileEntryRef<'_>> = entries
        .iter()
        .map(|(k, e)| {
            (
                k.as_str(),
                FileEntryRef {
                    value: &e.value,
                    kind: e.kind,
                },
            )
        })
        .collect();
    let text = Zeroizing::new(toml::to_string(&view).map_err(|_| VaultError::Decrypt)?);
    age::encrypt(&identity.to_public(), text.as_bytes())
        .map_err(|_| VaultError::Key("age encryption failed".into()))
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

/// Identity for re-encrypting. Generates and stores one only when neither the vault file
/// nor an identity exists, so an existing file is never orphaned by a fresh key.
fn identity_for_write(backing: &Backing) -> Result<age::x25519::Identity, VaultError> {
    if let Some(id) = load_identity(&backing.key)? {
        return Ok(id);
    }
    if backing.path.exists() {
        return Err(VaultError::Key(
            "vault file exists but its age identity is missing".into(),
        ));
    }
    let id = age::x25519::Identity::generate();
    let secret = id.to_string();
    match &backing.key {
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

/// Write `bytes` to a temp file next to `path`, fsync, then rename over `path`.
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), VaultError> {
    let dir = path
        .parent()
        .filter(|d| !d.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(dir)?;
    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "vault".into());
    let tmp = dir.join(format!(".{file_name}.{}.tmp", std::process::id()));
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
    use std::sync::atomic::{AtomicUsize, Ordering};

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
        let ascii = json_escape_ascii(&c);
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
            format!("[{ascii}]"),
            format!("https://x.test/?a={pct}"),
            format!("?a={comp}"),
            format!("a={form}&b=1"),
            format!("?a={lower}"),
        ] {
            let out = v.scrub(&s);
            assert!(!out.contains(&canary()), "value survived in {out}");
            assert!(out.contains("@vault(test)"), "{out}");
        }
        assert_eq!(v.scrub("nothing here"), "nothing here");
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
            v.resolve_into("[@vault(empty)]", |r| r.expose().to_owned())
                .unwrap(),
            "[]"
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
        // A panic message carrying the value is scrubbed by the hook (no crash here).
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
    }
}
