//! T-1: host -> tool folder. eTLD+1 from the compiled-in public suffix list (D-300), a more
//! specific folder when one exists (`mail.google.com/`, docs/07), then `_site.yaml` aliases.

use std::fs;
use std::path::Path;

/// Folder names that are never sites.
pub const GENERIC: &str = "_generic";
pub const ARCHIVE: &str = "_archive";
pub const SITE_FILE: &str = "_site.yaml";

/// `tools/<site>/_site.yaml` (T-1): aliases, pinned list. Other keys (notes...) are ignored.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SiteConfig {
    pub aliases: Vec<String>,
    pub pinned: Vec<String>,
}

impl SiteConfig {
    /// Reads `<root>/<folder>/_site.yaml`; a missing or unreadable file is the empty config.
    pub fn read(root: &Path, folder: &str) -> SiteConfig {
        match fs::read_to_string(root.join(folder).join(SITE_FILE)) {
            Ok(s) => SiteConfig::parse(&s),
            Err(_) => SiteConfig::default(),
        }
    }

    /// The subset of YAML `_site.yaml` uses: `key: [a, b]` or `key:` followed by `- a` items.
    pub fn parse(yaml: &str) -> SiteConfig {
        SiteConfig {
            aliases: yaml_list(yaml, "aliases")
                .into_iter()
                .map(|a| normalize_host(&a))
                .collect(),
            pinned: yaml_list(yaml, "pinned"),
        }
    }
}

fn unquote(s: &str) -> String {
    let s = s.trim();
    let s = s.split(" #").next().unwrap_or(s).trim();
    s.trim_matches(|c| c == '"' || c == '\'').trim().to_string()
}

/// A top-level list value from a tiny YAML subset (flow or block sequence of scalars).
fn yaml_list(yaml: &str, key: &str) -> Vec<String> {
    let mut lines = yaml.lines().peekable();
    while let Some(line) = lines.next() {
        if line.starts_with(char::is_whitespace) {
            continue;
        }
        let Some(rest) = line.strip_prefix(key).and_then(|r| r.strip_prefix(':')) else {
            continue;
        };
        let rest = rest.split(" #").next().unwrap_or("").trim();
        if let Some(inner) = rest.strip_prefix('[') {
            let inner = inner.split(']').next().unwrap_or("");
            return inner
                .split(',')
                .map(unquote)
                .filter(|s| !s.is_empty())
                .collect();
        }
        if !rest.is_empty() {
            return vec![unquote(rest)];
        }
        let mut out = Vec::new();
        while let Some(next) = lines.peek() {
            let t = next.trim();
            if t.is_empty() || t.starts_with('#') {
                lines.next();
                continue;
            }
            match t.strip_prefix('-') {
                Some(item) if next.starts_with(char::is_whitespace) || next.starts_with('-') => {
                    let v = unquote(item);
                    if !v.is_empty() {
                        out.push(v);
                    }
                    lines.next();
                }
                _ => break,
            }
        }
        return out;
    }
    Vec::new()
}

/// Lowercase, no scheme, no path, no port, no trailing dot.
pub fn normalize_host(host: &str) -> String {
    let h = host.trim();
    let h = h.split_once("://").map(|(_, r)| r).unwrap_or(h);
    let h = h.split(['/', '?', '#']).next().unwrap_or(h);
    let h = h.rsplit_once('@').map(|(_, r)| r).unwrap_or(h);
    let h = if h.starts_with('[') {
        // IPv6 literal: keep as is up to the closing bracket.
        h.split(']')
            .next()
            .map(|s| format!("{s}]"))
            .unwrap_or_default()
    } else {
        h.split(':').next().unwrap_or(h).to_string()
    };
    h.trim_end_matches('.').to_ascii_lowercase()
}

/// eTLD+1 via the public suffix list; the host itself for IPs, `localhost` and bare suffixes.
pub fn registrable(host: &str) -> String {
    let h = normalize_host(host);
    if h.parse::<std::net::IpAddr>().is_ok() || h.starts_with('[') {
        return h;
    }
    match psl::domain_str(&h) {
        Some(d) => d.to_string(),
        None => h,
    }
}

/// `app.luma.com` -> [`app.luma.com`, `luma.com`]: the host and its parents down to eTLD+1.
fn candidates(host: &str, etld1: &str) -> Vec<String> {
    let mut out = vec![host.to_string()];
    let mut cur = host;
    while cur != etld1 {
        match cur.split_once('.') {
            Some((_, rest)) if rest.len() >= etld1.len() => {
                out.push(rest.to_string());
                cur = rest;
            }
            _ => break,
        }
    }
    if out.last().map(String::as_str) != Some(etld1) {
        out.push(etld1.to_string());
    }
    out
}

fn is_site_dir(name: &str) -> bool {
    !name.starts_with('_') && !name.starts_with('.')
}

/// T-1. See the module doc for the order.
pub fn folder_for(root: &Path, host: &str) -> String {
    let h = normalize_host(host);
    if h.is_empty() || h.starts_with('_') {
        return h;
    }
    let etld1 = registrable(&h);
    let cands = candidates(&h, &etld1);
    for c in &cands {
        if is_site_dir(c) && root.join(c).is_dir() {
            return c.clone();
        }
    }
    if let Ok(entries) = fs::read_dir(root) {
        let mut folders: Vec<String> = entries
            .flatten()
            .filter(|e| e.path().is_dir())
            .filter_map(|e| e.file_name().into_string().ok())
            .filter(|n| is_site_dir(n))
            .collect();
        folders.sort();
        for c in &cands {
            for f in &folders {
                if SiteConfig::read(root, f).aliases.iter().any(|a| a == c) {
                    return f.clone();
                }
            }
        }
    }
    etld1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registrable_domains() {
        assert_eq!(registrable("jobs.ashbyhq.com"), "ashbyhq.com");
        assert_eq!(registrable("app.luma.com"), "luma.com");
        assert_eq!(registrable("lu.ma"), "lu.ma");
        assert_eq!(registrable("https://www.bbc.co.uk/news"), "bbc.co.uk");
        assert_eq!(registrable("LUMA.COM."), "luma.com");
        assert_eq!(registrable("127.0.0.1:8080"), "127.0.0.1");
        assert_eq!(registrable("localhost"), "localhost");
    }

    #[test]
    fn yaml_subset() {
        let c = SiteConfig::parse("aliases: [lu.ma, \"app.luma.com\"]\nnotes: x\npinned:\n  - register\n  - 'list_events'\nother: 1\n");
        assert_eq!(c.aliases, vec!["lu.ma", "app.luma.com"]);
        assert_eq!(c.pinned, vec!["register", "list_events"]);
        let c = SiteConfig::parse("aliases:\n- lu.ma # short link\n");
        assert_eq!(c.aliases, vec!["lu.ma"]);
        assert_eq!(SiteConfig::parse("notes: hi\n"), SiteConfig::default());
    }
}
