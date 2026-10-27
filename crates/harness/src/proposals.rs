//! Reflect proposals (G-3, T-7, T-8; D-142): items reflect writes for the user to decide on.
//!
//! * T-8 merge: near-identical tools (same kind, same arg shape, same fingerprint role
//!   sequence) under three or more site folders. Arg shape is the ordered list of arg types
//!   with `?` for optional ones; names are left out, since sites name the same input
//!   differently. Tools without a fingerprint are never grouped (no role sequence to compare).
//!   The id is a hash of the shape, so a later pass with a fourth domain adds nothing new.
//! * T-7 retire: a pinned tool that would have been retired (ephemeral and idle 14 days or 10
//!   site visits, or needs_repair after 5 misses). Pinned tools are never auto-retired, so the
//!   user decides.
//!
//! Where they go (D-142, plan default 7): one JSON-flow line per item appended under the
//! trailing `proposals:` key of `agent-memory/blockers.yaml` (`SB_PROPOSALS_FILE`), deduped by
//! id, and a copy of the pass's items in the run's `files/proposals.yaml`. When the file is
//! missing or `proposals:` is not its last top-level key, only the run copy is written and the
//! result carries a warning. Every line is scrubbed (rule 2).

use harness_core::run::RunId;
use harness_core::scrub;
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use toolstore::{ArgSpec, ToolHeader, GENERIC, MISSES_TO_RETIRE};

/// Environment variable naming the proposals file (default: see [`default_proposals_file`]).
pub const PROPOSALS_FILE_VAR: &str = "SB_PROPOSALS_FILE";

/// File under the run's `files/` holding this pass's proposals.
pub const RUN_COPY: &str = "proposals.yaml";

/// T-8: tools this alike under at least this many domains give a merge proposal.
pub const MERGE_MIN_DOMAINS: usize = 3;

/// `<tools_root>/../agent-memory/blockers.yaml`.
pub fn default_proposals_file(tools_root: &Path) -> PathBuf {
    let root = std::path::absolute(tools_root).unwrap_or_else(|_| tools_root.to_path_buf());
    root.parent()
        .unwrap_or(&root)
        .join("agent-memory")
        .join("blockers.yaml")
}

/// `SB_PROPOSALS_FILE` if set and not blank, else [`default_proposals_file`].
pub fn proposals_file_from_env(tools_root: &Path) -> PathBuf {
    match std::env::var(PROPOSALS_FILE_VAR) {
        Ok(v) if !v.trim().is_empty() => PathBuf::from(v.trim()),
        _ => default_proposals_file(tools_root),
    }
}

/// One proposal: an id and the fields written after it.
#[derive(Debug, Clone, PartialEq)]
pub struct Proposal {
    pub id: String,
    pub fields: Map<String, Value>,
}

impl Proposal {
    /// The item as one JSON object (valid YAML flow), scrubbed: `{"id":"...",...}`.
    pub fn line(&self) -> String {
        let mut m = Map::new();
        m.insert("id".into(), Value::String(self.id.clone()));
        for (k, v) in &self.fields {
            if k != "id" {
                m.insert(k.clone(), v.clone());
            }
        }
        scrub(&Value::Object(m).to_string())
    }
}

/// FNV-1a 64: a stable id for a shape (std's hasher is randomised per process).
fn fnv64(s: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// `string,number?`: arg types in order, `?` for optional (names left out).
pub fn arg_shape(args: &[ArgSpec]) -> String {
    args.iter()
        .map(|a| {
            let mut t = a.ty.trim().to_ascii_lowercase();
            if a.optional {
                t.push('?');
            }
            t
        })
        .collect::<Vec<_>>()
        .join(",")
}

/// `form,input,button`: the `@fingerprint` roles in order, lower case.
pub fn role_sequence(h: &ToolHeader) -> String {
    h.fingerprint
        .iter()
        .map(|f| f.role.trim().to_ascii_lowercase())
        .collect::<Vec<_>>()
        .join(",")
}

/// A tool as the merge check sees it.
#[derive(Debug, Clone, Copy)]
pub struct MergeInput<'a> {
    pub domain: &'a str,
    pub header: &'a ToolHeader,
}

/// T-8 merge proposals for `tools` (site folders only; `_generic` is the merge target).
pub fn merge_proposals(tools: &[MergeInput<'_>], run: &RunId, day: &str) -> Vec<Proposal> {
    // shape key -> (kind, args, roles, domain -> names)
    type Group = (String, String, String, BTreeMap<String, Vec<String>>);
    let mut groups: BTreeMap<String, Group> = BTreeMap::new();
    for t in tools {
        if t.domain == GENERIC {
            continue;
        }
        let roles = role_sequence(t.header);
        if roles.is_empty() {
            continue;
        }
        let kind = t.header.kind.as_str().to_string();
        let args = arg_shape(&t.header.args);
        let key = format!("{kind}|{args}|{roles}");
        let g = groups
            .entry(key)
            .or_insert_with(|| (kind, args, roles, BTreeMap::new()));
        g.3.entry(t.domain.to_string())
            .or_default()
            .push(t.header.name.clone());
    }
    let mut out = Vec::new();
    for (key, (kind, args, roles, domains)) in groups {
        if domains.len() < MERGE_MIN_DOMAINS {
            continue;
        }
        let members: Vec<String> = domains
            .iter()
            .flat_map(|(d, names)| names.iter().map(move |n| format!("{d}/{n}")))
            .collect();
        let mut f = Map::new();
        f.insert("kind".into(), json!("merge"));
        f.insert("spec".into(), json!("T-8"));
        f.insert("tool_kind".into(), json!(kind));
        f.insert("args".into(), json!(args));
        f.insert("roles".into(), json!(roles));
        f.insert("tools".into(), json!(members));
        f.insert("domains".into(), json!(domains.len()));
        f.insert(
            "proposal".into(),
            json!(format!(
                "merge into one {GENERIC}/ {kind} that takes the site-specific bits as \
                 arguments; user approves (T-8)"
            )),
        );
        f.insert("run".into(), json!(run.0));
        f.insert("date".into(), json!(day));
        out.push(Proposal {
            id: format!("t8-merge-{:016x}", fnv64(&key)),
            fields: f,
        });
    }
    out
}

/// Why a pinned tool would have been retired, if it would.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StaleWhy {
    /// Ephemeral and unused (the text says how).
    Idle(String),
    /// needs_repair with this many misses.
    Misses(i32),
}

/// T-7 retire proposal for a pinned tool that would have been retired.
pub fn retire_proposal(
    domain: &str,
    name: &str,
    version: i32,
    why: &StaleWhy,
    run: &RunId,
    day: &str,
) -> Proposal {
    let why = match why {
        StaleWhy::Idle(w) => format!("pinned and {w}"),
        StaleWhy::Misses(n) => {
            format!("pinned and needs_repair after {n} misses (T-7 retires at {MISSES_TO_RETIRE})")
        }
    };
    let mut f = Map::new();
    f.insert("kind".into(), json!("retire"));
    f.insert("spec".into(), json!("T-7"));
    f.insert("tool".into(), json!(format!("{domain}/{name}")));
    f.insert("version".into(), json!(version));
    f.insert("why".into(), json!(why));
    f.insert(
        "proposal".into(),
        json!(format!(
            "pinned tools are never auto-retired; retire with `harness tools retire \
             {domain}/{name}` or unpin it"
        )),
    );
    f.insert("run".into(), json!(run.0));
    f.insert("date".into(), json!(day));
    Proposal {
        id: format!("t7-retire-{domain}-{name}-v{version}"),
        fields: f,
    }
}

/// What [`write_proposals`] did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Written {
    /// Ids appended to the proposals file in this pass.
    pub new: Vec<String>,
    /// Ids that were already in the proposals file.
    pub known: Vec<String>,
    /// The run copy, when there was anything to copy.
    pub run_copy: Option<PathBuf>,
    /// Why the proposals file was not written, when it was not.
    pub warning: Option<String>,
}

/// The id of a proposals item line (`- {"id":"x",...}` or `- id: x`), if it has one.
fn item_id(line: &str) -> Option<String> {
    let t = line.trim_start().strip_prefix('-')?.trim();
    if t.starts_with('{') {
        let v: Value = serde_json::from_str(t).ok()?;
        return v.get("id")?.as_str().map(str::to_owned);
    }
    let v = t.strip_prefix("id:")?.trim();
    Some(v.trim_matches(['"', '\'']).to_owned())
}

/// Where the `proposals:` items go in `text`: the item indent and the ids already there, or
/// why the file cannot take them (`proposals:` missing, not the last top-level key, or given
/// an inline value).
fn proposals_section(text: &str) -> Result<(String, Vec<String>), String> {
    let lines: Vec<&str> = text.lines().collect();
    let Some(at) = lines.iter().position(|l| l.trim_end() == "proposals:") else {
        return Err("it has no top-level `proposals:` key with an empty value".into());
    };
    let mut ids = Vec::new();
    let mut indent: Option<String> = None;
    for l in &lines[at + 1..] {
        let t = l.trim_start();
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        let lead = &l[..l.len() - t.len()];
        if lead.is_empty() && !t.starts_with('-') {
            return Err("`proposals:` is not its last top-level key".into());
        }
        if t.starts_with("- ") || t == "-" {
            indent.get_or_insert_with(|| lead.to_owned());
            if lead == indent.as_deref().unwrap_or_default() {
                if let Some(id) = item_id(l) {
                    ids.push(id);
                }
            }
        }
    }
    Ok((indent.unwrap_or_else(|| "  ".into()), ids))
}

/// Appends the items of `items` whose ids `file` does not hold yet under its trailing
/// `proposals:` key, and writes every item of this pass to `<run_files>/proposals.yaml`.
/// `file: None`, a missing file or one whose `proposals:` is not last: run copy only, with a
/// warning. Errors are I/O errors on the run copy or on appending to the file.
pub fn write_proposals(
    file: Option<&Path>,
    run_files: &Path,
    run: &RunId,
    items: &[Proposal],
) -> std::io::Result<Written> {
    let mut out = Written::default();
    if items.is_empty() {
        return Ok(out);
    }
    let lines: Vec<(String, String)> = items.iter().map(|p| (p.id.clone(), p.line())).collect();

    let mut copy = format!(
        "# reflect proposals of run {} (G-3, T-7, T-8); the user decides.\nproposals:\n",
        scrub(&run.0)
    );
    for (_, l) in &lines {
        copy.push_str("  - ");
        copy.push_str(l);
        copy.push('\n');
    }
    let path = run_files.join(RUN_COPY);
    std::fs::write(&path, scrub(&copy))?;
    out.run_copy = Some(path);

    let Some(file) = file else {
        out.warning = Some("no proposals file configured; written to the run folder only".into());
        return Ok(out);
    };
    let text = match std::fs::read_to_string(file) {
        Ok(t) => t,
        Err(e) => {
            out.warning = Some(format!(
                "proposals file {} not readable ({e}); written to the run folder only",
                file.display()
            ));
            return Ok(out);
        }
    };
    let (indent, known) = match proposals_section(&text) {
        Ok(s) => s,
        Err(why) => {
            out.warning = Some(format!(
                "proposals file {}: {why}; written to the run folder only",
                file.display()
            ));
            return Ok(out);
        }
    };
    let mut add = String::new();
    if !text.is_empty() && !text.ends_with('\n') {
        add.push('\n');
    }
    for (id, l) in &lines {
        if known.contains(id) || out.new.contains(id) {
            out.known.push(id.clone());
            continue;
        }
        add.push_str(&indent);
        add.push_str("- ");
        add.push_str(l);
        add.push('\n');
        out.new.push(id.clone());
    }
    if !out.new.is_empty() {
        let mut f = std::fs::OpenOptions::new().append(true).open(file)?;
        f.write_all(scrub(&add).as_bytes())?;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use toolstore::parse_header;

    fn header(name: &str, args: &str, fp: &str) -> ToolHeader {
        parse_header(&format!(
            "// @name {name}\n// @kind reader\n// @gate Read\n// @phase 1\n// @args {args}\n\
             // @fingerprint {fp}\nreturn 1;\n"
        ))
        .unwrap()
    }

    #[test]
    fn three_domains_alike_give_one_merge() {
        let a = header("jobs", "q:string", "form input button\"Search\"");
        let b = header("list", "query:string", "form input button\"Find\"");
        let c = header("find", "term:string", "form input button");
        let d = header("other", "q:number", "form input button");
        let run = RunId("r".into());
        let two = [
            MergeInput {
                domain: "a.com",
                header: &a,
            },
            MergeInput {
                domain: "b.com",
                header: &b,
            },
        ];
        assert!(merge_proposals(&two, &run, "2026-10-26").is_empty());
        let mut all = two.to_vec();
        all.push(MergeInput {
            domain: "c.com",
            header: &c,
        });
        all.push(MergeInput {
            domain: "d.com",
            header: &d,
        });
        let p = merge_proposals(&all, &run, "2026-10-26");
        assert_eq!(p.len(), 1, "{p:?}");
        assert_eq!(
            p[0].fields["tools"],
            json!(["a.com/jobs", "b.com/list", "c.com/find"])
        );
        // The id is the shape's, not the members'.
        let p2 = merge_proposals(&all[..3], &run, "2026-10-27");
        assert_eq!(p2[0].id, p[0].id);
    }

    #[test]
    fn section_rules() {
        assert_eq!(
            proposals_section("blockers:\n  - id: B-1\nproposals:\n").unwrap(),
            ("  ".to_string(), vec![])
        );
        let (ind, ids) =
            proposals_section("proposals:\n- {\"id\":\"x\"}\n- id: y\n  text: z\n").unwrap();
        assert_eq!(ind, "");
        assert_eq!(ids, ["x", "y"]);
        assert!(proposals_section("proposals:\nblockers:\n").is_err());
        assert!(proposals_section("proposals: []\n").is_err());
        assert!(proposals_section("blockers:\n").is_err());
    }

    fn item(id: &str) -> Proposal {
        let mut fields = Map::new();
        fields.insert("kind".into(), json!("retire"));
        Proposal {
            id: id.into(),
            fields,
        }
    }

    #[test]
    fn appends_once_under_trailing_proposals_and_keeps_the_rest() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("blockers.yaml");
        let run_files = tmp.path().join("files");
        std::fs::create_dir_all(&run_files).unwrap();
        let head = "blockers:\n  - id: B-1\nproposals:\n  - id: old\n    text: x";
        std::fs::write(&file, head).unwrap();
        let run = RunId("run-1".into());
        let w = write_proposals(Some(&file), &run_files, &run, &[item("old"), item("p1")]).unwrap();
        assert_eq!(w.new, ["p1"]);
        assert_eq!(w.known, ["old"]);
        assert_eq!(w.warning, None);
        let text = std::fs::read_to_string(&file).unwrap();
        assert_eq!(
            text,
            format!("{head}\n  - {{\"id\":\"p1\",\"kind\":\"retire\"}}\n")
        );
        let copy = std::fs::read_to_string(run_files.join(RUN_COPY)).unwrap();
        assert!(copy.contains("- {\"id\":\"old\"") && copy.contains("- {\"id\":\"p1\""));

        // Again: nothing new, the file is unchanged.
        let w = write_proposals(Some(&file), &run_files, &run, &[item("p1")]).unwrap();
        assert!(w.new.is_empty());
        assert_eq!(std::fs::read_to_string(&file).unwrap(), text);
    }

    #[test]
    fn a_file_it_cannot_append_to_is_left_alone_with_a_warning() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("blockers.yaml");
        let original = "proposals:\nblockers:\n  - id: B-1\n";
        std::fs::write(&file, original).unwrap();
        let run = RunId("run-1".into());
        let w = write_proposals(Some(&file), tmp.path(), &run, &[item("p1")]).unwrap();
        assert!(w.new.is_empty());
        assert!(w
            .warning
            .unwrap_or_default()
            .contains("not its last top-level key"));
        assert_eq!(std::fs::read_to_string(&file).unwrap(), original);
        assert!(tmp.path().join(RUN_COPY).is_file());

        let missing = tmp.path().join("nope.yaml");
        let w = write_proposals(Some(&missing), tmp.path(), &run, &[item("p1")]).unwrap();
        assert!(w.warning.is_some());
        assert!(!missing.exists());

        let w = write_proposals(None, tmp.path(), &run, &[]).unwrap();
        assert_eq!(w, Written::default(), "no items: nothing written");
    }

    #[test]
    fn retire_ids_carry_the_version_and_shapes_ignore_names() {
        let run = RunId("r".into());
        let p = retire_proposal("a.com", "t", 3, &StaleWhy::Misses(5), &run, "2026-10-26");
        assert_eq!(p.id, "t7-retire-a.com-t-v3");
        assert!(p.fields["why"]
            .as_str()
            .unwrap_or_default()
            .contains("after 5 misses"));
        let h = header("x", "a:string b?:number", "form input");
        assert_eq!(arg_shape(&h.args), "string,number?");
        assert_eq!(role_sequence(&h), "form,input");
        // No fingerprint: never grouped, however many domains.
        let bare = parse_header("// @name x\n// @kind reader\n// @gate Read\nreturn 1;\n").unwrap();
        let tools: Vec<MergeInput<'_>> = ["a.com", "b.com", "c.com"]
            .iter()
            .map(|d| MergeInput {
                domain: d,
                header: &bare,
            })
            .collect();
        assert!(merge_proposals(&tools, &run, "2026-10-26").is_empty());
    }
}
