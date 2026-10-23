//! T-3 loader: tools whose `@match` fits the path, site first, then `_generic/` at lower
//! precedence; the skeleton line (T-3.3). Invalid files are skipped and reported (D-306).

use crate::domain::{ARCHIVE, GENERIC};
use crate::header::parse_header;
use crate::{Tool, ToolStore};
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

/// Reads and parses every `*.js` directly in `dir`, in file-name order.
fn scan_dir(dir: &Path, folder: &str, path: &str, report: &mut LoadReport) {
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
    pub fn scan(&self, domain: &str, path: &str) -> LoadReport {
        let mut report = LoadReport::default();
        let folder = self.folder_for(domain);
        let path = url_path(path);
        if folder.is_empty() || folder == ARCHIVE || folder.contains(['/', '\\']) {
            return report;
        }
        if folder != GENERIC {
            scan_dir(&self.root.join(&folder), &folder, &path, &mut report);
        }
        scan_dir(&self.root.join(GENERIC), GENERIC, &path, &mut report);
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
