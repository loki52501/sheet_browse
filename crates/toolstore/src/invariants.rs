//! T-9 / G-4 invariants (D-29, D-307): learn as data, never as permission.
//!
//! Two repair kinds. A relocate may change only `@fingerprint`. A re-forge may change the body
//! and raise `@gate`, but may not lower `@gate`, reduce the planner's `?s` count, add an
//! `@vault` key the old file did not use, rename the tool, change `@kind`, lower `@phase`
//! (2 -> 1, D-313), or put a scheme, host, `//` or `..` in `@match`. A planner's output class is `max(@gate floor, sb-act class)` and must
//! pass sb-act's `?s` rule (D-50), checked through [`PlannerCheck`] (D-301).

use crate::header::{
    body_start, header_lines, match_violation, parse_header, unsafe_header_line, ToolKind,
};
use crate::{StoreError, Tool};
use harness_core::Class;
use sb_act::ActClass;
use sb_sheet::IndexRow;

/// Which kind of repair produced the new file (D-307).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepairKind {
    /// T-4: only `@fingerprint` may change.
    Relocate,
    /// T-5: body and a higher `@gate` may change.
    Reforge,
}

fn invariant(msg: impl Into<String>) -> StoreError {
    StoreError::Invariant(msg.into())
}

/// The distinct `@vault(name)` keys in `src`, in first-use order.
pub fn vault_keys(src: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut rest = src;
    while let Some(p) = rest.find("@vault(") {
        let after = &rest[p + "@vault(".len()..];
        match after.find(')') {
            Some(end) => {
                let key = after[..end].trim().to_string();
                if !out.contains(&key) {
                    out.push(key);
                }
                rest = &after[end + 1..];
            }
            None => break,
        }
    }
    out
}

/// `src` with JS comments (`// ...`, `/* ... */`) replaced by a space, string and template
/// literals kept. `//` inside a string (`'https://x'`) is not a comment. Regex literals are
/// not recognised (a planner has no reason to contain `?s` in one).
pub(crate) fn strip_js_comments(src: &str) -> String {
    #[derive(Clone, Copy, PartialEq)]
    enum Mode {
        Code,
        Str(char),
        Template,
    }
    let c: Vec<char> = src.chars().collect();
    let mut out = String::with_capacity(src.len());
    let mut mode = Mode::Code;
    // Open `${` of templates: brace depth inside each.
    let mut subst: Vec<u32> = Vec::new();
    let mut i = 0;
    while i < c.len() {
        let ch = c[i];
        let next = c.get(i + 1).copied();
        match mode {
            Mode::Code => match (ch, next) {
                ('/', Some('/')) => {
                    while i < c.len() && c[i] != '\n' {
                        i += 1;
                    }
                    out.push(' ');
                    continue;
                }
                ('/', Some('*')) => {
                    i += 2;
                    while i < c.len() && !(c[i] == '*' && c.get(i + 1) == Some(&'/')) {
                        i += 1;
                    }
                    i += 2;
                    out.push(' ');
                    continue;
                }
                ('\'' | '"', _) => mode = Mode::Str(ch),
                ('`', _) => mode = Mode::Template,
                ('{', _) => {
                    if let Some(d) = subst.last_mut() {
                        *d += 1;
                    }
                }
                ('}', _) => match subst.last_mut() {
                    Some(0) => {
                        subst.pop();
                        mode = Mode::Template;
                    }
                    Some(d) => *d -= 1,
                    None => {}
                },
                _ => {}
            },
            Mode::Str(q) => {
                if ch == '\\' {
                    out.push(ch);
                    if let Some(n) = next {
                        out.push(n);
                    }
                    i += 2;
                    continue;
                }
                if ch == q || ch == '\n' {
                    mode = Mode::Code;
                }
            }
            Mode::Template => {
                if ch == '\\' {
                    out.push(ch);
                    if let Some(n) = next {
                        out.push(n);
                    }
                    i += 2;
                    continue;
                }
                if ch == '`' {
                    mode = Mode::Code;
                } else if ch == '$' && next == Some('{') {
                    out.push_str("${");
                    subst.push(0);
                    mode = Mode::Code;
                    i += 2;
                    continue;
                }
            }
        }
        out.push(ch);
        i += 1;
    }
    out
}

/// The `?s` act steps in the body of a tool file (the header is not counted): occurrences
/// of `?s` outside JS comments that are not the start of a longer word (`'?sort=1'` is a
/// query string, not a stop).
pub fn stop_count(src: &str) -> usize {
    let code = strip_js_comments(&src[body_start(src)..]);
    let b = code.as_bytes();
    code.match_indices("?s")
        .filter(|(i, _)| {
            b.get(i + 2)
                .is_none_or(|&n| !(n.is_ascii_alphanumeric() || n == b'_' || n == b'$'))
        })
        .count()
}

/// The header lines other than `@fingerprint`, normalised, for the relocate comparison.
fn non_fingerprint_header(src: &str) -> Vec<(String, String)> {
    header_lines(src)
        .into_iter()
        .filter(|(_, k, _)| *k != "fingerprint")
        .map(|(_, k, v)| (k.to_string(), v.to_string()))
        .collect()
}

/// T-9 / G-4 static check of a repair, `old` and `new` being whole tool files.
///
/// D-578: the new file's header block may not hold a lone CR, U+2028, U+2029 or another
/// control character but tab; JS would end a `//` comment there and run the rest of the
/// line, which every other check here reads as a comment.
pub fn check_repair(old: &str, new: &str, kind: RepairKind) -> Result<(), StoreError> {
    if let Some(n) = unsafe_header_line(new) {
        return Err(invariant(format!(
            "repair writes a control character or line terminator into header line {n} \
             (B-33, D-578)"
        )));
    }
    for (_, key, value) in header_lines(new) {
        if key == "match" {
            for p in value.split_whitespace() {
                if let Some(why) = match_violation(p) {
                    return Err(invariant(format!(
                        "@match widened: {p:?} {why} (G-4, D-29)"
                    )));
                }
            }
        }
    }
    let o =
        parse_header(old).map_err(|e| invariant(format!("old file has no valid header: {e}")))?;
    let n =
        parse_header(new).map_err(|e| invariant(format!("new file has no valid header: {e}")))?;
    if n.name != o.name {
        return Err(invariant(format!(
            "repair renamed the tool {} -> {}",
            o.name, n.name
        )));
    }
    if n.gate < o.gate {
        return Err(invariant(format!(
            "@gate lowered {} -> {} (G-4, D-29)",
            o.gate, n.gate
        )));
    }
    if kind == RepairKind::Relocate && n.gate != o.gate {
        return Err(invariant(format!(
            "relocate changed @gate {} -> {} (T-9)",
            o.gate, n.gate
        )));
    }
    // D-313: a reader turned planner (or back) is a new tool, not a repair.
    if n.kind != o.kind {
        return Err(invariant(format!(
            "repair changed @kind {} -> {} (G-4, D-313)",
            o.kind.as_str(),
            n.kind.as_str()
        )));
    }
    // D-313: @phase 1 runs on fetched HTML and gains page.api; 1 -> 2 only narrows.
    if n.phase < o.phase {
        return Err(invariant(format!(
            "@phase lowered {} -> {} (G-4, D-313)",
            o.phase, n.phase
        )));
    }
    let (so, sn) = (stop_count(old), stop_count(new));
    if (o.kind == ToolKind::Planner || n.kind == ToolKind::Planner) && sn < so {
        return Err(invariant(format!(
            "repair removed ?s ({so} -> {sn}) (T-9, D-29)"
        )));
    }
    let old_keys = vault_keys(old);
    if let Some(k) = vault_keys(new).into_iter().find(|k| !old_keys.contains(k)) {
        return Err(invariant(format!(
            "repair added vault key {} the tool did not use (G-4, D-29)",
            harness_core::vault::vault_literal(&k)
        )));
    }
    if kind == RepairKind::Relocate {
        if non_fingerprint_header(old) != non_fingerprint_header(new) {
            return Err(invariant(
                "relocate changed a header line other than @fingerprint (D-307)",
            ));
        }
        if old[body_start(old)..] != new[body_start(new)..] {
            return Err(invariant("relocate changed the tool body (D-307)"));
        }
    }
    Ok(())
}

/// D-301: how toolstore reaches sb-act. `check` parses `program`, classifies it against
/// `index` and applies the planner `?s` rule (D-50); `Err` carries the reason.
pub trait PlannerCheck: Send + Sync {
    fn check(&self, program: &str, index: &[IndexRow]) -> Result<ActClass, String>;
}

/// The default [`PlannerCheck`]: `sb_act::{parse, check_planner, classify}`.
#[derive(Debug, Clone, Copy, Default)]
pub struct SbAct;

impl PlannerCheck for SbAct {
    fn check(&self, program: &str, index: &[IndexRow]) -> Result<ActClass, String> {
        let steps = sb_act::parse(program).map_err(|e| e.to_string())?;
        sb_act::check_planner(&steps, index).map_err(|e| e.to_string())?;
        Ok(sb_act::classify(&steps, index))
    }
}

/// T-9: a planner's output class with hints, `class = max(@gate floor, classified)`.
pub fn planner_act_class(
    tool: &Tool,
    program: &str,
    index: &[IndexRow],
    check: &dyn PlannerCheck,
) -> Result<ActClass, StoreError> {
    if tool.header.kind != ToolKind::Planner {
        return Err(invariant(format!(
            "{} is a reader; only planners return programs",
            tool.namespaced
        )));
    }
    // D-309: the emitted program is authoritative. A key the source does not spell out as
    // a literal `@vault(k)` (built by concatenation, taken from args) is never resolved.
    let allowed = vault_keys(&tool.source);
    if let Some(k) = vault_keys(program)
        .into_iter()
        .find(|k| !allowed.contains(k))
    {
        return Err(invariant(format!(
            "{} output uses vault key {} that is not a literal in the tool file (G-4, D-309)",
            tool.namespaced,
            harness_core::vault::vault_literal(&k)
        )));
    }
    let mut ac = check
        .check(program, index)
        .map_err(|e| invariant(format!("{} output rejected: {e}", tool.namespaced)))?;
    ac.class = ac.class.at_least(tool.header.gate);
    Ok(ac)
}

/// T-9: `max(@gate floor, classified)` for a planner's output program.
pub fn planner_class(
    tool: &Tool,
    program: &str,
    index: &[IndexRow],
    check: &dyn PlannerCheck,
) -> Result<Class, StoreError> {
    planner_act_class(tool, program, index, check).map(|a| a.class)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    const OLD: &str = "// @name register\n// @kind planner\n// @match /events/*\n// @gate Submit\n// @fingerprint button\"Register\"\nexport default function (page, args) {\n  const p = '@vault(luma_pw)';\n  return `t#${page.find('input \"Email\"')}=${page.q(args.email)}; c#${page.find('button \"Register\"')}; ?s`;\n}\n";

    fn with(old: &str, from: &str, to: &str) -> String {
        assert!(old.contains(from), "{from}");
        old.replacen(from, to, 1)
    }

    fn msg(r: Result<(), StoreError>) -> String {
        match r {
            Err(StoreError::Invariant(m)) => m,
            other => panic!("expected Invariant, got {other:?}"),
        }
    }

    #[test]
    fn relocate_may_change_only_the_fingerprint() {
        let new = with(OLD, "button\"Register\"", "button\"Sign up\"");
        check_repair(OLD, &new, RepairKind::Relocate).unwrap();
        let body = with(OLD, "return", "return  ");
        assert!(msg(check_repair(OLD, &body, RepairKind::Relocate)).contains("body"));
        let hdr = with(OLD, "@kind planner", "@kind planner\n// @phase 2");
        assert!(msg(check_repair(OLD, &hdr, RepairKind::Relocate)).contains("header"));
    }

    #[test]
    fn gate_lowered_is_rejected_for_both_kinds() {
        let new = with(OLD, "@gate Submit", "@gate Fill");
        for k in [RepairKind::Relocate, RepairKind::Reforge] {
            assert!(msg(check_repair(OLD, &new, k)).contains("@gate lowered"));
        }
    }

    #[test]
    fn gate_changed_on_relocate_is_rejected_but_reforge_may_raise() {
        let new = with(OLD, "@gate Submit", "@gate Pay");
        assert!(
            msg(check_repair(OLD, &new, RepairKind::Relocate)).contains("relocate changed @gate")
        );
        check_repair(OLD, &new, RepairKind::Reforge).unwrap();
    }

    #[test]
    fn removing_stop_is_rejected() {
        let new = with(OLD, "; ?s`", "`");
        assert!(msg(check_repair(OLD, &new, RepairKind::Reforge)).contains("removed ?s"));
    }

    #[test]
    fn new_vault_key_is_rejected_old_key_is_fine() {
        let new = with(OLD, "return", "const x = '@vault(bank_pin)';\n  return");
        let m = msg(check_repair(OLD, &new, RepairKind::Reforge));
        assert!(m.contains("@vault(bank_pin)"), "{m}");
        let same = with(OLD, "return", "const y = '@vault(luma_pw)';\n  return");
        check_repair(OLD, &same, RepairKind::Reforge).unwrap();
        assert_eq!(vault_keys(OLD), vec!["luma_pw"]);
    }

    #[test]
    fn widened_match_is_rejected() {
        for m in [
            "https://evil.com/*",
            "//evil.com/*",
            "evil.com/*",
            "/a/../b",
            "/*  *",
        ] {
            let new = with(OLD, "@match /events/*", &format!("@match {m}"));
            assert!(
                msg(check_repair(OLD, &new, RepairKind::Reforge)).contains("@match widened"),
                "{m}"
            );
        }
        let ok = with(OLD, "@match /events/*", "@match /*");
        check_repair(OLD, &ok, RepairKind::Reforge).unwrap();
    }

    #[test]
    fn kind_change_is_rejected_both_ways() {
        let reader = with(OLD, "@kind planner", "@kind reader");
        for k in [RepairKind::Relocate, RepairKind::Reforge] {
            let m = msg(check_repair(OLD, &reader, k));
            assert!(m.contains("@kind planner -> reader"), "{m}");
            let m = msg(check_repair(&reader, OLD, k));
            assert!(m.contains("@kind reader -> planner"), "{m}");
        }
    }

    #[test]
    fn phase_lowered_is_rejected_raised_is_allowed() {
        // OLD has no @phase line, which means 2.
        let p1 = with(OLD, "@kind planner", "@kind planner\n// @phase 1");
        let explicit2 = with(OLD, "@kind planner", "@kind planner\n// @phase 2");
        for old in [OLD, explicit2.as_str()] {
            let m = msg(check_repair(old, &p1, RepairKind::Reforge));
            assert!(m.contains("@phase lowered 2 -> 1"), "{m}");
        }
        check_repair(&p1, &explicit2, RepairKind::Reforge).unwrap();
        check_repair(&p1, OLD, RepairKind::Reforge).unwrap();
        check_repair(&p1, &p1, RepairKind::Reforge).unwrap();
    }

    #[test]
    fn rename_and_garbage_are_rejected() {
        let new = with(OLD, "@name register", "@name register2");
        assert!(msg(check_repair(OLD, &new, RepairKind::Reforge)).contains("renamed"));
        assert!(msg(check_repair(OLD, "garbage", RepairKind::Reforge)).contains("no valid header"));
    }

    struct Fake(Result<ActClass, String>);
    impl PlannerCheck for Fake {
        fn check(&self, _: &str, _: &[IndexRow]) -> Result<ActClass, String> {
            self.0.clone()
        }
    }

    fn tool(kind: &str, gate: &str) -> Tool {
        let src = format!("// @name t\n// @kind {kind}\n// @gate {gate}\n");
        Tool {
            domain: "luma.com".into(),
            namespaced: "luma.t".into(),
            path: PathBuf::from("t.js"),
            header: parse_header(&src).unwrap(),
            source: src,
        }
    }

    #[test]
    fn planner_class_is_max_of_floor_and_classified() {
        let fill = Fake(Ok(ActClass {
            class: Class::Fill,
            hints: vec![],
        }));
        let t = tool("planner", "Submit");
        assert_eq!(
            planner_class(&t, "t#1=a", &[], &fill).unwrap(),
            Class::Submit
        );
        let pay = Fake(Ok(ActClass {
            class: Class::Pay,
            hints: vec!["job_application".into()],
        }));
        let ac = planner_act_class(&t, "c#9; ?s", &[], &pay).unwrap();
        assert_eq!(ac.class, Class::Pay);
        assert_eq!(ac.hints, vec!["job_application"]);
    }

    #[test]
    fn planner_check_failure_and_reader_are_invariants() {
        let bad = Fake(Err(
            "step 2 is Submit but the next step is not ?s (D-50)".into()
        ));
        let t = tool("planner", "Submit");
        assert!(matches!(
            planner_class(&t, "c#9", &[], &bad),
            Err(StoreError::Invariant(m)) if m.contains("?s")
        ));
        let ok = Fake(Ok(ActClass {
            class: Class::Read,
            hints: vec![],
        }));
        assert!(matches!(
            planner_class(&tool("reader", "Read"), "", &[], &ok),
            Err(StoreError::Invariant(_))
        ));
    }
}
