//! `gh run view <id> --log` output: one line per log line, `<job>\t<step>\t<timestamp> <text>`.
//! A step failed when it has a `##[error]` line; the first such line in the log marks the
//! first failing step (gh prints jobs, and steps within a job, in order).

/// Failing lines kept.
const MAX_FAILING: usize = 20;
/// Lines before the first error shown when the failing lines alone say little.
const MAX_CONTEXT: usize = 10;
/// Chars per shown line.
const LINE_CHARS: usize = 300;

/// The first failing step of a run log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FailingStep {
    pub job: String,
    pub step: String,
    /// 1-based line of the first `##[error]` in the log.
    pub line_no: usize,
    /// Error-looking lines of the step (marker `##[error]` shown as `[error] `), in order,
    /// deduplicated, at most 20.
    pub failing: Vec<String>,
    /// When `failing` has at most two lines: up to 10 lines right before the first error.
    pub context: Vec<String>,
}

struct Line<'a> {
    job: &'a str,
    step: &'a str,
    text: &'a str,
}

fn parse_line(l: &str) -> Line<'_> {
    let l = l.strip_suffix('\r').unwrap_or(l);
    let mut it = l.splitn(3, '\t');
    match (it.next(), it.next(), it.next()) {
        (Some(job), Some(step), Some(rest)) => Line {
            job,
            step,
            text: strip_timestamp(rest),
        },
        _ => Line {
            job: "",
            step: "",
            text: l,
        },
    }
}

/// `2024-05-01T10:00:00.1234567Z text` -> `text`; a leading BOM goes too.
fn strip_timestamp(rest: &str) -> &str {
    let rest = rest.strip_prefix('\u{feff}').unwrap_or(rest);
    let (head, tail) = rest.split_once(' ').unwrap_or((rest, ""));
    let b = head.as_bytes();
    let is_ts = b.len() >= 20
        && b[4] == b'-'
        && b[7] == b'-'
        && b[10] == b'T'
        && head.ends_with('Z')
        && b[..4].iter().all(u8::is_ascii_digit);
    if is_ts {
        tail.strip_prefix('\u{feff}').unwrap_or(tail)
    } else {
        rest
    }
}

fn is_error_line(text: &str) -> bool {
    let t = text.trim_start();
    t.contains("##[error]")
        || t.starts_with("error")
        || t.starts_with("Error")
        || t.starts_with("ERROR")
        || t.contains("FAILED")
        || t.contains("panicked at")
        || t.contains("assertion")
}

fn is_group_marker(text: &str) -> bool {
    let t = text.trim_start();
    t.starts_with("##[group]") || t.starts_with("##[endgroup]")
}

fn show(text: &str) -> String {
    let t = text.trim();
    let t = match t.find("##[error]") {
        Some(i) => format!("{}[error] {}", &t[..i], &t[i + "##[error]".len()..]),
        None => t.to_string(),
    };
    crate::text::line(&t, LINE_CHARS)
}

/// The first failing step, or `None` when no line carries `##[error]`.
pub fn first_failing_step(log: &str) -> Option<FailingStep> {
    let lines: Vec<Line<'_>> = log.lines().map(parse_line).collect();
    let first = lines.iter().position(|l| l.text.contains("##[error]"))?;
    let (job, step) = (lines[first].job, lines[first].step);
    let same = |l: &Line<'_>| l.job == job && l.step == step;

    let mut failing: Vec<String> = Vec::new();
    for l in lines.iter().filter(|l| same(l) && is_error_line(l.text)) {
        let s = show(l.text);
        if !s.is_empty() && !failing.contains(&s) {
            failing.push(s);
            if failing.len() == MAX_FAILING {
                break;
            }
        }
    }
    let mut context = Vec::new();
    if failing.len() <= 2 {
        for l in lines[..first].iter().rev() {
            if !same(l) || context.len() == MAX_CONTEXT {
                break;
            }
            if l.text.trim().is_empty() || is_group_marker(l.text) || is_error_line(l.text) {
                continue;
            }
            context.push(show(l.text));
        }
        context.reverse();
    }
    Some(FailingStep {
        job: crate::text::line(job, LINE_CHARS),
        step: crate::text::line(step, LINE_CHARS),
        line_no: first + 1,
        failing,
        context,
    })
}

/// The model's view of a run log: the first failing step, short, and where the full log is.
pub fn summary(log: &str, saved_as: &str) -> String {
    let n = log.lines().count();
    let mut out = match first_failing_step(log) {
        None => format!("no failing step in the log (no ##[error] line in {n} lines)\n"),
        Some(f) => {
            let mut s = format!(
                "first failing step: job {:?}, step {:?} (line {} of {n})\nfailing lines:\n",
                f.job, f.step, f.line_no
            );
            for l in &f.failing {
                s.push_str("  ");
                s.push_str(l);
                s.push('\n');
            }
            if !f.context.is_empty() {
                s.push_str("before the error:\n");
                for l in &f.context {
                    s.push_str("  ");
                    s.push_str(l);
                    s.push('\n');
                }
            }
            s
        }
    };
    out.push_str(&format!("full log: {saved_as} ({n} lines)"));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOG: &str = "build\tSet up job\t2024-05-01T10:00:00.0000000Z Current runner version: '2.316.0'\n\
build\tRun cargo test\t2024-05-01T10:00:01.0000000Z ##[group]Run cargo test\n\
build\tRun cargo test\t2024-05-01T10:00:02.0000000Z running 3 tests\n\
build\tRun cargo test\t2024-05-01T10:00:03.0000000Z test a ... FAILED\n\
build\tRun cargo test\t2024-05-01T10:00:04.0000000Z ##[error]Process completed with exit code 101.\n\
lint\tclippy\t2024-05-01T10:01:00.0000000Z ##[error]other failure\n";

    #[test]
    fn finds_first_failing_step_and_its_lines() {
        let f = first_failing_step(LOG).unwrap();
        assert_eq!(f.job, "build");
        assert_eq!(f.step, "Run cargo test");
        assert_eq!(f.line_no, 5);
        assert_eq!(
            f.failing,
            vec![
                "test a ... FAILED".to_string(),
                "[error] Process completed with exit code 101.".to_string()
            ]
        );
        assert_eq!(f.context, vec!["running 3 tests".to_string()]);
    }

    #[test]
    fn no_error_marker_is_none() {
        assert!(first_failing_step("a\tb\t2024-05-01T10:00:00.0000000Z fine\n").is_none());
        assert!(summary("x\n", "files/gh-run.log").starts_with("no failing step"));
    }

    #[test]
    fn timestamp_strip_is_char_safe() {
        assert_eq!(strip_timestamp("\u{e9}\u{e9}\u{e9}"), "\u{e9}\u{e9}\u{e9}");
        assert_eq!(strip_timestamp("2024-05-01T10:00:00.0Z hi"), "hi");
    }
}
