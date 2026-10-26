//! T-3 loader: tools whose `@match` fits the path, site first, then `_generic/` at lower
//! precedence; the skeleton line (T-3.3). Invalid files are skipped and reported (D-306).
//! The live pair ([`ToolStore::scan_live`], [`ToolStore::load_live`]) also skips every tool
//! whose `tools.health` row is retired, even when its file is still in place (D-315).

use crate::domain::{valid_domain, GENERIC};
use crate::header::parse_header;
use crate::{StoreError, Tool, ToolStore};
use harness_core::memory::{MemoryStore, ToolHealth, ToolStatus};
use std::fs;
use std::path::{Path, PathBuf};

/// What `scan` found (D-306): loaded tools in precedence order, and files it ignored with why.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LoadReport {
    pub tools: Vec<Tool>,
    pub skipped: Vec<(PathBuf, String)>,
}

/// The MCP namespace of a folder: its first label (`luma.com` -> `luma`,
/// `mail.google.com` -> `mail`); `_generic` -> `generic`.
pub fn namespace(folder: &str) -> String {
    let f = folder.trim_start_matches('_');
    f.split('.').next().unwrap_or(f).to_string()
}

/// The path part of `path_or_url`, without query or fragment; `/` when empty.
pub fn url_path(path_or_url: &str) -> String {
    let s = path_or_url.trim();
    let s = match s.split_once("://") {
        Some((_, rest)) => rest.find('/').map(|i| &rest[i..]).unwrap_or(""),
        None => s,
    };
    let s = s.split(['?', '#']).next().unwrap_or("");
    if s.is_empty() {
        "/".to_string()
    } else if s.starts_with('/') {
        s.to_string()
    } else {
        format!("/{s}")
    }
}

/// `@match` glob on a URL path: `*` matches any run of characters (including `/`), `?` one
/// character, everything else literally. `/*` therefore matches every path of the site.
pub fn glob_match(pattern: &str, path: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let s: Vec<char> = path.chars().collect();
    let (mut pi, mut si) = (0usize, 0usize);
    let (mut star, mut mark) = (None::<usize>, 0usize);
    while si < s.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == s[si]) {
            pi += 1;
            si += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            mark = si;
            pi += 1;
        } else if let Some(st) = star {
            pi = st + 1;
            mark += 1;
            si = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

/// The version of `name`'s retired row in `rows`, if that row is retired (D-315).
fn retired_version(rows: &[ToolHealth], name: &str) -> Option<i32> {
    rows.iter()
        .find(|h| h.name == name && h.status == ToolStatus::Retired)
        .map(|h| h.version)
}

/// Reads and parses every `*.js` directly in `dir`, in file-name order. `retired(name)` is
/// `Some(version)` when the tool's own row in this folder is retired: such a file is skipped
/// and reported, and does not shadow a `_generic` tool of the same name (D-315).
fn scan_dir(
    dir: &Path,
    folder: &str,
    path: &str,
    retired: &dyn Fn(&str) -> Option<i32>,
    report: &mut LoadReport,
) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut files: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file() && p.extension().is_some_and(|x| x == "js"))
        .collect();
    files.sort();
    let ns = namespace(folder);
    for file in files {
        let src = match fs::read_to_string(&file) {
            Ok(s) => s,
            Err(e) => {
                report.skipped.push((file, format!("unreadable: {e}")));
                continue;
            }
        };
        let header = match parse_header(&src) {
            Ok(h) => h,
            Err(e) => {
                report.skipped.push((file, format!("bad header: {e}")));
                continue;
            }
        };
        let stem = file.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        if stem != header.name {
            let why = format!("@name {} does not match file name {stem}.js", header.name);
            report.skipped.push((file, why));
            continue;
        }
        if !header.matches.iter().any(|m| glob_match(m, path)) {
            continue;
        }
        if let Some(v) = retired(&header.name) {
            report.skipped.push((file, format!("retired (v{v})")));
            continue;
        }
        if report.tools.iter().any(|t| t.header.name == header.name) {
            // A site tool shadows a `_generic` one of the same name (T-3.4).
            continue;
        }
        report.tools.push(Tool {
            domain: folder.to_string(),
            namespaced: format!("{ns}.{}", header.name),
            path: file,
            header,
            source: src,
        });
    }
}

impl ToolStore {
    /// T-3 with the D-306 report. `domain` may be a host or a folder; it goes through
    /// `folder_for` first. `path` may be a path or a full URL.
    ///
    /// Files only: this does not consult `tools.health`, so a retired tool whose file is still
    /// in place (a failed archive step) is listed. Runtime callers use
    /// [`ToolStore::scan_live`] (D-315).
    pub fn scan(&self, domain: &str, path: &str) -> LoadReport {
        self.scan_filtered(&self.folder_for(domain), path, &|_| None, &|_| None)
    }

    /// [`ToolStore::scan`] that also skips every tool whose `tools.health` row in its own
    /// folder (the site folder or `_generic`) is retired, even when its file is still in place;
    /// such a file goes to `skipped` with why `retired (v<version>)`, and a retired site tool
    /// no longer shadows a `_generic` tool of the same name (D-315). Both folders' rows are
    /// read before any file; a memory error returns `Err`, never an unfiltered list.
    pub async fn scan_live(
        &self,
        store: &dyn MemoryStore,
        domain: &str,
        path: &str,
    ) -> Result<LoadReport, StoreError> {
        let folder = self.folder_for(domain);
        if !valid_domain(&folder) {
            return Ok(LoadReport::default());
        }
        let site = if folder != GENERIC {
            store.tool_health(&folder).await?
        } else {
            Vec::new()
        };
        let generic = store.tool_health(GENERIC).await?;
        Ok(
            self.scan_filtered(&folder, path, &|n| retired_version(&site, n), &|n| {
                retired_version(&generic, n)
            }),
        )
    }

    /// I-7 `load` with the `tools.health` filter of [`ToolStore::scan_live`] (D-315): the
    /// runtime entry point (srv-web, sb-mcp, toolforge).
    pub async fn load_live(
        &self,
        store: &dyn MemoryStore,
        domain: &str,
        path: &str,
    ) -> Result<Vec<Tool>, StoreError> {
        Ok(self.scan_live(store, domain, path).await?.tools)
    }

    /// The shared body of `scan` and `scan_live` for a `folder` already resolved by
    /// `folder_for`: `site_retired` and `generic_retired` say which names are retired in the
    /// site folder and in `_generic`.
    fn scan_filtered(
        &self,
        folder: &str,
        path: &str,
        site_retired: &dyn Fn(&str) -> Option<i32>,
        generic_retired: &dyn Fn(&str) -> Option<i32>,
    ) -> LoadReport {
        let mut report = LoadReport::default();
        let path = url_path(path);
        // D-310: `folder_for` returns `""` for anything that is not a site folder; the check
        // is repeated here so no other string is ever joined under the root.
        if !valid_domain(folder) {
            return report;
        }
        if folder != GENERIC {
            let dir = self.root.join(folder);
            scan_dir(&dir, folder, &path, site_retired, &mut report);
        }
        let dir = self.root.join(GENERIC);
        scan_dir(&dir, GENERIC, &path, generic_retired, &mut report);
        report
    }
}

/// The T-3 skeleton line: `tools for this site: luma.register(name,email,plan?) · luma.list_events(city)`.
/// Optional arguments carry `?`. `None` when no tool loaded.
pub fn skeleton_line(tools: &[Tool]) -> Option<String> {
    if tools.is_empty() {
        return None;
    }
    let items: Vec<String> = tools
        .iter()
        .map(|t| {
            let args: Vec<String> = t
                .header
                .args
                .iter()
                .map(|a| {
                    if a.optional {
                        format!("{}?", a.name)
                    } else {
                        a.name.clone()
                    }
                })
                .collect();
            format!("{}({})", t.namespaced, args.join(","))
        })
        .collect();
    Some(format!("tools for this site: {}", items.join(" · ")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn globs() {
        assert!(glob_match("/*", "/"));
        assert!(glob_match("/*", "/a/b/c"));
        assert!(glob_match("/events/*", "/events/123/register"));
        assert!(!glob_match("/events/*", "/jobs/1"));
        assert!(glob_match("/e?ents", "/events"));
        assert!(glob_match("/*/apply", "/acme/apply"));
        assert!(!glob_match("/*/apply", "/acme/apply/x"));
        assert!(glob_match("/thanks", "/thanks"));
        assert!(!glob_match("/thanks", "/thanks2"));
    }

    #[test]
    fn paths_and_namespaces() {
        assert_eq!(url_path("https://jobs.ashbyhq.com/acme?x=1#y"), "/acme");
        assert_eq!(url_path("https://lu.ma"), "/");
        assert_eq!(url_path(""), "/");
        assert_eq!(url_path("events/1"), "/events/1");
        assert_eq!(namespace("luma.com"), "luma");
        assert_eq!(namespace("mail.google.com"), "mail");
        assert_eq!(namespace("_generic"), "generic");
    }
}
