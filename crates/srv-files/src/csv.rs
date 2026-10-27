//! F-4 `csv.clean(path, rules, out?)`: normalise fields, drop duplicates, flag missing
//! values; writes a new file and returns a short report. The input is never changed.
//!
//! Rules (all optional): `trim` (default true), `collapse_spaces` (default true: runs of
//! whitespace inside a cell become one space), `case` (`{column: lower|upper|title}`),
//! `key` (dedupe key columns; default: the whole row), `dedupe` (default true), `required`
//! (columns that must not be blank; default: the key columns), `drop_empty_rows` (default
//! true), `delimiter` (default `,`, or tab for `.tsv`). Duplicate keys compare after
//! normalising, ignoring ASCII case. Rows missing a required value are kept and named in an
//! added `_missing` column. The output defaults to `files/<name>.clean.csv`; a bare `out`
//! lands in this run's `files/` and a bare input name not found in the base folder is read
//! from there (B-564, D-570, D-571). Classify and call use one `out_path`.

use crate::paths::Roots;
use crate::text;
use crate::util::{self, parse, recheck, Verdict};
use async_trait::async_trait;
use harness_core::{Class, Classified, ToolCtx, ToolError, ToolHandler, ToolOutput, ToolSpec};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Duplicates listed by line in the report.
const DUP_SHOWN: usize = 5;
/// Column name of the missing-value flags.
pub const MISSING_COL: &str = "_missing";

pub fn tools() -> Vec<ToolSpec> {
    vec![ToolSpec {
        name: "csv.clean".into(),
        description: "Dedupe and normalise a CSV/TSV into a new file; use it instead of deduping rows yourself. Drops duplicate rows by rules.key (the columns that identify a row, e.g. [\"email\"]; default the whole row; compared trimmed, spaces collapsed, ASCII case ignored), trims and collapses spaces, sets case per column, flags rows missing rules.required values (default the key columns) in a _missing column. Returns a short report (rows in/out, duplicates by line). A bare path not found next to runs/ is read from this run's files/. out: a bare name lands in this run's files/ (default files/<name>.clean.csv) and is listed in the run report. rules: {key, dedupe, required, trim, collapse_spaces, case:{col:lower|upper|title}, drop_empty_rows, delimiter}.".into(),
        class: Class::Fill,
        args: json!({"type":"object","properties":{
            "path":{"type":"string"},
            "rules":{"type":"object","properties":{
                "key":{"type":"array","items":{"type":"string"},"description":"columns that identify a row for dedupe; default the whole row"},
                "dedupe":{"type":"boolean","default":true},
                "required":{"type":"array","items":{"type":"string"}},
                "trim":{"type":"boolean","default":true},
                "collapse_spaces":{"type":"boolean","default":true},
                "case":{"type":"object","additionalProperties":{"enum":["lower","upper","title"]}},
                "drop_empty_rows":{"type":"boolean","default":true},
                "delimiter":{"type":"string","maxLength":1}
            },"additionalProperties":false},
            "out":{"type":"string","description":"output file; a bare name lands in this run's files/"}
        },"required":["path"],"additionalProperties":false}),
        handler: Arc::new(Clean),
    }]
}

struct Clean;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Args {
    path: String,
    #[serde(default)]
    rules: Rules,
    out: Option<String>,
}

#[derive(Deserialize, Default, Debug, Clone)]
#[serde(deny_unknown_fields, default)]
pub struct Rules {
    pub key: Vec<String>,
    pub dedupe: Option<bool>,
    pub required: Option<Vec<String>>,
    pub trim: Option<bool>,
    pub collapse_spaces: Option<bool>,
    pub case: BTreeMap<String, String>,
    pub drop_empty_rows: Option<bool>,
    pub delimiter: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Case {
    Lower,
    Upper,
    Title,
}

/// Where the cleaned file goes.
fn out_path(roots: &Roots, a: &Args) -> Result<(PathBuf, PathBuf), ToolError> {
    let input = roots.resolve_existing(&a.path)?;
    let out = match &a.out {
        Some(o) => roots.resolve_out(o)?,
        None => {
            let stem = input
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "data".into());
            let ext = match util::ext(&input).as_str() {
                "tsv" => "tsv",
                _ => "csv",
            };
            util::fresh_name(&roots.files, &format!("{stem}.clean"), ext)
        }
    };
    Ok((input, out))
}

#[async_trait]
impl ToolHandler for Clean {
    async fn classify(&self, ctx: &ToolCtx, args: &Value) -> Result<Classified, ToolError> {
        let roots = Roots::of(ctx)?;
        let a: Args = parse(args)?;
        let (input, out) = out_path(&roots, &a)?;
        let v = util::write_verdict(&roots, &out, true, Class::Fill);
        Ok(v.classified(
            "csv.clean",
            vec![
                ("path".into(), roots.shown(&input)),
                ("out".into(), roots.shown(&out)),
            ],
        ))
    }

    async fn call(&self, ctx: &ToolCtx, args: Value) -> Result<ToolOutput, ToolError> {
        let roots = Roots::of(ctx)?;
        let a: Args = parse(&args)?;
        let (input, out) = out_path(&roots, &a)?;
        let v: Verdict = util::write_verdict(&roots, &out, true, Class::Fill);
        recheck(ctx, &v)?;
        if input == out {
            return Err(ToolError::Args("out must differ from path".into()));
        }
        let delim = delimiter(&a.rules, &input)?;
        let table = read_table(&input, delim)
            .map_err(|e| ToolError::Failed(format!("read {}: {e}", roots.shown(&input))))?;
        let cleaned = clean(table, &a.rules)?;
        if let Some(parent) = out.parent() {
            crate::safeio::create_dirs(parent)
                .map_err(|e| ToolError::Failed(format!("mkdir: {e}")))?;
        }
        // An out file hard-linked only after the check is refused (D-553), as fs.write.
        let links_ok = v.class >= Class::Delete;
        write_table(&out, delim, &cleaned.header, &cleaned.rows, links_ok)
            .map_err(|e| ToolError::Failed(format!("write {}: {e}", roots.shown(&out))))?;
        let report = cleaned.report(&text::one_line(&roots.shown(&out), 300));
        Ok(ToolOutput {
            text: report,
            summary: Some(format!(
                "csv.clean {}: {} rows in, {} out",
                text::one_line(&roots.shown(&input), 120),
                cleaned.rows_in,
                cleaned.rows.len()
            )),
            consequences: json!({"wrote": roots.shown(&out)}),
            files: util::run_files(&roots, &[&out]),
            stopped: None,
        })
    }
}

fn delimiter(rules: &Rules, input: &Path) -> Result<u8, ToolError> {
    match rules.delimiter.as_deref() {
        Some("\\t") | Some("\t") => Ok(b'\t'),
        Some(d) if d.len() == 1 && d.is_ascii() => Ok(d.as_bytes()[0]),
        Some(_) => Err(ToolError::Args(
            "delimiter must be one ASCII character".into(),
        )),
        None if util::ext(input) == "tsv" => Ok(b'\t'),
        None => Ok(b','),
    }
}

/// A header and rows of cells (ragged rows padded to the header width).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Table {
    pub header: Vec<String>,
    /// (1-based line of the record, cells).
    pub rows: Vec<(u64, Vec<String>)>,
}

/// Reads a delimited file; the first record is the header. Invalid UTF-8 is replaced.
pub fn read_table(path: &Path, delim: u8) -> Result<Table, String> {
    let mut rdr = ::csv::ReaderBuilder::new()
        .has_headers(false)
        .flexible(true)
        .delimiter(delim)
        .from_path(path)
        .map_err(|e| e.to_string())?;
    let mut t = Table::default();
    let mut rec = ::csv::ByteRecord::new();
    let mut first = true;
    loop {
        match rdr.read_byte_record(&mut rec) {
            Ok(false) => break,
            Ok(true) => {}
            Err(e) => return Err(e.to_string()),
        }
        let line = rec.position().map(|p| p.line()).unwrap_or(0);
        let cells: Vec<String> = rec
            .iter()
            .map(|c| String::from_utf8_lossy(c).into_owned())
            .collect();
        if first {
            first = false;
            t.header = cells
                .into_iter()
                .enumerate()
                .map(|(i, h)| {
                    let h = h.trim_start_matches('\u{FEFF}').trim().to_string();
                    if h.is_empty() {
                        format!("col{}", i + 1)
                    } else {
                        h
                    }
                })
                .collect();
            continue;
        }
        t.rows.push((line, cells));
    }
    let width = t
        .rows
        .iter()
        .map(|(_, r)| r.len())
        .chain(std::iter::once(t.header.len()))
        .max()
        .unwrap_or(0);
    while t.header.len() < width {
        let n = t.header.len() + 1;
        t.header.push(format!("col{n}"));
    }
    for (_, r) in &mut t.rows {
        r.resize(width, String::new());
    }
    Ok(t)
}

fn write_table(
    path: &Path,
    delim: u8,
    header: &[String],
    rows: &[Vec<String>],
    links_ok: bool,
) -> Result<(), String> {
    // Opened, then checked to be the classified file, before it is replaced (D-549), and
    // refused when it gained a hard link the approved class did not count (D-553).
    let f = crate::safeio::open_write(path, false).map_err(|e| e.to_string())?;
    if !crate::safeio::links_ok(&f, links_ok) {
        return Err(format!(
            "not written: {} gained another hard link after the check; call it again",
            crate::paths::plain(path)
        ));
    }
    let mut w = ::csv::WriterBuilder::new().delimiter(delim).from_writer(f);
    w.write_record(header).map_err(|e| e.to_string())?;
    for r in rows {
        w.write_record(r).map_err(|e| e.to_string())?;
    }
    w.flush().map_err(|e| e.to_string())
}

/// The cleaned table and what happened.
#[derive(Debug, Default)]
pub struct Cleaned {
    pub header: Vec<String>,
    pub rows: Vec<Vec<String>>,
    pub rows_in: usize,
    pub empty_dropped: usize,
    /// (line of the dropped row, line of the row it repeats, shown key).
    pub duplicates: Vec<(u64, u64, String)>,
    pub key_names: Vec<String>,
    /// required column -> rows missing it.
    pub missing: Vec<(String, usize)>,
    pub rows_flagged: usize,
    /// column -> blank cells (after cleaning), only columns with blanks.
    pub blanks: Vec<(String, usize)>,
}

fn column_index(header: &[String], name: &str) -> Result<usize, ToolError> {
    header
        .iter()
        .position(|h| h == name)
        .or_else(|| header.iter().position(|h| h.eq_ignore_ascii_case(name)))
        .ok_or_else(|| {
            ToolError::Args(format!(
                "no column {:?}; columns: {}",
                name,
                text::one_line(&header.join(", "), 400)
            ))
        })
}

fn normalise(cell: &str, trim: bool, collapse: bool, case: Option<Case>) -> String {
    let mut s = if collapse {
        let mut out = String::with_capacity(cell.len());
        let mut last_space = false;
        for c in cell.chars() {
            if c.is_whitespace() {
                if !last_space {
                    out.push(' ');
                }
                last_space = true;
            } else {
                out.push(c);
                last_space = false;
            }
        }
        out
    } else {
        cell.to_string()
    };
    if trim || collapse {
        s = s.trim().to_string();
    }
    match case {
        Some(Case::Lower) => s.to_lowercase(),
        Some(Case::Upper) => s.to_uppercase(),
        Some(Case::Title) => {
            let mut out = String::with_capacity(s.len());
            let mut start = true;
            for c in s.chars() {
                if c.is_whitespace() || c == '-' {
                    start = true;
                    out.push(c);
                } else if start {
                    out.extend(c.to_uppercase());
                    start = false;
                } else {
                    out.extend(c.to_lowercase());
                }
            }
            out
        }
        None => s,
    }
}

/// Applies `rules` to `t` (see the module docs).
pub fn clean(t: Table, rules: &Rules) -> Result<Cleaned, ToolError> {
    let trim = rules.trim.unwrap_or(true);
    let collapse = rules.collapse_spaces.unwrap_or(true);
    let mut cases: HashMap<usize, Case> = HashMap::new();
    for (col, c) in &rules.case {
        let idx = column_index(&t.header, col)?;
        let c = match c.to_ascii_lowercase().as_str() {
            "lower" => Case::Lower,
            "upper" => Case::Upper,
            "title" => Case::Title,
            other => {
                return Err(ToolError::Args(format!(
                    "case {:?}: use lower, upper or title",
                    text::one_line(other, 20)
                )))
            }
        };
        cases.insert(idx, c);
    }
    let key: Vec<usize> = rules
        .key
        .iter()
        .map(|k| column_index(&t.header, k))
        .collect::<Result<_, _>>()?;
    let required: Vec<usize> = match &rules.required {
        Some(r) => r
            .iter()
            .map(|k| column_index(&t.header, k))
            .collect::<Result<_, _>>()?,
        None => key.clone(),
    };
    let dedupe = rules.dedupe.unwrap_or(true);
    let drop_empty = rules.drop_empty_rows.unwrap_or(true);

    let mut out = Cleaned {
        rows_in: t.rows.len(),
        key_names: key.iter().map(|&i| t.header[i].clone()).collect(),
        ..Cleaned::default()
    };
    let mut seen: HashMap<Vec<String>, u64> = HashMap::new();
    let mut missing_counts = vec![0usize; t.header.len()];
    let mut blank_counts = vec![0usize; t.header.len()];
    let flag = !required.is_empty();
    for (line, cells) in t.rows {
        let cells: Vec<String> = cells
            .iter()
            .enumerate()
            .map(|(i, c)| normalise(c, trim, collapse, cases.get(&i).copied()))
            .collect();
        if drop_empty && cells.iter().all(|c| c.trim().is_empty()) {
            out.empty_dropped += 1;
            continue;
        }
        if dedupe {
            let k: Vec<String> = if key.is_empty() {
                cells.iter().map(|c| fold(c)).collect()
            } else {
                key.iter().map(|&i| fold(&cells[i])).collect()
            };
            if let Some(&first) = seen.get(&k) {
                let shown = if key.is_empty() {
                    "whole row".to_string()
                } else {
                    key.iter()
                        .map(|&i| format!("{} {:?}", out.header_name(&t.header, i), cells[i]))
                        .collect::<Vec<_>>()
                        .join(", ")
                };
                out.duplicates.push((line, first, shown));
                continue;
            }
            seen.insert(k, line);
        }
        let mut miss: Vec<&str> = Vec::new();
        for &i in &required {
            if cells[i].trim().is_empty() {
                missing_counts[i] += 1;
                miss.push(&t.header[i]);
            }
        }
        for (i, c) in cells.iter().enumerate() {
            if c.trim().is_empty() {
                blank_counts[i] += 1;
            }
        }
        let mut row = cells.clone();
        if flag {
            if !miss.is_empty() {
                out.rows_flagged += 1;
            }
            row.push(miss.join(";"));
        }
        out.rows.push(row);
    }
    out.header = t.header.clone();
    if flag {
        out.header.push(MISSING_COL.into());
    }
    out.missing = required
        .iter()
        .map(|&i| (t.header[i].clone(), missing_counts[i]))
        .filter(|(_, n)| *n > 0)
        .collect();
    out.blanks = t
        .header
        .iter()
        .zip(blank_counts)
        .filter(|(_, n)| *n > 0)
        .map(|(h, n)| (h.clone(), n))
        .collect();
    Ok(out)
}

/// Dedupe comparison: whitespace collapsed, trimmed, ASCII case folded.
fn fold(s: &str) -> String {
    s.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase()
}

impl Cleaned {
    fn header_name<'a>(&self, header: &'a [String], i: usize) -> &'a str {
        header.get(i).map(String::as_str).unwrap_or("?")
    }

    /// The short report the model reads (cell values escaped and capped).
    pub fn report(&self, out_shown: &str) -> String {
        let mut lines = vec![format!("wrote: {out_shown}")];
        let key = if self.key_names.is_empty() {
            "whole row".to_string()
        } else {
            self.key_names.join(", ")
        };
        lines.push(format!(
            "rows: {} in, {} out; {} duplicates dropped (key: {}); {} empty rows dropped",
            self.rows_in,
            self.rows.len(),
            self.duplicates.len(),
            text::one_line(&key, 200),
            self.empty_dropped
        ));
        if !self.missing.is_empty() {
            let parts: Vec<String> = self
                .missing
                .iter()
                .map(|(c, n)| format!("{} {n}", text::one_line(c, 60)))
                .collect();
            lines.push(format!(
                "missing required values: {} ({} rows flagged in {MISSING_COL})",
                parts.join(", "),
                self.rows_flagged
            ));
        }
        if !self.blanks.is_empty() {
            let parts: Vec<String> = self
                .blanks
                .iter()
                .take(12)
                .map(|(c, n)| format!("{} {n}", text::one_line(c, 60)))
                .collect();
            lines.push(format!("blank cells: {}", parts.join(", ")));
        }
        for (line, first, shown) in self.duplicates.iter().take(DUP_SHOWN) {
            lines.push(format!(
                "duplicate: line {line} repeats line {first} ({})",
                text::one_line(shown, 160)
            ));
        }
        if self.duplicates.len() > DUP_SHOWN {
            lines.push(format!(
                "and {} more duplicates",
                self.duplicates.len() - DUP_SHOWN
            ));
        }
        lines.join("\n")
    }
}
