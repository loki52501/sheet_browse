//! F-3 document tools: `pdf.read`, `pdf.write`, `md.to_pdf`, `xlsx.read`, `xlsx.write`,
//! `docx.read`, `docx.write`, `pptx.write`. Thin wrappers that shell out; nothing is
//! reimplemented here:
//!
//! * pdf, xlsx, pptx: the recipes the installed skills document (pypdf to read,
//!   reportlab to write PDFs, openpyxl for workbooks, python-pptx for decks), in
//!   `docs_ops.py`, run as `python -c` with the request as JSON on stdin; after a
//!   workbook with formulas is written, the xlsx skill's own `scripts/recalc.py` runs
//!   (it needs LibreOffice; when that is missing the reply says the formulas were not
//!   recalculated).
//! * docx: `pandoc -t markdown` to read and `pandoc -f markdown` to write, as the docx
//!   skill documents.
//!
//! Configuration: SB_PYTHON, SB_PANDOC, SB_SKILLS_DIR (the folder holding `pdf/`,
//! `xlsx/`, `docx/`, `pptx/`), else found on PATH and under the Claude Code synced skills
//! folder. A missing program or Python package is an error that names it and how to get
//! it, never a panic. Written documents go to this run's `files/` unless `out` says
//! otherwise; replacing a file outside `runs/` asks first.

use crate::csv::Table;
use crate::paths::Roots;
use crate::proc::{self, Tap};
use crate::text::{self, MAX_CHARS, READ_DEFAULT_CHARS};
use crate::util::{self, parse, recheck, Verdict};
use async_trait::async_trait;
use harness_core::{Class, Classified, ToolCtx, ToolError, ToolHandler, ToolOutput, ToolSpec};
use serde::Deserialize;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

/// The helper program (see the module docs).
const OPS_PY: &str = include_str!("docs_ops.py");
/// Cap on one document operation.
const OP_TIMEOUT: Duration = Duration::from_secs(120);
/// Largest reply read from a helper.
const REPLY_MAX: usize = 32 * 1024 * 1024;
/// Where Claude Code keeps synced skills on this machine (searched when SB_SKILLS_DIR is
/// unset).
const SYNCED_SKILLS: &str = "E:/apps/Claude/claude-code/skills/synced";

/// Where the helpers are. `None` fields are looked up (see the module docs).
#[derive(Debug, Clone, Default)]
pub struct DocsConfig {
    pub python: Option<PathBuf>,
    pub pandoc: Option<PathBuf>,
    pub skills: Option<PathBuf>,
}

impl DocsConfig {
    pub fn from_env() -> DocsConfig {
        let var = |k: &str| {
            std::env::var_os(k)
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
        };
        DocsConfig {
            python: var("SB_PYTHON"),
            pandoc: var("SB_PANDOC"),
            skills: var("SB_SKILLS_DIR"),
        }
    }

    pub fn python(&self) -> Option<PathBuf> {
        // The WindowsApps `python.exe` is a Store installer stub, not Python.
        let stub = |p: &Path| {
            p.to_string_lossy()
                .to_ascii_lowercase()
                .contains("windowsapps")
        };
        self.python.clone().or_else(|| {
            proc::find_program("python", stub).or_else(|| proc::find_program("python3", stub))
        })
    }

    pub fn pandoc(&self) -> Option<PathBuf> {
        self.pandoc
            .clone()
            .or_else(|| proc::find_program("pandoc", |_| false))
    }

    /// The folder holding `pdf/`, `xlsx/`, `docx/`, `pptx/` skills.
    pub fn skills(&self) -> Option<PathBuf> {
        if let Some(s) = &self.skills {
            return Some(s.clone());
        }
        let mut cands: Vec<PathBuf> = Vec::new();
        if let Ok(rd) = std::fs::read_dir(SYNCED_SKILLS) {
            let mut dirs: Vec<PathBuf> = rd.flatten().map(|e| e.path()).collect();
            dirs.sort();
            cands.extend(dirs);
        }
        let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" });
        if let Some(h) = home {
            cands.push(PathBuf::from(h).join(".claude").join("skills"));
        }
        cands.into_iter().find(|d| {
            d.join("xlsx").join("SKILL.md").is_file() || d.join("pdf").join("SKILL.md").is_file()
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Op {
    PdfRead,
    PdfWrite,
    MdToPdf,
    XlsxRead,
    XlsxWrite,
    DocxRead,
    DocxWrite,
    PptxWrite,
}

impl Op {
    fn name(self) -> &'static str {
        match self {
            Op::PdfRead => "pdf.read",
            Op::PdfWrite => "pdf.write",
            Op::MdToPdf => "md.to_pdf",
            Op::XlsxRead => "xlsx.read",
            Op::XlsxWrite => "xlsx.write",
            Op::DocxRead => "docx.read",
            Op::DocxWrite => "docx.write",
            Op::PptxWrite => "pptx.write",
        }
    }

    fn skill(self) -> &'static str {
        match self {
            Op::PdfRead | Op::PdfWrite | Op::MdToPdf => "pdf",
            Op::XlsxRead | Op::XlsxWrite => "xlsx",
            Op::DocxRead | Op::DocxWrite => "docx",
            Op::PptxWrite => "pptx",
        }
    }

    /// (extension, default file stem) of written documents.
    fn output(self) -> Option<(&'static str, &'static str)> {
        match self {
            Op::PdfWrite | Op::MdToPdf => Some(("pdf", "document")),
            Op::XlsxWrite => Some(("xlsx", "workbook")),
            Op::DocxWrite => Some(("docx", "document")),
            Op::PptxWrite => Some(("pptx", "deck")),
            _ => None,
        }
    }
}

struct Docs {
    op: Op,
    cfg: DocsConfig,
}

pub fn tools() -> Vec<ToolSpec> {
    tools_with(DocsConfig::from_env())
}

/// The document tools with explicit helper locations (tests).
pub fn tools_with(cfg: DocsConfig) -> Vec<ToolSpec> {
    let read_props = |extra: Value| {
        let mut p = json!({
            "path":{"type":"string"},
            "offset":{"type":"integer","minimum":0},
            "max_chars":{"type":"integer","minimum":1,"maximum":MAX_CHARS}
        });
        if let (Some(o), Some(e)) = (p.as_object_mut(), extra.as_object()) {
            o.extend(e.clone());
        }
        json!({"type":"object","properties":p,"required":["path"],"additionalProperties":false})
    };
    let out = json!({"type":"string","description":"output file; a bare name lands in this run's files/"});
    let specs: Vec<(Op, &str, Class, Value)> = vec![
        (
            Op::PdfRead,
            "Text of a PDF (pypdf), page by page; pages like \"1-3,7\". At most max_chars (default 8000) from offset.",
            Class::Read,
            read_props(json!({"pages":{"type":"string"}})),
        ),
        (
            Op::PdfWrite,
            "Write a PDF from light markdown (# headings, - bullets, **bold**, ``` code) with reportlab. Default out: files/document.pdf.",
            Class::Fill,
            json!({"type":"object","properties":{"text":{"type":"string"},"title":{"type":"string"},"out":out},
                "required":["text"],"additionalProperties":false}),
        ),
        (
            Op::MdToPdf,
            "Render a markdown file to PDF (same renderer as pdf.write). Default out: files/<name>.pdf.",
            Class::Fill,
            json!({"type":"object","properties":{"path":{"type":"string"},"title":{"type":"string"},"out":out},
                "required":["path"],"additionalProperties":false}),
        ),
        (
            Op::XlsxRead,
            "Cell values of a workbook (openpyxl), one `## sheet` block per sheet, cells joined by |; max_rows per sheet (default 200).",
            Class::Read,
            read_props(json!({"sheet":{"type":"string"},"max_rows":{"type":"integer","minimum":1,"maximum":5000}})),
        ),
        (
            Op::XlsxWrite,
            "Write a workbook (openpyxl): sheets [{name, rows: [[cell...]...], header: true}]. Strings starting with = are formulas (recalculated with the xlsx skill's recalc.py when LibreOffice is present). Default out: files/workbook.xlsx.",
            Class::Fill,
            json!({"type":"object","properties":{
                "sheets":{"type":"array","items":{"type":"object","properties":{
                    "name":{"type":"string"},
                    "rows":{"type":"array","items":{"type":"array"}},
                    "header":{"type":"boolean","default":true}
                },"required":["rows"]}},
                "out":out
            },"required":["sheets"],"additionalProperties":false}),
        ),
        (
            Op::DocxRead,
            "Text of a .docx as markdown (pandoc -t markdown). At most max_chars (default 8000) from offset.",
            Class::Read,
            read_props(json!({})),
        ),
        (
            Op::DocxWrite,
            "Write a .docx from markdown (pandoc). Default out: files/document.docx.",
            Class::Fill,
            json!({"type":"object","properties":{"markdown":{"type":"string"},"out":out},
                "required":["markdown"],"additionalProperties":false}),
        ),
        (
            Op::PptxWrite,
            "Write a .pptx (python-pptx): optional title slide, then slides [{title, bullets: [..], notes}]. Default out: files/deck.pptx.",
            Class::Fill,
            json!({"type":"object","properties":{
                "title":{"type":"string"},
                "subtitle":{"type":"string"},
                "slides":{"type":"array","items":{"type":"object","properties":{
                    "title":{"type":"string"},
                    "bullets":{"type":"array","items":{"type":"string"}},
                    "notes":{"type":"string"}
                }}},
                "out":out
            },"additionalProperties":false}),
        ),
    ];
    specs
        .into_iter()
        .map(|(op, description, class, args)| ToolSpec {
            name: op.name().into(),
            description: description.into(),
            class,
            args,
            handler: Arc::new(Docs {
                op,
                cfg: cfg.clone(),
            }),
        })
        .collect()
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadArgs {
    path: String,
    pages: Option<String>,
    sheet: Option<String>,
    max_rows: Option<usize>,
    offset: Option<usize>,
    max_chars: Option<usize>,
}

/// Fields of the write tools; each tool's schema allows only its own.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WriteArgs {
    out: Option<String>,
    text: Option<String>,
    title: Option<String>,
    subtitle: Option<String>,
    path: Option<String>,
    sheets: Option<Value>,
    markdown: Option<String>,
    slides: Option<Value>,
}

impl Docs {
    /// The output path of a write tool and its verdict.
    fn plan_out(&self, roots: &Roots, a: &WriteArgs) -> Result<(PathBuf, Verdict), ToolError> {
        let Some((ext, stem)) = self.op.output() else {
            return Err(ToolError::Args("not a write tool".into()));
        };
        let p = match &a.out {
            Some(o) => {
                let p = roots.resolve_out(o)?;
                if util::ext(&p) != ext {
                    return Err(ToolError::Args(format!("out must end in .{ext}")));
                }
                p
            }
            None => {
                let stem = match (self.op, &a.path) {
                    (Op::MdToPdf, Some(src)) => Path::new(src)
                        .file_stem()
                        .map(|s| s.to_string_lossy().into_owned())
                        .unwrap_or_else(|| stem.into()),
                    _ => stem.to_string(),
                };
                util::fresh_name(&roots.files, &stem, ext)
            }
        };
        let v = util::write_verdict(roots, &p, true, Class::Fill);
        Ok((p, v))
    }

    fn missing(&self, what: &str) -> ToolError {
        let skill = match self.cfg.skills() {
            Some(s) => format!(
                "; see {}",
                crate::paths::plain(&s.join(self.op.skill()).join("SKILL.md"))
            ),
            None => String::new(),
        };
        ToolError::Failed(format!("{} needs {what}{skill}", self.op.name()))
    }

    /// Runs one `docs_ops.py` request.
    async fn py(&self, req: Value) -> Result<Value, ToolError> {
        run_py(&self.cfg, req).await.map_err(|e| match e {
            PyError::NoPython => self.missing("Python 3 (install it or set SB_PYTHON)"),
            PyError::Package(p) => {
                self.missing(&format!("the Python package {p} (pip install {p})"))
            }
            PyError::Failed(m) => ToolError::Failed(format!("{}: {m}", self.op.name())),
        })
    }

    async fn pandoc(&self, args: &[String], stdin: Option<Vec<u8>>) -> Result<String, ToolError> {
        let Some(pandoc) = self.cfg.pandoc() else {
            return Err(self.missing("pandoc (install it or set SB_PANDOC)"));
        };
        let mut cmd = tokio::process::Command::new(pandoc);
        cmd.args(args);
        let (out, err, exit) = run_capture(cmd, stdin).await.map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                self.missing("pandoc (install it or set SB_PANDOC)")
            } else {
                ToolError::Failed(format!("{}: could not run pandoc: {e}", self.op.name()))
            }
        })?;
        if exit.timed_out {
            return Err(ToolError::Failed(format!(
                "{}: pandoc did not finish in {} s",
                self.op.name(),
                OP_TIMEOUT.as_secs()
            )));
        }
        if exit.code != Some(0) {
            return Err(ToolError::Failed(format!(
                "{}: pandoc failed: {}",
                self.op.name(),
                text::one_line(text::tail_chars(&err, 400), 400)
            )));
        }
        Ok(out)
    }
}

enum PyError {
    NoPython,
    Package(String),
    Failed(String),
}

/// Runs a child to completion with stdin, capturing stdout and stderr (capped).
async fn run_capture(
    cmd: tokio::process::Command,
    stdin: Option<Vec<u8>>,
) -> std::io::Result<(String, String, proc::Exit)> {
    let out = Tap::shared(REPLY_MAX, None);
    let err = Tap::shared(16 * 1024, None);
    let exit = proc::run(cmd, stdin, OP_TIMEOUT, out.clone(), err.clone()).await?;
    let o = out.lock().unwrap_or_else(|e| e.into_inner());
    if o.cut() {
        return Err(std::io::Error::other("the helper's output is too large"));
    }
    let e = err.lock().unwrap_or_else(|e| e.into_inner());
    Ok((o.text(), e.text(), exit))
}

async fn run_py(cfg: &DocsConfig, req: Value) -> Result<Value, PyError> {
    let python = cfg.python().ok_or(PyError::NoPython)?;
    // An empty working folder and PYTHONSAFEPATH: nothing next to the documents can be
    // imported in place of a real module.
    let cwd = tempfile::tempdir().map_err(|e| PyError::Failed(format!("temp folder: {e}")))?;
    let mut cmd = tokio::process::Command::new(&python);
    cmd.arg("-c")
        .arg(OPS_PY)
        .current_dir(cwd.path())
        .env("PYTHONSAFEPATH", "1")
        .env("PYTHONIOENCODING", "utf-8")
        .env("PYTHONUTF8", "1");
    let body = serde_json::to_vec(&req).map_err(|e| PyError::Failed(e.to_string()))?;
    let (out, err, exit) = run_capture(cmd, Some(body)).await.map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            PyError::NoPython
        } else {
            PyError::Failed(format!("could not run python: {e}"))
        }
    })?;
    if exit.timed_out {
        return Err(PyError::Failed(format!(
            "python did not finish in {} s",
            OP_TIMEOUT.as_secs()
        )));
    }
    let reply: Value = match serde_json::from_str(out.trim()) {
        Ok(v) => v,
        Err(_) => {
            return Err(PyError::Failed(format!(
                "python failed: {}",
                text::one_line(text::tail_chars(&err, 400), 400)
            )))
        }
    };
    if reply.get("ok").and_then(Value::as_bool) == Some(true) {
        return Ok(reply);
    }
    if let Some(m) = reply.get("missing").and_then(Value::as_str) {
        return Err(PyError::Package(text::one_line(m, 60)));
    }
    let e = reply
        .get("error")
        .and_then(Value::as_str)
        .unwrap_or("unknown error");
    Err(PyError::Failed(text::one_line(e, 500)))
}

/// A workbook sheet as a table (first sheet, or `sheet`), for `diff` table mode.
pub async fn xlsx_table(p: &Path, sheet: Option<&str>) -> Result<Table, ToolError> {
    let cfg = DocsConfig::from_env();
    let reply = run_py(
        &cfg,
        json!({"op":"xlsx.rows","path":crate::paths::plain(p),"sheet":sheet}),
    )
    .await
    .map_err(|e| match e {
        PyError::NoPython => {
            ToolError::Failed("reading xlsx needs Python 3 (set SB_PYTHON)".into())
        }
        PyError::Package(p) => ToolError::Failed(format!(
            "reading xlsx needs the Python package {p} (pip install {p})"
        )),
        PyError::Failed(m) => ToolError::Failed(format!("xlsx: {m}")),
    })?;
    let cells = |v: &Value| -> Vec<String> {
        v.as_array()
            .map(|a| {
                a.iter()
                    .map(|c| c.as_str().unwrap_or("").to_string())
                    .collect()
            })
            .unwrap_or_default()
    };
    let header = reply.get("header").map(cells).unwrap_or_default();
    let rows = reply
        .get("rows")
        .and_then(Value::as_array)
        .map(|rs| {
            rs.iter()
                .enumerate()
                .map(|(i, r)| (i as u64 + 2, cells(r)))
                .collect()
        })
        .unwrap_or_default();
    Ok(Table { header, rows })
}

#[async_trait]
impl ToolHandler for Docs {
    async fn classify(&self, ctx: &ToolCtx, args: &Value) -> Result<Classified, ToolError> {
        let roots = Roots::of(ctx)?;
        if self.op.output().is_none() {
            let a: ReadArgs = parse(args)?;
            let p = roots.resolve(&a.path)?;
            return Ok(Classified {
                class: Some(Class::Read),
                values: vec![("path".into(), roots.shown(&p))],
                ..Classified::default()
            });
        }
        let a: WriteArgs = parse(args)?;
        let (p, v) = self.plan_out(&roots, &a)?;
        Ok(v.classified(self.op.name(), vec![("out".into(), roots.shown(&p))]))
    }

    async fn call(&self, ctx: &ToolCtx, args: Value) -> Result<ToolOutput, ToolError> {
        let roots = Roots::of(ctx)?;
        if self.op.output().is_none() {
            return self.read(&roots, parse(&args)?).await;
        }
        let a: WriteArgs = parse(&args)?;
        let (out, v) = self.plan_out(&roots, &a)?;
        recheck(ctx, &v)?;
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| ToolError::Failed(format!("mkdir: {e}")))?;
        }
        let out_s = crate::paths::plain(&out);
        let mut note = String::new();
        match self.op {
            Op::PdfWrite => {
                self.py(json!({"op":"pdf.write","out":out_s,"text":a.text.unwrap_or_default(),"title":a.title}))
                    .await?;
            }
            Op::MdToPdf => {
                let src = roots.resolve(a.path.as_deref().unwrap_or(""))?;
                self.py(json!({"op":"md.to_pdf","out":out_s,"path":crate::paths::plain(&src),"title":a.title}))
                    .await?;
            }
            Op::XlsxWrite => {
                let reply = self
                    .py(json!({"op":"xlsx.write","out":out_s,"sheets":a.sheets.unwrap_or(Value::Null)}))
                    .await?;
                if reply.get("formulas").and_then(Value::as_bool) == Some(true) {
                    note = self.recalc(&out).await;
                }
            }
            Op::DocxWrite => {
                self.pandoc(
                    &[
                        "-f".into(),
                        "markdown".into(),
                        "-t".into(),
                        "docx".into(),
                        "-o".into(),
                        out_s.clone(),
                    ],
                    Some(a.markdown.unwrap_or_default().into_bytes()),
                )
                .await?;
            }
            Op::PptxWrite => {
                self.py(json!({"op":"pptx.write","out":out_s,"title":a.title,"subtitle":a.subtitle,"slides":a.slides}))
                    .await?;
            }
            _ => {}
        }
        let shown = text::one_line(&roots.shown(&out), 300);
        Ok(ToolOutput {
            text: if note.is_empty() {
                format!("wrote: {shown}")
            } else {
                format!("wrote: {shown}\n{note}")
            },
            consequences: json!({"wrote": roots.shown(&out)}),
            files: util::run_files(&roots, &[&out]),
            ..ToolOutput::default()
        })
    }
}

impl Docs {
    async fn read(&self, roots: &Roots, a: ReadArgs) -> Result<ToolOutput, ToolError> {
        let max = util::limit("max_chars", a.max_chars, READ_DEFAULT_CHARS, MAX_CHARS)?;
        let p = roots.resolve(&a.path)?;
        if !p.is_file() {
            return Err(ToolError::Args(format!(
                "{} is not a file",
                text::one_line(&roots.shown(&p), 300)
            )));
        }
        let ps = crate::paths::plain(&p);
        let (body, head) = match self.op {
            Op::PdfRead => {
                let r = self
                    .py(json!({"op":"pdf.read","path":ps,"pages":a.pages}))
                    .await?;
                let pages = r.get("pages").and_then(Value::as_u64).unwrap_or(0);
                (str_of(&r, "text"), format!("{pages} pages"))
            }
            Op::XlsxRead => {
                let r = self
                    .py(json!({"op":"xlsx.read","path":ps,"sheet":a.sheet,"max_rows":a.max_rows.unwrap_or(200).clamp(1, 5000)}))
                    .await?;
                let sheets = r
                    .get("sheets")
                    .and_then(Value::as_array)
                    .map(|s| s.len())
                    .unwrap_or(0);
                (str_of(&r, "text"), format!("{sheets} sheets"))
            }
            Op::DocxRead => (
                self.pandoc(
                    &[
                        "-t".into(),
                        "markdown".into(),
                        "--wrap=none".into(),
                        ps.clone(),
                    ],
                    None,
                )
                .await?,
                "markdown".to_string(),
            ),
            _ => return Err(ToolError::Args("not a read tool".into())),
        };
        let shown = text::one_line(&roots.shown(&p), 300);
        let body = text::page(&body, a.offset.unwrap_or(0), max);
        Ok(ToolOutput {
            text: format!("{}: {shown} ({head})\n{body}", self.op.name()),
            summary: Some(format!("{} {shown}", self.op.name())),
            consequences: json!({"path": roots.shown(&p)}),
            ..ToolOutput::default()
        })
    }

    /// Runs the xlsx skill's recalc.py on `out`; returns a note for the model.
    async fn recalc(&self, out: &Path) -> String {
        let Some(skills) = self.cfg.skills() else {
            return "formulas not recalculated: the xlsx skill folder was not found (set SB_SKILLS_DIR)".into();
        };
        let script = skills.join("xlsx").join("scripts").join("recalc.py");
        if !script.is_file() {
            return format!(
                "formulas not recalculated: {} is missing",
                crate::paths::plain(&script)
            );
        }
        let Some(python) = self.cfg.python() else {
            return "formulas not recalculated: Python 3 was not found".into();
        };
        let cwd = match tempfile::tempdir() {
            Ok(d) => d,
            Err(e) => return format!("formulas not recalculated: temp folder: {e}"),
        };
        let mut cmd = tokio::process::Command::new(python);
        cmd.arg(&script)
            .arg(crate::paths::plain(out))
            .arg("60")
            .current_dir(cwd.path())
            .env("PYTHONIOENCODING", "utf-8");
        match run_capture(cmd, None).await {
            Ok((stdout, stderr, exit)) => {
                if exit.timed_out {
                    return "formulas not recalculated: recalc.py timed out".into();
                }
                match serde_json::from_str::<Value>(stdout.trim()) {
                    Ok(v) if v.get("error").is_some() => format!(
                        "formulas not recalculated: {}",
                        text::one_line(&str_of(&v, "error"), 300)
                    ),
                    Ok(v) => format!(
                        "recalculated with the xlsx skill: {}",
                        text::one_line(&v.to_string(), 300)
                    ),
                    Err(_) => format!(
                        "formulas not recalculated: {}",
                        text::one_line(text::tail_chars(&format!("{stdout} {stderr}"), 300), 300)
                    ),
                }
            }
            Err(e) => format!("formulas not recalculated: {e}"),
        }
    }
}

fn str_of(v: &Value, k: &str) -> String {
    v.get(k).and_then(Value::as_str).unwrap_or("").to_string()
}
