//! F-2: the gate class of a shell command, derived from what it does (D-520).
//!
//! * Read: commands known to only read (ls, cat, grep, git status, curl GET...), and
//!   changes whose every target resolves inside `runs/` (`rm -rf runs/x`).
//! * Fill: writes outside `runs/` that create or append (`echo x >> notes.txt`, `mkdir`,
//!   `cp a new.txt`), local git and cargo work (commit, build, test, fmt).
//! * Submit: sending data (`curl -d`, `curl -X POST|PUT|PATCH`, `wget --post-data`,
//!   `gh pr create`).
//! * Send: `git send-email`.
//! * Account: publishing or changing remote settings (`git push`, `gh api -X POST`,
//!   `gh pr merge`, `cargo publish`, `git config` writes).
//! * Delete: deleting, replacing or moving away anything outside `runs/` (or a run's
//!   bookkeeping), discarding work (`git reset --hard`, `git clean`, `git checkout --`),
//!   rewriting remote history (`git push --force`), HTTP DELETE.
//!
//! Fail closed: a command the classifier does not know, a target it cannot resolve (a
//! variable, `~`, a brace expansion, a relative path after a `cd` it cannot follow, input
//! from xargs or find -exec), or a line it cannot parse is Delete: it asks, and an
//! unanswered ask skips (H-1.3). The class of a compound command is the highest class of
//! its parts, command substitutions included.

use super::lex::{self, Simple, Word};
use crate::paths::{self, Place, Roots};
use crate::util::Verdict;
use harness_core::Class;
use std::path::{Path, PathBuf};

/// How deep `bash -c`, `eval` and substitutions may nest before the line is refused.
const MAX_DEPTH: u8 = 6;

/// Commands that only read (their redirections are still checked).
const READ_ONLY: &[&str] = &[
    "ls",
    "dir",
    "vdir",
    "cat",
    "head",
    "tail",
    "wc",
    "grep",
    "egrep",
    "fgrep",
    "rg",
    "ag",
    "ack",
    "echo",
    "printf",
    "pwd",
    "which",
    "where",
    "whereis",
    "type",
    "file",
    "stat",
    "du",
    "df",
    "cut",
    "tr",
    "diff",
    "cmp",
    "comm",
    "date",
    "whoami",
    "hostname",
    "uname",
    "true",
    "false",
    "test",
    "[",
    "[[",
    "]]",
    "basename",
    "dirname",
    "realpath",
    "readlink",
    "md5sum",
    "sha1sum",
    "sha224sum",
    "sha256sum",
    "sha384sum",
    "sha512sum",
    "b2sum",
    "cksum",
    "base64",
    "base32",
    "jq",
    "column",
    "nl",
    "less",
    "more",
    "tree",
    "od",
    "xxd",
    "hexdump",
    "strings",
    "seq",
    "sleep",
    "id",
    "groups",
    "ps",
    "uptime",
    "free",
    "printenv",
    "rev",
    "tac",
    "fold",
    "fmt",
    "paste",
    "join",
    "expand",
    "unexpand",
    "numfmt",
    "factor",
    "expr",
    "nproc",
    "locale",
    "tty",
    "ping",
    "nslookup",
    "dig",
    "host",
    "tracert",
    "traceroute",
    "whois",
    "cal",
    "getconf",
    "tasklist",
    "systeminfo",
    "export",
    "unset",
    "shift",
    "read",
    "wait",
    "exit",
    "return",
    "break",
    "continue",
    "local",
    "declare",
    "typeset",
    "let",
    ":",
    "history",
    "clear",
    "bc",
    "iconv",
    "file",
    "zcat",
    "zless",
    "bzcat",
    "xzcat",
    "md5",
    "shasum",
];

/// Shell keywords skipped in front of a command.
const KEYWORDS: &[&str] = &[
    "if", "then", "else", "elif", "fi", "do", "done", "while", "until", "!", "{", "}", "time",
];

/// Where relative paths resolve while the line is walked: every folder the shell may be
/// in at this point (a relative target must be safe from each), `None` after a `cd` the
/// classifier cannot follow. A `cd` replaces the set only when everything after it in the
/// line is joined by `&&` and the line has no subshell, pipe, `||` or `&`: then a failed
/// `cd` stops the rest. Otherwise (`cd x; rm y`, `(cd x); rm y`) the new folder is added.
#[derive(Clone)]
struct Env<'a> {
    roots: &'a Roots,
    cwds: Option<Vec<PathBuf>>,
    /// Set per command: a `cd` here replaces the folder set (see above).
    cd_replaces: bool,
}

impl Env<'_> {
    /// Every candidate folder is scratch.
    fn cwd_scratch(&self) -> Option<bool> {
        self.cwds
            .as_ref()
            .map(|c| c.iter().all(|d| self.roots.place(d) == Place::Scratch))
    }
}

/// The class of `cmd` run in `cwd`. `cmd_exe`: the line runs under cmd.exe (no bash on
/// this machine); `%` and `^` then mean things this bash lexer does not see.
pub fn classify(roots: &Roots, cwd: &Path, cmd: &str, cmd_exe: bool) -> Verdict {
    let mut v = Verdict::new(Class::Read);
    if cmd_exe && (cmd.contains('%') || cmd.contains('^')) {
        v.raise(Class::Delete, "cmd.exe syntax (% or ^) is not parsed");
        return v;
    }
    let mut env = Env {
        roots,
        cwds: Some(vec![cwd.to_path_buf()]),
        cd_replaces: false,
    };
    source(&mut env, cmd, 0, &mut v);
    v
}

fn source(env: &mut Env, src: &str, depth: u8, v: &mut Verdict) {
    if depth > MAX_DEPTH {
        v.raise(Class::Delete, "commands nested too deep to check");
        return;
    }
    let parsed = match lex::parse(src) {
        Ok(p) => p,
        Err(e) => {
            v.raise(Class::Delete, format!("cannot parse the command ({e})"));
            return;
        }
    };
    for sub in &parsed.subs {
        let mut inner = env.clone();
        source(&mut inner, sub, depth + 1, v);
    }
    for (i, c) in parsed.commands.iter().enumerate() {
        env.cd_replaces = !parsed.structured
            && parsed.commands[i..]
                .iter()
                .all(|c| matches!(c.next_sep, "&&" | ""));
        simple(env, c, depth, v);
    }
}

fn simple(env: &mut Env, c: &Simple, depth: u8, v: &mut Verdict) {
    for r in &c.redirs {
        let Some(t) = &r.target else { continue };
        match r.op.as_str() {
            ">" | ">|" | "&>" | ">&" | "<>" => write_target(env, t, true, v),
            ">>" | "&>>" => write_target(env, t, false, v),
            _ => {}
        }
    }
    words(env, &c.words, false, depth, v);
}

/// Classifies a command given as words. `unknown_args`: more arguments arrive at run time
/// (xargs, find -exec), so targets cannot be checked.
fn words(env: &mut Env, ws: &[Word], unknown_args: bool, depth: u8, v: &mut Verdict) {
    let mut i = 0;
    while i < ws.len() {
        let w = &ws[i];
        let t = w.text.as_str();
        let keyword = KEYWORDS.contains(&t) && w.is_static() && !w.quoted;
        let assignment = is_assignment(w);
        if assignment {
            risky_assignment(w, v);
        }
        if !(keyword || assignment) {
            break;
        }
        i += 1;
    }
    let ws = &ws[i..];
    let Some(head) = ws.first() else { return };
    if !head.is_static() {
        v.raise(Class::Delete, "the command name is decided at run time");
        return;
    }
    let name = command_name(&head.text);
    let args = &ws[1..];
    if args.len() == 1 && matches!(args[0].text.as_str(), "--version" | "--help" | "-V") {
        return;
    }
    match name.as_str() {
        "for" | "select" => {}
        "export" | "declare" | "local" | "typeset" | "readonly" => {
            for a in args.iter().filter(|a| is_assignment(a)) {
                risky_assignment(a, v);
            }
        }
        "tree" => {
            if let Some(o) = flag_value(args, "-o") {
                write_target(env, &o, true, v);
            }
        }
        "iconv" => {
            let out = flag_value(args, "-o").or_else(|| {
                args.iter().find_map(|a| {
                    a.text.strip_prefix("--output=").map(|r| Word {
                        text: r.to_string(),
                        ..a.clone()
                    })
                })
            });
            if let Some(o) = out {
                write_target(env, &o, true, v);
            }
        }
        "xxd" => {
            // `xxd infile outfile`: the second operand is written.
            if let Some(o) = operands(args, false).get(1) {
                write_target(env, o, true, v);
            }
        }
        "rg" => {
            if args.iter().any(|a| a.text.starts_with("--pre")) {
                v.raise(Class::Delete, "rg --pre runs a program on every file");
            }
        }
        n if READ_ONLY.contains(&n) => {}
        "cd" | "pushd" | "chdir" => cd(env, args),
        "popd" => env.cwds = None,
        "env" => {
            let mut k = 0;
            while k < args.len() {
                let a = &args[k];
                if a.text == "-C" || a.text == "--chdir" || a.text.starts_with("--chdir=") {
                    env.cwds = None;
                    k += if a.text.contains('=') { 1 } else { 2 };
                } else if a.text == "-u" || a.text == "--unset" {
                    k += 2;
                } else if is_assignment(a) {
                    risky_assignment(a, v);
                    k += 1;
                } else if a.text.starts_with('-') {
                    k += 1;
                } else {
                    break;
                }
            }
            words(env, &args[k.min(args.len())..], unknown_args, depth, v);
        }
        "command" | "builtin" | "exec" | "nohup" | "setsid" => {
            let k = args
                .iter()
                .position(|a| !a.text.starts_with('-'))
                .unwrap_or(args.len());
            words(env, &args[k..], unknown_args, depth, v);
        }
        "nice" | "ionice" | "stdbuf" | "timeout" => {
            let mut k = 0;
            while k < args.len() && args[k].text.starts_with('-') {
                let takes = matches!(
                    args[k].text.as_str(),
                    "-n" | "-c" | "-s" | "-k" | "-i" | "-o" | "-e"
                );
                k += if takes { 2 } else { 1 };
            }
            if name == "timeout" {
                k += 1; // the duration
            }
            words(env, &args[k.min(args.len())..], unknown_args, depth, v);
        }
        "sudo" | "doas" | "su" | "runas" | "gsudo" => {
            v.raise(Class::Delete, format!("{name} runs as another user"))
        }
        "bash" | "sh" | "zsh" | "dash" | "ksh" | "ash" => {
            match args.iter().position(|a| {
                a.text.starts_with('-') && !a.text.starts_with("--") && a.text.contains('c')
            }) {
                Some(k) => match args.get(k + 1) {
                    Some(script) if !script.dynamic => {
                        let mut inner = env.clone();
                        source(&mut inner, &script.text, depth + 1, v);
                    }
                    _ => v.raise(
                        Class::Delete,
                        format!("{name} -c with a script decided at run time"),
                    ),
                },
                None => v.raise(
                    Class::Delete,
                    format!("{name} runs a script the classifier cannot see"),
                ),
            }
        }
        "eval" => {
            if args.iter().all(|a| !a.dynamic) {
                let joined: Vec<&str> = args.iter().map(|a| a.text.as_str()).collect();
                let mut inner = env.clone();
                source(&mut inner, &joined.join(" "), depth + 1, v);
            } else {
                v.raise(Class::Delete, "eval of text decided at run time");
            }
        }
        "xargs" => {
            let mut k = 0;
            while k < args.len() && args[k].text.starts_with('-') {
                let a = args[k].text.as_str();
                let takes = matches!(
                    a,
                    "-n" | "-I" | "-P" | "-d" | "-L" | "-E" | "-s" | "-a" | "-i"
                );
                k += if takes && a.len() == 2 { 2 } else { 1 };
            }
            words(env, &args[k.min(args.len())..], true, depth, v);
        }
        "find" => find(env, args, depth, v),
        "rm" | "unlink" | "rmdir" | "shred" | "trash" | "del" | "erase" | "rd" | "remove-item"
        | "ri" => {
            let windows_style = matches!(name.as_str(), "del" | "erase" | "rd" | "rmdir");
            let targets = operands(args, windows_style);
            if unknown_args {
                v.raise(
                    Class::Delete,
                    format!("{name} of paths that arrive at run time"),
                );
            }
            for t in targets {
                delete_target(env, t, v);
            }
        }
        "mv" | "move" | "move-item" => move_or_copy(env, args, true, unknown_args, v),
        "cp" | "copy" | "copy-item" | "install" => move_or_copy(env, args, false, unknown_args, v),
        "rsync" => {
            if args
                .iter()
                .any(|a| a.text.starts_with("--delete") || a.text.starts_with("--remove-source"))
            {
                v.raise(Class::Delete, "rsync --delete or --remove-source-files");
            }
            if args.iter().any(|a| {
                a.text == "-e" || a.text.starts_with("--rsh") || a.text.starts_with("--rsync-path")
            }) {
                v.raise(Class::Delete, "rsync -e runs a program");
            }
            move_or_copy(env, args, false, unknown_args, v);
        }
        "touch" | "mkdir" | "chmod" | "chown" | "chgrp" | "attrib" | "icacls" | "setfacl" => {
            let mut ops = operands(args, false);
            if matches!(name.as_str(), "chmod" | "chown" | "chgrp" | "setfacl") && !ops.is_empty() {
                ops.remove(0); // the mode or owner
            }
            if unknown_args {
                v.raise(
                    Class::Delete,
                    format!("{name} of paths that arrive at run time"),
                );
            }
            for t in ops {
                modify_target(env, t, &name, v);
            }
        }
        "ln" => {
            let force = args
                .iter()
                .any(|a| short_flag(a, 'f') || a.text == "--force");
            let ops = operands(args, false);
            if unknown_args {
                v.raise(Class::Delete, "ln of paths that arrive at run time");
            }
            if let Some(dst) = ops.last() {
                if force && ops.len() > 1 {
                    write_target(env, dst, true, v);
                } else {
                    modify_target(env, dst, "ln", v);
                }
            }
        }
        "truncate" => {
            let mut k = 0;
            let mut ops = Vec::new();
            while k < args.len() {
                let a = &args[k];
                if a.text == "-s" || a.text == "-r" || a.text == "--reference" {
                    k += 2;
                    continue;
                }
                if !a.text.starts_with('-') {
                    ops.push(a);
                }
                k += 1;
            }
            for t in ops {
                write_target(env, t, true, v);
            }
        }
        "dd" => {
            for a in args {
                if let Some(rest) = a.text.strip_prefix("of=") {
                    let w = Word {
                        text: rest.to_string(),
                        ..a.clone()
                    };
                    write_target(env, &w, true, v);
                }
            }
        }
        "tee" => {
            let append = args
                .iter()
                .any(|a| short_flag(a, 'a') || a.text == "--append");
            for t in operands(args, false) {
                write_target(env, t, !append, v);
            }
        }
        "sort" => {
            let mut k = 0;
            while k < args.len() {
                let a = &args[k].text;
                if a == "-o" || a == "--output" {
                    if let Some(t) = args.get(k + 1) {
                        write_target(env, t, true, v);
                    }
                    k += 2;
                    continue;
                }
                if let Some(rest) = a.strip_prefix("--output=").or_else(|| a.strip_prefix("-o")) {
                    let w = Word {
                        text: rest.to_string(),
                        ..args[k].clone()
                    };
                    write_target(env, &w, true, v);
                }
                k += 1;
            }
        }
        "uniq" => {
            let mut k = 0;
            let mut ops = Vec::new();
            while k < args.len() {
                let a = &args[k].text;
                if matches!(a.as_str(), "-f" | "-s" | "-w") {
                    k += 2;
                    continue;
                }
                if !a.starts_with('-') || a == "-" {
                    ops.push(&args[k]);
                }
                k += 1;
            }
            if let Some(out) = ops.get(1) {
                write_target(env, out, true, v);
            }
        }
        "sed" => sed(env, args, v),
        "awk" | "gawk" | "mawk" | "nawk" => awk(args, v),
        "gzip" | "gunzip" | "bzip2" | "bunzip2" | "xz" | "unxz" | "zstd" | "unzstd" => {
            let to_stdout = args.iter().any(|a| {
                short_flag(a, 'c')
                    || short_flag(a, 'l')
                    || short_flag(a, 't')
                    || a.text == "--stdout"
            });
            let keep = args
                .iter()
                .any(|a| short_flag(a, 'k') || a.text == "--keep");
            if to_stdout {
                return;
            }
            for t in operands(args, false) {
                if keep {
                    modify_target(env, t, &name, v);
                } else {
                    delete_target(env, t, v);
                }
            }
        }
        "tar" => tar(env, args, v),
        "unzip" => {
            if args
                .iter()
                .any(|a| matches!(a.text.as_str(), "-l" | "-v" | "-t" | "-Z"))
            {
                return;
            }
            let dest = flag_value(args, "-d");
            extract_into(env, dest, "unzip", v);
        }
        "zip" => {
            if args
                .iter()
                .any(|a| short_flag(a, 'm') || a.text == "--move")
            {
                v.raise(Class::Delete, "zip -m deletes the files it adds");
            }
            if args
                .iter()
                .any(|a| a.text == "-TT" || a.text.starts_with("--unzip-command"))
            {
                v.raise(Class::Delete, "zip -TT runs a program");
            }
            if let Some(archive) = operands(args, false).first() {
                write_target(env, archive, false, v);
            }
        }
        "curl" => curl(env, args, v),
        "wget" => wget(env, args, v),
        "git" => git(env, args, v),
        "gh" => gh(args, v),
        "cargo" => cargo(args, v),
        "npm" | "pnpm" | "yarn" | "bun" => npm(&name, args, v),
        "pip" | "pip3" => pip(args, v),
        "docker" | "podman" => {
            let sub = args
                .iter()
                .find(|a| !a.text.starts_with('-'))
                .map(|a| a.text.as_str());
            if !matches!(
                sub,
                Some(
                    "ps" | "images"
                        | "logs"
                        | "inspect"
                        | "version"
                        | "info"
                        | "stats"
                        | "top"
                        | "port"
                        | "history"
                )
            ) {
                v.raise(
                    Class::Delete,
                    format!("{name} {} is not known to be read-only", sub.unwrap_or("")),
                );
            }
        }
        _ => v.raise(
            Class::Delete,
            format!(
                "{} is not a command the classifier knows to be safe",
                crate::text::one_line(&name, 60)
            ),
        ),
    }
}

/// `NAME=value` (an assignment, not an argument).
fn is_assignment(w: &Word) -> bool {
    let Some((name, _)) = w.text.split_once('=') else {
        return false;
    };
    !name.is_empty()
        && !name.starts_with(|c: char| c.is_ascii_digit())
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Variables that make later commands run a program or load code (`PAGER`, `LESSOPEN`,
/// `GIT_SSH_COMMAND`, `LD_PRELOAD`, `BASH_ENV`, `PATH`...). Setting one is Delete: the
/// classifier cannot see what it will run.
fn risky_assignment(w: &Word, v: &mut Verdict) {
    let Some((name, _)) = w.text.split_once('=') else {
        return;
    };
    let n = name.to_ascii_uppercase();
    let risky = matches!(
        n.as_str(),
        "PATH"
            | "BASH_ENV"
            | "ENV"
            | "PROMPT_COMMAND"
            | "IFS"
            | "SHELLOPTS"
            | "BASHOPTS"
            | "CDPATH"
            | "LESSOPEN"
            | "LESSCLOSE"
            | "PAGER"
            | "MANPAGER"
            | "EDITOR"
            | "VISUAL"
            | "BROWSER"
            | "NODE_OPTIONS"
            | "PERL5OPT"
            | "PERL5LIB"
            | "RUBYOPT"
            | "PYTHONSTARTUP"
            | "PYTHONPATH"
            | "COMSPEC"
            | "SHELL"
    ) || n.starts_with("LD_")
        || n.starts_with("DYLD_")
        || (n.starts_with("GIT_")
            && (n.contains("SSH")
                || n.contains("EDITOR")
                || n.contains("PAGER")
                || n.contains("ASKPASS")
                || n.contains("EXTERNAL")
                || n.contains("CONFIG")
                || n.contains("EXEC")
                || n.contains("PROXY")
                || n.contains("TEMPLATE")))
        || n.contains("ASKPASS")
        || n.starts_with("BASH_FUNC_");
    if risky {
        v.raise(
            Class::Delete,
            format!(
                "sets {}, which can make later commands run a program",
                crate::text::one_line(name, 40)
            ),
        );
    }
}

/// The command's base name, lowercase, without a Windows program extension.
fn command_name(text: &str) -> String {
    let base = text
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(text)
        .to_lowercase();
    for ext in [".exe", ".cmd", ".bat", ".com"] {
        if let Some(stem) = base.strip_suffix(ext) {
            return stem.to_string();
        }
    }
    base
}

/// `-xyz` contains short flag `f` (`--long` does not count).
fn short_flag(w: &Word, f: char) -> bool {
    w.text.starts_with('-')
        && !w.text.starts_with("--")
        && w.text.len() > 1
        && w.text[1..].contains(f)
}

/// The arguments that are not flags. Everything after `--` is an operand. With
/// `windows_style`, `/s` `/q` `/f` `/p` `/a` are flags too.
fn operands(args: &[Word], windows_style: bool) -> Vec<&Word> {
    let mut out = Vec::new();
    let mut rest = false;
    for a in args {
        let t = a.text.as_str();
        if rest {
            out.push(a);
        } else if t == "--" && a.is_static() {
            rest = true;
        } else {
            let dash_flag = t.starts_with('-') && t.len() > 1 && !a.quoted;
            let slash_flag = windows_style
                && t.len() == 2
                && t.starts_with('/')
                && matches!(
                    t.as_bytes()[1].to_ascii_lowercase(),
                    b's' | b'q' | b'f' | b'p' | b'a'
                );
            if !(dash_flag || slash_flag) {
                out.push(a);
            }
        }
    }
    out
}

/// Value of `-x VALUE` or `-xVALUE`.
fn flag_value(args: &[Word], flag: &str) -> Option<Word> {
    for (k, a) in args.iter().enumerate() {
        if a.text == flag {
            return args.get(k + 1).cloned();
        }
        if let Some(rest) = a.text.strip_prefix(flag) {
            if !rest.is_empty() && flag.len() == 2 {
                return Some(Word {
                    text: rest.to_string(),
                    ..a.clone()
                });
            }
        }
    }
    None
}

/// A word resolved to paths.
enum Target {
    Path(PathBuf),
    Null,
    Unknown(String),
}

fn resolve(env: &Env, w: &Word) -> Vec<Target> {
    let shown = crate::text::one_line(&w.text, 80);
    if w.dynamic {
        return vec![Target::Unknown(format!(
            "{shown}... is decided at run time"
        ))];
    }
    if w.tilde || w.brace {
        return vec![Target::Unknown(format!("{shown} expands at run time"))];
    }
    let t = w.text.as_str();
    if matches!(t, "/dev/null" | "/dev/stdout" | "/dev/stderr" | "/dev/tty")
        || t.eq_ignore_ascii_case("nul")
    {
        return vec![Target::Null];
    }
    let converted = paths::msys_drive(t);
    if cfg!(windows) && converted.starts_with('/') {
        return vec![Target::Unknown(format!("{shown} is a Git Bash path"))];
    }
    let absolute = Path::new(&converted).is_absolute();
    match &env.cwds {
        _ if absolute => resolve_in(env, w, &env.roots.base, &converted, &shown),
        Some(cwds) => cwds
            .iter()
            .flat_map(|cwd| resolve_in(env, w, cwd, &converted, &shown))
            .collect(),
        None => vec![Target::Unknown(format!(
            "{shown} is relative to a folder the classifier cannot follow"
        ))],
    }
}

/// `converted` (from word `w`) resolved against one folder.
fn resolve_in(env: &Env, w: &Word, cwd: &Path, converted: &str, shown: &str) -> Vec<Target> {
    if !w.glob {
        return vec![one(cwd, converted)];
    }
    // A glob: every match, plus its fixed folder part (which must not climb out).
    let norm = converted.replace('\\', "/");
    if norm
        .split('/')
        .any(|seg| seg.contains("..") && seg.contains(['*', '?', '[']))
    {
        return vec![Target::Unknown(format!("{shown}: a glob with .."))];
    }
    let full = if Path::new(converted).is_absolute() {
        norm.clone()
    } else {
        format!(
            "{}/{}",
            glob::Pattern::escape(&paths::plain(cwd).replace('\\', "/")),
            norm
        )
    };
    let Ok(matches) = glob::glob(&full) else {
        return vec![Target::Unknown(format!(
            "{shown}: a glob the classifier cannot read"
        ))];
    };
    let fixed: Vec<&str> = norm
        .split('/')
        .take_while(|seg| !seg.contains(['*', '?', '[']))
        .collect();
    let mut out = vec![];
    let fixed_path = fixed.join("/");
    let anchor = if fixed_path.is_empty() {
        cwd.to_path_buf()
    } else {
        match paths::resolve(cwd, &fixed_path) {
            Ok(p) => p,
            Err(e) => return vec![Target::Unknown(format!("{shown}: {e}"))],
        }
    };
    // Matching is relative to `anchor`; it must itself be runs/ or inside it for the
    // matches to be scratch, and each match is checked below.
    if !anchor.starts_with(&env.roots.runs) {
        out.push(Target::Path(anchor.join("*")));
    }
    for m in matches.flatten() {
        out.push(one(cwd, &m.to_string_lossy()));
    }
    out
}

fn one(cwd: &Path, raw: &str) -> Target {
    match paths::resolve(cwd, raw) {
        Ok(p) => Target::Path(p),
        Err(e) => Target::Unknown(format!("{}: {e}", crate::text::one_line(raw, 80))),
    }
}

fn delete_target(env: &Env, w: &Word, v: &mut Verdict) {
    for t in resolve(env, w) {
        match t {
            Target::Null => {}
            Target::Unknown(why) => v.raise(
                Class::Delete,
                format!("cannot tell what this deletes: {why}"),
            ),
            Target::Path(p) => match env.roots.place(&p) {
                Place::Scratch => v.raise(
                    Class::Read,
                    format!("deletes {} inside runs/", env.roots.shown(&p)),
                ),
                Place::Bookkeeping => v.raise(
                    Class::Delete,
                    format!("deletes the run records at {}", env.roots.shown(&p)),
                ),
                Place::Outside => v.raise(
                    Class::Delete,
                    format!("deletes {} (outside runs/)", env.roots.shown(&p)),
                ),
            },
        }
    }
}

/// A file the command writes. `truncate`: replaces the content of an existing file.
fn write_target(env: &Env, w: &Word, truncate: bool, v: &mut Verdict) {
    for t in resolve(env, w) {
        match t {
            Target::Null => {}
            Target::Unknown(why) => v.raise(
                Class::Delete,
                format!("cannot tell what this writes: {why}"),
            ),
            Target::Path(p) => match env.roots.place(&p) {
                Place::Scratch => {}
                Place::Bookkeeping => v.raise(
                    Class::Delete,
                    format!("writes the run records at {}", env.roots.shown(&p)),
                ),
                Place::Outside if truncate && paths::exists(&p) => v.raise(
                    Class::Delete,
                    format!("replaces {} (outside runs/)", env.roots.shown(&p)),
                ),
                Place::Outside => v.raise(
                    Class::Fill,
                    format!("writes {} (outside runs/)", env.roots.shown(&p)),
                ),
            },
        }
    }
}

/// A path the command creates or changes in place without removing content.
fn modify_target(env: &Env, w: &Word, what: &str, v: &mut Verdict) {
    for t in resolve(env, w) {
        match t {
            Target::Null => {}
            Target::Unknown(why) => v.raise(
                Class::Delete,
                format!("cannot tell what {what} changes: {why}"),
            ),
            Target::Path(p) => match env.roots.place(&p) {
                Place::Scratch => {}
                Place::Bookkeeping => v.raise(
                    Class::Delete,
                    format!("{what} on the run records at {}", env.roots.shown(&p)),
                ),
                Place::Outside => v.raise(
                    Class::Fill,
                    format!("{what} {} (outside runs/)", env.roots.shown(&p)),
                ),
            },
        }
    }
}

/// Follows `cd DIR` (see [`Env`]): the folder set becomes `{DIR}` when the cd's failure
/// would stop the rest of the line, else `DIR` joins it. Unknown after `cd`, `cd -`, a
/// dynamic or missing folder, or a Git Bash path.
fn cd(env: &mut Env, args: &[Word]) {
    let ops = operands(args, false);
    let Some(old) = env.cwds.clone() else { return };
    let target = match ops.first() {
        Some(w) if w.is_static() && w.text != "-" => {
            let converted = paths::msys_drive(&w.text);
            if cfg!(windows) && converted.starts_with('/') {
                None
            } else {
                let mut news = Vec::new();
                for cwd in &old {
                    match paths::resolve(cwd, &converted) {
                        Ok(p) if paths::is_dir(&p) => news.push(p),
                        _ => {}
                    }
                }
                (!news.is_empty()).then_some(news)
            }
        }
        _ => None,
    };
    env.cwds = match target {
        None => None,
        Some(news) if env.cd_replaces => Some(news),
        Some(mut news) => {
            for o in old {
                if !news.contains(&o) {
                    news.push(o);
                }
            }
            Some(news)
        }
    };
}

fn move_or_copy(env: &Env, args: &[Word], is_move: bool, unknown_args: bool, v: &mut Verdict) {
    let what = if is_move { "mv" } else { "cp" };
    let no_clobber = args
        .iter()
        .any(|a| short_flag(a, 'n') || a.text == "--no-clobber");
    let target_dir = flag_value(args, "-t").or_else(|| {
        args.iter().find_map(|a| {
            a.text.strip_prefix("--target-directory=").map(|r| Word {
                text: r.to_string(),
                ..a.clone()
            })
        })
    });
    let mut ops: Vec<&Word> = operands(args, false);
    if let Some(t) = &target_dir {
        ops.retain(|w| w.text != t.text);
    }
    if unknown_args {
        v.raise(
            Class::Delete,
            format!("{what} of paths that arrive at run time"),
        );
        return;
    }
    let (srcs, dst): (Vec<&Word>, Word) = match target_dir {
        Some(d) => (ops, d),
        None => {
            let Some((last, rest)) = ops.split_last() else {
                return;
            };
            (rest.to_vec(), (*last).clone())
        }
    };
    let dst_t = resolve(env, &dst);
    let dst_path = match dst_t.as_slice() {
        [Target::Path(p)] => p.clone(),
        [Target::Null] if !is_move => return,
        _ => {
            v.raise(Class::Delete, format!("cannot tell where {what} writes"));
            return;
        }
    };
    let dst_is_dir = paths::is_dir(&dst_path);
    for s in srcs {
        for st in resolve(env, s) {
            let src = match st {
                Target::Path(p) => p,
                Target::Null => continue,
                Target::Unknown(why) => {
                    v.raise(
                        Class::Delete,
                        format!("cannot tell what {what} takes: {why}"),
                    );
                    continue;
                }
            };
            let fin = match (dst_is_dir, src.file_name()) {
                (true, Some(n)) => dst_path.join(n),
                _ => dst_path.clone(),
            };
            let sp = env.roots.place(&src);
            let dp = env.roots.place(&fin);
            if is_move && (sp == Place::Bookkeeping || env.roots.runs.starts_with(&src)) {
                v.raise(
                    Class::Delete,
                    format!("moves the run records at {}", env.roots.shown(&src)),
                );
            }
            if is_move && sp == Place::Outside && dp == Place::Scratch {
                v.raise(
                    Class::Delete,
                    format!(
                        "moves {} from outside runs/ into runs/",
                        env.roots.shown(&src)
                    ),
                );
            }
            match dp {
                // Into runs/: a move from outside was raised above; a copy only reads.
                Place::Scratch => {}
                Place::Bookkeeping => v.raise(
                    Class::Delete,
                    format!("writes the run records at {}", env.roots.shown(&fin)),
                ),
                Place::Outside if !no_clobber && paths::exists(&fin) => v.raise(
                    Class::Delete,
                    format!("replaces {} (outside runs/)", env.roots.shown(&fin)),
                ),
                Place::Outside => v.raise(
                    Class::Fill,
                    format!("{what} to {} (outside runs/)", env.roots.shown(&fin)),
                ),
            }
            if is_move && sp == Place::Outside && dp == Place::Outside {
                v.raise(Class::Fill, format!("moves {}", env.roots.shown(&src)));
            }
        }
    }
}

fn find(env: &mut Env, args: &[Word], depth: u8, v: &mut Verdict) {
    let starts: Vec<&Word> = args
        .iter()
        .take_while(|a| !a.text.starts_with('-') && a.text != "(" && a.text != "!")
        .collect();
    let mut k = 0;
    while k < args.len() {
        let a = args[k].text.as_str();
        match a {
            "-delete" => {
                if starts.is_empty() {
                    delete_target(
                        env,
                        &Word {
                            text: ".".into(),
                            ..Word::default()
                        },
                        v,
                    );
                }
                for s in &starts {
                    delete_target(env, s, v);
                }
            }
            "-exec" | "-execdir" | "-ok" | "-okdir" => {
                let end = args[k + 1..]
                    .iter()
                    .position(|w| w.text == ";" || w.text == "+")
                    .map(|p| k + 1 + p)
                    .unwrap_or(args.len());
                let cmd: Vec<Word> = args[k + 1..end]
                    .iter()
                    .filter(|w| w.text != "{}")
                    .cloned()
                    .collect();
                let mut inner = env.clone();
                words(&mut inner, &cmd, true, depth + 1, v);
                k = end;
            }
            "-fprint" | "-fprint0" | "-fprintf" | "-fls" => {
                if let Some(t) = args.get(k + 1) {
                    write_target(env, t, true, v);
                }
            }
            _ => {}
        }
        k += 1;
    }
}

fn sed(env: &Env, args: &[Word], v: &mut Verdict) {
    let mut in_place = false;
    let mut scripts: Vec<&Word> = Vec::new();
    let mut files: Vec<&Word> = Vec::new();
    let mut k = 0;
    let mut explicit_script = false;
    while k < args.len() {
        let a = &args[k];
        let t = a.text.as_str();
        if t == "-e" || t == "--expression" {
            if let Some(s) = args.get(k + 1) {
                scripts.push(s);
            }
            explicit_script = true;
            k += 2;
            continue;
        }
        if t == "-f" || t == "--file" || t.starts_with("--file=") {
            v.raise(
                Class::Delete,
                "sed -f runs a script file the classifier cannot see",
            );
            k += 2;
            continue;
        }
        if t.starts_with("--in-place") || (t.starts_with("-i") && !t.starts_with("--")) || t == "-I"
        {
            in_place = true;
        } else if t.starts_with("--expression=") {
            explicit_script = true;
        } else if t.starts_with('-') && t.len() > 1 {
            if short_flag(a, 'i') {
                in_place = true;
            }
        } else if !explicit_script && scripts.is_empty() {
            scripts.push(a);
        } else {
            files.push(a);
        }
        k += 1;
    }
    for a in args {
        if let Some(rest) = a.text.strip_prefix("--expression=") {
            if sed_writes(rest) {
                v.raise(
                    Class::Delete,
                    "the sed script writes files or runs commands",
                );
            }
        }
    }
    for s in scripts {
        if s.dynamic || sed_writes(&s.text) {
            v.raise(
                Class::Delete,
                "the sed script writes files or runs commands",
            );
        }
    }
    if in_place {
        for f in files {
            write_target(env, f, true, v);
        }
    }
}

/// True when a sed script has a `w`, `W` or `e` command, an `s///w` or `s///e` flag, or a
/// command this check does not know (fail closed).
fn sed_writes(script: &str) -> bool {
    let c: Vec<char> = script.chars().collect();
    let mut i = 0;
    let skip_delim = |i: &mut usize, d: char| -> bool {
        // Moves past the next unescaped `d`; false if there is none.
        while *i < c.len() {
            if c[*i] == '\\' {
                *i += 2;
                continue;
            }
            if c[*i] == d {
                *i += 1;
                return true;
            }
            *i += 1;
        }
        false
    };
    while i < c.len() {
        // Address part.
        loop {
            match c.get(i) {
                Some(ch)
                    if ch.is_ascii_digit()
                        || matches!(ch, '$' | ',' | '!' | ' ' | '\t' | '~' | '+') =>
                {
                    i += 1
                }
                Some('/') => {
                    i += 1;
                    if !skip_delim(&mut i, '/') {
                        return true;
                    }
                    while matches!(c.get(i), Some('I' | 'M')) {
                        i += 1;
                    }
                }
                Some('\\') => {
                    let Some(&d) = c.get(i + 1) else { return true };
                    i += 2;
                    if !skip_delim(&mut i, d) {
                        return true;
                    }
                }
                _ => break,
            }
        }
        let Some(&cmd) = c.get(i) else { break };
        i += 1;
        match cmd {
            ';' | '\n' | '{' | '}' | ' ' | '\t' => {}
            'w' | 'W' | 'e' => return true,
            's' | 'y' => {
                let Some(&d) = c.get(i) else { return true };
                i += 1;
                if !skip_delim(&mut i, d) || !skip_delim(&mut i, d) {
                    return true;
                }
                while let Some(&f) = c.get(i) {
                    if f == ';' || f == '\n' || f == '}' {
                        break;
                    }
                    if cmd == 's' && (f == 'w' || f == 'e' || f == 'W') {
                        return true;
                    }
                    i += 1;
                }
            }
            'a' | 'i' | 'c' | 'b' | 't' | 'T' | ':' | 'r' | 'R' | 'q' | 'Q' | 'l' | 'L' => {
                while let Some(&f) = c.get(i) {
                    if f == '\n' || (f == ';' && !matches!(cmd, 'a' | 'i' | 'c' | 'r' | 'R')) {
                        break;
                    }
                    i += 1;
                }
            }
            'p' | 'P' | 'd' | 'D' | 'n' | 'N' | 'h' | 'H' | 'g' | 'G' | 'x' | '=' | 'z' | 'F' => {}
            _ => return true,
        }
    }
    false
}

fn awk(args: &[Word], v: &mut Verdict) {
    let mut k = 0;
    while k < args.len() {
        let t = args[k].text.as_str();
        if t == "-f" || t.starts_with("--file") {
            v.raise(
                Class::Delete,
                "awk -f runs a program file the classifier cannot see",
            );
            return;
        }
        if t == "-F" || t == "-v" {
            k += 2;
            continue;
        }
        if t.starts_with('-') && t.len() > 1 {
            k += 1;
            continue;
        }
        let prog = &args[k];
        let p = prog.text.as_str();
        let prints_out = {
            let mut found = false;
            for (idx, _) in p.match_indices("print") {
                let rest = &p[idx..];
                let stmt = rest.split([';', '}', '\n']).next().unwrap_or(rest);
                if stmt.contains('>') || stmt.contains('|') {
                    found = true;
                }
            }
            found
        };
        if prog.dynamic || p.contains("system") || p.contains('|') || prints_out {
            v.raise(
                Class::Delete,
                "the awk program writes files or runs commands",
            );
        }
        return;
    }
}

fn tar(env: &Env, args: &[Word], v: &mut Verdict) {
    let first = args.first().map(|a| a.text.as_str()).unwrap_or("");
    let cluster = first.trim_start_matches('-');
    let has = |c: char, long: &str| {
        (!first.starts_with("--") && cluster.contains(c)) || args.iter().any(|a| a.text == long)
    };
    let runs_program = args.iter().any(|a| {
        let t = a.text.as_str();
        [
            "--to-command",
            "--use-compress-program",
            "--checkpoint-action",
            "--info-script",
            "--new-volume-script",
            "--rsh-command",
        ]
        .iter()
        .any(|o| t.starts_with(o))
            || matches!(t, "-I" | "-F")
    });
    if runs_program {
        v.raise(Class::Delete, "tar runs a program");
        return;
    }
    if has('t', "--list") {
        return;
    }
    if has('c', "--create") || has('r', "--append") || has('u', "--update") {
        if let Some(f) = flag_value(args, "-f").or_else(|| {
            // Old-style `tar cf out.tar ...`: the archive is the next word.
            (!first.starts_with('-') && cluster.contains('f'))
                .then(|| args.get(1).cloned())
                .flatten()
        }) {
            write_target(env, &f, has('c', "--create"), v);
        }
        return;
    }
    if has('x', "--extract") || has('x', "--get") {
        let dest = flag_value(args, "-C").or_else(|| {
            args.iter().find_map(|a| {
                a.text.strip_prefix("--directory=").map(|r| Word {
                    text: r.to_string(),
                    ..a.clone()
                })
            })
        });
        extract_into(env, dest, "tar -x", v);
        return;
    }
    v.raise(Class::Delete, "tar mode not recognised");
}

/// Extracting can replace any file under `dest` (default: the working folder).
fn extract_into(env: &Env, dest: Option<Word>, what: &str, v: &mut Verdict) {
    let scratch = match dest {
        Some(w) => {
            let ts = resolve(env, &w);
            !ts.is_empty()
                && ts
                    .iter()
                    .all(|t| matches!(t, Target::Path(p) if env.roots.place(p) == Place::Scratch))
        }
        None => env.cwd_scratch() == Some(true),
    };
    if !scratch {
        v.raise(
            Class::Delete,
            format!("{what} can replace files outside runs/"),
        );
    }
}

fn curl(env: &Env, args: &[Word], v: &mut Verdict) {
    const SHORT_WITH_VALUE: &str = "AbcCdDeEFHKmoPQrtTuUwxXyYz";
    let mut method: Option<String> = None;
    let mut sends = false;
    let mut get = false;
    let mut k = 0;
    let mut writes: Vec<(Word, bool)> = Vec::new();
    let mut remote_name = false;
    let mut output_dir: Option<Word> = None;
    while k < args.len() {
        let a = &args[k];
        let t = a.text.as_str();
        let next = args.get(k + 1).cloned();
        if let Some(long) = t.strip_prefix("--") {
            let (name, inline) = match long.split_once('=') {
                Some((n, val)) => (n, Some(val.to_string())),
                None => (long, None),
            };
            let value = || {
                inline
                    .clone()
                    .map(|s| Word {
                        text: s,
                        ..a.clone()
                    })
                    .or(next.clone())
            };
            let mut consumed = 0;
            match name {
                "request" => {
                    method = value().map(|w| w.text.to_ascii_uppercase());
                    consumed = 1;
                }
                "data" | "data-raw" | "data-binary" | "data-urlencode" | "data-ascii" | "json"
                | "form" | "form-string" | "upload-file" => {
                    sends = true;
                    consumed = 1;
                }
                "get" => get = true,
                "output" | "cookie-jar" | "dump-header" | "trace" | "trace-ascii" | "stderr"
                | "etag-save" | "libcurl" | "hsts" | "alt-svc" => {
                    if let Some(w) = value() {
                        writes.push((w, true));
                    }
                    consumed = 1;
                }
                "remote-name" | "remote-name-all" => remote_name = true,
                "config" => {
                    v.raise(
                        Class::Delete,
                        "curl --config runs options the classifier cannot see",
                    );
                    consumed = 1;
                }
                "output-dir" => {
                    output_dir = value();
                    consumed = 1;
                }
                "header" | "user-agent" | "user" | "url" | "referer" | "cookie" | "max-time"
                | "connect-timeout" | "proxy" | "cert" | "key" | "cacert" | "retry"
                | "write-out" | "range" | "resolve" | "connect-to" => consumed = 1,
                _ => {}
            }
            k += 1 + if inline.is_some() { 0 } else { consumed };
            continue;
        }
        if t.starts_with('-') && t.len() > 1 && !a.quoted {
            let chars: Vec<char> = t[1..].chars().collect();
            let mut j = 0;
            let mut used_next = false;
            while j < chars.len() {
                let f = chars[j];
                if SHORT_WITH_VALUE.contains(f) {
                    let attached: String = chars[j + 1..].iter().collect();
                    let val = if attached.is_empty() {
                        used_next = true;
                        next.clone()
                    } else {
                        Some(Word {
                            text: attached,
                            ..a.clone()
                        })
                    };
                    match f {
                        'X' => method = val.map(|w| w.text.to_ascii_uppercase()),
                        'd' | 'F' | 'T' => sends = true,
                        'o' | 'c' | 'D' => {
                            if let Some(w) = val {
                                writes.push((w, true));
                            }
                        }
                        'K' => v.raise(
                            Class::Delete,
                            "curl -K runs options the classifier cannot see",
                        ),
                        _ => {}
                    }
                    break;
                }
                match f {
                    'G' => get = true,
                    'O' => remote_name = true,
                    _ => {}
                }
                j += 1;
            }
            k += if used_next { 2 } else { 1 };
            continue;
        }
        k += 1;
    }
    let m = method.unwrap_or_else(|| {
        if sends && !get {
            "POST".into()
        } else {
            "GET".into()
        }
    });
    match m.as_str() {
        "GET" | "HEAD" | "OPTIONS" => {}
        "DELETE" => v.raise(Class::Delete, "curl sends an HTTP DELETE"),
        other => v.raise(
            Class::Submit,
            format!(
                "curl sends data (HTTP {})",
                crate::text::one_line(other, 12)
            ),
        ),
    }
    for (w, trunc) in writes {
        if w.text == "-" {
            continue;
        }
        // --output-dir prefixes relative -o names.
        let w = match &output_dir {
            Some(d) if !Path::new(&paths::msys_drive(&w.text)).is_absolute() => Word {
                text: format!("{}/{}", d.text, w.text),
                dynamic: d.dynamic || w.dynamic,
                glob: d.glob || w.glob,
                brace: d.brace || w.brace,
                tilde: d.tilde,
                quoted: true,
            },
            _ => w,
        };
        write_target(env, &w, trunc, v);
    }
    // curl -O replaces a file of the same name: outside runs/ that is Delete.
    if remote_name {
        let scratch = match &output_dir {
            Some(d) => {
                let ts = resolve(env, d);
                !ts.is_empty()
                    && ts.iter().all(
                        |t| matches!(t, Target::Path(p) if env.roots.place(p) == Place::Scratch),
                    )
            }
            None => env.cwd_scratch() == Some(true),
        };
        if !scratch {
            v.raise(Class::Delete, "curl -O may replace a file outside runs/");
        }
    }
}

fn wget(env: &Env, args: &[Word], v: &mut Verdict) {
    let mut output: Option<Word> = None;
    let mut prefix: Option<Word> = None;
    let mut k = 0;
    while k < args.len() {
        let a = &args[k];
        let t = a.text.as_str();
        let next = args.get(k + 1).cloned();
        let (name, inline) = match t.split_once('=') {
            Some((n, val)) if t.starts_with("--") => (
                n,
                Some(Word {
                    text: val.to_string(),
                    ..a.clone()
                }),
            ),
            _ => (t, None),
        };
        let val = || inline.clone().or(next.clone());
        let step = if inline.is_some() { 1 } else { 2 };
        match name {
            "--post-data" | "--post-file" | "--body-data" | "--body-file" => {
                v.raise(Class::Submit, "wget sends data");
                k += step;
                continue;
            }
            "--method" => {
                let m = val()
                    .map(|w| w.text.to_ascii_uppercase())
                    .unwrap_or_default();
                match m.as_str() {
                    "GET" | "HEAD" | "OPTIONS" => {}
                    "DELETE" => v.raise(Class::Delete, "wget sends an HTTP DELETE"),
                    _ => v.raise(Class::Submit, "wget sends data"),
                }
                k += step;
                continue;
            }
            "-O" | "--output-document" => {
                output = val();
                k += step;
                continue;
            }
            "-P" | "--directory-prefix" => {
                prefix = val();
                k += step;
                continue;
            }
            "-o" | "--output-file" | "-a" | "--append-output" => {
                if let Some(w) = val() {
                    write_target(env, &w, name == "-o" || name == "--output-file", v);
                }
                k += step;
                continue;
            }
            "-i" | "--input-file" | "-U" | "--user-agent" | "--header" | "-t" | "--tries"
            | "-T" | "--timeout" => {
                k += step;
                continue;
            }
            _ => {}
        }
        if let Some(rest) = t
            .strip_prefix("-O")
            .filter(|r| !r.is_empty() && !t.starts_with("--"))
        {
            output = Some(Word {
                text: rest.to_string(),
                ..a.clone()
            });
        }
        k += 1;
    }
    match (output, prefix) {
        (Some(o), _) if o.text == "-" => {}
        (Some(o), _) => write_target(env, &o, true, v),
        (None, Some(p)) => modify_target(env, &p, "wget downloads into", v),
        (None, None) => download_into_cwd(env, "wget", v),
    }
}

fn download_into_cwd(env: &Env, what: &str, v: &mut Verdict) {
    let bookkeeping = env
        .cwds
        .as_ref()
        .is_some_and(|c| c.iter().any(|d| env.roots.place(d) == Place::Bookkeeping));
    match env.cwd_scratch() {
        Some(true) => {}
        Some(false) if !bookkeeping => v.raise(
            Class::Fill,
            format!("{what} saves a file in the working folder"),
        ),
        _ => v.raise(
            Class::Delete,
            format!("{what} saves a file where the classifier cannot tell"),
        ),
    }
}

/// git config keys that make git run a program.
fn git_config_runs(kv: &str) -> bool {
    let key = kv.split('=').next().unwrap_or(kv).to_ascii_lowercase();
    key.starts_with("alias.")
        || key.starts_with("core.")
        || key.starts_with("credential")
        || key.starts_with("filter.")
        || key.starts_with("diff.")
        || key.starts_with("merge.")
        || key.starts_with("gpg.")
        || key.starts_with("sequence.")
        || key.ends_with("command")
        || key.ends_with(".cmd")
        || key.ends_with("textconv")
        || key.contains("hook")
        || key.contains("editor")
        || key.contains("pager")
        || key.contains("program")
}

fn git(outer: &Env, args: &[Word], v: &mut Verdict) {
    // `git -C dir` changes the folder for this command only.
    let mut env = outer.clone();
    env.cd_replaces = true;
    let env = &mut env;
    let mut k = 0;
    while k < args.len() && args[k].text.starts_with('-') {
        let t = args[k].text.as_str();
        match t {
            "-C" => {
                if let Some(d) = args.get(k + 1) {
                    cd(env, std::slice::from_ref(d));
                }
                k += 2;
            }
            "-c" => {
                if args
                    .get(k + 1)
                    .is_none_or(|kv| kv.dynamic || git_config_runs(&kv.text))
                {
                    v.raise(Class::Delete, "git -c sets a key that runs a program");
                }
                k += 2;
            }
            "--git-dir" | "--work-tree" | "--namespace" | "--super-prefix" => k += 2,
            t if t.starts_with("--exec-path") || t.starts_with("--config-env") => {
                v.raise(Class::Delete, "git runs programs from a changed path");
                k += 1;
            }
            _ => k += 1,
        }
    }
    let Some(sub) = args.get(k) else { return };
    let rest = &args[k + 1..];
    let has = |f: &str| rest.iter().any(|a| a.text == f);
    let has_short = |c: char| rest.iter().any(|a| short_flag(a, c));
    let first_op = rest
        .iter()
        .find(|a| !a.text.starts_with('-'))
        .map(|a| a.text.as_str());
    let s = sub.text.as_str();
    if !sub.is_static() {
        v.raise(Class::Delete, "git subcommand decided at run time");
        return;
    }
    // Diff options (log, show, diff, format-patch...) can write a file.
    for (j, a) in rest.iter().enumerate() {
        if let Some(f) = a.text.strip_prefix("--output=") {
            let w = Word {
                text: f.to_string(),
                ..a.clone()
            };
            write_target(env, &w, true, v);
        } else if a.text == "--output" || (a.text == "-o" && s == "format-patch") {
            match rest.get(j + 1) {
                Some(w) if a.text == "--output" => write_target(env, w, true, v),
                Some(w) => modify_target(env, w, "git format-patch -o", v),
                None => {}
            }
        }
    }
    let runs_program = match s {
        "grep" => rest
            .iter()
            .any(|a| a.text.starts_with("-O") || a.text.starts_with("--open-files-in-pager")),
        "rebase" => rest
            .iter()
            .any(|a| a.text.starts_with("-x") || a.text.starts_with("--exec")),
        "submodule" => first_op == Some("foreach"),
        "bisect" => first_op == Some("run"),
        _ => false,
    };
    if runs_program {
        v.raise(Class::Delete, format!("git {s} runs a program"));
        return;
    }
    match s {
        "status" | "log" | "show" | "shortlog" | "describe" | "rev-parse" | "rev-list"
        | "ls-files" | "ls-tree" | "ls-remote" | "cat-file" | "blame" | "grep" | "whatchanged"
        | "help" | "version" | "request-pull" | "show-ref" | "for-each-ref" | "name-rev"
        | "merge-base" | "count-objects" | "check-ignore" | "var" | "range-diff" | "diff" => {}
        "reflog" => {
            if matches!(first_op, Some("expire" | "delete")) {
                v.raise(Class::Delete, "git reflog expire/delete drops history");
            }
        }
        "remote" => match first_op {
            None | Some("show" | "get-url") => {}
            Some("remove" | "rm" | "prune") => v.raise(Class::Delete, "git remote remove/prune"),
            Some(_) => v.raise(Class::Account, "git remote changes repository settings"),
        },
        "branch" => {
            if has_short('d') || has_short('D') || has("--delete") {
                v.raise(Class::Delete, "git branch -d deletes a branch");
            } else if first_op.is_some() || has_short('m') || has_short('M') {
                v.raise(Class::Fill, "git branch creates or renames a branch");
            }
        }
        "tag" => {
            if has_short('d') || has("--delete") {
                v.raise(Class::Delete, "git tag -d deletes a tag");
            } else if first_op.is_some() && !has_short('l') && !has("--list") {
                v.raise(Class::Fill, "git tag creates a tag");
            }
        }
        "config" => {
            let reads = rest
                .iter()
                .any(|a| a.text.starts_with("--get") || matches!(a.text.as_str(), "--list" | "-l"))
                || (rest.iter().filter(|a| !a.text.starts_with('-')).count() <= 1
                    && !rest.iter().any(|a| {
                        a.text.starts_with("--unset")
                            || a.text.starts_with("--remove")
                            || a.text.starts_with("--rename")
                            || a.text == "--add"
                            || a.text == "--edit"
                            || a.text == "-e"
                    }));
            if !reads {
                v.raise(Class::Account, "git config changes settings");
            }
        }
        "stash" => match first_op {
            Some("list" | "show") => {}
            Some("drop" | "clear") => {
                v.raise(Class::Delete, "git stash drop/clear deletes saved work")
            }
            _ => v.raise(Class::Fill, "git stash changes the working tree"),
        },
        "add" | "commit" | "fetch" | "pull" | "merge" | "rebase" | "cherry-pick" | "revert"
        | "switch" | "mv" | "init" | "clone" | "am" | "apply" | "notes" | "worktree" | "bisect"
        | "submodule" | "format-patch" | "gc" | "maintenance" | "sparse-checkout" | "lfs" => {
            if s == "worktree" && matches!(first_op, Some("remove" | "prune")) {
                v.raise(
                    Class::Delete,
                    "git worktree remove/prune deletes a working tree",
                );
            } else if s == "gc" && rest.iter().any(|a| a.text.starts_with("--prune")) {
                v.raise(Class::Delete, "git gc --prune drops unreachable commits");
            } else {
                v.raise(Class::Fill, format!("git {s} changes the repository"));
            }
        }
        "checkout" => {
            if has_short('b') || has_short('B') || has("--orphan") {
                v.raise(Class::Fill, "git checkout -b creates a branch");
            } else if has("--") || has(".") || has_short('f') || has("--force") || has("-p") {
                v.raise(Class::Delete, "git checkout discards working-tree changes");
            } else {
                let paths_exist = rest
                    .iter()
                    .filter(|a| !a.text.starts_with('-'))
                    .any(|a| match (&env.cwds, a.is_static()) {
                        (Some(cwds), true) => cwds.iter().any(|cwd| {
                            paths::resolve(cwd, &a.text)
                                .map(|p| paths::exists(&p))
                                .unwrap_or(true)
                        }),
                        _ => true,
                    });
                if paths_exist {
                    v.raise(Class::Delete, "git checkout of a path discards its changes");
                } else {
                    v.raise(Class::Fill, "git checkout switches branch");
                }
            }
        }
        "restore" => {
            if has("--staged") && !has("--worktree") && !has("-W") {
                v.raise(Class::Fill, "git restore --staged unstages");
            } else {
                v.raise(Class::Delete, "git restore discards working-tree changes");
            }
        }
        "reset" => {
            if has("--hard") || has("--merge") || has("--keep") {
                v.raise(Class::Delete, "git reset --hard discards work");
            } else {
                v.raise(Class::Fill, "git reset moves the branch");
            }
        }
        "clean" => {
            if !(has_short('n') || has("--dry-run")) {
                v.raise(Class::Delete, "git clean deletes untracked files");
            }
        }
        "rm" => {
            if has("--cached") {
                v.raise(Class::Fill, "git rm --cached untracks files");
            } else {
                v.raise(Class::Delete, "git rm deletes files");
            }
        }
        "push" => {
            let destructive = rest.iter().any(|a| {
                let t = a.text.as_str();
                matches!(
                    t,
                    "-f" | "--force" | "-d" | "--delete" | "--mirror" | "--prune"
                ) || t.starts_with("--force-with-lease")
                    || t.starts_with("--force-if-includes")
                    || (short_flag(a, 'f') && !t.starts_with("--"))
                    || (!t.starts_with('-') && (t.starts_with(':') || t.starts_with('+')))
            });
            if destructive {
                v.raise(Class::Delete, "git push rewrites or deletes remote history");
            } else {
                v.raise(Class::Account, "git push publishes commits to a remote");
            }
        }
        "send-email" => v.raise(Class::Send, "git send-email sends mail"),
        _ => v.raise(
            Class::Delete,
            format!(
                "git {} is not known to be safe",
                crate::text::one_line(s, 40)
            ),
        ),
    }
}

fn gh(args: &[Word], v: &mut Verdict) {
    let ops: Vec<&str> = args
        .iter()
        .filter(|a| !a.text.starts_with('-'))
        .map(|a| a.text.as_str())
        .collect();
    let Some(&group) = ops.first() else { return };
    if group == "api" {
        let method = flag_value(args, "-X")
            .or_else(|| flag_value(args, "--method"))
            .or_else(|| {
                args.iter().find_map(|a| {
                    a.text.strip_prefix("--method=").map(|r| Word {
                        text: r.into(),
                        ..a.clone()
                    })
                })
            })
            .map(|w| w.text.to_ascii_uppercase());
        let fields = args.iter().any(|a| {
            matches!(
                a.text.as_str(),
                "-f" | "-F" | "--field" | "--raw-field" | "--input"
            ) || a.text.starts_with("--field=")
                || a.text.starts_with("--raw-field=")
                || a.text.starts_with("--input=")
        });
        match method.as_deref() {
            Some("DELETE") => v.raise(Class::Delete, "gh api DELETE"),
            Some("GET" | "HEAD") | None if !(fields && method.is_none()) => {}
            _ => v.raise(Class::Account, "gh api writes to GitHub"),
        }
        return;
    }
    if matches!(group, "status" | "search" | "help" | "version") {
        return;
    }
    if group == "browse" {
        v.raise(Class::Navigate, "gh browse opens a browser");
        return;
    }
    let action = ops.get(1).copied().unwrap_or("");
    match action {
        "list" | "ls" | "view" | "status" | "diff" | "checks" | "watch" | "" => {}
        "download" | "clone" | "checkout" => v.raise(Class::Fill, format!("gh {group} {action}")),
        "create" | "comment" | "review" | "edit" | "close" | "reopen" | "ready" | "lock"
        | "unlock" | "transfer" | "pin" | "unpin" | "develop" => v.raise(
            Class::Submit,
            format!("gh {group} {action} posts to GitHub"),
        ),
        "delete" | "remove" | "rm" | "purge" | "delete-asset" => {
            v.raise(Class::Delete, format!("gh {group} {action}"))
        }
        "merge" | "set" | "enable" | "disable" | "run" | "rerun" | "cancel" | "login"
        | "logout" | "install" | "upgrade" | "rename" | "archive" | "unarchive" | "sync"
        | "fork" | "upload" => v.raise(
            Class::Account,
            format!("gh {group} {action} changes GitHub"),
        ),
        _ => v.raise(
            Class::Delete,
            format!(
                "gh {} {} is not known to be safe",
                crate::text::one_line(group, 30),
                crate::text::one_line(action, 30)
            ),
        ),
    }
}

fn cargo(args: &[Word], v: &mut Verdict) {
    let sub = args
        .iter()
        .find(|a| !a.text.starts_with('-') && !a.text.starts_with('+'))
        .map(|a| a.text.as_str());
    match sub {
        None
        | Some(
            "tree" | "metadata" | "version" | "search" | "help" | "locate-project" | "pkgid"
            | "verify-project" | "read-manifest",
        ) => {}
        Some(
            "build" | "b" | "check" | "c" | "test" | "t" | "clippy" | "fmt" | "doc" | "d" | "bench"
            | "run" | "r" | "update" | "add" | "remove" | "new" | "init" | "fetch" | "fix"
            | "generate-lockfile" | "install" | "vendor" | "package",
        ) => v.raise(
            Class::Fill,
            format!("cargo {} builds or changes the project", sub.unwrap_or("")),
        ),
        Some("publish" | "yank" | "owner" | "login" | "logout") => {
            v.raise(Class::Account, "cargo publishes to a registry")
        }
        Some(other) => v.raise(
            Class::Delete,
            format!(
                "cargo {} is not known to be safe",
                crate::text::one_line(other, 30)
            ),
        ),
    }
}

fn npm(name: &str, args: &[Word], v: &mut Verdict) {
    let sub = args
        .iter()
        .find(|a| !a.text.starts_with('-'))
        .map(|a| a.text.as_str());
    match sub {
        None
        | Some(
            "ls" | "list" | "view" | "info" | "outdated" | "why" | "help" | "audit" | "explain",
        ) => {
            if args.iter().any(|a| a.text == "fix") {
                v.raise(
                    Class::Fill,
                    format!("{name} audit fix changes dependencies"),
                );
            }
        }
        Some(
            "test" | "t" | "tst" | "run" | "run-script" | "start" | "build" | "ci" | "install"
            | "i" | "add" | "update" | "up" | "remove" | "rm" | "uninstall" | "rebuild",
        ) => v.raise(
            Class::Fill,
            format!("{name} {} builds or changes the project", sub.unwrap_or("")),
        ),
        Some(
            "publish" | "unpublish" | "deprecate" | "owner" | "login" | "logout" | "adduser"
            | "token" | "access" | "dist-tag",
        ) => v.raise(Class::Account, format!("{name} publishes to a registry")),
        Some(other) => v.raise(
            Class::Delete,
            format!(
                "{name} {} is not known to be safe",
                crate::text::one_line(other, 30)
            ),
        ),
    }
}

fn pip(args: &[Word], v: &mut Verdict) {
    let sub = args
        .iter()
        .find(|a| !a.text.starts_with('-'))
        .map(|a| a.text.as_str());
    match sub {
        None | Some("list" | "show" | "freeze" | "check" | "help" | "index" | "search") => {}
        Some("install" | "download" | "wheel") => v.raise(Class::Fill, "pip installs packages"),
        Some("uninstall") => v.raise(Class::Delete, "pip uninstall removes packages"),
        Some(other) => v.raise(
            Class::Delete,
            format!(
                "pip {} is not known to be safe",
                crate::text::one_line(other, 30)
            ),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fx {
        _t: tempfile::TempDir,
        roots: Roots,
    }

    fn fx() -> Fx {
        let t = tempfile::tempdir().unwrap();
        let run = t.path().join("runs").join("2026-10-25T00-00-00-t");
        std::fs::create_dir_all(run.join("files")).unwrap();
        std::fs::create_dir_all(run.join("asks")).unwrap();
        std::fs::create_dir_all(t.path().join("runs").join("x")).unwrap();
        std::fs::write(t.path().join("file.txt"), "keep").unwrap();
        std::fs::write(t.path().join("runs").join("x").join("a.txt"), "a").unwrap();
        let roots = Roots::from_run_dir(&run).unwrap();
        Fx { _t: t, roots }
    }

    fn class(f: &Fx, cmd: &str) -> Class {
        classify(&f.roots, &f.roots.base, cmd, false).class
    }

    #[test]
    fn acceptance_rm() {
        let f = fx();
        assert_eq!(class(&f, "rm -rf runs/x"), Class::Read);
        assert_eq!(class(&f, "rm file.txt"), Class::Delete);
        assert_eq!(class(&f, "rm -rf runs/../file.txt"), Class::Delete);
        assert_eq!(class(&f, "rm -rf runs/x/*"), Class::Read);
        assert_eq!(
            class(&f, "rm -rf runs/*"),
            Class::Delete,
            "includes the run folder"
        );
        assert_eq!(class(&f, "rm -rf runs"), Class::Delete);
        assert_eq!(
            class(&f, "rm -rf runs/2026-10-25T00-00-00-t/asks"),
            Class::Delete
        );
        assert_eq!(
            class(&f, "rm -rf runs/2026-10-25T00-00-00-t/files"),
            Class::Read
        );
        assert_eq!(class(&f, "rm $X"), Class::Delete);
        assert_eq!(class(&f, "rm ~/x"), Class::Delete);
        assert_eq!(class(&f, "rm runs/{x,y}"), Class::Delete);
        assert_eq!(class(&f, "cd runs && rm -rf x"), Class::Read);
        assert_eq!(
            class(&f, "cd runs/x && rm -rf ../../file.txt"),
            Class::Delete
        );
        assert_eq!(class(&f, "cd $D && rm -rf x"), Class::Delete);
        assert_eq!(class(&f, "rm -- -rf"), Class::Delete);
        assert_eq!(class(&f, "del /s /q file.txt"), Class::Delete);
        assert_eq!(class(&f, "Remove-Item -Recurse runs/x"), Class::Read);
        assert_eq!(class(&f, "/usr/bin/rm -f file.txt"), Class::Delete);
        assert_eq!(class(&f, "\\rm file.txt"), Class::Delete);
        assert_eq!(class(&f, "RM.EXE file.txt"), Class::Delete);
    }

    #[test]
    fn compound_and_hidden_commands() {
        let f = fx();
        assert_eq!(class(&f, "ls; rm file.txt"), Class::Delete);
        assert_eq!(class(&f, "ls && rm file.txt"), Class::Delete);
        assert_eq!(class(&f, "false || rm file.txt"), Class::Delete);
        assert_eq!(class(&f, "cat file.txt | rm file.txt"), Class::Delete);
        assert_eq!(class(&f, "echo $(rm file.txt)"), Class::Delete);
        assert_eq!(class(&f, "echo `rm file.txt`"), Class::Delete);
        assert_eq!(class(&f, "bash -c 'rm file.txt'"), Class::Delete);
        assert_eq!(class(&f, "bash -c 'ls runs'"), Class::Read);
        assert_eq!(class(&f, "eval rm file.txt"), Class::Delete);
        assert_eq!(class(&f, "ls | xargs rm"), Class::Delete);
        assert_eq!(class(&f, "find . -name '*.tmp' -delete"), Class::Delete);
        assert_eq!(class(&f, "find runs/x -delete"), Class::Read);
        assert_eq!(class(&f, "find . -exec rm {} ;"), Class::Delete);
        assert_eq!(class(&f, "find . -name x"), Class::Read);
        assert_eq!(class(&f, "env FOO=1 rm file.txt"), Class::Delete);
        assert_eq!(class(&f, "FOO=1 ls"), Class::Read);
        assert_eq!(class(&f, "sudo ls"), Class::Delete);
        assert_eq!(class(&f, "python -c 'print(1)'"), Class::Delete);
        assert_eq!(class(&f, "python --version"), Class::Read);
        assert_eq!(class(&f, "$CMD file.txt"), Class::Delete);
        assert_eq!(class(&f, "echo 'unterminated"), Class::Delete);
        assert_eq!(class(&f, "if true; then rm file.txt; fi"), Class::Delete);
        assert_eq!(class(&f, "for f in a b; do echo $f; done"), Class::Read);
        assert_eq!(class(&f, "for f in a b; do rm $f; done"), Class::Delete);
    }

    #[test]
    fn redirections() {
        let f = fx();
        assert_eq!(class(&f, "echo hi > file.txt"), Class::Delete);
        assert_eq!(class(&f, "echo hi >> file.txt"), Class::Fill);
        assert_eq!(class(&f, "echo hi > new.txt"), Class::Fill);
        assert_eq!(class(&f, "echo hi > runs/x/out.txt"), Class::Read);
        assert_eq!(class(&f, "ls 2>/dev/null"), Class::Read);
        assert_eq!(class(&f, "ls > $OUT"), Class::Delete);
        assert_eq!(class(&f, "ls 2>&1 | head"), Class::Read);
        assert_eq!(
            class(&f, "echo yes > runs/2026-10-25T00-00-00-t/asks/1.answer"),
            Class::Delete
        );
        assert_eq!(
            class(&f, "cat <<EOF > runs/x/n.txt\nrm -rf /\nEOF"),
            Class::Read
        );
        assert_eq!(class(&f, "ls | tee file.txt"), Class::Delete);
        assert_eq!(class(&f, "ls | tee -a file.txt"), Class::Fill);
        assert_eq!(class(&f, "sort -o file.txt file.txt"), Class::Delete);
    }

    #[test]
    fn network_and_vcs() {
        let f = fx();
        assert_eq!(class(&f, "curl https://example.com"), Class::Read);
        assert_eq!(class(&f, "curl -sSL https://example.com"), Class::Read);
        assert_eq!(class(&f, "curl -X POST https://example.com"), Class::Submit);
        assert_eq!(class(&f, "curl -XPUT https://example.com"), Class::Submit);
        assert_eq!(class(&f, "curl --request DELETE https://x"), Class::Delete);
        assert_eq!(class(&f, "curl -d a=1 https://x"), Class::Submit);
        assert_eq!(class(&f, "curl --json '{}' https://x"), Class::Submit);
        assert_eq!(class(&f, "curl -o file.txt https://x"), Class::Delete);
        assert_eq!(class(&f, "curl -o runs/x/p.html https://x"), Class::Read);
        assert_eq!(class(&f, "wget --post-data=a https://x"), Class::Submit);
        assert_eq!(class(&f, "git status"), Class::Read);
        assert_eq!(class(&f, "git log --oneline | head"), Class::Read);
        assert_eq!(class(&f, "git commit -m x"), Class::Fill);
        assert_eq!(class(&f, "git push"), Class::Account);
        assert_eq!(class(&f, "git push --force origin main"), Class::Delete);
        assert_eq!(class(&f, "git push origin +main"), Class::Delete);
        assert_eq!(class(&f, "git reset --hard HEAD~1"), Class::Delete);
        assert_eq!(class(&f, "git clean -fdx"), Class::Delete);
        assert_eq!(class(&f, "git clean -n"), Class::Read);
        assert_eq!(class(&f, "git checkout -- file.txt"), Class::Delete);
        assert_eq!(class(&f, "git checkout -b feat"), Class::Fill);
        assert_eq!(class(&f, "git -c alias.x='!rm -rf /' x"), Class::Delete);
        assert_eq!(class(&f, "git frobnicate"), Class::Delete);
        assert_eq!(class(&f, "gh run view 123 --log"), Class::Read);
        assert_eq!(class(&f, "gh api repos/a/b"), Class::Read);
        assert_eq!(class(&f, "gh api -X POST repos/a/b/issues"), Class::Account);
        assert_eq!(
            class(&f, "gh api repos/a/b/issues -f title=x"),
            Class::Account
        );
        assert_eq!(class(&f, "gh pr merge 3"), Class::Account);
        assert_eq!(class(&f, "gh repo delete a/b"), Class::Delete);
        assert_eq!(class(&f, "cargo test -p srv-files"), Class::Fill);
        assert_eq!(class(&f, "cargo publish"), Class::Account);
    }

    #[test]
    fn edits_moves_copies() {
        let f = fx();
        assert_eq!(class(&f, "sed -n 1,5p file.txt"), Class::Read);
        assert_eq!(class(&f, "sed -i s/a/b/ file.txt"), Class::Delete);
        assert_eq!(class(&f, "sed -i s/a/b/ runs/x/a.txt"), Class::Read);
        assert_eq!(class(&f, "sed 's/a/b/w out' file.txt"), Class::Delete);
        assert_eq!(class(&f, "sed -e '1e rm x' file.txt"), Class::Delete);
        assert_eq!(class(&f, "awk '{print $1}' file.txt"), Class::Read);
        assert_eq!(class(&f, "awk '$1 > 5' file.txt"), Class::Read);
        assert_eq!(class(&f, "awk '{print > \"o\"}' file.txt"), Class::Delete);
        assert_eq!(class(&f, "awk 'BEGIN{system(\"rm x\")}'"), Class::Delete);
        assert_eq!(class(&f, "mv file.txt runs/x/"), Class::Delete);
        assert_eq!(class(&f, "mv runs/x/a.txt runs/x/b.txt"), Class::Read);
        assert_eq!(class(&f, "mv runs/x/a.txt file.txt"), Class::Delete);
        assert_eq!(class(&f, "mv runs/x/a.txt new.txt"), Class::Fill);
        assert_eq!(class(&f, "cp file.txt runs/x/"), Class::Read);
        assert_eq!(class(&f, "cp runs/x/a.txt file.txt"), Class::Delete);
        assert_eq!(class(&f, "cp -n runs/x/a.txt file.txt"), Class::Fill);
        assert_eq!(class(&f, "mkdir -p newdir"), Class::Fill);
        assert_eq!(class(&f, "mkdir -p runs/x/y"), Class::Read);
        assert_eq!(class(&f, "touch file.txt"), Class::Fill);
        assert_eq!(class(&f, "truncate -s 0 file.txt"), Class::Delete);
        assert_eq!(class(&f, "gzip file.txt"), Class::Delete);
        assert_eq!(class(&f, "gzip -c file.txt"), Class::Read);
        assert_eq!(class(&f, "tar -tf a.tar"), Class::Read);
        assert_eq!(class(&f, "tar -xf a.tar"), Class::Delete);
        assert_eq!(class(&f, "tar -xf a.tar -C runs/x"), Class::Read);
        assert_eq!(class(&f, "dd if=/dev/zero of=file.txt"), Class::Delete);
    }

    #[test]
    fn cd_is_followed_only_where_it_must_have_happened() {
        let f = fx();
        assert_eq!(class(&f, "cd runs/x && rm -rf a.txt"), Class::Read);
        assert_eq!(class(&f, "cd runs/x && ls && rm -rf a.txt"), Class::Read);
        // `;` and newlines run on after a failed cd; a subshell or pipe keeps its own cd.
        assert_eq!(class(&f, "cd runs/x; rm -rf a.txt"), Class::Delete);
        assert_eq!(class(&f, "cd runs/x\nrm -rf a.txt"), Class::Delete);
        assert_eq!(class(&f, "(cd runs/x); rm -rf a.txt"), Class::Delete);
        assert_eq!(class(&f, "(cd runs/x && rm -rf a.txt)"), Class::Delete);
        assert_eq!(class(&f, "cd runs/x | rm -rf a.txt"), Class::Delete);
        assert_eq!(class(&f, "cd runs/x && rm a.txt; rm b.txt"), Class::Delete);
        assert_eq!(class(&f, "cd /c/Windows && rm -rf x"), Class::Delete);
        // git -C is for that command only.
        assert_eq!(
            class(&f, "git -C runs/x status && rm -rf a.txt"),
            Class::Delete
        );
    }

    #[test]
    fn hidden_program_runs() {
        let f = fx();
        assert_eq!(class(&f, "PAGER='rm -rf x' git log"), Class::Delete);
        assert_eq!(
            class(&f, "export LESSOPEN='|rm x %s'; less a"),
            Class::Delete
        );
        assert_eq!(class(&f, "env GIT_SSH_COMMAND=x git fetch"), Class::Delete);
        assert_eq!(class(&f, "RUST_LOG=debug cargo test"), Class::Fill);
        assert_eq!(
            class(&f, "tar --to-command=sh -xf a.tar -C runs/x"),
            Class::Delete
        );
        assert_eq!(class(&f, "tar -I zstd -tf a.tar"), Class::Delete);
        assert_eq!(class(&f, "rg --pre cat pattern"), Class::Delete);
        assert_eq!(class(&f, "git rebase -x 'rm a' main"), Class::Delete);
        assert_eq!(class(&f, "git submodule foreach rm a"), Class::Delete);
        assert_eq!(class(&f, "git grep -Ovim x"), Class::Delete);
        assert_eq!(class(&f, "zip -TT 'rm a' z.zip a"), Class::Delete);
        assert_eq!(class(&f, "rsync -e 'sh -c x' a runs/x/"), Class::Delete);
    }

    #[test]
    fn output_files_of_readers() {
        let f = fx();
        assert_eq!(class(&f, "git log --output=file.txt"), Class::Delete);
        assert_eq!(class(&f, "git log --output=runs/x/log.txt"), Class::Read);
        assert_eq!(class(&f, "tree -o file.txt"), Class::Delete);
        assert_eq!(
            class(&f, "iconv -f latin1 -t utf8 -o file.txt a"),
            Class::Delete
        );
        assert_eq!(class(&f, "xxd runs/x/a.txt file.txt"), Class::Delete);
        assert_eq!(class(&f, "xxd runs/x/a.txt"), Class::Read);
        assert_eq!(
            class(&f, "cd runs && curl --output-dir .. -o file.txt https://x"),
            Class::Delete
        );
        assert_eq!(class(&f, "curl -O https://x/a"), Class::Delete);
        assert_eq!(class(&f, "cd runs/x && curl -O https://x/a"), Class::Read);
    }

    #[test]
    fn sed_script_check() {
        assert!(!sed_writes("s/a/b/g"));
        assert!(!sed_writes("1,5p"));
        assert!(!sed_writes("/x/d;s|a|b|"));
        assert!(!sed_writes("$!N;P;D"));
        assert!(sed_writes("s/a/b/w out"));
        assert!(sed_writes("/x/w out"));
        assert!(sed_writes("1e ls"));
        assert!(sed_writes("s/a/b/e"));
        assert!(sed_writes("1!G;h;$!d;Z"));
    }

    #[test]
    fn cmd_exe_fallback_refuses_percent() {
        let f = fx();
        assert_eq!(
            classify(&f.roots, &f.roots.base, "echo %PATH%", true).class,
            Class::Delete
        );
        assert_eq!(
            classify(&f.roots, &f.roots.base, "echo hi", true).class,
            Class::Read
        );
    }
}
