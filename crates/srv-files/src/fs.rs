//! F-1 `fs.read`, `fs.write`, `fs.glob`, `fs.move`, `fs.mkdir`, `fs.delete`.
//!
//! Classes (D-520, D-523): reads are Read. Writes are Fill, raised to Delete when they
//! delete, replace or move away something outside `runs/` (or touch a run's bookkeeping,
//! see [`crate::paths`], or a file with other hard links). Moving a file from outside
//! `runs/` anywhere is Delete: into `runs/` it could then be deleted without asking, and
//! elsewhere it is gone from where it was. `fs.delete` inside `runs/` is Read, the class
//! `shell("rm -rf runs/x")` has (S3, B-520). Every call rechecks its class against the
//! approved one before it acts.

use crate::paths::{self, Place, Roots};
use crate::text::{self, MAX_CHARS, READ_DEFAULT_CHARS};
use crate::util::{self, parse, recheck, Verdict};
use async_trait::async_trait;
use harness_core::{Class, Classified, ToolCtx, ToolError, ToolHandler, ToolOutput, ToolSpec};
use serde::Deserialize;
use serde_json::{json, Value};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Largest prefix of a file `fs.read` loads.
const READ_MAX_BYTES: u64 = 32 * 1024 * 1024;
/// Default and largest number of paths `fs.glob` lists.
const GLOB_DEFAULT: usize = 200;
const GLOB_MAX: usize = 1000;
/// Matches counted before `fs.glob` stops walking.
const GLOB_COUNT_MAX: usize = 20_000;
/// Longest `fs.glob` walk.
const GLOB_TIME: std::time::Duration = std::time::Duration::from_secs(20);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Op {
    Read,
    Write,
    Glob,
    Move,
    Mkdir,
    Delete,
}

struct Fs(Op);

pub fn tools() -> Vec<ToolSpec> {
    let spec = |name: &str, description: &str, class: Class, op: Op, args: Value| ToolSpec {
        name: name.into(),
        description: description.into(),
        class,
        args,
        handler: Arc::new(Fs(op)),
    };
    vec![
        spec(
            "fs.read",
            "Read a text file (relative paths start at the folder that holds runs/). Returns at most max_chars (default 8000, up to 20000) from char offset; a trailer names the next offset.",
            Class::Read,
            Op::Read,
            json!({"type":"object","properties":{
                "path":{"type":"string"},
                "offset":{"type":"integer","minimum":0},
                "max_chars":{"type":"integer","minimum":1,"maximum":MAX_CHARS}
            },"required":["path"],"additionalProperties":false}),
        ),
        spec(
            "fs.write",
            "Write (or append) text to a file, creating folders. Deliverables belong in this run's files/ (runs/<run>/files/). Replacing a file outside runs/ asks first.",
            Class::Fill,
            Op::Write,
            json!({"type":"object","properties":{
                "path":{"type":"string"},
                "content":{"type":"string"},
                "append":{"type":"boolean","default":false}
            },"required":["path","content"],"additionalProperties":false}),
        ),
        spec(
            "fs.glob",
            "List paths matching a glob (`**` recurses), sorted, at most max (default 200). Folders end in /.",
            Class::Read,
            Op::Glob,
            json!({"type":"object","properties":{
                "pattern":{"type":"string"},
                "max":{"type":"integer","minimum":1,"maximum":GLOB_MAX}
            },"required":["pattern"],"additionalProperties":false}),
        ),
        spec(
            "fs.move",
            "Move or rename a file or folder. Into an existing folder keeps the name. Replacing a file, or moving something from outside runs/ into it, asks first.",
            Class::Fill,
            Op::Move,
            json!({"type":"object","properties":{
                "from":{"type":"string"},
                "to":{"type":"string"},
                "overwrite":{"type":"boolean","default":false}
            },"required":["from","to"],"additionalProperties":false}),
        ),
        spec(
            "fs.mkdir",
            "Create a folder and its parents.",
            Class::Fill,
            Op::Mkdir,
            json!({"type":"object","properties":{"path":{"type":"string"}},
                "required":["path"],"additionalProperties":false}),
        ),
        spec(
            "fs.delete",
            "Delete a file, or a folder with recursive=true. Inside runs/ it just runs (class Read, like shell rm there); anywhere else it is class Delete and asks first.",
            Class::Read,
            Op::Delete,
            json!({"type":"object","properties":{
                "path":{"type":"string"},
                "recursive":{"type":"boolean","default":false}
            },"required":["path"],"additionalProperties":false}),
        ),
    ]
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadArgs {
    path: String,
    offset: Option<usize>,
    max_chars: Option<usize>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WriteArgs {
    path: String,
    content: String,
    #[serde(default)]
    append: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GlobArgs {
    pattern: String,
    max: Option<usize>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MoveArgs {
    from: String,
    to: String,
    #[serde(default)]
    overwrite: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PathArgs {
    path: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DeleteArgs {
    path: String,
    #[serde(default)]
    recursive: bool,
}

/// What a move would do: the resolved source, the final destination, the class.
struct MovePlan {
    from: PathBuf,
    to: PathBuf,
    verdict: Verdict,
}

fn plan_move(roots: &Roots, a: &MoveArgs) -> Result<MovePlan, ToolError> {
    let from = roots.resolve(&a.from)?;
    let mut to = roots.resolve(&a.to)?;
    if paths::is_dir(&to) {
        if let Some(name) = from.file_name() {
            to = to.join(name);
        }
    }
    let mut v = Verdict::new(Class::Fill);
    let src = roots.place(&from);
    let dst = roots.place(&to);
    if src == Place::Bookkeeping || paths::contains_runs(roots, &from) {
        v.raise(
            Class::Delete,
            format!("moves the run records at {}", roots.shown(&from)),
        );
    }
    if src == Place::Outside && dst == Place::Scratch {
        v.raise(
            Class::Delete,
            format!(
                "moves {} from outside runs/ into runs/, where it could be deleted without asking",
                roots.shown(&from)
            ),
        );
    } else if src == Place::Outside {
        // S4, D-520 (3): moving away outside scratch.
        v.raise(
            Class::Delete,
            format!("moves {} away (outside runs/)", roots.shown(&from)),
        );
    }
    if dst == Place::Bookkeeping {
        v.raise(
            Class::Delete,
            format!("writes the run's own records ({})", roots.shown(&to)),
        );
    } else if dst == Place::Outside && a.overwrite && paths::exists(&to) {
        v.raise(
            Class::Delete,
            format!("replaces {} (outside runs/)", roots.shown(&to)),
        );
    } else if src == Place::Outside || dst == Place::Outside {
        v.raise(
            Class::Fill,
            format!("moves {} to {}", roots.shown(&from), roots.shown(&to)),
        );
    }
    Ok(MovePlan {
        from,
        to,
        verdict: v,
    })
}

fn delete_verdict(roots: &Roots, p: &Path) -> Verdict {
    let mut v = Verdict::new(Class::Read);
    match roots.place(p) {
        Place::Scratch => v.raise(
            Class::Read,
            format!("deletes {} (in runs/)", roots.shown(p)),
        ),
        Place::Bookkeeping => v.raise(
            Class::Delete,
            format!("deletes the run records at {}", roots.shown(p)),
        ),
        Place::Outside => v.raise(
            Class::Delete,
            format!("deletes {} (outside runs/)", roots.shown(p)),
        ),
    }
    v
}

fn mkdir_verdict(roots: &Roots, p: &Path) -> Verdict {
    let mut v = Verdict::new(Class::Fill);
    if roots.place(p) == Place::Bookkeeping {
        v.raise(
            Class::Delete,
            format!("writes the run's own records ({})", roots.shown(p)),
        );
    }
    v
}

impl Fs {
    fn verdict(
        &self,
        roots: &Roots,
        args: &Value,
    ) -> Result<(Verdict, Vec<(String, String)>), ToolError> {
        Ok(match self.0 {
            Op::Read => {
                let a: ReadArgs = parse(args)?;
                let p = roots.resolve(&a.path)?;
                (
                    Verdict::new(Class::Read),
                    vec![("path".into(), roots.shown(&p))],
                )
            }
            Op::Glob => {
                let a: GlobArgs = parse(args)?;
                (
                    Verdict::new(Class::Read),
                    vec![("pattern".into(), a.pattern)],
                )
            }
            Op::Write => {
                let a: WriteArgs = parse(args)?;
                let p = roots.resolve(&a.path)?;
                (
                    util::write_verdict(roots, &p, !a.append, Class::Fill),
                    vec![("path".into(), roots.shown(&p))],
                )
            }
            Op::Move => {
                let a: MoveArgs = parse(args)?;
                let m = plan_move(roots, &a)?;
                (
                    m.verdict,
                    vec![
                        ("from".into(), roots.shown(&m.from)),
                        ("to".into(), roots.shown(&m.to)),
                    ],
                )
            }
            Op::Mkdir => {
                let a: PathArgs = parse(args)?;
                let p = roots.resolve(&a.path)?;
                (
                    mkdir_verdict(roots, &p),
                    vec![("path".into(), roots.shown(&p))],
                )
            }
            Op::Delete => {
                let a: DeleteArgs = parse(args)?;
                let p = roots.resolve(&a.path)?;
                (
                    delete_verdict(roots, &p),
                    vec![("path".into(), roots.shown(&p))],
                )
            }
        })
    }
}

fn io_err(what: &str, roots: &Roots, p: &Path, e: std::io::Error) -> ToolError {
    ToolError::Failed(format!("{what} {}: {e}", roots.shown(p)))
}

#[async_trait]
impl ToolHandler for Fs {
    async fn classify(&self, ctx: &ToolCtx, args: &Value) -> Result<Classified, ToolError> {
        let roots = Roots::of(ctx)?;
        let (v, values) = self.verdict(&roots, args)?;
        let name = match self.0 {
            Op::Read => "fs.read",
            Op::Write => "fs.write",
            Op::Glob => "fs.glob",
            Op::Move => "fs.move",
            Op::Mkdir => "fs.mkdir",
            Op::Delete => "fs.delete",
        };
        Ok(v.classified(name, values))
    }

    async fn call(&self, ctx: &ToolCtx, args: Value) -> Result<ToolOutput, ToolError> {
        let roots = Roots::of(ctx)?;
        let (v, _) = self.verdict(&roots, &args)?;
        recheck(ctx, &v)?;
        match self.0 {
            Op::Read => read(&roots, parse(&args)?),
            Op::Glob => {
                // A wide pattern walks a lot of disk: off the async threads, with a cap.
                let a: GlobArgs = parse(&args)?;
                let r = roots.clone();
                match tokio::time::timeout(
                    GLOB_TIME,
                    tokio::task::spawn_blocking(move || glob(&r, a)),
                )
                .await
                {
                    Ok(Ok(res)) => res,
                    Ok(Err(e)) => Err(ToolError::Failed(format!("glob: {e}"))),
                    Err(_) => Err(ToolError::Failed(format!(
                        "glob took over {} s; narrow the pattern",
                        GLOB_TIME.as_secs()
                    ))),
                }
            }
            Op::Write => write(&roots, parse(&args)?),
            Op::Move => mv(&roots, parse(&args)?),
            Op::Mkdir => {
                let a: PathArgs = parse(&args)?;
                let p = roots.resolve(&a.path)?;
                std::fs::create_dir_all(&p).map_err(|e| io_err("mkdir", &roots, &p, e))?;
                Ok(ToolOutput {
                    text: format!("made {}", text::one_line(&roots.shown(&p), 300)),
                    ..ToolOutput::default()
                })
            }
            Op::Delete => delete(&roots, parse(&args)?),
        }
    }
}

fn read(roots: &Roots, a: ReadArgs) -> Result<ToolOutput, ToolError> {
    let max = util::limit("max_chars", a.max_chars, READ_DEFAULT_CHARS, MAX_CHARS)?;
    let p = roots.resolve(&a.path)?;
    let shown = text::one_line(&roots.shown(&p), 300);
    let meta = std::fs::metadata(&p).map_err(|e| io_err("read", roots, &p, e))?;
    if meta.is_dir() {
        return Err(ToolError::Args(format!(
            "{shown} is a folder; list it with fs.glob"
        )));
    }
    let mut bytes = Vec::new();
    std::fs::File::open(&p)
        .and_then(|f| f.take(READ_MAX_BYTES).read_to_end(&mut bytes))
        .map_err(|e| io_err("read", roots, &p, e))?;
    if bytes.iter().take(8192).any(|&b| b == 0) {
        return Ok(ToolOutput {
            text: format!(
                "{shown} is binary ({} bytes); use pdf.read, xlsx.read or docx.read for documents",
                meta.len()
            ),
            summary: Some(format!("read {shown} (binary)")),
            ..ToolOutput::default()
        });
    }
    let content = String::from_utf8_lossy(&bytes);
    let content = content.strip_prefix('\u{FEFF}').unwrap_or(&content);
    let mut out = if content.is_empty() {
        format!("{shown} is empty")
    } else {
        text::page(content, a.offset.unwrap_or(0), max)
    };
    if meta.len() > READ_MAX_BYTES {
        out.push_str(&format!(
            "\ntruncated: only the first {READ_MAX_BYTES} of {} bytes were loaded",
            meta.len()
        ));
    }
    Ok(ToolOutput {
        text: out,
        summary: Some(format!("read {shown}")),
        consequences: json!({"path": roots.shown(&p)}),
        ..ToolOutput::default()
    })
}

fn glob(roots: &Roots, a: GlobArgs) -> Result<ToolOutput, ToolError> {
    let max = util::limit("max", a.max, GLOB_DEFAULT, GLOB_MAX)?;
    if a.pattern.trim().is_empty() {
        return Err(ToolError::Args("empty pattern".into()));
    }
    let pat = paths::msys_drive(&a.pattern).replace('\\', "/");
    let absolute = Path::new(&pat).is_absolute() || pat.starts_with('/');
    let full = if absolute {
        pat.clone()
    } else {
        format!(
            "{}/{}",
            glob::Pattern::escape(&paths::plain(&roots.base).replace('\\', "/")),
            pat
        )
    };
    let entries = glob::glob(&full).map_err(|e| ToolError::Args(format!("pattern: {e}")))?;
    let mut found: Vec<String> = Vec::new();
    let mut total = 0usize;
    for p in entries.flatten() {
        if total == GLOB_COUNT_MAX {
            break;
        }
        total += 1;
        if found.len() < GLOB_MAX {
            let dir = p.is_dir();
            let p = std::fs::canonicalize(&p).unwrap_or(p);
            let mut s = roots.shown(&p);
            if dir {
                s.push('/');
            }
            found.push(s);
        }
    }
    found.sort();
    let shown_n = found.len().min(max);
    let mut out: Vec<String> = found
        .iter()
        .take(max)
        .map(|s| text::one_line(s, 400))
        .collect();
    if out.is_empty() {
        out.push(format!("no match for {}", text::one_line(&a.pattern, 200)));
    } else if total > shown_n {
        let of = if total == GLOB_COUNT_MAX {
            format!("at least {total}")
        } else {
            total.to_string()
        };
        out.push(format!(
            "truncated: {shown_n} of {of} matches shown; narrow the pattern"
        ));
    }
    Ok(ToolOutput {
        text: out.join("\n"),
        summary: Some(format!(
            "glob {} ({total} matches)",
            text::one_line(&a.pattern, 120)
        )),
        ..ToolOutput::default()
    })
}

fn write(roots: &Roots, a: WriteArgs) -> Result<ToolOutput, ToolError> {
    use std::io::Write;
    let p = roots.resolve(&a.path)?;
    if paths::is_dir(&p) {
        return Err(ToolError::Args(format!(
            "{} is a folder",
            text::one_line(&roots.shown(&p), 300)
        )));
    }
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent).map_err(|e| io_err("mkdir", roots, parent, e))?;
    }
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .append(a.append)
        .truncate(!a.append)
        .open(&p)
        .map_err(|e| io_err("write", roots, &p, e))?;
    f.write_all(a.content.as_bytes())
        .and_then(|_| f.flush())
        .map_err(|e| io_err("write", roots, &p, e))?;
    let shown = text::one_line(&roots.shown(&p), 300);
    Ok(ToolOutput {
        text: format!(
            "wrote: {shown} ({} bytes{})",
            a.content.len(),
            if a.append { ", appended" } else { "" }
        ),
        consequences: json!({"wrote": roots.shown(&p)}),
        files: util::run_files(roots, &[&p]),
        ..ToolOutput::default()
    })
}

fn mv(roots: &Roots, a: MoveArgs) -> Result<ToolOutput, ToolError> {
    let m = plan_move(roots, &a)?;
    if !paths::exists(&m.from) {
        return Err(ToolError::Args(format!(
            "{} does not exist",
            text::one_line(&roots.shown(&m.from), 300)
        )));
    }
    if paths::exists(&m.to) {
        if !a.overwrite {
            return Err(ToolError::Args(format!(
                "{} exists; pass overwrite=true to replace it",
                text::one_line(&roots.shown(&m.to), 300)
            )));
        }
        if paths::is_dir(&m.to) {
            return Err(ToolError::Args(format!(
                "{} is an existing folder; it is not replaced",
                text::one_line(&roots.shown(&m.to), 300)
            )));
        }
    }
    if m.to.starts_with(&m.from) {
        return Err(ToolError::Args("cannot move a folder into itself".into()));
    }
    if let Some(parent) = m.to.parent() {
        std::fs::create_dir_all(parent).map_err(|e| io_err("mkdir", roots, parent, e))?;
    }
    if let Err(e) = std::fs::rename(&m.from, &m.to) {
        // Across volumes rename fails; a file is copied then removed, a folder is not.
        if m.from.is_file() {
            std::fs::copy(&m.from, &m.to).map_err(|e| io_err("copy", roots, &m.from, e))?;
            std::fs::remove_file(&m.from).map_err(|e| io_err("remove", roots, &m.from, e))?;
        } else {
            return Err(io_err("move", roots, &m.from, e));
        }
    }
    Ok(ToolOutput {
        text: format!(
            "moved {} to {}",
            text::one_line(&roots.shown(&m.from), 300),
            text::one_line(&roots.shown(&m.to), 300)
        ),
        consequences: json!({"moved": roots.shown(&m.from), "to": roots.shown(&m.to)}),
        files: util::run_files(roots, &[&m.to]),
        ..ToolOutput::default()
    })
}

fn delete(roots: &Roots, a: DeleteArgs) -> Result<ToolOutput, ToolError> {
    let p = roots.resolve(&a.path)?;
    let shown = text::one_line(&roots.shown(&p), 300);
    let meta = std::fs::symlink_metadata(&p).map_err(|e| io_err("delete", roots, &p, e))?;
    if meta.is_dir() {
        if a.recursive {
            // remove_dir_all does not follow links or junctions inside the folder.
            std::fs::remove_dir_all(&p)
        } else {
            std::fs::remove_dir(&p)
        }
        .map_err(|e| {
            ToolError::Failed(format!(
                "delete {shown}: {e} (a folder with content needs recursive=true)"
            ))
        })?;
    } else {
        std::fs::remove_file(&p).map_err(|e| io_err("delete", roots, &p, e))?;
    }
    Ok(ToolOutput {
        text: format!("deleted {shown}"),
        consequences: json!({"deleted": roots.shown(&p)}),
        ..ToolOutput::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> (tempfile::TempDir, Roots) {
        let t = tempfile::tempdir().unwrap();
        let run = t.path().join("runs").join("2026-10-25T00-00-00-t");
        std::fs::create_dir_all(run.join("files")).unwrap();
        std::fs::create_dir_all(t.path().join("runs").join("x")).unwrap();
        std::fs::write(t.path().join("file.txt"), "keep").unwrap();
        std::fs::write(t.path().join("runs").join("x").join("a.txt"), "a").unwrap();
        let roots = Roots::from_run_dir(&run).unwrap();
        (t, roots)
    }

    fn mv(roots: &Roots, from: &str, to: &str) -> Class {
        let a = MoveArgs {
            from: from.into(),
            to: to.into(),
            overwrite: false,
        };
        plan_move(roots, &a).unwrap().verdict.class
    }

    #[test]
    fn delete_inside_runs_is_read_like_shell_rm_s3() {
        let (_t, r) = setup();
        let p = r.resolve("runs/x/a.txt").unwrap();
        assert_eq!(delete_verdict(&r, &p).class, Class::Read);
        let p = r.resolve("file.txt").unwrap();
        assert_eq!(delete_verdict(&r, &p).class, Class::Delete);
    }

    #[test]
    fn moving_away_outside_runs_is_delete_s4() {
        let (_t, r) = setup();
        assert_eq!(mv(&r, "file.txt", "other.txt"), Class::Delete);
        assert_eq!(mv(&r, "file.txt", "runs/x/"), Class::Delete);
        assert_eq!(mv(&r, "runs/x/a.txt", "runs/x/b.txt"), Class::Fill);
        assert_eq!(mv(&r, "runs/x/a.txt", "new.txt"), Class::Fill);
    }

    #[test]
    fn writing_config_that_runs_programs_is_delete() {
        let (_t, r) = setup();
        for p in [
            ".git/hooks/pre-commit",
            "runs/x/.git/config",
            ".gitattributes",
            "runs/x/.GIT/hooks/post-checkout",
            ".curlrc",
        ] {
            let path = r.resolve(p).unwrap();
            let v = util::write_verdict(&r, &path, false, Class::Fill);
            assert_eq!(v.class, Class::Delete, "{p}");
        }
        let fine = r.resolve("runs/x/git-notes.txt").unwrap();
        assert_eq!(
            util::write_verdict(&r, &fine, false, Class::Fill).class,
            Class::Fill
        );
    }

    #[test]
    fn hard_linked_targets_are_delete_b7() {
        let (_t, r) = setup();
        let h = r.runs.join("x").join("h");
        if std::fs::hard_link(r.base.join("file.txt"), &h).is_err() {
            eprintln!("skipped: no hard links on this file system");
            return;
        }
        let write = |append: bool| util::write_verdict(&r, &h, !append, Class::Fill).class;
        assert_eq!(write(false), Class::Delete);
        assert_eq!(write(true), Class::Delete);
        assert_eq!(delete_verdict(&r, &h).class, Class::Delete);
        assert_eq!(mv(&r, "runs/x/h", "runs/x/h2"), Class::Delete);
        let plain = r.runs.join("x").join("a.txt");
        assert_eq!(
            util::write_verdict(&r, &plain, true, Class::Fill).class,
            Class::Fill
        );
    }
}
