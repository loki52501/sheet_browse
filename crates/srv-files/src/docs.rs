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
//! * docx: `pandoc -t markdown-raw_html` to read and `pandoc -f markdown` to write, as
//!   the docx skill documents, with `--sandbox` (pandoc 2.15+), an empty working folder,
//!   and for docx.write a Lua filter that drops images (a local path or URL in the
//!   model's markdown would otherwise be read or fetched into the document; S8).
//!
//! Configuration: SB_PYTHON, SB_PANDOC, SB_SKILLS_DIR (the folder holding `pdf/`,
//! `xlsx/`, `docx/`, `pptx/`), else found on PATH and under the Claude Code skills folder
//! (`$CLAUDE_CONFIG_DIR/skills`, else `~/.claude/skills`; S7). A missing program, Python
//! package or skills folder is an error or note that names it and how to get it, never
//! a panic. Written documents go to this run's `files/` unless `out` says
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
/// How deep under `<Claude config>/skills` the skills folder is looked for.
const SKILLS_DEPTH: usize = 3;
/// Folders looked at while searching for it.
const SKILLS_SCAN_MAX: usize = 500;
/// The first pandoc with `--sandbox`.
const PANDOC_SANDBOX: (u32, u32) = (2, 15);
/// A Lua filter for docx.write: an image in the model's markdown would make pandoc read a
/// local file or fetch a URL into the document, so images become a marker (S8).
const NO_IMAGES_LUA: &str = "function Image(el) return pandoc.Str('[image omitted]') end\n";

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

    /// The folder holding `pdf/`, `xlsx/`, `docx/`, `pptx/` skills: SB_SKILLS_DIR, else
    /// the first folder (breadth first, sorted) up to [`SKILLS_DEPTH`] below the Claude
    /// Code skills folder ([`claude_skills_root`]) that holds `xlsx/SKILL.md` or
    /// `pdf/SKILL.md`. No machine-specific path is built in (S7).
    pub fn skills(&self) -> Option<PathBuf> {
        if let Some(s) = &self.skills {
            return Some(s.clone());
        }
        find_skills(&claude_skills_root()?)
    }
}

/// `<CLAUDE_CONFIG_DIR or ~/.claude>/skills`.
pub fn claude_skills_root() -> Option<PathBuf> {
    std::env::var_os("CLAUDE_CONFIG_DIR")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|h| h.join(".claude")))
        .map(|c| c.join("skills"))
}

fn find_skills(root: &Path) -> Option<PathBuf> {
    let holds = |d: &Path| {
        d.join("xlsx").join("SKILL.md").is_file() || d.join("pdf").join("SKILL.md").is_file()
    };
    let mut level = vec![root.to_path_buf()];
    let mut seen = 0;
    for _ in 0..=SKILLS_DEPTH {
        let mut next = Vec::new();
        for d in &level {
            seen += 1;
            if seen > SKILLS_SCAN_MAX {
                return None;
            }
            if holds(d) {
                return Some(d.clone());
            }
            if let Ok(rd) = std::fs::read_dir(d) {
                let mut sub: Vec<PathBuf> = rd
                    .flatten()
                    .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
                    .map(|e| e.path())
                    .collect();
                sub.sort();
                next.extend(sub);
            }
        }
        level = next;
    }
    None
}

/// Whether `pandoc` knows `--sandbox` (2.15+), asked once per program (capped like every
/// helper run).
async fn pandoc_has_sandbox(pandoc: &Path) -> bool {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};
    static SEEN: OnceLock<Mutex<HashMap<PathBuf, bool>>> = OnceLock::new();
    let seen = SEEN.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(&b) = seen.lock().unwrap_or_else(|e| e.into_inner()).get(pandoc) {
        return b;
    }
    let mut cmd = tokio::process::Command::new(pandoc);
    cmd.arg("--version");
    let b = match run_capture(cmd, None).await {
        Ok((out, _, exit)) if exit.code == Some(0) => {
            pandoc_version(&out).is_some_and(|v| v >= PANDOC_SANDBOX)
        }
        _ => false,
    };
    seen.lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(pandoc.to_path_buf(), b);
    b
}

/// `(major, minor)` from `pandoc --version` ("pandoc 2.12", "pandoc.exe 3.1.9").
fn pandoc_version(out: &str) -> Option<(u32, u32)> {
    let v = out.lines().next()?.split_whitespace().nth(1)?;
    let mut parts = v.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next().unwrap_or("0").parse().ok()?;
    Some((major, minor))
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
            "Text of a .docx as markdown without raw HTML (pandoc -t markdown-raw_html). At most max_chars (default 8000) from offset.",
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
        // S8: no file reads or fetches beyond the input (pandoc 2.15+); docx.write also
        // drops images through a Lua filter, which older pandoc honours too.
        let sandbox = pandoc_has_sandbox(&pandoc).await;
        let mut cmd = tokio::process::Command::new(&pandoc);
        if sandbox {
            cmd.arg("--sandbox");
        }
        let _filter = if self.op == Op::DocxWrite {
            let f = tempfile::Builder::new()
                .suffix(".lua")
                .tempfile()
                .and_then(|mut f| {
                    std::io::Write::write_all(&mut f, NO_IMAGES_LUA.as_bytes())?;
                    Ok(f)
                })
                .map_err(|e| ToolError::Failed(format!("{}: temp file: {e}", self.op.name())))?;
            cmd.arg(format!("--lua-filter={}", crate::paths::plain(f.path())));
            Some(f)
        } else {
            None
        };
        cmd.args(args);
        // An empty working folder: relative resource paths find nothing.
        let cwd = tempfile::tempdir()
            .map_err(|e| ToolError::Failed(format!("{}: temp folder: {e}", self.op.name())))?;
        cmd.current_dir(cwd.path());
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
                        // No raw HTML for the model (rule 5, S8).
                        "markdown-raw_html".into(),
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
            let root = claude_skills_root()
                .map(|r| crate::paths::plain(&r))
                .unwrap_or_else(|| "the Claude config folder (no home folder)".into());
            return format!("formulas not recalculated: no folder holding xlsx/SKILL.md was found under {root}; set SB_SKILLS_DIR to the folder that holds the xlsx skill");
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pandoc_versions() {
        assert_eq!(pandoc_version("pandoc 2.12\nCompiled with"), Some((2, 12)));
        assert_eq!(pandoc_version("pandoc.exe 3.1.9\n"), Some((3, 1)));
        assert_eq!(pandoc_version("pandoc 3\n"), Some((3, 0)));
        assert_eq!(pandoc_version("garbage"), None);
        assert!(pandoc_version("pandoc 2.12").unwrap() < PANDOC_SANDBOX);
        assert!(pandoc_version("pandoc 2.15").unwrap() >= PANDOC_SANDBOX);
    }

    #[test]
    fn skills_are_found_below_the_claude_folder_s7() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path().join("skills");
        assert_eq!(find_skills(&root), None);
        let synced = root.join("synced").join("a");
        std::fs::create_dir_all(synced.join("xlsx")).unwrap();
        std::fs::write(synced.join("xlsx").join("SKILL.md"), "x").unwrap();
        assert_eq!(find_skills(&root), Some(synced));
        std::fs::create_dir_all(root.join("pdf")).unwrap();
        std::fs::write(root.join("pdf").join("SKILL.md"), "x").unwrap();
        assert_eq!(find_skills(&root), Some(root.clone()), "nearest first");
    }

    /// S8: the filter docx.write passes keeps pandoc from reading a local image or URL.
    #[test]
    fn docx_write_filter_drops_images_s8() {
        let Some(pandoc) = DocsConfig::from_env().pandoc() else {
            eprintln!("skipped: no pandoc (set SB_PANDOC)");
            return;
        };
        let t = tempfile::tempdir().unwrap();
        let lua = t.path().join("f.lua");
        std::fs::write(&lua, NO_IMAGES_LUA).unwrap();
        let img = t.path().join("secret.png");
        std::fs::write(&img, "not really a png").unwrap();
        let md = format!(
            "# T\n\n![alt]({}) and ![web](https://example.invalid/x.png)\n",
            crate::paths::plain(&img).replace('\\', "/")
        );
        let mut child = std::process::Command::new(pandoc)
            .args(["-f", "markdown", "-t", "markdown"])
            .arg(format!("--lua-filter={}", crate::paths::plain(&lua)))
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        std::io::Write::write_all(child.stdin.as_mut().unwrap(), md.as_bytes()).unwrap();
        let out = child.wait_with_output().unwrap();
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(out.status.success(), "{text}");
        assert!(text.contains("image omitted"), "{text}");
        assert!(
            !text.contains("secret.png") && !text.contains("example.invalid"),
            "{text}"
        );
    }

    #[test]
    fn no_machine_path_is_built_in_s7() {
        let src = include_str!("docs.rs");
        assert!(!src.contains(concat!("E:", "/apps")));
    }
}
