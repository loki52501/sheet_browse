//! F-5 `diff(a, b, mode=text|table)` for document audits.
//!
//! * `text`: a unified diff of the two files' lines (`similar`), `context` lines around
//!   each change (default 2). When it does not fit in `max_chars` (default 8000, up to
//!   20 000) the whole diff, scrubbed, is saved in `files/` and its path is in the text.
//! * `table`: CSV, TSV or xlsx (first sheet, or `sheet`) rows matched by `key` columns
//!   (default: the first column): columns only on one side, rows only on one side, and
//!   per row the cells that changed (compared trimmed).

use crate::csv::{read_table, Table};
use crate::paths::Roots;
use crate::text::{self, MAX_CHARS, READ_DEFAULT_CHARS};
use crate::util::{self, parse};
use async_trait::async_trait;
use harness_core::{Class, Classified, ToolCtx, ToolError, ToolHandler, ToolOutput, ToolSpec};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Largest file `diff` loads.
const MAX_BYTES: u64 = 16 * 1024 * 1024;
/// How long the line diff may search for a minimal result.
const DIFF_DEADLINE: std::time::Duration = std::time::Duration::from_secs(10);
/// Keys and changed rows listed in a table diff.
const KEYS_SHOWN: usize = 10;
const ROWS_SHOWN: usize = 20;
const CELLS_SHOWN: usize = 6;

pub fn tools() -> Vec<ToolSpec> {
    vec![ToolSpec {
        name: "diff".into(),
        description: "Compare two files. mode=text (default): unified diff of lines. mode=table: CSV/TSV/xlsx rows matched by key columns (default the first column), reporting rows added, removed and the cells that changed.".into(),
        class: Class::Read,
        args: json!({"type":"object","properties":{
            "a":{"type":"string"},
            "b":{"type":"string"},
            "mode":{"enum":["text","table"],"default":"text"},
            "key":{"type":"array","items":{"type":"string"}},
            "sheet":{"type":"string"},
            "context":{"type":"integer","minimum":0,"maximum":20},
            "max_chars":{"type":"integer","minimum":1,"maximum":MAX_CHARS}
        },"required":["a","b"],"additionalProperties":false}),
        handler: Arc::new(Diff),
    }]
}

struct Diff;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Args {
    a: String,
    b: String,
    mode: Option<String>,
    #[serde(default)]
    key: Vec<String>,
    sheet: Option<String>,
    context: Option<usize>,
    max_chars: Option<usize>,
}

#[async_trait]
impl ToolHandler for Diff {
    async fn classify(&self, ctx: &ToolCtx, args: &Value) -> Result<Classified, ToolError> {
        let roots = Roots::of(ctx)?;
        let a: Args = parse(args)?;
        let pa = roots.resolve(&a.a)?;
        let pb = roots.resolve(&a.b)?;
        Ok(Classified {
            class: Some(Class::Read),
            values: vec![
                ("a".into(), roots.shown(&pa)),
                ("b".into(), roots.shown(&pb)),
            ],
            ..Classified::default()
        })
    }

    async fn call(&self, ctx: &ToolCtx, args: Value) -> Result<ToolOutput, ToolError> {
        let roots = Roots::of(ctx)?;
        let a: Args = parse(&args)?;
        let pa = roots.resolve(&a.a)?;
        let pb = roots.resolve(&a.b)?;
        match a.mode.as_deref().unwrap_or("text") {
            "text" => {
                // Up to 16 MiB a side: off the async threads, and the diff itself has a
                // deadline after which `similar` falls back to a coarser result (S6).
                let opts = TextOpts {
                    context: a.context,
                    max_chars: a.max_chars,
                };
                tokio::task::spawn_blocking(move || text_diff(&roots, opts, &pa, &pb))
                    .await
                    .map_err(|e| ToolError::Failed(format!("diff: {e}")))?
            }
            "table" => {
                let ta = load_table(&pa, a.sheet.as_deref()).await?;
                let tb = load_table(&pb, a.sheet.as_deref()).await?;
                let report = table_diff(
                    &ta,
                    &tb,
                    &a.key,
                    &text::one_line(&roots.shown(&pa), 200),
                    &text::one_line(&roots.shown(&pb), 200),
                )?;
                Ok(ToolOutput {
                    summary: Some(first(&report)),
                    text: report,
                    ..ToolOutput::default()
                })
            }
            other => Err(ToolError::Args(format!(
                "mode {:?}: use text or table",
                text::one_line(other, 20)
            ))),
        }
    }
}

fn first(s: &str) -> String {
    s.lines().next().unwrap_or("").to_string()
}

fn read_text(p: &Path) -> Result<String, ToolError> {
    let mut bytes = Vec::new();
    std::fs::File::open(p)
        .and_then(|f| f.take(MAX_BYTES).read_to_end(&mut bytes))
        .map_err(|e| ToolError::Failed(format!("read {}: {e}", crate::paths::plain(p))))?;
    let s = String::from_utf8_lossy(&bytes).into_owned();
    Ok(s.strip_prefix('\u{FEFF}').map(str::to_string).unwrap_or(s))
}

/// The text-mode arguments (owned, for the blocking task).
struct TextOpts {
    context: Option<usize>,
    max_chars: Option<usize>,
}

fn text_diff(roots: &Roots, a: TextOpts, pa: &Path, pb: &Path) -> Result<ToolOutput, ToolError> {
    let max = util::limit("max_chars", a.max_chars, READ_DEFAULT_CHARS, MAX_CHARS)?;
    let left = read_text(pa)?;
    let right = read_text(pb)?;
    let sa = roots.shown(pa);
    let sb = roots.shown(pb);
    let d = similar::TextDiff::configure()
        .timeout(DIFF_DEADLINE)
        .diff_lines(&left, &right);
    let (mut plus, mut minus) = (0usize, 0usize);
    for op in d.ops() {
        for change in d.iter_changes(op) {
            match change.tag() {
                similar::ChangeTag::Insert => plus += 1,
                similar::ChangeTag::Delete => minus += 1,
                similar::ChangeTag::Equal => {}
            }
        }
    }
    let head = format!(
        "diff {} vs {}: +{plus} -{minus} lines",
        text::one_line(&sa, 200),
        text::one_line(&sb, 200)
    );
    if plus == 0 && minus == 0 {
        return Ok(ToolOutput {
            text: format!("{head} (identical)"),
            summary: Some(head),
            ..ToolOutput::default()
        });
    }
    let unified = d
        .unified_diff()
        .context_radius(a.context.unwrap_or(2).min(20))
        .header(&sa, &sb)
        .to_string();
    let (shown, cut) = text::take_chars(&unified, 0, max);
    let mut out = format!("{head}\n{}", text::untrusted(&shown));
    let mut files = Vec::new();
    if cut {
        let stamp = chrono::Utc::now().format("%Y%m%dT%H%M%S");
        let path = util::fresh_name(&roots.files, &format!("diff-{stamp}"), "diff");
        // The one write check every sink goes through (D-525): files/ must still be scratch.
        let saved = std::fs::create_dir_all(&roots.files)
            .map_err(|e| e.to_string())
            .and_then(|_| util::own_output_ok(roots, &path))
            .and_then(|_| {
                std::fs::write(&path, harness_core::scrub(&unified)).map_err(|e| e.to_string())
            });
        match saved {
            Ok(()) => {
                out.push_str(&format!(
                    "\ntruncated: {} of {} chars shown; saved: {}",
                    shown.chars().count(),
                    unified.chars().count(),
                    text::one_line(&roots.shown(&path), 300)
                ));
                files.push(path);
            }
            Err(e) => out.push_str(&format!(
                "\ntruncated: {} of {} chars shown; not saved: {e}",
                shown.chars().count(),
                unified.chars().count()
            )),
        }
    }
    Ok(ToolOutput {
        text: out,
        summary: Some(head),
        files: util::run_files(
            roots,
            &files.iter().map(PathBuf::as_path).collect::<Vec<_>>(),
        ),
        ..ToolOutput::default()
    })
}

async fn load_table(p: &Path, sheet: Option<&str>) -> Result<Table, ToolError> {
    match util::ext(p).as_str() {
        "xlsx" | "xlsm" => crate::docs::xlsx_table(p, sheet).await,
        "tsv" | "tab" => read_table(p, b'\t').map_err(|e| ToolError::Failed(format!("read: {e}"))),
        _ => read_table(p, b',').map_err(|e| ToolError::Failed(format!("read: {e}"))),
    }
}

fn col(header: &[String], name: &str) -> Option<usize> {
    header
        .iter()
        .position(|h| h == name)
        .or_else(|| header.iter().position(|h| h.eq_ignore_ascii_case(name)))
}

/// The table diff report (see the module docs).
pub fn table_diff(
    a: &Table,
    b: &Table,
    key: &[String],
    a_name: &str,
    b_name: &str,
) -> Result<String, ToolError> {
    let key: Vec<String> = if key.is_empty() {
        match a.header.first() {
            Some(h) => vec![h.clone()],
            None => return Err(ToolError::Args(format!("{a_name} has no columns"))),
        }
    } else {
        key.to_vec()
    };
    let ka: Vec<usize> = key
        .iter()
        .map(|k| col(&a.header, k).ok_or_else(|| missing_key(k, a_name, &a.header)))
        .collect::<Result<_, _>>()?;
    let kb: Vec<usize> = key
        .iter()
        .map(|k| col(&b.header, k).ok_or_else(|| missing_key(k, b_name, &b.header)))
        .collect::<Result<_, _>>()?;

    let index = |t: &Table, ks: &[usize]| {
        let mut map: HashMap<Vec<String>, usize> = HashMap::new();
        let mut order: Vec<Vec<String>> = Vec::new();
        let mut dups = 0usize;
        for (i, (_, row)) in t.rows.iter().enumerate() {
            let k: Vec<String> = ks
                .iter()
                .map(|&c| row.get(c).map(|s| s.trim().to_string()).unwrap_or_default())
                .collect();
            if map.contains_key(&k) {
                dups += 1;
            } else {
                map.insert(k.clone(), i);
                order.push(k);
            }
        }
        (map, order, dups)
    };
    let (ma, oa, da) = index(a, &ka);
    let (mb, ob, db) = index(b, &kb);

    let only_a: Vec<&Vec<String>> = oa.iter().filter(|k| !mb.contains_key(*k)).collect();
    let only_b: Vec<&Vec<String>> = ob.iter().filter(|k| !ma.contains_key(*k)).collect();
    let common_cols: Vec<(String, usize, usize)> = a
        .header
        .iter()
        .enumerate()
        .filter_map(|(ia, h)| col(&b.header, h).map(|ib| (h.clone(), ia, ib)))
        .filter(|(_, ia, _)| !ka.contains(ia))
        .collect();
    let mut changed: Vec<String> = Vec::new();
    let mut changed_n = 0usize;
    for k in &oa {
        let (Some(&ra), Some(&rb)) = (ma.get(k), mb.get(k)) else {
            continue;
        };
        let row_a = &a.rows[ra].1;
        let row_b = &b.rows[rb].1;
        let mut cells = Vec::new();
        for (h, ia, ib) in &common_cols {
            let va = row_a.get(*ia).map(|s| s.trim()).unwrap_or("");
            let vb = row_b.get(*ib).map(|s| s.trim()).unwrap_or("");
            if va != vb {
                cells.push(format!(
                    "{} {:?} -> {:?}",
                    text::one_line(h, 40),
                    text::one_line(va, 60),
                    text::one_line(vb, 60)
                ));
            }
        }
        if !cells.is_empty() {
            changed_n += 1;
            if changed.len() < ROWS_SHOWN {
                let more = cells.len().saturating_sub(CELLS_SHOWN);
                cells.truncate(CELLS_SHOWN);
                let mut line = format!("- {}: {}", show_key(&key, k), cells.join("; "));
                if more > 0 {
                    line.push_str(&format!("; and {more} more cells"));
                }
                changed.push(line);
            }
        }
    }

    let mut out = vec![format!(
        "table diff {a_name} ({} rows) vs {b_name} ({} rows), key: {}: {} only in a, {} only in b, {} changed",
        a.rows.len(),
        b.rows.len(),
        text::one_line(&key.join(", "), 100),
        only_a.len(),
        only_b.len(),
        changed_n
    )];
    let cols_a: Vec<&str> = a
        .header
        .iter()
        .filter(|h| col(&b.header, h).is_none())
        .map(String::as_str)
        .collect();
    let cols_b: Vec<&str> = b
        .header
        .iter()
        .filter(|h| col(&a.header, h).is_none())
        .map(String::as_str)
        .collect();
    if !cols_a.is_empty() {
        out.push(format!(
            "columns only in a: {}",
            text::one_line(&cols_a.join(", "), 300)
        ));
    }
    if !cols_b.is_empty() {
        out.push(format!(
            "columns only in b: {}",
            text::one_line(&cols_b.join(", "), 300)
        ));
    }
    if da + db > 0 {
        out.push(format!(
            "repeated keys (first row used): {da} in a, {db} in b"
        ));
    }
    let list = |ks: &[&Vec<String>]| {
        let mut s: Vec<String> = ks
            .iter()
            .take(KEYS_SHOWN)
            .map(|k| show_key(&key, k))
            .collect();
        if ks.len() > KEYS_SHOWN {
            s.push(format!("and {} more", ks.len() - KEYS_SHOWN));
        }
        s.join(", ")
    };
    if !only_a.is_empty() {
        out.push(format!("only in a: {}", list(&only_a)));
    }
    if !only_b.is_empty() {
        out.push(format!("only in b: {}", list(&only_b)));
    }
    if changed_n > 0 {
        out.push("changed:".into());
        out.extend(changed);
        if changed_n > ROWS_SHOWN {
            out.push(format!("and {} more changed rows", changed_n - ROWS_SHOWN));
        }
    }
    Ok(out.join("\n"))
}

fn show_key(names: &[String], k: &[String]) -> String {
    names
        .iter()
        .zip(k)
        .map(|(n, v)| format!("{}={}", text::one_line(n, 40), text::one_line(v, 60)))
        .collect::<Vec<_>>()
        .join(" ")
}

fn missing_key(k: &str, file: &str, header: &[String]) -> ToolError {
    ToolError::Args(format!(
        "{file} has no column {:?}; columns: {}",
        text::one_line(k, 60),
        text::one_line(&header.join(", "), 300)
    ))
}
