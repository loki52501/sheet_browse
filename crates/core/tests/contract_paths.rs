//! Contract paths (pr-ready section 4): every harness-core item `agent-memory/interfaces.yaml`
//! names (its `rust` paths and the methods its `name` leads with) must exist at exactly
//! that path and signature, so a wrong path fails compilation here.
//! Covers I-4 (vault) and I-13 (sched, watch) so far; other interfaces are added as their
//! tasks touch them.

use std::path::{Path, PathBuf};

use harness_core::vault::{
    default_key, default_path, scrub, vault_literal, vault_refs, EntryKind, KeySource, Resolved,
    Vault, VaultEditor, VaultError, MIN_VALUE_CHARS,
};

/// I-4 `name`: `Vault::resolve_into(template, sink)`, the vault's own `scrub` and
/// `install_global`, and `Vault::open`, the way every consumer gets a vault.
#[test]
fn vault_open_resolve_into_scrub_install() {
    let _open: fn(&Path, &KeySource) -> Result<Vault, VaultError> = Vault::open;
    let _scrub: fn(&Vault, &str) -> String = Vault::scrub;
    let _install: fn(&Vault) = Vault::install_global;
    // Never called: it pins that the sink receives `&Resolved` and `expose` gives `&str`.
    fn _resolve(v: &Vault) -> Result<usize, VaultError> {
        v.resolve_into("", |r: &Resolved<'_>| r.expose().len())
    }
    let _ = _resolve;
}

/// I-4, D-84: `Vault::open_for_set(path, key) -> Result<VaultEditor, VaultError>`, and the
/// editor's whole surface: set, remove, names, save.
#[test]
fn vault_open_for_set_returns_a_vault_editor() {
    let _f: fn(&Path, &KeySource) -> Result<VaultEditor, VaultError> = Vault::open_for_set;
    let _set: fn(&mut VaultEditor, &str, &str, EntryKind) -> Result<(), VaultError> =
        VaultEditor::set;
    let _remove: fn(&mut VaultEditor, &str) -> Result<(), VaultError> = VaultEditor::remove;
    let _names: fn(&VaultEditor) -> Vec<(String, EntryKind)> = VaultEditor::names;
    let _save: fn(VaultEditor) -> Result<(), VaultError> = VaultEditor::save;
    // The crate root re-export.
    let _root: Option<harness_core::VaultEditor> = None;
}

/// I-4 free functions and constants (D-76).
#[test]
fn vault_free_items() {
    let _: fn() -> Result<PathBuf, VaultError> = default_path;
    let _: fn() -> KeySource = default_key;
    let _: fn(&str) -> String = scrub;
    let _: fn(&str) -> String = vault_literal;
    let _: fn(&str) -> Result<Vec<&str>, VaultError> = vault_refs;
    let _: usize = MIN_VALUE_CHARS;
    let _: Option<&Resolved<'_>> = None;
}

/// I-4 error variants named in the contract (D-76, D-84).
#[test]
fn vault_error_variants() {
    let errors = [
        VaultError::ValueTooShort {
            name: "n".into(),
            min: MIN_VALUE_CHARS,
        },
        VaultError::ScrubUnavailable,
        VaultError::ValueInLiteral { name: "n".into() },
    ];
    assert_eq!(errors.len(), 3);
}

/// I-13 (draft), as touched by T-16 (B-197, D-140): every listed sched path, the new
/// `LaunchRequest.schedule` field, `run_preamble`, and the `watch` module items.
#[test]
fn sched_paths_and_launch_request_schedule() {
    use harness_core::memory::Schedule;
    use harness_core::run::RunFolder;
    #[allow(unused_imports)]
    use harness_core::sched::{
        changed_dir, next_after, notify_changed, parse_cron, run_preamble, Clock, Finished, Fired,
        LaunchError, LaunchExit, LaunchRequest, Launcher, ManualClock, RunSpec, Runner, SchedError,
        Scheduler, SystemClock, CHANGED_DIR, FIRE_TOOL, LAUNCH_TOOL,
    };
    use harness_core::watch::{
        normalise, state_hash, target_key, WatchCheck, WatchOutcome, CHECK_TOOL, NOTIFICATIONS_LOG,
        NOTIFICATION_FILE, NOTIFY_TOOL, WATCH_FILE,
    };
    fn _req<'a>(run: &'a RunFolder, s: Option<&'a Schedule>) -> LaunchRequest<'a> {
        LaunchRequest {
            run,
            runs_root: Path::new("runs"),
            prompt: "",
            schedule: s,
        }
    }
    let _ = _req;
    let _: fn(&LaunchRequest<'_>) -> String = run_preamble;
    let _: fn(&str) -> String = normalise;
    let _: fn(&str) -> String = state_hash;
    let _: fn(&Schedule) -> String = target_key;
    let _: fn(&WatchCheck) -> String = WatchCheck::report_line;
    let _: Option<WatchOutcome> = None;
    let _: [&str; 5] = [
        WATCH_FILE,
        NOTIFICATION_FILE,
        NOTIFICATIONS_LOG,
        CHECK_TOOL,
        NOTIFY_TOOL,
    ];
}
