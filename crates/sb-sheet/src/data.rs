//! Data tables embedded from `crates/sb-sheet/data/` (D-201): role aliases and role priors
//! (`roles.txt`, D-49, B-6.2) and query synonyms (`synonyms.txt`, B-6.2).
//! Owned by coder A (T-10).
//!
//! The files are compiled in with `include_str!` and parsed once on first use (`OnceLock`).
//! Lookups take any spelling: keys are compared in normal form (lowercase, split on
//! non-alphanumerics, joined by one space). Unknown keys return an empty slice.

use crate::tokens::normalize;
use std::collections::HashMap;
use std::sync::OnceLock;

const ROLES_TXT: &str = include_str!("../data/roles.txt");
const SYNONYMS_TXT: &str = include_str!("../data/synonyms.txt");

struct Roles {
    /// alias -> ARIA roles, in file order.
    aliases: HashMap<String, Vec<String>>,
    /// ARIA role -> first alias listing it.
    alias_of: HashMap<String, String>,
    /// normalized word or phrase -> ARIA roles.
    priors: HashMap<String, Vec<String>>,
    /// Longest prior key, in words.
    prior_max_words: usize,
}

struct Synonyms {
    /// normalized entry -> the other entries of its groups, as written (lowercased, trimmed).
    map: HashMap<String, Vec<String>>,
    /// Longest entry, in words.
    max_words: usize,
}

fn push_unique(v: &mut Vec<String>, s: &str) {
    if !v.iter().any(|x| x == s) {
        v.push(s.to_string());
    }
}

fn parse_roles(src: &str) -> Roles {
    let mut roles = Roles {
        aliases: HashMap::new(),
        alias_of: HashMap::new(),
        priors: HashMap::new(),
        prior_max_words: 1,
    };
    for line in src.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((head, tail)) = line.split_once(':') else {
            continue;
        };
        let mut head = head.split_whitespace();
        match head.next() {
            Some("alias") => {
                let Some(alias) = head.next() else { continue };
                let alias = alias.to_lowercase();
                let entry = roles.aliases.entry(alias.clone()).or_default();
                for role in tail.split_whitespace() {
                    let role = role.to_lowercase();
                    push_unique(entry, &role);
                    roles.alias_of.entry(role).or_insert_with(|| alias.clone());
                }
            }
            Some("prior") => {
                let targets: Vec<String> = head.map(str::to_lowercase).collect();
                for word in tail.split(',') {
                    let key = normalize(word);
                    if key.is_empty() {
                        continue;
                    }
                    roles.prior_max_words = roles.prior_max_words.max(key.split(' ').count());
                    let entry = roles.priors.entry(key).or_default();
                    for t in &targets {
                        push_unique(entry, t);
                    }
                }
            }
            _ => {}
        }
    }
    roles
}

fn parse_synonyms(src: &str) -> Synonyms {
    let mut syn = Synonyms {
        map: HashMap::new(),
        max_words: 1,
    };
    for line in src.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let entries: Vec<String> = line
            .split('|')
            .map(|e| e.trim().to_lowercase())
            .filter(|e| !normalize(e).is_empty())
            .collect();
        for (i, e) in entries.iter().enumerate() {
            let key = normalize(e);
            syn.max_words = syn.max_words.max(key.split(' ').count());
            let out = syn.map.entry(key.clone()).or_default();
            for (j, other) in entries.iter().enumerate() {
                if i != j && normalize(other) != key {
                    push_unique(out, other);
                }
            }
        }
    }
    syn
}

fn roles() -> &'static Roles {
    static ROLES: OnceLock<Roles> = OnceLock::new();
    ROLES.get_or_init(|| parse_roles(ROLES_TXT))
}

fn syn() -> &'static Synonyms {
    static SYN: OnceLock<Synonyms> = OnceLock::new();
    SYN.get_or_init(|| parse_synonyms(SYNONYMS_TXT))
}

/// ARIA roles a short alias stands for (`input` -> `textbox, searchbox`). Empty if unknown.
pub fn aria_roles(alias: &str) -> &'static [String] {
    roles()
        .aliases
        .get(&alias.trim().to_lowercase())
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

/// The alias a sheet line prints for an ARIA role (`textbox` -> `input`); the role itself
/// when no alias lists it.
pub fn alias_for(aria_role: &str) -> &str {
    match roles().alias_of.get(&aria_role.trim().to_lowercase()) {
        Some(a) => a.as_str(),
        None => aria_role,
    }
}

/// ARIA roles a query word or phrase points at (B-6.2 role prior, +2). Empty if none.
pub fn role_prior(word: &str) -> &'static [String] {
    roles()
        .priors
        .get(&normalize(word))
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

/// Synonyms of a word or phrase from `synonyms.txt` (lowercased, as written). Empty if none.
pub fn synonyms(word: &str) -> &'static [String] {
    syn()
        .map
        .get(&normalize(word))
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

/// Roles for a `name:role` query suffix: an alias's roles, or the word itself when it is an
/// ARIA role some alias lists. `None` when the word is neither.
pub(crate) fn forced_roles(word: &str) -> Option<Vec<String>> {
    let w = word.trim().to_lowercase();
    let by_alias = aria_roles(&w);
    if !by_alias.is_empty() {
        return Some(by_alias.to_vec());
    }
    roles().alias_of.contains_key(&w).then(|| vec![w])
}

/// Longest phrase (in words) in the prior table.
pub(crate) fn prior_max_words() -> usize {
    roles().prior_max_words
}

/// Longest phrase (in words) in the synonym table.
pub(crate) fn synonym_max_words() -> usize {
    syn().max_words
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alias_tables() {
        assert_eq!(aria_roles("input"), ["textbox", "searchbox"]);
        assert_eq!(aria_roles("Select"), ["combobox", "listbox"]);
        assert_eq!(aria_roles("image"), ["img"]);
        assert!(aria_roles("nonsense").is_empty());
        assert_eq!(alias_for("textbox"), "input");
        assert_eq!(alias_for("searchbox"), "input");
        assert_eq!(alias_for("combobox"), "select");
        assert_eq!(alias_for("listbox"), "select");
        assert_eq!(alias_for("img"), "image");
        assert_eq!(alias_for("iframe"), "iframe");
        assert_eq!(alias_for("button"), "button");
        // unknown role prints as itself
        assert_eq!(alias_for("marquee"), "marquee");
    }

    #[test]
    fn every_alias_role_maps_back_to_an_alias() {
        for (alias, roles_) in &roles().aliases {
            assert!(!roles_.is_empty(), "{alias}");
            for r in roles_ {
                assert!(!aria_roles(alias_for(r)).is_empty(), "{r}");
            }
        }
    }

    #[test]
    fn forced_roles_accept_alias_or_aria() {
        assert_eq!(forced_roles("input").unwrap(), ["textbox", "searchbox"]);
        assert_eq!(forced_roles("textbox").unwrap(), ["textbox"]);
        assert_eq!(forced_roles("combobox").unwrap(), ["combobox"]);
        assert!(forced_roles("foo").is_none());
    }

    #[test]
    fn role_priors() {
        assert!(role_prior("email").contains(&"textbox".to_string()));
        assert!(role_prior("Password").contains(&"textbox".to_string()));
        assert!(role_prior("agree").contains(&"checkbox".to_string()));
        assert!(role_prior("register").contains(&"button".to_string()));
        assert!(role_prior("sign-up").contains(&"button".to_string()));
        assert!(role_prior("country").contains(&"combobox".to_string()));
        assert!(role_prior("details").contains(&"link".to_string()));
        // a word on two prior lines accumulates
        let search = role_prior("search");
        assert!(
            search.contains(&"searchbox".to_string()) && search.contains(&"button".to_string())
        );
        assert!(role_prior("zebra").is_empty());
        assert!(prior_max_words() >= 3);
    }

    #[test]
    fn synonyms_are_symmetric() {
        assert!(synonyms("email").contains(&"e-mail".to_string()));
        assert!(synonyms("e-mail").contains(&"email".to_string()));
        assert!(synonyms("E Mail").contains(&"email".to_string()));
        assert!(synonyms("register").contains(&"sign up".to_string()));
        assert!(synonyms("sign up").contains(&"register".to_string()));
        assert!(synonyms("zip").contains(&"postcode".to_string()));
        assert!(synonyms("postcode").contains(&"zip".to_string()));
        assert!(synonyms("agree").contains(&"accept".to_string()));
        assert!(synonyms("quuxx").is_empty());
        // never lists itself
        assert!(!synonyms("email").iter().any(|s| s == "email"));
        assert!(synonym_max_words() >= 3);
        for (k, vs) in &syn().map {
            for v in vs {
                assert!(
                    synonyms(v).iter().any(|b| normalize(b) == *k),
                    "{k} -> {v} is not symmetric"
                );
            }
        }
    }

    #[test]
    fn synonym_table_size() {
        let lines = SYNONYMS_TXT
            .lines()
            .filter(|l| !l.trim().is_empty() && !l.trim_start().starts_with('#'))
            .count();
        assert!((180..=260).contains(&lines), "{lines} groups");
    }
}
