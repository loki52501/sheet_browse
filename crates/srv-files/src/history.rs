//! F-6 `history.search(q, since, k)`: Chrome's browsing history without a browser (use
//! case 3). The live `History` SQLite is locked while Chrome runs, so it is copied (with
//! its `-wal`/`-journal` files) into a temp folder, the copy is queried, and the copy is
//! deleted. Source: SB_CHROME_HISTORY, else the Default profile's `History`.
//!
//! Every word of `q` must appear in the URL or the title (ASCII case-insensitive); rows
//! come newest first as `{url, title, visited}`, `visited` in UTC (Chrome stores
//! microseconds since 1601-01-01).

use crate::text;
use crate::util::{self, parse};
use async_trait::async_trait;
use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use harness_core::{Class, Classified, ToolCtx, ToolError, ToolHandler, ToolOutput, ToolSpec};
use serde::Deserialize;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Seconds from 1601-01-01 to 1970-01-01.
const EPOCH_GAP_SECS: i64 = 11_644_473_600;
const K_DEFAULT: usize = 20;
const K_MAX: usize = 100;
/// Words of `q` used (the rest are ignored and said so).
const MAX_TERMS: usize = 8;

pub fn tools() -> Vec<ToolSpec> {
    tools_with(None)
}

/// `history.search` reading `path` instead of the default History file (tests).
pub fn tools_with(path: Option<PathBuf>) -> Vec<ToolSpec> {
    vec![ToolSpec {
        name: "history.search".into(),
        description: "Search Chrome's browsing history (a copy; Chrome may stay open). Every word of q must be in the URL or title. since: YYYY-MM-DD, an RFC 3339 time, or 7d / 12h / 30m. Returns at most k (default 20, max 100) rows {url, title, visited}, newest first.".into(),
        class: Class::Read,
        args: json!({"type":"object","properties":{
            "q":{"type":"string"},
            "since":{"type":"string"},
            "k":{"type":"integer","minimum":1,"maximum":K_MAX}
        },"required":["q"],"additionalProperties":false}),
        handler: Arc::new(History { path }),
    }]
}

struct History {
    path: Option<PathBuf>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Args {
    q: String,
    since: Option<String>,
    k: Option<usize>,
}

/// One history row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub url: String,
    pub title: String,
    pub visited: DateTime<Utc>,
}

/// SB_CHROME_HISTORY, else the Default profile's History for this OS.
pub fn default_path() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("SB_CHROME_HISTORY").filter(|p| !p.is_empty()) {
        return Some(PathBuf::from(p));
    }
    if cfg!(windows) {
        let local = std::env::var_os("LOCALAPPDATA")?;
        Some(
            PathBuf::from(local)
                .join("Google")
                .join("Chrome")
                .join("User Data")
                .join("Default")
                .join("History"),
        )
    } else {
        let home = PathBuf::from(std::env::var_os("HOME")?);
        if cfg!(target_os = "macos") {
            Some(home.join("Library/Application Support/Google/Chrome/Default/History"))
        } else {
            Some(home.join(".config/google-chrome/Default/History"))
        }
    }
}

/// Chrome time (µs since 1601-01-01 UTC) to UTC.
pub fn from_chrome(us: i64) -> Option<DateTime<Utc>> {
    let secs = us.div_euclid(1_000_000) - EPOCH_GAP_SECS;
    let micros = us.rem_euclid(1_000_000);
    Utc.timestamp_opt(secs, (micros * 1000) as u32).single()
}

/// UTC to Chrome time.
pub fn to_chrome(t: DateTime<Utc>) -> i64 {
    (t.timestamp() + EPOCH_GAP_SECS) * 1_000_000 + i64::from(t.timestamp_subsec_micros())
}

/// `since` as a time: `YYYY-MM-DD` (UTC midnight), RFC 3339, or `<n>d|h|m` before `now`.
pub fn parse_since(s: &str, now: DateTime<Utc>) -> Result<DateTime<Utc>, ToolError> {
    let s = s.trim();
    let bad = || {
        ToolError::Args(format!(
            "since {:?}: use YYYY-MM-DD, an RFC 3339 time, or 7d / 12h / 30m",
            text::one_line(s, 40)
        ))
    };
    if let Ok(t) = DateTime::parse_from_rfc3339(s) {
        return Ok(t.with_timezone(&Utc));
    }
    if let Ok(d) = NaiveDate::parse_from_str(s, "%Y-%m-%d") {
        return d
            .and_hms_opt(0, 0, 0)
            .map(|n| Utc.from_utc_datetime(&n))
            .ok_or_else(bad);
    }
    let unit = s.chars().last().ok_or_else(bad)?;
    let num = &s[..s.len() - unit.len_utf8()];
    let n: i64 = num.parse().map_err(|_| bad())?;
    if n < 0 {
        return Err(bad());
    }
    let d = match unit {
        'd' => chrono::Duration::try_days(n),
        'h' => chrono::Duration::try_hours(n),
        'm' => chrono::Duration::try_minutes(n),
        _ => None,
    }
    .ok_or_else(bad)?;
    now.checked_sub_signed(d).ok_or_else(bad)
}

/// `LIKE` pattern matching `term` anywhere, with `\` escaping `%`, `_` and `\`.
fn like(term: &str) -> String {
    let mut s = String::from("%");
    for c in term.chars() {
        if matches!(c, '%' | '_' | '\\') {
            s.push('\\');
        }
        s.push(c);
    }
    s.push('%');
    s
}

/// Copies `src` (and its -wal/-journal) into a temp folder, queries the copy, deletes it.
pub fn search(
    src: &Path,
    terms: &[String],
    since: Option<DateTime<Utc>>,
    k: usize,
) -> Result<Vec<Row>, String> {
    if !src.is_file() {
        return Err(format!(
            "no Chrome History file at {} (set SB_CHROME_HISTORY)",
            crate::paths::plain(src)
        ));
    }
    let dir = tempfile::tempdir().map_err(|e| format!("temp folder: {e}"))?;
    let copy = dir.path().join("History");
    std::fs::copy(src, &copy).map_err(|e| {
        format!("could not copy the History file ({e}); if Chrome holds it exclusively, close Chrome and retry")
    })?;
    for suffix in ["-wal", "-journal"] {
        let side = PathBuf::from(format!("{}{suffix}", src.to_string_lossy()));
        if side.is_file() {
            let _ = std::fs::copy(&side, dir.path().join(format!("History{suffix}")));
        }
    }
    let rows = query(&copy, terms, since, k);
    drop(dir); // deletes the copy
    rows
}

fn query(
    db: &Path,
    terms: &[String],
    since: Option<DateTime<Utc>>,
    k: usize,
) -> Result<Vec<Row>, String> {
    use rusqlite::types::Value as Sql;
    let conn = rusqlite::Connection::open(db).map_err(|e| format!("open the copy: {e}"))?;
    let mut sql = String::from(
        "SELECT url, COALESCE(title, ''), last_visit_time FROM urls WHERE last_visit_time >= ?1",
    );
    let mut params: Vec<Sql> = vec![Sql::Integer(since.map(to_chrome).unwrap_or(0))];
    for t in terms {
        params.push(Sql::Text(like(t)));
        let n = params.len();
        sql.push_str(&format!(
            " AND (url LIKE ?{n} ESCAPE '\\' OR title LIKE ?{n} ESCAPE '\\')"
        ));
    }
    params.push(Sql::Integer(k as i64));
    sql.push_str(&format!(
        " ORDER BY last_visit_time DESC LIMIT ?{}",
        params.len()
    ));
    let mut stmt = conn
        .prepare(&sql)
        .map_err(|e| format!("the copy is not a Chrome History database: {e}"))?;
    let rows = stmt
        .query_map(rusqlite::params_from_iter(params.iter()), |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
            ))
        })
        .map_err(|e| format!("query: {e}"))?;
    let mut out = Vec::new();
    for r in rows {
        let (url, title, t) = r.map_err(|e| format!("query: {e}"))?;
        if let Some(visited) = from_chrome(t) {
            out.push(Row {
                url,
                title,
                visited,
            });
        }
    }
    Ok(out)
}

#[async_trait]
impl ToolHandler for History {
    async fn classify(&self, _ctx: &ToolCtx, args: &Value) -> Result<Classified, ToolError> {
        let a: Args = parse(args)?;
        Ok(Classified {
            class: Some(Class::Read),
            values: vec![("q".into(), text::one_line(&a.q, 200))],
            ..Classified::default()
        })
    }

    async fn call(&self, _ctx: &ToolCtx, args: Value) -> Result<ToolOutput, ToolError> {
        let a: Args = parse(&args)?;
        let k = util::limit("k", a.k, K_DEFAULT, K_MAX)?;
        let since = a
            .since
            .as_deref()
            .map(|s| parse_since(s, Utc::now()))
            .transpose()?;
        let all: Vec<String> = a.q.split_whitespace().map(str::to_string).collect();
        if all.is_empty() && since.is_none() {
            return Err(ToolError::Args("give q words or a since".into()));
        }
        let terms: Vec<String> = all.iter().take(MAX_TERMS).cloned().collect();
        let src = self.path.clone().or_else(default_path).ok_or_else(|| {
            ToolError::Failed("no Chrome profile folder; set SB_CHROME_HISTORY".into())
        })?;
        let t2 = terms.clone();
        let rows = tokio::task::spawn_blocking(move || search(&src, &t2, since, k))
            .await
            .map_err(|e| ToolError::Failed(format!("history search: {e}")))?
            .map_err(ToolError::Failed)?;
        let mut lines = vec![format!(
            "{} results for {:?}{}{}",
            rows.len(),
            text::one_line(&terms.join(" "), 120),
            since
                .map(|s| format!(" since {}", s.format("%Y-%m-%dT%H:%M:%SZ")))
                .unwrap_or_default(),
            if all.len() > MAX_TERMS {
                format!(" (only the first {MAX_TERMS} words were used)")
            } else {
                String::new()
            }
        )];
        for r in &rows {
            lines.push(
                json!({
                    "url": text::one_line(&r.url, 500),
                    "title": text::one_line(&r.title, 200),
                    "visited": r.visited.format("%Y-%m-%dT%H:%M:%SZ").to_string(),
                })
                .to_string(),
            );
        }
        Ok(ToolOutput {
            summary: Some(lines[0].clone()),
            text: lines.join("\n"),
            consequences: json!({"rows": rows.len()}),
            ..ToolOutput::default()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chrome_time_round_trips() {
        let t = Utc.with_ymd_and_hms(2026, 10, 20, 14, 3, 0).unwrap();
        assert_eq!(from_chrome(to_chrome(t)), Some(t));
        assert_eq!(
            to_chrome(Utc.timestamp_opt(0, 0).unwrap()),
            EPOCH_GAP_SECS * 1_000_000
        );
    }

    #[test]
    fn since_forms() {
        let now = Utc.with_ymd_and_hms(2026, 10, 25, 12, 0, 0).unwrap();
        assert_eq!(
            parse_since("7d", now).unwrap(),
            Utc.with_ymd_and_hms(2026, 10, 18, 12, 0, 0).unwrap()
        );
        assert_eq!(
            parse_since("2026-10-01", now).unwrap(),
            Utc.with_ymd_and_hms(2026, 10, 1, 0, 0, 0).unwrap()
        );
        assert!(parse_since("2026-10-01T05:00:00+02:00", now).is_ok());
        assert!(parse_since("soon", now).is_err());
        assert!(parse_since("7x", now).is_err());
        assert!(parse_since("", now).is_err());
        assert!(parse_since("é", now).is_err());
    }

    #[test]
    fn like_escapes_wildcards() {
        assert_eq!(like("50%_a\\"), "%50\\%\\_a\\\\%");
    }
}
