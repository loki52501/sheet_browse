//! I-4 Vault (docs/01 H-2). CONTRACT: change only through `interface_changes`.
//!
//! The one rule: a resolved value goes into a sink (the `insertText` message, the HTTP
//! header) and nowhere else. It is never returned to a tool caller, logged, put in the
//! ledger, a panic message or a fixture. Every string that leaves the process passes
//! through [`scrub`], which turns any vault value back into `@vault(name)`.

use std::fmt;
use std::path::{Path, PathBuf};

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

/// A resolved template, alive only for the duration of a sink call.
/// No `Clone`, no `Display`, no `Serialize`; `Debug` prints the unresolved template.
/// (T-05: zero the buffer on drop with `zeroize`; plain overwrites can be optimised away.)
pub struct Resolved<'a> {
    template: &'a str,
    value: String,
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

/// The vault (H-2): `vault.toml.age` decrypted with a key from [`KeySource`].
pub struct Vault {
    // Backend state is area core's choice (T-05). Values must never be reachable through
    // `Debug`, `Serialize` or any accessor other than a sink call.
    _private: (),
}

impl fmt::Debug for Vault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Vault(..)")
    }
}

impl Vault {
    /// Opens and decrypts `path` (`vault.toml.age`).
    pub fn open(path: &Path, key: &KeySource) -> Result<Vault, VaultError> {
        let _ = (path, key);
        todo!("T-05: H-2 age file + keychain")
    }

    /// In-memory vault for tests. Fixtures must use a planted canary (`SB_VAULT_CANARY`), never a real value.
    pub fn in_memory(entries: impl IntoIterator<Item = (String, String, EntryKind)>) -> Vault {
        let _ = entries.into_iter();
        todo!("T-05: in-memory backend")
    }

    /// `harness vault set <name> [--personal]`: add or replace an entry and re-encrypt.
    pub fn set(&mut self, name: &str, value: &str, kind: EntryKind) -> Result<(), VaultError> {
        let _ = (name, value, kind);
        todo!("T-05")
    }

    /// Entry names and kinds; never values.
    pub fn names(&self) -> Vec<(String, EntryKind)> {
        todo!("T-05")
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
        let _ = (template, sink);
        todo!("T-05: H-2 resolve at the sink")
    }

    /// Replaces every vault value found in `s` with `@vault(name)`.
    pub fn scrub(&self, s: &str) -> String {
        let _ = s;
        todo!("T-05: H-2 scrub")
    }

    /// Makes this vault the one the process-wide [`scrub`] uses (logs, panics, ledger).
    pub fn install_global(&self) {
        todo!("T-05")
    }
}

/// `core::scrub(s)` (H-2): scrub with the process-wide vault installed by
/// [`Vault::install_global`]. Before a vault is installed it returns `s` unchanged.
pub fn scrub(s: &str) -> String {
    let _ = s;
    todo!("T-05: H-2 global scrub")
}

/// The `@vault(name)` literals in `s`, in order, by name. Used by classifiers and the
/// toolforge invariant "a repair cannot add a vault key" (G-4).
pub fn vault_refs(s: &str) -> Result<Vec<&str>, VaultError> {
    let _ = s;
    todo!("T-05")
}

/// The literal a value is shown as everywhere outside a sink.
pub fn vault_literal(name: &str) -> String {
    format!("@vault({name})")
}
