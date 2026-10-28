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
pub(crate) fn retired_version(rows: &[ToolHealth], name: &str) -> Option<i32> {
    rows.iter()
        .find(|h| h.name == name && h.status == ToolStatus::Retired)
        .map(|h| h.version)
}

/// One file `scan_dir` looked at, in file-name order: a file it ignored and why, or a parsed
/// tool whose `@match` fits the path (not yet filtered by `tools.health` or shadowing).
enum Scanned {
    Skip(PathBuf, String),
    Fits(Tool),
}

/// Reads and parses every `*.js` directly in `dir`, in file-name order. Retired rows and the
/// site-shadows-generic rule are applied later, by [`finish`], so the live loader can read
/// every file before it reads any row (D-315).
fn scan_dir(dir: &Path, folder: &str, path: &str) -> Vec<Scanned> {
    let mut out = Vec::new();
    let Ok(entries) = fs::read_dir(dir) else {
        return out;
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
                out.push(Scanned::Skip(file, format!("unreadable: {e}")));
                continue;
            }
        };
        let header = match parse_header(&src) {
            Ok(h) => h,
            Err(e) => {
                out.push(Scanned::Skip(file, format!("bad header: {e}")));
                continue;
            }
        };
        let stem = file.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        if stem != header.name {
            let why = format!("@name {} does not match file name {stem}.js", header.name);
            out.push(Scanned::Skip(file, why));
            continue;
        }
        if !header.matches.iter().any(|m| glob_match(m, path)) {
            continue;
        }
        out.push(Scanned::Fits(Tool {
            domain: folder.to_string(),
            namespaced: format!("{ns}.{}", header.name),
            path: file,
            header,
            source: src,
        }));
    }
    out
}

/// `Some(version)` when a tool name's own row in one folder is retired (D-315).
type Retired<'a> = &'a dyn Fn(&str) -> Option<i32>;

/// The single filter pass over what [`scan_dir`] found, site folder first, then `_generic`.
/// `site_retired(name)` / `generic_retired(name)` are `Some(version)` when the tool's own
/// row in that folder is retired: such a file goes to `skipped` as `retired (v<N>)` and does
/// not shadow a `_generic` tool of the same name (D-315). Otherwise the first tool of a name
/// wins, so a site tool shadows a `_generic` one (T-3.4).
fn finish(
    site: Vec<Scanned>,
    generic: Vec<Scanned>,
    site_retired: Retired<'_>,
    generic_retired: Retired<'_>,
) -> LoadReport {
    let mut report = LoadReport::default();
    let folders: [(Vec<Scanned>, Retired<'_>); 2] =
        [(site, site_retired), (generic, generic_retired)];
    for (found, retired) in folders {
        for s in found {
            let tool = match s {
                Scanned::Skip(file, why) => {
                    report.skipped.push((file, why));
                    continue;
                }
                Scanned::Fits(t) => t,
            };
            if let Some(v) = retired(&tool.header.name) {
                report.skipped.push((tool.path, format!("retired (v{v})")));
                continue;
            }
            if report
                .tools
                .iter()
                .any(|t| t.header.name == tool.header.name)
            {
                continue;
            }
            report.tools.push(tool);
        }
    }
    report
}

impl ToolStore {
    /// T-3 with the D-306 report. `domain` may be a host or a folder; it goes through
    /// `folder_for` first. `path` may be a path or a full URL.
    ///
    /// Files only: this does not consult `tools.health`, so a retired tool whose file is still
    /// in place (a failed archive step) is listed. Runtime callers use
    /// [`ToolStore::scan_live`] (D-315).
    pub fn scan(&self, domain: &str, path: &str) -> LoadReport {
        let (site, generic) = self.read_folders(&self.folder_for(domain), path);
        finish(site, generic, &|_| None, &|_| None)
    }

    /// [`ToolStore::scan`] that also skips every tool whose `tools.health` row in its own
    /// folder (the site folder or `_generic`) is retired, even when its file is still in place;
    /// such a file goes to `skipped` with why `retired (v<version>)`, and a retired site tool
    /// no longer shadows a `_generic` tool of the same name (D-315).
    ///
    /// Every file is read before any row. Retire saves the row before it moves the file, so a
    /// file seen here is filtered by any retire that committed before the row read, whether
    /// or not its archive step then failed; a retire that lands after the row read is after
    /// this call. A memory error on either folder's read returns `Err`, never an unfiltered
    /// list.
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
        let (site_found, generic_found) = self.read_folders(&folder, path);
        let site = if folder != GENERIC {
            store.tool_health(&folder).await?
        } else {
            Vec::new()
        };
        let generic = store.tool_health(GENERIC).await?;
        Ok(finish(
            site_found,
            generic_found,
            &|n| retired_version(&site, n),
            &|n| retired_version(&generic, n),
        ))
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

    /// The file half shared by `scan` and `scan_live` for a `folder` already resolved by
    /// `folder_for`: what the site folder (empty for `_generic` itself) and `_generic` hold.
    fn read_folders(&self, folder: &str, path: &str) -> (Vec<Scanned>, Vec<Scanned>) {
        let path = url_path(path);
        // D-310: `folder_for` returns `""` for anything that is not a site folder; the check
        // is repeated here so no other string is ever joined under the root.
        if !valid_domain(folder) {
            return (Vec::new(), Vec::new());
        }
        let site = if folder != GENERIC {
            scan_dir(&self.root.join(folder), folder, &path)
        } else {
            Vec::new()
        };
        let generic = scan_dir(&self.root.join(GENERIC), GENERIC, &path);
        (site, generic)
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
