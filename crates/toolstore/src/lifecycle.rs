//! T-7 lifecycle through I-1 `MemoryStore` (`tools.health`), T-4 relocate patching, T-5
//! re-forge, and the D-303 sweep.
//!
//! Counters are per version (D-302): `runs` is the clean streak (success += 1, any failure
//! resets it), `fails` counts failures and misses; a version bump resets both. Ledger events
//! for file actions (relocate, re-forge, retire, archive, delete) go out with server `core`
//! (LedgerEvent: "core for ... repairs, forges") and tool `tools.<action>`.
//!
//! Nothing here commits: callers stage `tools/<domain>` and `tools/_archive/<domain>` with
//! [`ToolStore::git_commit`] and their task id.

use crate::domain::{check_tool_id, valid_domain, valid_name, SiteConfig, ARCHIVE};
use crate::header::{parse_header, patch_fingerprint, render_fingerprint};
use crate::invariants::{check_repair, RepairKind};
use crate::{RunOutcome, StoreError, ToolRunResult, ToolStore};
use chrono::{DateTime, Duration, Utc};
use harness_core::memory::{LedgerEvent, MemoryStore, ToolHealth, ToolStatus, Ts};
use serde_json::json;
use std::fs;
use std::path::{Path, PathBuf};

/// T-7: ephemeral/repaired -> saved after this many clean runs.
pub const CLEAN_RUNS_TO_SAVE: i32 = 3;
/// T-7: needs_repair -> retired after this many misses (fails, D-302).
pub const MISSES_TO_RETIRE: i32 = 5;
/// T-7 / D-303: an ephemeral tool unused this long is retired.
pub const EPHEMERAL_IDLE_DAYS: i64 = 14;
/// T-7 / D-303: ... or unused over this many site visits.
pub const EPHEMERAL_IDLE_VISITS: u32 = 10;
/// T-7 / D-303: archived files are deleted after this long.
pub const ARCHIVE_DELETE_DAYS: i64 = 90;

/// One thing `sweep` did (D-303). Paths are absolute.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SweepAction {
    /// An ephemeral tool went unused: status retired, file moved to the archive.
    Retired {
        domain: String,
        name: String,
        version: i32,
        archived: Option<PathBuf>,
        why: String,
    },
    /// A retired tool's file was still in place and moved to the archive.
    Archived {
        domain: String,
        name: String,
        version: i32,
        to: PathBuf,
    },
    /// An archived file older than 90 days was deleted.
    Deleted { domain: String, path: PathBuf },
}

fn micros(t: Ts) -> Ts {
    DateTime::from_timestamp_micros(t.timestamp_micros()).unwrap_or(t)
}

/// Starts a new version: counters reset (D-302).
fn bump(h: &mut ToolHealth) {
    h.version += 1;
    h.runs = 0;
    h.fails = 0;
}

/// The date of a tool file's `@forged` line (`2026-10-22 ...` or an RFC 3339 time), if any.
fn forged_at(p: &Path) -> Option<Ts> {
    let src = fs::read_to_string(p).ok()?;
    let forged = parse_header(&src).ok()?.forged?;
    let tok = forged.split_whitespace().next()?;
    if let Ok(t) = DateTime::parse_from_rfc3339(tok) {
        return Some(t.with_timezone(&Utc));
    }
    let d = chrono::NaiveDate::parse_from_str(tok, "%Y-%m-%d").ok()?;
    Some(d.and_hms_opt(0, 0, 0)?.and_utc())
}

fn mtime(p: &Path) -> Option<Ts> {
    fs::metadata(p)
        .and_then(|m| m.modified())
        .ok()
        .map(DateTime::<Utc>::from)
}

impl ToolStore {
    fn rel(&self, p: &Path) -> String {
        p.strip_prefix(&self.root)
            .unwrap_or(p)
            .to_string_lossy()
            .replace('\\', "/")
    }

    fn site_pinned(&self, domain: &str, name: &str) -> bool {
        SiteConfig::read(&self.root, domain)
            .pinned
            .iter()
            .any(|p| p == name)
    }

    /// The `tools.health` row of one tool, if any.
    pub async fn health(
        &self,
        store: &dyn MemoryStore,
        domain: &str,
        name: &str,
    ) -> Result<Option<ToolHealth>, StoreError> {
        Ok(store
            .tool_health(domain)
            .await?
            .into_iter()
            .find(|h| h.name == name))
    }

    /// A first row for a tool on disk with no row yet: `ephemeral` when its header says
    /// `@forged`, else `saved` (hand-written, G-5); `ephemeral` when the file is missing.
    fn new_row(&self, domain: &str, name: &str) -> ToolHealth {
        let status = match fs::read_to_string(self.tool_path(domain, name))
            .ok()
            .and_then(|s| parse_header(&s).ok())
        {
            Some(h) if h.forged.is_none() => ToolStatus::Saved,
            _ => ToolStatus::Ephemeral,
        };
        ToolHealth {
            domain: domain.to_string(),
            name: name.to_string(),
            version: 1,
            status,
            runs: 0,
            fails: 0,
            last_ok: None,
            last_fail: None,
            why: None,
            pinned: self.site_pinned(domain, name),
        }
    }

    /// The row of a tool, or a new one; refuses a bad domain or name (D-310) so nothing below
    /// ever builds a path from one.
    async fn row(
        &self,
        store: &dyn MemoryStore,
        domain: &str,
        name: &str,
    ) -> Result<ToolHealth, StoreError> {
        check_tool_id(domain, name)?;
        Ok(self
            .health(store, domain, name)
            .await?
            .unwrap_or_else(|| self.new_row(domain, name)))
    }

    #[allow(clippy::too_many_arguments)]
    async fn ledger(
        &self,
        store: &dyn MemoryStore,
        action: &str,
        h: &ToolHealth,
        at: Ts,
        summary: String,
        consequences: serde_json::Value,
        why: Option<String>,
    ) -> Result<(), StoreError> {
        store
            .append(LedgerEvent {
                run_id: None,
                ts: micros(at),
                server: "core".into(),
                tool: format!("tools.{action}"),
                class: None,
                mode: None,
                args: json!({"domain": h.domain, "name": h.name, "version": h.version}),
                result_summary: summary,
                consequences,
                duration_ms: 0,
                ok: true,
                why,
            })
            .await?;
        Ok(())
    }

    /// T-9 / G-4: writes a rejected repair's reason to `tools.health.why` (row created if
    /// missing). Status and counters are untouched.
    pub async fn log_invariant(
        &self,
        store: &dyn MemoryStore,
        domain: &str,
        name: &str,
        err: &StoreError,
    ) -> Result<(), StoreError> {
        let mut h = self.row(store, domain, name).await?;
        h.why = Some(format!("rejected: {err}"));
        store.tool_health_set(h).await?;
        Ok(())
    }

    /// G-2 step 5: a newly forged tool's row, `ephemeral` at version 1. A retired name may be
    /// forged again (next version); a live one must go through [`ToolStore::reforge`].
    pub async fn register_forged(
        &self,
        store: &dyn MemoryStore,
        domain: &str,
        name: &str,
    ) -> Result<ToolHealth, StoreError> {
        check_tool_id(domain, name)?;
        let h = match self.health(store, domain, name).await? {
            None => ToolHealth {
                status: ToolStatus::Ephemeral,
                ..self.new_row(domain, name)
            },
            Some(mut h) if h.status == ToolStatus::Retired => {
                bump(&mut h);
                h.status = ToolStatus::Ephemeral;
                h.why = Some("forged again after retirement".into());
                h
            }
            Some(h) => {
                return Err(StoreError::Invariant(format!(
                    "{domain}/{name} exists ({:?}); a new version is a re-forge",
                    h.status
                )))
            }
        };
        store.tool_health_set(h.clone()).await?;
        Ok(h)
    }

    /// Marks the row retired, persists it, then moves the file to the archive and writes the
    /// ledger. The row goes first so a failed move is recoverable: a retired row whose file is
    /// still in place is archived by the next [`ToolStore::sweep`].
    async fn retire(
        &self,
        store: &dyn MemoryStore,
        h: &mut ToolHealth,
        why: String,
        at: Ts,
    ) -> Result<Option<PathBuf>, StoreError> {
        check_tool_id(&h.domain, &h.name)?;
        h.status = ToolStatus::Retired;
        h.why = Some(why.clone());
        store.tool_health_set(h.clone()).await?;
        let archived = self.archive(&h.domain, &h.name, h.version)?;
        let summary = match &archived {
            Some(p) => format!(
                "retired {}/{}, archived to {}",
                h.domain,
                h.name,
                self.rel(p)
            ),
            None => format!("retired {}/{} (no file)", h.domain, h.name),
        };
        let cons = json!({"archived": archived.as_ref().map(|p| self.rel(p))});
        self.ledger(store, "retire", h, at, summary, cons, Some(why))
            .await?;
        Ok(archived)
    }

    /// I-7 `record(result)`: one T-7 transition, written through `MemoryStore` (I-1).
    ///
    /// - unknown tool: row created (see `new_row`), then the outcome applies;
    /// - `Ok`: streak += 1; ephemeral/repaired -> saved at 3; needs_repair -> repaired,
    ///   version += 1 (D-304);
    /// - `Relocated`: `@fingerprint` patched on disk (relocate invariants), version += 1,
    ///   status repaired;
    /// - `Failed`: fails += 1, streak reset, why;
    /// - `FingerprintMiss` / `ExpectsMiss`: as `Failed`, and status needs_repair;
    /// - needs_repair with fails >= 5 and not pinned: retired, file archived;
    /// - `ask` (raised floor, T-5, D-310) is left only by [`ToolStore::confirm`]: every
    ///   outcome updates counters (and Relocated the fingerprint) but the status stays `ask`;
    /// - `Relocated` on a retired tool is refused.
    pub async fn record(
        &self,
        store: &dyn MemoryStore,
        result: ToolRunResult,
    ) -> Result<ToolHealth, StoreError> {
        let ToolRunResult {
            domain,
            name,
            outcome,
            at,
        } = result;
        let mut h = self.row(store, &domain, &name).await?;
        let retired = h.status == ToolStatus::Retired;
        // T-5 / D-310: the user's confirmation is permission; no outcome stands in for it.
        let held = h.status == ToolStatus::Ask;
        let fail = |h: &mut ToolHealth, why: String, miss: bool| {
            h.fails += 1;
            h.runs = 0;
            h.last_fail = Some(at);
            h.why = Some(why);
            if miss && !retired && !held {
                h.status = ToolStatus::NeedsRepair;
            }
        };
        match outcome {
            RunOutcome::Ok => {
                if h.status == ToolStatus::NeedsRepair {
                    bump(&mut h);
                    h.status = ToolStatus::Repaired;
                    h.why = Some("fingerprint matches again".into());
                }
                h.runs += 1;
                h.last_ok = Some(at);
                if matches!(h.status, ToolStatus::Ephemeral | ToolStatus::Repaired)
                    && h.runs >= CLEAN_RUNS_TO_SAVE
                {
                    h.status = ToolStatus::Saved;
                    h.why = None;
                }
            }
            RunOutcome::Relocated { fingerprint } => {
                if retired {
                    return Err(StoreError::Invariant(format!(
                        "{domain}/{name} is retired; a relocate cannot revive it (T-7)"
                    )));
                }
                let path = self.try_tool_path(&domain, &name)?;
                let old = fs::read_to_string(&path)?;
                let new = patch_fingerprint(&old, &fingerprint);
                if let Err(e) = check_repair(&old, &new, RepairKind::Relocate) {
                    self.log_invariant(store, &domain, &name, &e).await?;
                    return Err(e);
                }
                fs::write(&path, &new)?;
                let before = parse_header(&old)
                    .map(|hd| render_fingerprint(&hd.fingerprint))
                    .unwrap_or_default();
                let after = render_fingerprint(&fingerprint);
                bump(&mut h);
                if !held {
                    h.status = ToolStatus::Repaired;
                }
                let why = format!("relocated: {before} -> {after}");
                h.why = Some(why.clone());
                let summary = format!("relocated {domain}/{name} to version {}", h.version);
                let cons = json!({"patched": self.rel(&path), "fingerprint": after});
                self.ledger(store, "relocate", &h, at, summary, cons, Some(why))
                    .await?;
            }
            RunOutcome::Failed { why } => fail(&mut h, format!("failed: {why}"), false),
            RunOutcome::FingerprintMiss { why } => {
                fail(&mut h, format!("fingerprint miss: {why}"), true)
            }
            RunOutcome::ExpectsMiss { why } => fail(&mut h, format!("expects miss: {why}"), true),
        }
        let pinned = h.pinned || self.site_pinned(&domain, &name);
        if h.status == ToolStatus::NeedsRepair && h.fails >= MISSES_TO_RETIRE && !pinned {
            let last = h.why.clone().unwrap_or_default();
            let why = format!("retired after {} misses; last {last}", h.fails);
            self.retire(store, &mut h, why, at).await?;
        }
        store.tool_health_set(h.clone()).await?;
        Ok(h)
    }

    /// T-5 re-forge under the same name: invariants (D-307), old file to
    /// `_archive/<domain>/<name>.<version>.js`, new file written, version += 1, status
    /// `repaired`, or `ask` when the new `@gate` floor is higher or the row was already `ask`
    /// (until [`ToolStore::confirm`], D-310).
    ///
    /// Order: archive the old file, write the new one, persist the row, then the ledger. A
    /// failure after the archive move puts the old file back and then removes the archived
    /// copy, so the file and the row never disagree. If putting it back fails as well, the
    /// archived copy is kept (it is the only one left) and the error names both failures.
    pub async fn reforge(
        &self,
        store: &dyn MemoryStore,
        domain: &str,
        name: &str,
        new_src: &str,
        at: Ts,
    ) -> Result<ToolHealth, StoreError> {
        check_tool_id(domain, name)?;
        let path = self.try_tool_path(domain, name)?;
        let old = fs::read_to_string(&path)?;
        if let Err(e) = check_repair(&old, new_src, RepairKind::Reforge) {
            self.log_invariant(store, domain, name, &e).await?;
            return Err(e);
        }
        let gate = |src: &str| {
            parse_header(src)
                .map(|h| h.gate)
                .map_err(|e| StoreError::Invariant(format!("bad header: {e}")))
        };
        let (og, ng) = (gate(&old)?, gate(new_src)?);
        let mut h = self.row(store, domain, name).await?;
        let held = h.status == ToolStatus::Ask;
        let archived = self.archive(domain, name, h.version)?;
        // Puts the old file back after `cause`. The old content is in memory, so restoring
        // does not depend on the archived copy being readable. The archived copy is removed
        // only once the restore succeeded: if the restore fails too, it is the only copy of
        // the tool, so it stays and the error names both failures and where it is.
        let undo = |archived: &Option<PathBuf>, cause: StoreError| -> StoreError {
            match fs::write(&path, &old) {
                Ok(()) => {
                    if let Some(a) = archived {
                        // A leftover copy in the archive is harmless; the sweep deletes it.
                        let _ = fs::remove_file(a);
                    }
                    cause
                }
                Err(restore) => {
                    let kept = archived
                        .as_ref()
                        .map(|a| self.rel(a))
                        .unwrap_or_else(|| "nowhere".into());
                    StoreError::Io(std::io::Error::new(
                        restore.kind(),
                        format!(
                            "reforge {domain}/{name} failed ({cause}) and restoring {} failed                              too ({restore}); the old version is kept at {kept}",
                            self.rel(&path)
                        ),
                    ))
                }
            }
        };
        if let Err(e) = fs::write(&path, new_src) {
            return Err(undo(&archived, e.into()));
        }
        bump(&mut h);
        let why = if ng > og {
            h.status = ToolStatus::Ask;
            format!("re-forged; @gate floor raised {og} -> {ng}, ask the user (T-5)")
        } else if held {
            h.status = ToolStatus::Ask;
            "re-forged while a raised @gate floor awaits the user; still ask (T-5, D-310)"
                .to_string()
        } else {
            h.status = ToolStatus::Repaired;
            "re-forged from a successful trace (T-5)".to_string()
        };
        h.why = Some(why.clone());
        if let Err(e) = store.tool_health_set(h.clone()).await {
            return Err(undo(&archived, e.into()));
        }
        let summary = format!("re-forged {domain}/{name} to version {}", h.version);
        let cons =
            json!({"archived": archived.as_ref().map(|p| self.rel(p)), "written": self.rel(&path)});
        self.ledger(store, "reforge", &h, at, summary, cons, Some(why))
            .await?;
        Ok(h)
    }

    /// The user confirmed a raised floor (T-5): `ask` -> `repaired`. Other statuses unchanged.
    pub async fn confirm(
        &self,
        store: &dyn MemoryStore,
        domain: &str,
        name: &str,
    ) -> Result<ToolHealth, StoreError> {
        let mut h = self.row(store, domain, name).await?;
        if h.status == ToolStatus::Ask {
            h.status = ToolStatus::Repaired;
            h.why = Some("raised floor confirmed by the user".into());
            store.tool_health_set(h.clone()).await?;
        }
        Ok(h)
    }

    /// G-5 `harness tools pin` / unpin.
    pub async fn set_pinned(
        &self,
        store: &dyn MemoryStore,
        domain: &str,
        name: &str,
        pinned: bool,
    ) -> Result<ToolHealth, StoreError> {
        let mut h = self.row(store, domain, name).await?;
        h.pinned = pinned;
        store.tool_health_set(h.clone()).await?;
        Ok(h)
    }

    /// Site folders plus `_generic` and every domain under `_archive/`.
    fn swept_domains(&self) -> Vec<String> {
        let mut out = Vec::new();
        let mut list = |dir: PathBuf| {
            if let Ok(rd) = fs::read_dir(dir) {
                for e in rd.flatten() {
                    if !e.path().is_dir() {
                        continue;
                    }
                    if let Ok(n) = e.file_name().into_string() {
                        if valid_domain(&n) && !out.contains(&n) {
                            out.push(n);
                        }
                    }
                }
            }
        };
        list(self.root.clone());
        list(self.root.join(ARCHIVE));
        out.sort();
        out
    }

    /// D-303 sweep (reflect): unused ephemeral tools (14 days since last use, or
    /// `visits(row) >= 10` site visits without use, counted by the caller from the ledger) are
    /// retired and archived; retired tools still in place are archived; archived files older
    /// than 90 days are deleted. Pinned tools are never touched. One ledger event per action.
    pub async fn sweep(
        &self,
        store: &dyn MemoryStore,
        now: Ts,
        visits: &(dyn Fn(&ToolHealth) -> u32 + Sync),
    ) -> Result<Vec<SweepAction>, StoreError> {
        let mut done = Vec::new();
        for domain in self.swept_domains() {
            let rows = store.tool_health(&domain).await?;
            for mut h in rows.iter().cloned() {
                // D-310: a row whose name is not a safe file name is never acted on.
                if !valid_name(&h.name) || h.pinned || self.site_pinned(&domain, &h.name) {
                    continue;
                }
                let path = self.try_tool_path(&domain, &h.name)?;
                match h.status {
                    ToolStatus::Ephemeral => {
                        // Last use is the later of the last success and the last failure. A
                        // tool never run counts from the later of its @forged date and the
                        // file mtime, so a file written (registered, re-forged) recently is
                        // never idle however old its @forged line is.
                        let since = h
                            .last_ok
                            .max(h.last_fail)
                            .or_else(|| forged_at(&path).max(mtime(&path)));
                        let idle = since.map(|t| now - t >= Duration::days(EPHEMERAL_IDLE_DAYS));
                        let v = visits(&h);
                        let why = if idle == Some(true) {
                            let day = since.map(|t| t.format("%Y-%m-%d").to_string());
                            format!(
                                "ephemeral, unused {EPHEMERAL_IDLE_DAYS} days (last used {})",
                                day.unwrap_or_default()
                            )
                        } else if v >= EPHEMERAL_IDLE_VISITS {
                            format!("ephemeral, unused over {v} site visits")
                        } else {
                            continue;
                        };
                        let archived = self.retire(store, &mut h, why.clone(), now).await?;
                        store.tool_health_set(h.clone()).await?;
                        done.push(SweepAction::Retired {
                            domain: domain.clone(),
                            name: h.name.clone(),
                            version: h.version,
                            archived,
                            why,
                        });
                    }
                    ToolStatus::Retired if path.is_file() => {
                        if let Some(to) = self.archive(&domain, &h.name, h.version)? {
                            let summary = format!(
                                "archived retired {domain}/{} to {}",
                                h.name,
                                self.rel(&to)
                            );
                            let cons = json!({"archived": self.rel(&to)});
                            self.ledger(store, "archive", &h, now, summary, cons, h.why.clone())
                                .await?;
                            done.push(SweepAction::Archived {
                                domain: domain.clone(),
                                name: h.name.clone(),
                                version: h.version,
                                to,
                            });
                        }
                    }
                    _ => {}
                }
            }
            // Archived files past 90 days.
            let Ok(rd) = fs::read_dir(self.root.join(ARCHIVE).join(&domain)) else {
                continue;
            };
            let mut files: Vec<PathBuf> = rd
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.is_file())
                .collect();
            files.sort();
            for f in files {
                let fname = f.file_name().and_then(|s| s.to_str()).unwrap_or("");
                let name = fname.split('.').next().unwrap_or("");
                let row = rows.iter().find(|h| h.name == name);
                if row.is_some_and(|h| h.pinned) || self.site_pinned(&domain, name) {
                    continue;
                }
                let Some(t) = mtime(&f) else { continue };
                if now - t < Duration::days(ARCHIVE_DELETE_DAYS) {
                    continue;
                }
                fs::remove_file(&f)?;
                let h = row.cloned().unwrap_or_else(|| ToolHealth {
                    version: 0,
                    ..self.new_row(&domain, name)
                });
                let why = format!("archived over {ARCHIVE_DELETE_DAYS} days");
                let summary = format!("deleted {}", self.rel(&f));
                let cons = json!({"deleted": self.rel(&f)});
                self.ledger(store, "delete", &h, now, summary, cons, Some(why))
                    .await?;
                done.push(SweepAction::Deleted {
                    domain: domain.clone(),
                    path: f,
                });
            }
        }
        Ok(done)
    }
}
