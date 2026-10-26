//! F-2: the gate class of a shell command line, fail closed by construction (D-520,
//! D-523).
//!
//! A line is below Delete only when every simple command in it, command and process
//! substitutions included, is a form this file lists: a known program or builtin, used
//! with only the flags listed for it, and every path it replaces or deletes is a literal
//! path that resolves inside `runs/`. Anything else is Delete: it asks, and an
//! unanswered ask skips (H-1.3). That covers an unknown program, a program run by path,
//! an unknown flag (long-option abbreviations included), an argument decided at run time
//! where it could become an option or a target (`$x`, `$(..)`, a glob that could match
//! `-name`), a glob, brace or `~` target of a write, and these wherever they appear:
//! arithmetic (`$((..))`, `((..))`, `$[..]`), a `[..]` holding `$(` or a backtick,
//! `${..}` beyond a plain name, the builtins in [`FORBIDDEN`] (`let`, `declare`,
//! `printf -v`, `read`, `test -v`, `eval`, `exec`, `source`, `trap`, `xargs`, `ln`,
//! `bash -c`...), `find -exec/-delete/-fprint`, `cp -l/-s`, git options that run a
//! program, and build tools that run project code (`cargo`, `npm`, `make`, `python`...).
//!
//! Classes of the allowed forms:
//! * Read: readers (ls, cat, grep, git status/log/diff, curl GET...), and changes whose
//!   every target resolves inside `runs/` (`rm -rf runs/x`, `echo x > runs/x/a.txt`).
//! * Fill: creating or appending outside `runs/` (`echo x >> notes.txt`, `mkdir d`,
//!   `cp a new.txt`, moving a scratch file out to a new name) and local git work.
//! * Submit: sending data (`curl -d`, `curl -X POST`, `wget --post-data`, `gh pr create`).
//! * Send: `git send-email`.
//! * Account: `git push`, `git config` and `git remote` writes, `gh` writes to settings.
//! * Delete: deleting, replacing or moving away anything outside `runs/` (or a run's
//!   bookkeeping, or a file with other hard links), discarding git work, rewriting remote
//!   history, HTTP DELETE, and everything not listed.

use super::lex::{self, Simple, Word};
use crate::paths::{self, Place, Roots};
use crate::util::Verdict;
use harness_core::Class;
use std::path::{Path, PathBuf};

/// How deep command substitutions may nest before the line is refused.
const MAX_DEPTH: u8 = 6;

/// The reason given when the cmd.exe fallback refuses a line (B9).
pub const CMD_EXE_REFUSED: &str = "shell needs Git Bash (no bash was found; install Git for Windows or set SB_BASH to bash.exe): without it only one plain command runs, with no quotes, variables, globs, pipes, redirections or separators";

/// Delete wherever they appear (D-523): they evaluate text as code or arithmetic, assign
/// variables by name (a subscript in the name runs `$(..)`), change how later words are
/// read, run another program, or create links.
const FORBIDDEN: &[&str] = &[
    "let",
    "declare",
    "typeset",
    "local",
    "readonly",
    "export",
    "read",
    "mapfile",
    "readarray",
    "unset",
    "eval",
    "exec",
    "source",
    ".",
    "trap",
    "alias",
    "unalias",
    "function",
    "history",
    "enable",
    "builtin",
    "command",
    "env",
    "nohup",
    "time",
    "coproc",
    "select",
    "case",
    "xargs",
    "parallel",
    "ln",
    "link",
    "mklink",
    "hash",
    "shopt",
    "fc",
    "bind",
    "complete",
    "compgen",
    "getopts",
    "ulimit",
    "umask",
    "disown",
    "kill",
    "bash",
    "sh",
    "zsh",
    "dash",
    "ksh",
    "ash",
    "fish",
    "csh",
    "tcsh",
    "pwsh",
    "powershell",
    "cmd",
    "wsl",
    "sudo",
    "doas",
    "su",
    "runas",
    "gsudo",
    "setsid",
    "nice",
    "ionice",
    "stdbuf",
    "timeout",
    "watch",
    "script",
    "expect",
    "busybox",
    "start",
    "open",
    "xdg-open",
    "explorer",
    "rundll32",
    "regsvr32",
    "mshta",
    "wscript",
    "cscript",
    "rsync",
    "less",
    "more",
    "vi",
    "vim",
    "nano",
    "emacs",
];

/// Programs that run a project's own code or any program they are given (build scripts,
/// package scripts, hooks, interpreters): only their version is Read (S2, D-523).
const RUNS_CODE: &[&str] = &[
    "cargo",
    "rustc",
    "rustup",
    "npm",
    "npx",
    "pnpm",
    "yarn",
    "bun",
    "bunx",
    "node",
    "deno",
    "python",
    "python3",
    "py",
    "pip",
    "pip3",
    "uv",
    "uvx",
    "pipx",
    "poetry",
    "go",
    "java",
    "javac",
    "mvn",
    "gradle",
    "make",
    "cmake",
    "ninja",
    "docker",
    "podman",
    "kubectl",
    "terraform",
    "ruby",
    "gem",
    "bundle",
    "perl",
    "php",
    "dotnet",
    "gcc",
    "g++",
    "clang",
    "cc",
    "tsc",
    "jest",
    "pytest",
    "vitest",
    "gh-extension",
];

/// Programs whose every argument is harmless: no option writes a file, runs a program or
/// changes system state, so any flag and any run-time argument is fine.
const FREE: &[&str] = &[
    "echo",
    "true",
    "false",
    ":",
    "seq",
    "sleep",
    "factor",
    "expr",
    "rev",
    "tac",
    "fold",
    "fmt",
    "paste",
    "join",
    "expand",
    "unexpand",
    "numfmt",
    "nl",
    "cal",
    "getconf",
    "ps",
    "tasklist",
    "systeminfo",
    "ping",
    "nslookup",
    "dig",
    "host",
    "whois",
    "traceroute",
    "tracert",
    "where",
    "which",
    "printenv",
    "groups",
    "whoami",
    "jq",
    "bc",
    "zcat",
    "bzcat",
    "xzcat",
    "id",
    "locale",
    "nproc",
    "tty",
    "uptime",
];

/// Shell keywords skipped in front of a command.
const KEYWORDS: &[&str] = &[
    "if", "then", "else", "elif", "fi", "do", "done", "while", "until", "!", "{", "}",
];

/// The flags a program accepts. Anything else is an unknown flag (Delete).
#[derive(Clone, Copy)]
struct Flags {
    /// Short flags without a value (clusterable: `-la`).
    short: &'static str,
    /// Short flags with a value (attached `-n5` or the next word).
    short_val: &'static str,
    /// Long flags without a value (`--all`).
    long: &'static [&'static str],
    /// Long flags with a value (`--x=v` or `--x v`). A name in both lists takes a value
    /// only with `=`.
    long_val: &'static [&'static str],
    /// Whole words allowed as they are (`-nv`, `-uno`).
    exact: &'static [&'static str],
    /// Digits are allowed in a cluster (`-5`, `-M50`).
    num: bool,
}

const NO_FLAGS: Flags = Flags {
    short: "",
    short_val: "",
    long: &["help", "version"],
    long_val: &[],
    exact: &[],
    num: false,
};

/// A read-only program: the flags it may get. `dyn_ok`: none of its flags writes or runs
/// anything, so an argument decided at run time is fine. `max_ops`: more operands would
/// write (`hostname NAME`).
struct Reader {
    names: &'static [&'static str],
    flags: Flags,
    dyn_ok: bool,
    max_ops: Option<usize>,
}

const READERS: &[Reader] = &[
    Reader {
        names: &["ls", "dir", "vdir"],
        flags: Flags {
            short: "aAbBcCdfFgGhHiklLmnNopqQrRsStuUvxXZ1",
            short_val: "ITw",
            long: &[
                "all",
                "almost-all",
                "author",
                "escape",
                "ignore-backups",
                "color",
                "directory",
                "dired",
                "classify",
                "file-type",
                "full-time",
                "group-directories-first",
                "no-group",
                "human-readable",
                "si",
                "dereference-command-line",
                "dereference-command-line-symlink-to-dir",
                "dereference",
                "hyperlink",
                "inode",
                "kibibytes",
                "literal",
                "numeric-uid-gid",
                "hide-control-chars",
                "show-control-chars",
                "quote-name",
                "reverse",
                "recursive",
                "size",
                "context",
                "zero",
                "help",
                "version",
            ],
            long_val: &[
                "block-size",
                "color",
                "classify",
                "format",
                "hide",
                "hyperlink",
                "indicator-style",
                "ignore",
                "quoting-style",
                "sort",
                "time",
                "time-style",
                "tabsize",
                "width",
            ],
            exact: &[],
            num: false,
        },
        dyn_ok: true,
        max_ops: None,
    },
    Reader {
        names: &["cat"],
        flags: Flags {
            short: "AbeEnstTuv",
            short_val: "",
            long: &[
                "show-all",
                "number-nonblank",
                "show-ends",
                "number",
                "squeeze-blank",
                "show-tabs",
                "show-nonprinting",
                "help",
                "version",
            ],
            long_val: &[],
            exact: &[],
            num: false,
        },
        dyn_ok: true,
        max_ops: None,
    },
    Reader {
        names: &["head"],
        flags: Flags {
            short: "qvz",
            short_val: "cn",
            long: &[
                "quiet",
                "silent",
                "verbose",
                "zero-terminated",
                "help",
                "version",
            ],
            long_val: &["bytes", "lines"],
            exact: &[],
            num: true,
        },
        dyn_ok: true,
        max_ops: None,
    },
    Reader {
        names: &["tail"],
        flags: Flags {
            short: "fFqvz",
            short_val: "cns",
            long: &[
                "follow",
                "quiet",
                "silent",
                "verbose",
                "zero-terminated",
                "retry",
                "help",
                "version",
            ],
            long_val: &[
                "bytes",
                "lines",
                "follow",
                "pid",
                "sleep-interval",
                "max-unchanged-stats",
            ],
            exact: &[],
            num: true,
        },
        dyn_ok: true,
        max_ops: None,
    },
    Reader {
        names: &["wc"],
        flags: Flags {
            short: "clmwL",
            short_val: "",
            long: &[
                "bytes",
                "chars",
                "lines",
                "words",
                "max-line-length",
                "help",
                "version",
            ],
            long_val: &["files0-from", "total"],
            exact: &[],
            num: false,
        },
        dyn_ok: true,
        max_ops: None,
    },
    Reader {
        names: &["grep", "egrep", "fgrep"],
        flags: Flags {
            short: "EFGPiyvwxcLloqsbHhnTZzrRUaI",
            short_val: "efmABCDd",
            long: &[
                "extended-regexp",
                "fixed-strings",
                "basic-regexp",
                "perl-regexp",
                "ignore-case",
                "no-ignore-case",
                "invert-match",
                "word-regexp",
                "line-regexp",
                "count",
                "color",
                "colour",
                "files-without-match",
                "files-with-matches",
                "only-matching",
                "quiet",
                "silent",
                "no-messages",
                "byte-offset",
                "with-filename",
                "no-filename",
                "line-number",
                "initial-tab",
                "null",
                "null-data",
                "recursive",
                "dereference-recursive",
                "text",
                "binary",
                "line-buffered",
                "help",
                "version",
            ],
            long_val: &[
                "regexp",
                "file",
                "max-count",
                "after-context",
                "before-context",
                "context",
                "label",
                "include",
                "exclude",
                "exclude-dir",
                "exclude-from",
                "binary-files",
                "devices",
                "directories",
                "color",
                "colour",
            ],
            exact: &[],
            num: true,
        },
        dyn_ok: true,
        max_ops: None,
    },
    Reader {
        // No --pre / --pre-glob (they run a program on every file), no -z (runs
        // decompressors).
        names: &["rg"],
        flags: Flags {
            short: "iISsvwxclLnNoqFHhbuU",
            short_val: "efgtTABCmMjrE",
            long: &[
                "ignore-case",
                "smart-case",
                "case-sensitive",
                "invert-match",
                "word-regexp",
                "line-regexp",
                "count",
                "count-matches",
                "files-with-matches",
                "files-without-match",
                "line-number",
                "no-line-number",
                "only-matching",
                "quiet",
                "hidden",
                "no-ignore",
                "no-ignore-vcs",
                "no-ignore-dot",
                "follow",
                "fixed-strings",
                "with-filename",
                "no-filename",
                "files",
                "json",
                "vimgrep",
                "no-heading",
                "heading",
                "multiline",
                "pcre2",
                "stats",
                "text",
                "trim",
                "column",
                "byte-offset",
                "null",
                "type-list",
                "unrestricted",
                "help",
                "version",
            ],
            long_val: &[
                "color",
                "colors",
                "sort",
                "sortr",
                "glob",
                "iglob",
                "type",
                "type-not",
                "max-count",
                "max-depth",
                "max-columns",
                "after-context",
                "before-context",
                "context",
                "replace",
                "regexp",
                "file",
                "encoding",
                "threads",
            ],
            exact: &[],
            num: false,
        },
        dyn_ok: false,
        max_ops: None,
    },
    Reader {
        names: &["pwd"],
        flags: Flags {
            short: "LP",
            ..NO_FLAGS
        },
        dyn_ok: false,
        max_ops: Some(0),
    },
    Reader {
        names: &["type"],
        flags: Flags {
            short: "afptP",
            ..NO_FLAGS
        },
        dyn_ok: true,
        max_ops: None,
    },
    Reader {
        // No -C (compiles a magic file).
        names: &["file"],
        flags: Flags {
            short: "bcdEhiklLNnprsSvz0",
            short_val: "efFmP",
            long: &[
                "brief",
                "mime",
                "mime-type",
                "mime-encoding",
                "dereference",
                "no-dereference",
                "keep-going",
                "raw",
                "special-files",
                "uncompress",
                "no-pad",
                "print0",
                "preserve-date",
                "help",
                "version",
            ],
            long_val: &["exclude", "separator", "magic-file", "parameter"],
            exact: &[],
            num: false,
        },
        dyn_ok: false,
        max_ops: None,
    },
    Reader {
        names: &["stat"],
        flags: Flags {
            short: "LfQt",
            short_val: "c",
            long: &["dereference", "file-system", "terse", "help", "version"],
            long_val: &["format", "printf", "cached"],
            exact: &[],
            num: false,
        },
        dyn_ok: true,
        max_ops: None,
    },
    Reader {
        names: &["du"],
        flags: Flags {
            short: "0abcDhHklLmPsSx",
            short_val: "dBtX",
            long: &[
                "null",
                "all",
                "apparent-size",
                "bytes",
                "total",
                "dereference-args",
                "human-readable",
                "inodes",
                "count-links",
                "dereference",
                "no-dereference",
                "separate-dirs",
                "si",
                "summarize",
                "one-file-system",
                "time",
                "help",
                "version",
            ],
            long_val: &[
                "max-depth",
                "block-size",
                "threshold",
                "exclude",
                "exclude-from",
                "files0-from",
                "time",
                "time-style",
            ],
            exact: &[],
            num: false,
        },
        dyn_ok: true,
        max_ops: None,
    },
    Reader {
        names: &["df"],
        flags: Flags {
            short: "ahHiklPTv",
            short_val: "Btx",
            long: &[
                "all",
                "human-readable",
                "inodes",
                "local",
                "no-sync",
                "sync",
                "portability",
                "print-type",
                "si",
                "total",
                "output",
                "help",
                "version",
            ],
            long_val: &["block-size", "output", "type", "exclude-type"],
            exact: &[],
            num: false,
        },
        dyn_ok: true,
        max_ops: None,
    },
    Reader {
        names: &["cut"],
        flags: Flags {
            short: "nsz",
            short_val: "bcdf",
            long: &[
                "complement",
                "only-delimited",
                "zero-terminated",
                "help",
                "version",
            ],
            long_val: &[
                "bytes",
                "characters",
                "delimiter",
                "fields",
                "output-delimiter",
            ],
            exact: &[],
            num: false,
        },
        dyn_ok: true,
        max_ops: None,
    },
    Reader {
        names: &["tr"],
        flags: Flags {
            short: "cCdst",
            short_val: "",
            long: &[
                "complement",
                "delete",
                "squeeze-repeats",
                "truncate-set1",
                "help",
                "version",
            ],
            long_val: &[],
            exact: &[],
            num: false,
        },
        dyn_ok: true,
        max_ops: None,
    },
    Reader {
        // No -l / --paginate (pipes through pr).
        names: &["diff"],
        flags: Flags {
            short: "abBdEiNpqrstTuwyZ",
            short_val: "CUDFIxXSW",
            long: &[
                "brief",
                "report-identical-files",
                "unified",
                "context",
                "side-by-side",
                "ignore-case",
                "ignore-space-change",
                "ignore-all-space",
                "ignore-blank-lines",
                "ignore-tab-expansion",
                "ignore-trailing-space",
                "recursive",
                "new-file",
                "unidirectional-new-file",
                "text",
                "strip-trailing-cr",
                "color",
                "minimal",
                "speed-large-files",
                "suppress-common-lines",
                "left-column",
                "expand-tabs",
                "initial-tab",
                "no-dereference",
                "show-c-function",
                "help",
                "version",
            ],
            long_val: &[
                "unified",
                "context",
                "width",
                "ignore-matching-lines",
                "color",
                "exclude",
                "exclude-from",
                "label",
                "starting-file",
                "from-file",
                "to-file",
                "show-function-line",
                "tabsize",
                "palette",
            ],
            exact: &[],
            num: false,
        },
        dyn_ok: false,
        max_ops: None,
    },
    Reader {
        names: &["cmp"],
        flags: Flags {
            short: "bls",
            short_val: "in",
            long: &[
                "print-bytes",
                "verbose",
                "quiet",
                "silent",
                "help",
                "version",
            ],
            long_val: &["ignore-initial", "bytes"],
            exact: &[],
            num: false,
        },
        dyn_ok: true,
        max_ops: None,
    },
    Reader {
        names: &["comm"],
        flags: Flags {
            short: "123z",
            short_val: "",
            long: &[
                "check-order",
                "nocheck-order",
                "total",
                "zero-terminated",
                "help",
                "version",
            ],
            long_val: &["output-delimiter"],
            exact: &[],
            num: false,
        },
        dyn_ok: true,
        max_ops: None,
    },
    Reader {
        // An operand sets the host name.
        names: &["hostname"],
        flags: Flags {
            short: "fsidIaA",
            ..NO_FLAGS
        },
        dyn_ok: false,
        max_ops: Some(0),
    },
    Reader {
        names: &["uname"],
        flags: Flags {
            short: "asnrvmpio",
            short_val: "",
            long: &[
                "all",
                "kernel-name",
                "nodename",
                "kernel-release",
                "kernel-version",
                "machine",
                "processor",
                "hardware-platform",
                "operating-system",
                "help",
                "version",
            ],
            long_val: &[],
            exact: &[],
            num: false,
        },
        dyn_ok: false,
        max_ops: Some(0),
    },
    Reader {
        names: &["free"],
        flags: Flags {
            short: "bkmghltwv",
            short_val: "cs",
            ..NO_FLAGS
        },
        dyn_ok: false,
        max_ops: Some(0),
    },
    Reader {
        names: &["basename"],
        flags: Flags {
            short: "az",
            short_val: "s",
            long: &["multiple", "zero", "help", "version"],
            long_val: &["suffix"],
            exact: &[],
            num: false,
        },
        dyn_ok: true,
        max_ops: None,
    },
    Reader {
        names: &["dirname"],
        flags: Flags {
            short: "z",
            long: &["zero", "help", "version"],
            ..NO_FLAGS
        },
        dyn_ok: true,
        max_ops: None,
    },
    Reader {
        names: &["realpath", "readlink"],
        flags: Flags {
            short: "eEfmnLPqsvz",
            short_val: "",
            long: &[
                "canonicalize",
                "canonicalize-existing",
                "canonicalize-missing",
                "logical",
                "physical",
                "quiet",
                "silent",
                "strip",
                "no-symlinks",
                "no-newline",
                "verbose",
                "zero",
                "help",
                "version",
            ],
            long_val: &["relative-to", "relative-base"],
            exact: &[],
            num: false,
        },
        dyn_ok: true,
        max_ops: None,
    },
    Reader {
        names: &[
            "md5sum",
            "sha1sum",
            "sha224sum",
            "sha256sum",
            "sha384sum",
            "sha512sum",
            "b2sum",
            "cksum",
            "md5",
            "shasum",
        ],
        flags: Flags {
            short: "bctwz0",
            short_val: "la",
            long: &[
                "binary",
                "check",
                "tag",
                "text",
                "zero",
                "ignore-missing",
                "quiet",
                "status",
                "strict",
                "warn",
                "raw",
                "base64",
                "untagged",
                "help",
                "version",
            ],
            long_val: &["algorithm", "length"],
            exact: &[],
            num: false,
        },
        dyn_ok: true,
        max_ops: None,
    },
    Reader {
        names: &["base64", "base32", "basenc"],
        flags: Flags {
            short: "di",
            short_val: "w",
            long: &[
                "decode",
                "ignore-garbage",
                "base64",
                "base64url",
                "base32",
                "base32hex",
                "base16",
                "help",
                "version",
            ],
            long_val: &["wrap"],
            exact: &[],
            num: false,
        },
        dyn_ok: true,
        max_ops: None,
    },
    Reader {
        // No -o (writes), no -R (writes 00Tree.html in every folder).
        names: &["tree"],
        flags: Flags {
            short: "adlfxiqNQpugsDFrtUvcCnSAhJ",
            short_val: "LPIHT",
            long: &[
                "noreport",
                "dirsfirst",
                "filesfirst",
                "du",
                "si",
                "prune",
                "gitignore",
                "matchdirs",
                "ignore-case",
                "inodes",
                "device",
                "help",
                "version",
            ],
            long_val: &["charset", "filelimit", "timefmt", "sort", "info"],
            exact: &[],
            num: false,
        },
        dyn_ok: false,
        max_ops: None,
    },
    Reader {
        names: &["od"],
        flags: Flags {
            short: "bcdfiloxsv",
            short_val: "AjNtSw",
            long: &["output-duplicates", "traditional", "help", "version"],
            long_val: &[
                "address-radix",
                "skip-bytes",
                "read-bytes",
                "format",
                "strings",
                "width",
                "endian",
            ],
            exact: &[],
            num: false,
        },
        dyn_ok: true,
        max_ops: None,
    },
    Reader {
        // At most one operand: a second one is written.
        names: &["xxd"],
        flags: Flags {
            short: "bEipsuerR",
            short_val: "cglo",
            long: &["help", "version"],
            long_val: &[],
            exact: &[],
            num: false,
        },
        dyn_ok: false,
        max_ops: Some(1),
    },
    Reader {
        names: &["hexdump", "hd"],
        flags: Flags {
            short: "bcCdovx",
            short_val: "eEfnsL",
            ..NO_FLAGS
        },
        dyn_ok: true,
        max_ops: None,
    },
    Reader {
        names: &["strings"],
        flags: Flags {
            short: "adfw",
            short_val: "nteos",
            long: &[
                "all",
                "print-file-name",
                "include-all-whitespace",
                "help",
                "version",
            ],
            long_val: &["bytes", "radix", "encoding", "output-separator"],
            exact: &[],
            num: true,
        },
        dyn_ok: true,
        max_ops: None,
    },
];

/// Where relative paths resolve while the line is walked: every folder the shell may be
/// in at this point (a relative target must be safe from each), `None` after a `cd` the
/// classifier cannot follow. A `cd` replaces the set only when everything after it in the
/// line is joined by `&&` and the line has no subshell, pipe, `||` or `&`: then a failed
/// `cd` stops the rest. Otherwise (`cd x; rm y`, `(cd x); rm y`) the new folder is added.
/// Inside a loop a `cd` repeats, so the folder becomes unknown.
#[derive(Clone)]
struct Env<'a> {
    roots: &'a Roots,
    cwds: Option<Vec<PathBuf>>,
    /// Set per command: a `cd` here replaces the folder set (see above).
    cd_replaces: bool,
    /// `while`/`until`/`for` nesting.
    loop_depth: u32,
    /// Commands in the whole line that may create links (extract, clone, copy a tree).
    link_cmds: usize,
    /// The command being classified is one of them.
    in_link_cmd: bool,
}

impl Env<'_> {
    /// Every candidate folder is scratch.
    fn cwd_scratch(&self) -> Option<bool> {
        self.cwds
            .as_ref()
            .map(|c| c.iter().all(|d| self.roots.place(d) == Place::Scratch))
    }

    /// Another command in the line may create links inside `runs/` before this one runs
    /// (pipelines and `&` run out of order), so a path in `runs/` cannot be checked now.
    fn links_elsewhere(&self) -> bool {
        self.link_cmds > usize::from(self.in_link_cmd)
    }
}

/// The class of `cmd` run in `cwd`. `cmd_exe`: the line would run under cmd.exe (no bash
/// on this machine), where this bash lexer does not apply: only a single plain command
/// (see [`plain_argv`]) is classified; anything else is Delete with [`CMD_EXE_REFUSED`].
pub fn classify(roots: &Roots, cwd: &Path, cmd: &str, cmd_exe: bool) -> Verdict {
    let mut v = Verdict::new(Class::Read);
    if cmd_exe && plain_argv(cmd).is_none() {
        v.raise(Class::Delete, CMD_EXE_REFUSED);
        return v;
    }
    if let Some(why) = subscript_substitution(cmd) {
        v.raise(Class::Delete, why);
        return v;
    }
    let mut env = Env {
        roots,
        cwds: Some(vec![cwd.to_path_buf()]),
        cd_replaces: false,
        loop_depth: 0,
        link_cmds: link_commands(cmd, 0),
        in_link_cmd: false,
    };
    source(&mut env, cmd, 0, &mut v);
    v
}

/// The argv of `cmd` when it is one simple command made only of plain words (letters,
/// digits and `-_./:=,+@`; no quotes, `$`, globs, `%`, `^`, separators or
/// redirections), the only form run without bash (B9). The first word is no assignment.
pub fn plain_argv(cmd: &str) -> Option<Vec<String>> {
    if !cmd
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || " \t-_./:=,+@".contains(c))
    {
        return None;
    }
    let p = lex::parse(cmd).ok()?;
    if p.commands.len() != 1 || !p.subs.is_empty() || p.structured {
        return None;
    }
    let c = &p.commands[0];
    if !c.redirs.is_empty() || c.words.iter().any(|w| !w.is_static() || w.quoted) {
        return None;
    }
    let argv: Vec<String> = c.words.iter().map(|w| w.text.clone()).collect();
    if argv.first().is_none_or(|a| a.contains('=')) {
        return None;
    }
    Some(argv)
}

/// A `[` followed by `$(` or a backtick before the next `]`, anywhere in the line, even
/// inside quotes: a subscript or `[`-expression that `let`, `declare`, `test -v`,
/// `[[ -eq ]]` and friends evaluate as arithmetic, which runs the substitution (B2).
fn subscript_substitution(src: &str) -> Option<String> {
    for (i, c) in src.char_indices() {
        if c != '[' {
            continue;
        }
        let rest = &src[i + 1..];
        let inner = &rest[..rest.find(']').unwrap_or(rest.len())];
        if inner.contains("$(") || inner.contains('`') {
            return Some(
                "a [..] holding a command substitution (bash evaluates subscripts as arithmetic, which runs it)"
                    .into(),
            );
        }
    }
    None
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
    // A substitution runs where its command runs; after a `cd` in this line that folder
    // is not the starting one.
    let changes_dir = parsed.commands.iter().any(|c| {
        c.words.iter().any(|w| {
            matches!(
                command_name(&w.text).as_str(),
                "cd" | "pushd" | "popd" | "chdir"
            )
        })
    });
    for sub in &parsed.subs {
        let mut inner = env.clone();
        if changes_dir || env.loop_depth > 0 {
            inner.cwds = None;
        }
        source(&mut inner, sub, depth + 1, v);
    }
    for (i, c) in parsed.commands.iter().enumerate() {
        env.cd_replaces = !parsed.structured
            && parsed.commands[i..]
                .iter()
                .all(|c| matches!(c.next_sep, "&&" | ""));
        simple(env, c, v);
    }
}

/// `NAME=value`: the name, when the word is an assignment.
fn assignment_name(w: &Word) -> Option<&str> {
    let (name, _) = w.text.split_once('=')?;
    let ok = !name.is_empty()
        && !name.starts_with(|c: char| c.is_ascii_digit())
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    ok.then_some(name)
}

/// Variables a line may set. A standalone assignment sets a shell variable: lowercase
/// names are the line's own. A prefix (`X=1 cmd`) exports it to the program: only names
/// that change formatting. Everything else may make a later command run a program or
/// read elsewhere (PATH, IFS, PS4, BASH_ENV, PAGER, GIT_*, HOME, CURL_HOME...).
fn assignment_ok(name: &str, prefix: bool) -> bool {
    let lower = name
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
    if lower && !prefix {
        return true;
    }
    (name.starts_with("LC_")
        && name[3..]
            .chars()
            .all(|c| c.is_ascii_uppercase() || c == '_'))
        || matches!(
            name,
            "LANG"
                | "LANGUAGE"
                | "TZ"
                | "COLUMNS"
                | "LINES"
                | "NO_COLOR"
                | "FORCE_COLOR"
                | "CLICOLOR"
                | "CLICOLOR_FORCE"
                | "TERM"
                | "RUST_LOG"
                | "RUST_BACKTRACE"
                | "PYTHONIOENCODING"
                | "PYTHONUTF8"
        )
}

/// The command's name and arguments after keywords and assignments.
fn head_and_args(c: &Simple) -> (Option<&Word>, &[Word]) {
    let ws = &c.words;
    let mut i = 0;
    while i < ws.len() {
        let w = &ws[i];
        let keyword = w.is_static() && !w.quoted && KEYWORDS.contains(&w.text.as_str());
        if !(keyword || assignment_name(w).is_some()) {
            break;
        }
        i += 1;
    }
    (ws.get(i), ws.get(i + 1..).unwrap_or(&[]))
}

fn simple(env: &mut Env, c: &Simple, v: &mut Verdict) {
    let ws = &c.words;
    let mut i = 0;
    while i < ws.len() {
        let w = &ws[i];
        let t = w.text.as_str();
        if w.is_static() && !w.quoted && KEYWORDS.contains(&t) {
            match t {
                "while" | "until" => env.loop_depth += 1,
                "done" => env.loop_depth = env.loop_depth.saturating_sub(1),
                _ => {}
            }
            i += 1;
            continue;
        }
        if let Some(name) = assignment_name(w) {
            let prefix = ws[i + 1..].iter().any(|x| assignment_name(x).is_none());
            if !assignment_ok(name, prefix) {
                v.raise(
                    Class::Delete,
                    format!(
                        "sets {}, which can change what later commands run or read",
                        crate::text::one_line(name, 40)
                    ),
                );
            }
            i += 1;
            continue;
        }
        break;
    }
    for r in &c.redirs {
        redirection(env, r, v);
    }
    let ws = &ws[i..];
    let Some(head) = ws.first() else { return };
    if !head.is_static() {
        v.raise(Class::Delete, "the command name is decided at run time");
        return;
    }
    if head.text.contains(['/', '\\']) {
        v.raise(
            Class::Delete,
            format!(
                "runs the program {} by path (only known programs found on PATH are checked)",
                crate::text::one_line(&head.text, 80)
            ),
        );
        return;
    }
    let name = command_name(&head.text);
    let args = &ws[1..];
    env.in_link_cmd = may_link(&name, args);
    program(env, &name, args, v);
    env.in_link_cmd = false;
}

fn redirection(env: &Env, r: &lex::Redir, v: &mut Verdict) {
    let Some(t) = &r.target else { return };
    match r.op.as_str() {
        ">" | ">|" | "&>" | ">&" | "<>" => write_target(env, t, true, v),
        ">>" | "&>>" => write_target(env, t, false, v),
        _ => {
            if net_device(&t.text) {
                v.raise(Class::Delete, "reads a /dev/tcp or /dev/udp connection");
            }
        }
    }
}

/// bash's own network paths.
fn net_device(t: &str) -> bool {
    t.starts_with("/dev/tcp/") || t.starts_with("/dev/udp/")
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

/// A word that may expand to an option: decided at run time, a brace expansion, or a
/// glob whose first character is a pattern (it may match a file named `-x`).
fn may_be_option(w: &Word) -> bool {
    w.dynamic || w.brace || (w.glob && w.text.starts_with(['*', '?', '[']))
}

/// One parsed option.
struct Opt {
    /// `-x` or `--long`; `-#` for a digit run.
    name: String,
    value: Option<Word>,
}

/// Arguments split by [`parse_flags`].
struct Parsed<'a> {
    opts: Vec<Opt>,
    ops: Vec<&'a Word>,
}

impl Parsed<'_> {
    fn has(&self, names: &[&str]) -> bool {
        self.opts.iter().any(|o| names.contains(&o.name.as_str()))
    }

    fn value(&self, names: &[&str]) -> Option<&Word> {
        self.opts
            .iter()
            .rev()
            .find(|o| names.contains(&o.name.as_str()))
            .and_then(|o| o.value.as_ref())
    }

    fn values(&self, names: &[&str]) -> Vec<&Word> {
        self.opts
            .iter()
            .filter(|o| names.contains(&o.name.as_str()))
            .filter_map(|o| o.value.as_ref())
            .collect()
    }
}

/// Splits `args` into options and operands by `f`. An option not in `f` is an error, and
/// so (unless `dyn_ok`) is a word before `--` that may expand to an option.
fn parse_flags<'a>(f: &Flags, args: &'a [Word], dyn_ok: bool) -> Result<Parsed<'a>, String> {
    let mut p = Parsed {
        opts: Vec::new(),
        ops: Vec::new(),
    };
    let mut k = 0;
    let mut rest = false;
    while k < args.len() {
        let a = &args[k];
        let t = a.text.as_str();
        k += 1;
        if rest {
            p.ops.push(a);
            continue;
        }
        if may_be_option(a) {
            if !dyn_ok {
                return Err(format!(
                    "{} is decided at run time and could become an option",
                    shown_word(a)
                ));
            }
            p.ops.push(a);
            continue;
        }
        if !t.starts_with('-') || t == "-" {
            p.ops.push(a);
            continue;
        }
        if t == "--" {
            rest = true;
            continue;
        }
        if f.exact.contains(&t) {
            p.opts.push(Opt {
                name: t.into(),
                value: None,
            });
            continue;
        }
        if let Some(long) = t.strip_prefix("--") {
            let (name, inline) = match long.split_once('=') {
                Some((n, val)) => (n, Some(val)),
                None => (long, None),
            };
            let full = format!("--{name}");
            match inline {
                Some(val) if f.long_val.contains(&name) => p.opts.push(Opt {
                    name: full,
                    value: Some(Word {
                        text: val.into(),
                        ..a.clone()
                    }),
                }),
                None if f.long.contains(&name) => p.opts.push(Opt {
                    name: full,
                    value: None,
                }),
                None if f.long_val.contains(&name) => {
                    let Some(val) = args.get(k) else {
                        return Err(format!("{full} without a value"));
                    };
                    k += 1;
                    p.opts.push(Opt {
                        name: full,
                        value: Some(val.clone()),
                    });
                }
                _ => return Err(crate::text::one_line(t, 60)),
            }
            continue;
        }
        let chars: Vec<char> = t[1..].chars().collect();
        let mut j = 0;
        while j < chars.len() {
            let c = chars[j];
            if f.short_val.contains(c) {
                let attached: String = chars[j + 1..].iter().collect();
                let value = if attached.is_empty() {
                    let Some(val) = args.get(k) else {
                        return Err(format!("-{c} without a value"));
                    };
                    k += 1;
                    val.clone()
                } else {
                    Word {
                        text: attached,
                        ..a.clone()
                    }
                };
                p.opts.push(Opt {
                    name: format!("-{c}"),
                    value: Some(value),
                });
                break;
            }
            if f.num && c.is_ascii_digit() {
                let digits: String = chars[j..]
                    .iter()
                    .take_while(|d| d.is_ascii_digit())
                    .collect();
                j += digits.len();
                p.opts.push(Opt {
                    name: "-#".into(),
                    value: Some(Word {
                        text: digits,
                        ..Word::default()
                    }),
                });
                continue;
            }
            if f.short.contains(c) {
                p.opts.push(Opt {
                    name: format!("-{c}"),
                    value: None,
                });
                j += 1;
                continue;
            }
            return Err(format!("-{c}"));
        }
    }
    Ok(p)
}

fn shown_word(w: &Word) -> String {
    if w.text.is_empty() {
        "an argument".into()
    } else {
        crate::text::one_line(&w.text, 60)
    }
}

/// Parses with `f` or raises Delete for an unknown flag.
fn flags_or_delete<'a>(
    name: &str,
    f: &Flags,
    args: &'a [Word],
    dyn_ok: bool,
    v: &mut Verdict,
) -> Option<Parsed<'a>> {
    match parse_flags(f, args, dyn_ok) {
        Ok(p) => Some(p),
        Err(e) => {
            v.raise(
                Class::Delete,
                format!("{name}: {e} is not an option the classifier knows to be safe"),
            );
            None
        }
    }
}

fn program(env: &mut Env, name: &str, args: &[Word], v: &mut Verdict) {
    if FORBIDDEN.contains(&name) {
        v.raise(Class::Delete, forbidden_reason(name));
        return;
    }
    if RUNS_CODE.contains(&name) {
        runs_code(name, args, v);
        return;
    }
    if FREE.contains(&name) {
        return;
    }
    if let Some(r) = READERS.iter().find(|r| r.names.contains(&name)) {
        if let Some(p) = flags_or_delete(name, &r.flags, args, r.dyn_ok, v) {
            if r.max_ops.is_some_and(|m| p.ops.len() > m) {
                v.raise(
                    Class::Delete,
                    format!("{name} with more operands writes or changes something"),
                );
            }
        }
        return;
    }
    match name {
        "exit" | "return" | "break" | "continue" | "shift" | "wait" => {
            if args.iter().any(|a| !a.is_static()) {
                v.raise(
                    Class::Delete,
                    format!("{name} with an argument decided at run time"),
                );
            }
        }
        "printf" => printf(args, v),
        "date" => date(args, v),
        "test" | "[" => test(name, args, v),
        "[[" => double_bracket(args, v),
        "]]" => v.raise(Class::Delete, "[[ .. ]] split by && or || is not parsed"),
        "set" => set(args, v),
        "for" => for_loop(env, args, v),
        "cd" | "pushd" | "chdir" => cd(env, args),
        "popd" => env.cwds = None,
        "sort" => sort(env, args, v),
        "uniq" => uniq(env, args, v),
        "tee" => tee(env, args, v),
        "iconv" => iconv(env, args, v),
        "rm" => rm(env, args, v),
        "rmdir" => rmdir(env, args, v),
        "unlink" => {
            if let Some(p) = flags_or_delete(name, &NO_FLAGS, args, false, v) {
                for t in p.ops {
                    delete_target(env, t, v);
                }
            }
        }
        "mkdir" => mkdir(env, args, v),
        "touch" => touch(env, args, v),
        "cp" => cp(env, args, v),
        "mv" => mv(env, args, v),
        "truncate" => truncate(env, args, v),
        "sed" => sed(env, args, v),
        "awk" | "gawk" | "mawk" | "nawk" => awk(args, v),
        "gzip" | "gunzip" | "bzip2" | "bunzip2" | "xz" | "unxz" => gzip(env, name, args, v),
        "tar" => tar(env, args, v),
        "unzip" => unzip(env, args, v),
        "zip" => zip(env, args, v),
        "curl" => curl(env, args, v),
        "wget" => wget(env, args, v),
        "git" => git(env, args, v),
        "gh" => gh(args, v),
        "find" => find(args, v),
        _ => v.raise(
            Class::Delete,
            format!(
                "{} is not a command the classifier knows to be safe",
                crate::text::one_line(name, 60)
            ),
        ),
    }
}

fn forbidden_reason(name: &str) -> String {
    let why = match name {
        "let" | "declare" | "typeset" | "local" | "readonly" | "export" | "read" | "mapfile"
        | "readarray" | "unset" | "getopts" => {
            "assigns variables by name (a subscript in the name is arithmetic and runs $(..))"
        }
        "eval" | "exec" | "source" | "." | "trap" | "alias" | "unalias" | "function" | "fc"
        | "bind" | "complete" | "compgen" | "enable" | "builtin" | "command" | "hash" | "shopt" => {
            "evaluates text as shell code or changes how later commands run"
        }
        "history" => "history -w/-a/-r writes or reads any file",
        "env" | "nohup" | "time" | "coproc" | "select" | "case" | "nice" | "ionice" | "stdbuf"
        | "timeout" | "watch" | "setsid" | "script" | "expect" | "busybox" | "parallel"
        | "xargs" => "runs another command the classifier does not check",
        "ln" | "link" | "mklink" => "creates a link that can alias a file outside runs/",
        "sudo" | "doas" | "su" | "runas" | "gsudo" => "runs as another user",
        "rsync" => "rsync can delete, run programs and copy links",
        "less" | "more" | "vi" | "vim" | "nano" | "emacs" => {
            "an interactive program that can run commands"
        }
        _ => "runs a shell, an interpreter or another program",
    };
    format!("{name}: {why}")
}

fn runs_code(name: &str, args: &[Word], v: &mut Verdict) {
    let version = args.len() == 1
        && args[0].is_static()
        && (args[0].text == "--version"
            || (args[0].text == "version" && matches!(name, "go" | "cargo" | "docker" | "podman")));
    if !version {
        v.raise(
            Class::Delete,
            format!("{name} runs the project's own code or other programs (build scripts, package scripts, hooks, interpreters)"),
        );
    }
}

/// `printf` without `-v` (which assigns a variable by name, B2). The format must be a
/// literal so it cannot turn into `-v`.
fn printf(args: &[Word], v: &mut Verdict) {
    let mut a = args;
    if let Some(f) = a.first() {
        if f.is_static() && f.text == "--" {
            a = &a[1..];
        } else if f.is_static() && f.text.starts_with('-') {
            v.raise(Class::Delete, "printf -v assigns a variable by name");
            return;
        }
    }
    if a.first().is_some_and(|f| !f.is_static()) {
        v.raise(
            Class::Delete,
            "printf with a format decided at run time (it could become -v)",
        );
    }
}

/// `date` reads; an operand that is not `+FORMAT` sets the clock.
fn date(args: &[Word], v: &mut Verdict) {
    const F: Flags = Flags {
        short: "uRI",
        short_val: "dfr",
        long: &["utc", "universal", "rfc-email", "debug", "help", "version"],
        long_val: &["date", "file", "reference", "iso-8601", "rfc-3339"],
        exact: &[],
        num: false,
    };
    if let Some(p) = flags_or_delete("date", &F, args, false, v) {
        if p.ops.iter().any(|o| !o.text.starts_with('+')) {
            v.raise(Class::Delete, "date with an operand sets the clock");
        }
    }
}

/// `test` and `[`: no `-v`/`-R` (they evaluate a subscript), and a run-time word only
/// where its position cannot make it an operator (B2).
fn test(name: &str, args: &[Word], v: &mut Verdict) {
    let mut a: Vec<&Word> = args.iter().collect();
    if name == "[" {
        match a.last() {
            Some(w) if w.is_static() && w.text == "]" => {
                a.pop();
            }
            _ => {
                v.raise(Class::Delete, "[ without its closing ]");
                return;
            }
        }
    }
    if a.iter()
        .any(|w| w.is_static() && matches!(w.text.as_str(), "-v" | "-R"))
    {
        v.raise(Class::Delete, "test -v evaluates a variable subscript");
        return;
    }
    if a.iter().any(|w| w.split || w.glob || w.brace || w.tilde) {
        v.raise(
            Class::Delete,
            "test with an unquoted expansion (it can turn into any operator)",
        );
        return;
    }
    if a.iter().all(|w| w.is_static()) {
        return;
    }
    let op = |w: &Word| w.is_static() && w.text.starts_with('-') && w.text != "-v";
    let fixed = match a.len() {
        1 => true,
        2 => op(a[0]),
        3 => a[1].is_static() && !a[1].text.is_empty(),
        _ => false,
    };
    if !fixed {
        v.raise(
            Class::Delete,
            "test with a run-time word where it could become an operator",
        );
    }
}

/// `[[ .. ]]`: no `-v`/`-R`, and arithmetic comparisons only between literal integers
/// (bash evaluates their operands as arithmetic, B2).
fn double_bracket(args: &[Word], v: &mut Verdict) {
    if !args.last().is_some_and(|w| w.is_static() && w.text == "]]") {
        v.raise(Class::Delete, "[[ .. ]] split by && or || is not parsed");
        return;
    }
    let a = &args[..args.len() - 1];
    if a.iter()
        .any(|w| w.is_static() && matches!(w.text.as_str(), "-v" | "-R"))
    {
        v.raise(Class::Delete, "[[ -v ]] evaluates a variable subscript");
        return;
    }
    let int = |w: Option<&Word>| {
        w.is_some_and(|w| {
            w.is_static() && {
                let t = w.text.strip_prefix('-').unwrap_or(&w.text);
                !t.is_empty() && t.chars().all(|c| c.is_ascii_digit())
            }
        })
    };
    for (i, w) in a.iter().enumerate() {
        if w.is_static()
            && matches!(
                w.text.as_str(),
                "-eq" | "-ne" | "-lt" | "-le" | "-gt" | "-ge"
            )
            && !(i > 0 && int(a.get(i - 1)) && int(a.get(i + 1)))
        {
            v.raise(
                Class::Delete,
                "[[ ]] arithmetic comparison of a non-literal (bash evaluates it as arithmetic)",
            );
            return;
        }
    }
}

/// `set` with only -e -u -x -v -C and `-o` errexit/nounset/pipefail/xtrace/verbose/
/// noclobber, or `--` and positional values.
fn set(args: &[Word], v: &mut Verdict) {
    let mut k = 0;
    while k < args.len() {
        let w = &args[k];
        k += 1;
        if !w.is_static() {
            v.raise(Class::Delete, "set with an argument decided at run time");
            return;
        }
        let t = w.text.as_str();
        if t == "--" {
            return;
        }
        let Some(cluster) = t.strip_prefix('-').or_else(|| t.strip_prefix('+')) else {
            continue;
        };
        for c in cluster.chars() {
            match c {
                'e' | 'u' | 'x' | 'v' | 'C' => {}
                'o' => {
                    if let Some(o) = args.get(k) {
                        k += 1;
                        if !matches!(
                            o.text.as_str(),
                            "errexit" | "nounset" | "pipefail" | "xtrace" | "verbose" | "noclobber"
                        ) || !o.is_static()
                        {
                            v.raise(
                                Class::Delete,
                                format!(
                                    "set -o {} changes how the line runs",
                                    crate::text::one_line(&o.text, 30)
                                ),
                            );
                            return;
                        }
                    }
                }
                _ => {
                    v.raise(
                        Class::Delete,
                        format!("set -{c} changes how the line is read"),
                    );
                    return;
                }
            }
        }
    }
}

/// `for NAME in WORDS`: a loop (a `cd` inside it repeats); the name follows the
/// assignment rules.
fn for_loop(env: &mut Env, args: &[Word], v: &mut Verdict) {
    env.loop_depth += 1;
    match args.first() {
        Some(n) if n.is_static() && assignment_ok(&n.text, false) => {}
        _ => v.raise(
            Class::Delete,
            "for loop over a variable that changes how commands run",
        ),
    }
}

/// Follows `cd DIR` (see [`Env`]): the folder set becomes `{DIR}` when the cd's failure
/// would stop the rest of the line, else `DIR` joins it. Unknown after `cd`, `cd -`, a
/// dynamic or missing folder, a Git Bash path, an unknown option, or inside a loop.
fn cd(env: &mut Env, args: &[Word]) {
    let mut ops: Vec<&Word> = Vec::new();
    for a in args {
        if a.is_static() && matches!(a.text.as_str(), "-L" | "-P" | "-e" | "--") {
            continue;
        }
        if a.is_static() && a.text.starts_with('-') && a.text != "-" {
            env.cwds = None;
            return;
        }
        ops.push(a);
    }
    if env.loop_depth > 0 || ops.len() != 1 {
        env.cwds = None;
        return;
    }
    let Some(old) = env.cwds.clone() else { return };
    let w = ops[0];
    let target = if w.is_static() && w.text != "-" {
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
    } else {
        None
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

// ---- targets ----

/// A word resolved to paths.
enum Target {
    Path(PathBuf),
    Null,
    Unknown(String),
}

/// Resolves `w`. Globs are expanded only when `glob_ok` (deletes): a written path must be
/// literal.
fn resolve(env: &Env, w: &Word, glob_ok: bool) -> Vec<Target> {
    let shown = crate::text::one_line(&w.text, 80);
    if w.dynamic {
        return vec![Target::Unknown(format!(
            "{shown}... is decided at run time"
        ))];
    }
    if w.tilde || w.brace {
        return vec![Target::Unknown(format!("{shown} expands at run time"))];
    }
    if w.glob && !glob_ok {
        return vec![Target::Unknown(format!(
            "{shown} is a glob (a written path must be literal)"
        ))];
    }
    let t = w.text.as_str();
    if matches!(t, "/dev/null" | "/dev/stdout" | "/dev/stderr" | "/dev/tty")
        || t.eq_ignore_ascii_case("nul")
    {
        return vec![Target::Null];
    }
    if net_device(t) {
        return vec![Target::Unknown(format!("{shown} is a network connection"))];
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

/// A scratch path is only safe when no other command in the line may plant a link there.
fn scratch_ok(env: &Env, p: &Path, v: &mut Verdict) -> bool {
    if env.links_elsewhere() {
        v.raise(
            Class::Delete,
            format!(
                "{} in runs/: another command in this line extracts, clones or copies files that may be links",
                env.roots.shown(p)
            ),
        );
        return false;
    }
    true
}

fn delete_target(env: &Env, w: &Word, v: &mut Verdict) {
    for t in resolve(env, w, true) {
        match t {
            Target::Null => {}
            Target::Unknown(why) => v.raise(
                Class::Delete,
                format!("cannot tell what this deletes: {why}"),
            ),
            Target::Path(p) => match env.roots.place(&p) {
                Place::Scratch => {
                    if scratch_ok(env, &p, v) {
                        v.raise(
                            Class::Read,
                            format!("deletes {} inside runs/", env.roots.shown(&p)),
                        );
                    }
                }
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
    for t in resolve(env, w, false) {
        match t {
            Target::Null => {}
            Target::Unknown(why) => v.raise(
                Class::Delete,
                format!("cannot tell what this writes: {why}"),
            ),
            Target::Path(p) if paths::runs_code_on_use(&p) => config_write(env, &p, v),
            Target::Path(p) if paths::hard_linked(&p) => v.raise(
                Class::Delete,
                format!(
                    "writes {}, a file with other hard links (they may be outside runs/)",
                    env.roots.shown(&p)
                ),
            ),
            Target::Path(p) => match env.roots.place(&p) {
                Place::Scratch => {
                    scratch_ok(env, &p, v);
                }
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

/// Git hooks and config, `.gitattributes`, `.curlrc`...: allowed programs run what these
/// say (core.fsmonitor on `git status`, a pre-commit hook on `git commit`).
fn config_write(env: &Env, p: &Path, v: &mut Verdict) {
    v.raise(
        Class::Delete,
        format!(
            "writes {}, which git, curl or wget read as configuration that can run programs",
            env.roots.shown(p)
        ),
    );
}

/// A path the command creates or touches without removing content (mkdir, touch).
fn create_target(env: &Env, w: &Word, what: &str, v: &mut Verdict) {
    for t in resolve(env, w, false) {
        match t {
            Target::Null => {}
            Target::Unknown(why) => v.raise(
                Class::Delete,
                format!("cannot tell what {what} changes: {why}"),
            ),
            Target::Path(p) if paths::runs_code_on_use(&p) => config_write(env, &p, v),
            Target::Path(p) => match env.roots.place(&p) {
                Place::Scratch => {
                    scratch_ok(env, &p, v);
                }
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

/// Every candidate folder `dest` (or the working folder) is scratch.
fn dest_scratch(env: &Env, dest: Option<&Word>) -> bool {
    match dest {
        Some(w) => {
            let ts = resolve(env, w, false);
            !ts.is_empty()
                && ts
                    .iter()
                    .all(|t| matches!(t, Target::Path(p) if env.roots.place(p) == Place::Scratch))
        }
        None => env.cwd_scratch() == Some(true),
    }
}

// ---- link-capable commands (B7) ----

/// Commands in `src` (substitutions included) that may create links: copying a tree
/// without dereferencing, extracting an archive, git and gh commands that write a
/// working tree.
fn link_commands(src: &str, depth: u8) -> usize {
    if depth > MAX_DEPTH {
        return 0;
    }
    let Ok(p) = lex::parse(src) else { return 0 };
    let mut n: usize = p.subs.iter().map(|s| link_commands(s, depth + 1)).sum();
    for c in &p.commands {
        if let (Some(h), args) = head_and_args(c) {
            if may_link(&command_name(&h.text), args) {
                n += 1;
            }
        }
    }
    n
}

fn may_link(name: &str, args: &[Word]) -> bool {
    let short = |a: &Word, c: char| {
        a.text.starts_with('-') && !a.text.starts_with("--") && a.text[1..].contains(c)
    };
    match name {
        "cp" => {
            let tree = args.iter().any(|a| {
                ['r', 'R', 'a', 'd', 'P'].iter().any(|&c| short(a, c))
                    || matches!(
                        a.text.as_str(),
                        "--recursive" | "--archive" | "--no-dereference"
                    )
            });
            let deref = args
                .iter()
                .any(|a| short(a, 'L') || a.text == "--dereference");
            tree && !deref
        }
        "tar" => args.first().is_some_and(|a| {
            let t = a.text.trim_start_matches('-');
            (!a.text.starts_with("--") && t.contains('x'))
                || args
                    .iter()
                    .any(|a| matches!(a.text.as_str(), "--extract" | "--get"))
        }),
        "unzip" => true,
        "git" => args.iter().any(|a| {
            matches!(
                a.text.as_str(),
                "checkout"
                    | "switch"
                    | "clone"
                    | "pull"
                    | "merge"
                    | "rebase"
                    | "cherry-pick"
                    | "revert"
                    | "reset"
                    | "restore"
                    | "stash"
                    | "am"
                    | "apply"
                    | "worktree"
                    | "submodule"
            )
        }),
        "gh" => args.iter().any(|a| {
            matches!(
                a.text.as_str(),
                "clone" | "checkout" | "download" | "fork" | "sync"
            )
        }),
        _ => false,
    }
}

// ---- writers ----

fn sort(env: &Env, args: &[Word], v: &mut Verdict) {
    const F: Flags = Flags {
        short: "bdfgiMhnRrcCsuVz",
        short_val: "kotST",
        long: &[
            "ignore-leading-blanks",
            "dictionary-order",
            "ignore-case",
            "general-numeric-sort",
            "ignore-nonprinting",
            "month-sort",
            "human-numeric-sort",
            "numeric-sort",
            "random-sort",
            "reverse",
            "version-sort",
            "check",
            "merge",
            "stable",
            "unique",
            "zero-terminated",
            "debug",
            "help",
            "version",
        ],
        long_val: &[
            "key",
            "field-separator",
            "output",
            "buffer-size",
            "temporary-directory",
            "parallel",
            "sort",
            "files0-from",
            "random-source",
            "check",
        ],
        exact: &[],
        num: false,
    };
    let Some(p) = flags_or_delete("sort", &F, args, false, v) else {
        return;
    };
    for o in p.values(&["-o", "--output"]) {
        write_target(env, o, true, v);
    }
    for d in p.values(&["-T", "--temporary-directory"]) {
        create_target(env, d, "sort puts temporary files in", v);
    }
}

fn uniq(env: &Env, args: &[Word], v: &mut Verdict) {
    const F: Flags = Flags {
        short: "cdDiuz",
        short_val: "fsw",
        long: &[
            "count",
            "repeated",
            "all-repeated",
            "unique",
            "ignore-case",
            "zero-terminated",
            "group",
            "help",
            "version",
        ],
        long_val: &[
            "skip-fields",
            "skip-chars",
            "check-chars",
            "all-repeated",
            "group",
        ],
        exact: &[],
        num: false,
    };
    let Some(p) = flags_or_delete("uniq", &F, args, false, v) else {
        return;
    };
    if let Some(out) = p.ops.get(1) {
        write_target(env, out, true, v);
    }
}

fn tee(env: &Env, args: &[Word], v: &mut Verdict) {
    const F: Flags = Flags {
        short: "aip",
        short_val: "",
        long: &[
            "append",
            "ignore-interrupts",
            "output-error",
            "help",
            "version",
        ],
        long_val: &["output-error"],
        exact: &[],
        num: false,
    };
    let Some(p) = flags_or_delete("tee", &F, args, false, v) else {
        return;
    };
    let append = p.has(&["-a", "--append"]);
    for t in p.ops {
        write_target(env, t, !append, v);
    }
}

fn iconv(env: &Env, args: &[Word], v: &mut Verdict) {
    const F: Flags = Flags {
        short: "cls",
        short_val: "fto",
        long: &["list", "silent", "help", "version"],
        long_val: &["from-code", "to-code", "output"],
        exact: &[],
        num: false,
    };
    let Some(p) = flags_or_delete("iconv", &F, args, false, v) else {
        return;
    };
    for o in p.values(&["-o", "--output"]) {
        write_target(env, o, true, v);
    }
}

fn rm(env: &Env, args: &[Word], v: &mut Verdict) {
    const F: Flags = Flags {
        short: "fiIrRdv",
        short_val: "",
        long: &[
            "force",
            "recursive",
            "dir",
            "verbose",
            "one-file-system",
            "no-preserve-root",
            "preserve-root",
            "interactive",
            "help",
            "version",
        ],
        long_val: &["interactive", "preserve-root"],
        exact: &[],
        num: false,
    };
    let Some(p) = flags_or_delete("rm", &F, args, false, v) else {
        return;
    };
    for t in p.ops {
        delete_target(env, t, v);
    }
}

fn rmdir(env: &Env, args: &[Word], v: &mut Verdict) {
    const F: Flags = Flags {
        short: "pv",
        short_val: "",
        long: &[
            "parents",
            "ignore-fail-on-non-empty",
            "verbose",
            "help",
            "version",
        ],
        long_val: &[],
        exact: &[],
        num: false,
    };
    let Some(p) = flags_or_delete("rmdir", &F, args, false, v) else {
        return;
    };
    let parents = p.has(&["-p", "--parents"]);
    for t in p.ops {
        delete_target(env, t, v);
        if parents && t.is_static() {
            // `rmdir -p a/b/c` also removes a/b and a.
            let segs: Vec<&str> = t.text.split(['/', '\\']).collect();
            for n in 1..segs.len() {
                let w = Word {
                    text: segs[..n].join("/"),
                    ..t.clone()
                };
                if !w.text.is_empty() {
                    delete_target(env, &w, v);
                }
            }
        }
    }
}

fn mkdir(env: &Env, args: &[Word], v: &mut Verdict) {
    const F: Flags = Flags {
        short: "pv",
        short_val: "m",
        long: &["parents", "verbose", "help", "version"],
        long_val: &["mode"],
        exact: &[],
        num: false,
    };
    let Some(p) = flags_or_delete("mkdir", &F, args, false, v) else {
        return;
    };
    for t in p.ops {
        create_target(env, t, "mkdir", v);
    }
}

fn touch(env: &Env, args: &[Word], v: &mut Verdict) {
    const F: Flags = Flags {
        short: "acfhm",
        short_val: "drt",
        long: &["no-create", "no-dereference", "help", "version"],
        long_val: &["date", "reference", "time"],
        exact: &[],
        num: false,
    };
    let Some(p) = flags_or_delete("touch", &F, args, false, v) else {
        return;
    };
    for t in p.ops {
        create_target(env, t, "touch", v);
    }
}

fn truncate(env: &Env, args: &[Word], v: &mut Verdict) {
    const F: Flags = Flags {
        short: "co",
        short_val: "sr",
        long: &["no-create", "io-blocks", "help", "version"],
        long_val: &["size", "reference"],
        exact: &[],
        num: false,
    };
    let Some(p) = flags_or_delete("truncate", &F, args, false, v) else {
        return;
    };
    for t in p.ops {
        write_target(env, t, true, v);
    }
}

fn cp(env: &Env, args: &[Word], v: &mut Verdict) {
    if args.iter().any(|a| {
        (a.text.starts_with('-')
            && !a.text.starts_with("--")
            && (a.text[1..].contains('l') || a.text[1..].contains('s')))
            || matches!(a.text.as_str(), "--link" | "--symbolic-link")
    }) {
        v.raise(
            Class::Delete,
            "cp -l/-s makes links, which can alias a file outside runs/",
        );
        return;
    }
    const F: Flags = Flags {
        short: "abdfHiLnpPrRTuvx",
        short_val: "tS",
        long: &[
            "archive",
            "attributes-only",
            "backup",
            "copy-contents",
            "debug",
            "dereference",
            "force",
            "interactive",
            "no-dereference",
            "no-clobber",
            "no-target-directory",
            "one-file-system",
            "preserve",
            "recursive",
            "remove-destination",
            "strip-trailing-slashes",
            "update",
            "verbose",
            "help",
            "version",
        ],
        long_val: &[
            "backup",
            "preserve",
            "no-preserve",
            "reflink",
            "sparse",
            "suffix",
            "target-directory",
            "update",
        ],
        exact: &[],
        num: false,
    };
    let Some(p) = flags_or_delete("cp", &F, args, false, v) else {
        return;
    };
    move_or_copy(env, &p, false, v);
}

fn mv(env: &Env, args: &[Word], v: &mut Verdict) {
    const F: Flags = Flags {
        short: "bfinuvT",
        short_val: "tS",
        long: &[
            "backup",
            "force",
            "interactive",
            "no-clobber",
            "no-target-directory",
            "strip-trailing-slashes",
            "update",
            "verbose",
            "help",
            "version",
        ],
        long_val: &["backup", "suffix", "target-directory", "update"],
        exact: &[],
        num: false,
    };
    let Some(p) = flags_or_delete("mv", &F, args, false, v) else {
        return;
    };
    move_or_copy(env, &p, true, v);
}

fn move_or_copy(env: &Env, p: &Parsed, is_move: bool, v: &mut Verdict) {
    let what = if is_move { "mv" } else { "cp" };
    let no_clobber = p.has(&["-n", "--no-clobber"]);
    let target_dir = p.value(&["-t", "--target-directory"]).cloned();
    let (srcs, dst): (Vec<&Word>, Word) = match target_dir {
        Some(d) => (p.ops.clone(), d),
        None => {
            let Some((last, rest)) = p.ops.split_last() else {
                return;
            };
            (rest.to_vec(), (*last).clone())
        }
    };
    let dst_t = resolve(env, &dst, false);
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
        for st in resolve(env, s, true) {
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
            if paths::runs_code_on_use(&fin) {
                config_write(env, &fin, v);
            }
            if is_move && (sp == Place::Bookkeeping || env.roots.runs.starts_with(&src)) {
                v.raise(
                    Class::Delete,
                    format!("moves the run records at {}", env.roots.shown(&src)),
                );
            }
            if is_move && sp == Place::Outside {
                // From outside into runs/ it could then be deleted without asking; from
                // outside to outside it is moved away from where it was (S4, D-520).
                v.raise(
                    Class::Delete,
                    format!("moves {} away (outside runs/)", env.roots.shown(&src)),
                );
            }
            match dp {
                // Into runs/: a move from outside was raised above; a copy only reads.
                Place::Scratch => {
                    scratch_ok(env, &fin, v);
                }
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
        }
    }
}

fn sed(env: &Env, args: &[Word], v: &mut Verdict) {
    let mut in_place = false;
    let mut suffix = String::new();
    let mut scripts: Vec<&Word> = Vec::new();
    let mut files: Vec<&Word> = Vec::new();
    let mut explicit_script = false;
    let mut rest = false;
    let mut k = 0;
    let refuse = |v: &mut Verdict, why: String| {
        v.raise(Class::Delete, format!("sed: {why}"));
    };
    while k < args.len() {
        let a = &args[k];
        let t = a.text.as_str();
        k += 1;
        if rest || !t.starts_with('-') || t == "-" {
            if may_be_option(a) && !rest {
                return refuse(v, "an argument decided at run time could be -i".into());
            }
            if !explicit_script && scripts.is_empty() {
                scripts.push(a);
            } else {
                files.push(a);
            }
            continue;
        }
        if t == "--" {
            rest = true;
            continue;
        }
        if let Some(long) = t.strip_prefix("--") {
            let (name, val) = match long.split_once('=') {
                Some((n, x)) => (n, Some(x)),
                None => (long, None),
            };
            match (name, val) {
                (
                    "quiet" | "silent" | "regexp-extended" | "separate" | "null-data"
                    | "unbuffered" | "posix" | "debug" | "follow-symlinks" | "help" | "version",
                    None,
                ) => {}
                ("in-place", s) => {
                    in_place = true;
                    suffix = s.unwrap_or("").to_string();
                }
                ("expression", Some(s)) => {
                    explicit_script = true;
                    if sed_writes(s) {
                        return refuse(v, "the script writes files or runs commands".into());
                    }
                }
                ("expression", None) => {
                    explicit_script = true;
                    match args.get(k) {
                        Some(s) => {
                            scripts.push(s);
                            k += 1;
                        }
                        None => return refuse(v, "--expression without a script".into()),
                    }
                }
                ("line-length", Some(_)) => {}
                _ => {
                    return refuse(
                        v,
                        format!("{} is not a known option", crate::text::one_line(t, 40)),
                    )
                }
            }
            continue;
        }
        let chars: Vec<char> = t[1..].chars().collect();
        let mut j = 0;
        while j < chars.len() {
            match chars[j] {
                'n' | 'r' | 'E' | 's' | 'z' | 'u' => j += 1,
                'i' => {
                    in_place = true;
                    suffix = chars[j + 1..].iter().collect();
                    break;
                }
                c @ ('e' | 'l') => {
                    let attached: String = chars[j + 1..].iter().collect();
                    let val = if attached.is_empty() {
                        let Some(s) = args.get(k) else {
                            return refuse(v, format!("-{c} without a value"));
                        };
                        k += 1;
                        s.clone()
                    } else {
                        Word {
                            text: attached,
                            ..a.clone()
                        }
                    };
                    if c == 'e' {
                        explicit_script = true;
                        if !val.is_static() || sed_writes(&val.text) {
                            return refuse(v, "the script writes files or runs commands".into());
                        }
                    }
                    break;
                }
                'f' => return refuse(v, "-f runs a script file the classifier cannot see".into()),
                c => return refuse(v, format!("-{c} is not a known option")),
            }
        }
    }
    for s in scripts {
        if !s.is_static() || sed_writes(&s.text) {
            return refuse(v, "the script writes files or runs commands".into());
        }
    }
    if in_place {
        if suffix.contains(['/', '\\', '*']) {
            return refuse(
                v,
                "a backup suffix with a folder or * writes elsewhere".into(),
            );
        }
        for f in files {
            write_target(env, f, true, v);
            if !suffix.is_empty() {
                let backup = Word {
                    text: format!("{}{suffix}", f.text),
                    ..f.clone()
                };
                write_target(env, &backup, true, v);
            }
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

/// awk with only `-F` and `-v`, and a literal program that does not write (`print >`,
/// `|`), run programs (`system`) or load code (`@load`, `@include`).
fn awk(args: &[Word], v: &mut Verdict) {
    let mut k = 0;
    while k < args.len() {
        let a = &args[k];
        let t = a.text.as_str();
        if may_be_option(a) {
            // Before the program a run-time word is an option or the program itself.
            v.raise(
                Class::Delete,
                "awk: the program or an option is decided at run time",
            );
            return;
        }
        if t == "--" {
            k += 1;
            break;
        }
        if t == "-F" || t == "-v" {
            k += 2;
            continue;
        }
        if (t.starts_with("-F") || t.starts_with("-v")) && t.len() > 2 {
            k += 1;
            continue;
        }
        if t.starts_with('-') && t.len() > 1 {
            v.raise(
                Class::Delete,
                format!(
                    "awk: {} is not an option the classifier knows to be safe",
                    crate::text::one_line(t, 40)
                ),
            );
            return;
        }
        break;
    }
    let Some(prog) = args.get(k) else { return };
    let p = prog.text.as_str();
    let prints_out = p.match_indices("print").any(|(idx, _)| {
        let rest = &p[idx..];
        let stmt = rest.split([';', '}', '\n']).next().unwrap_or(rest);
        stmt.contains('>') || stmt.contains('|')
    });
    if !prog.is_static() || p.contains("system") || p.contains('|') || p.contains('@') || prints_out
    {
        v.raise(
            Class::Delete,
            "the awk program writes files, runs commands or loads code",
        );
    }
}

fn gzip(env: &Env, name: &str, args: &[Word], v: &mut Verdict) {
    const F: Flags = Flags {
        short: "cdfhklLnNqrtvz0123456789e",
        short_val: "ST",
        long: &[
            "stdout",
            "to-stdout",
            "decompress",
            "uncompress",
            "force",
            "keep",
            "list",
            "no-name",
            "name",
            "quiet",
            "recursive",
            "test",
            "verbose",
            "fast",
            "best",
            "compress",
            "help",
            "version",
        ],
        long_val: &["suffix", "threads"],
        exact: &[],
        num: false,
    };
    let Some(p) = flags_or_delete(name, &F, args, false, v) else {
        return;
    };
    if p.has(&[
        "-c",
        "-l",
        "-t",
        "--stdout",
        "--to-stdout",
        "--list",
        "--test",
    ]) {
        return;
    }
    let keep = p.has(&["-k", "--keep"]);
    let force = p.has(&["-f", "--force"]);
    for t in p.ops {
        if keep && !force {
            create_target(env, t, name, v);
        } else {
            // The input is removed (or, with -f, an existing output replaced).
            delete_target(env, t, v);
        }
    }
}

fn tar(env: &Env, args: &[Word], v: &mut Verdict) {
    const F: Flags = Flags {
        short: "cxtruvzjJakopOmS",
        short_val: "fCX",
        long: &[
            "create",
            "extract",
            "get",
            "list",
            "append",
            "update",
            "verbose",
            "gzip",
            "gunzip",
            "bzip2",
            "xz",
            "auto-compress",
            "keep-old-files",
            "skip-old-files",
            "no-same-owner",
            "no-same-permissions",
            "preserve-permissions",
            "to-stdout",
            "touch",
            "wildcards",
            "no-wildcards",
            "anchored",
            "no-anchored",
            "recursion",
            "no-recursion",
            "ignore-case",
            "totals",
            "sparse",
            "one-file-system",
            "exclude-vcs",
            "exclude-vcs-ignores",
            "numeric-owner",
            "help",
            "version",
        ],
        long_val: &[
            "file",
            "directory",
            "exclude",
            "exclude-from",
            "strip-components",
            "owner",
            "group",
            "mode",
            "mtime",
            "format",
        ],
        exact: &[],
        num: false,
    };
    // Old style `tar czf out.tgz dir`: the first word is a cluster without the dash.
    let mut words: Vec<Word> = args.to_vec();
    if let Some(first) = words.first_mut() {
        if first.is_static()
            && !first.text.starts_with('-')
            && !first.text.is_empty()
            && first.text.chars().all(|c| c.is_ascii_alphabetic())
        {
            let vals: Vec<usize> = first
                .text
                .char_indices()
                .filter(|(_, c)| F.short_val.contains(*c))
                .map(|(i, _)| i)
                .collect();
            if vals.len() > 1 || vals.first().is_some_and(|&i| i + 1 != first.text.len()) {
                v.raise(
                    Class::Delete,
                    "old-style tar options whose values the classifier cannot match",
                );
                return;
            }
            first.text = format!("-{}", first.text);
        }
    }
    let Some(p) = flags_or_delete("tar", &F, &words, false, v) else {
        return;
    };
    let create = p.has(&["-c", "--create"]);
    let extract = p.has(&["-x", "--extract", "--get"]);
    let list = p.has(&["-t", "--list"]);
    let append = p.has(&["-r", "--append", "-u", "--update"]);
    if [create, extract, list, append]
        .iter()
        .filter(|b| **b)
        .count()
        != 1
    {
        v.raise(Class::Delete, "tar mode not recognised");
        return;
    }
    if list {
        return;
    }
    let archive = p.value(&["-f", "--file"]);
    if create || append {
        if let Some(f) = archive.filter(|f| f.text != "-") {
            write_target(env, f, create, v);
        }
        return;
    }
    if p.has(&["-O", "--to-stdout"]) {
        return;
    }
    let dest = p.value(&["-C", "--directory"]);
    if !dest_scratch(env, dest) {
        v.raise(Class::Delete, "tar -x can replace files outside runs/");
    }
}

fn unzip(env: &Env, args: &[Word], v: &mut Verdict) {
    const F: Flags = Flags {
        short: "lvtZoqnjaCLcz",
        short_val: "dP",
        long: &["help", "version"],
        long_val: &[],
        exact: &[],
        num: false,
    };
    let Some(p) = flags_or_delete("unzip", &F, args, false, v) else {
        return;
    };
    if p.has(&["-l", "-v", "-t", "-Z", "-c", "-z"]) {
        return;
    }
    if !dest_scratch(env, p.value(&["-d"])) {
        v.raise(Class::Delete, "unzip can replace files outside runs/");
    }
}

fn zip(env: &Env, args: &[Word], v: &mut Verdict) {
    if args.iter().any(|a| {
        (a.text.starts_with('-') && !a.text.starts_with("--") && a.text[1..].contains('m'))
            || a.text == "--move"
    }) {
        v.raise(Class::Delete, "zip -m deletes the files it adds");
        return;
    }
    const F: Flags = Flags {
        short: "rqjyDvX0123456789",
        short_val: "",
        long: &[
            "recurse-paths",
            "quiet",
            "junk-paths",
            "symlinks",
            "no-dir-entries",
            "verbose",
            "no-extra",
            "help",
            "version",
        ],
        long_val: &[],
        exact: &[],
        num: false,
    };
    let Some(p) = flags_or_delete("zip", &F, args, false, v) else {
        return;
    };
    if let Some(archive) = p.ops.first().filter(|a| a.text != "-") {
        // zip adds .zip to a name without an extension.
        let file = archive.text.rsplit(['/', '\\']).next().unwrap_or("");
        let w = if file.contains('.') {
            (*archive).clone()
        } else {
            Word {
                text: format!("{}.zip", archive.text),
                ..(*archive).clone()
            }
        };
        write_target(env, &w, true, v);
    }
}

fn curl(env: &Env, args: &[Word], v: &mut Verdict) {
    if args.iter().any(|a| {
        matches!(a.text.as_str(), "-K" | "--config" | "-Q" | "--quote")
            || a.text.starts_with("--config=")
    }) {
        v.raise(
            Class::Delete,
            "curl --config/--quote runs options or commands the classifier cannot see",
        );
        return;
    }
    const F: Flags = Flags {
        short: "sSLfIikvGOJNqRZ#0146nlj",
        short_val: "XdHAeoumFTbcDrxyYzECUw",
        long: &[
            "get",
            "head",
            "location",
            "location-trusted",
            "silent",
            "show-error",
            "fail",
            "fail-with-body",
            "fail-early",
            "include",
            "insecure",
            "verbose",
            "compressed",
            "http1.0",
            "http1.1",
            "http2",
            "http2-prior-knowledge",
            "http3",
            "ipv4",
            "ipv6",
            "no-progress-meter",
            "progress-bar",
            "remote-name",
            "remote-name-all",
            "remote-header-name",
            "create-dirs",
            "globoff",
            "path-as-is",
            "raw",
            "no-buffer",
            "tcp-nodelay",
            "parallel",
            "netrc",
            "netrc-optional",
            "basic",
            "digest",
            "ntlm",
            "negotiate",
            "anyauth",
            "no-keepalive",
            "no-sessionid",
            "disable",
            "remote-time",
            "junk-session-cookies",
            "ssl-reqd",
            "tlsv1.2",
            "tlsv1.3",
            "list-only",
            "post301",
            "post302",
            "post303",
            "retry-connrefused",
            "retry-all-errors",
            "styled-output",
            "no-styled-output",
            "help",
            "version",
            "manual",
        ],
        long_val: &[
            "request",
            "data",
            "data-raw",
            "data-binary",
            "data-urlencode",
            "data-ascii",
            "json",
            "form",
            "form-string",
            "upload-file",
            "output",
            "cookie-jar",
            "dump-header",
            "trace",
            "trace-ascii",
            "stderr",
            "etag-save",
            "etag-compare",
            "libcurl",
            "hsts",
            "alt-svc",
            "output-dir",
            "header",
            "user-agent",
            "user",
            "url",
            "referer",
            "cookie",
            "max-time",
            "connect-timeout",
            "proxy",
            "cert",
            "key",
            "cacert",
            "capath",
            "retry",
            "retry-delay",
            "retry-max-time",
            "write-out",
            "range",
            "resolve",
            "connect-to",
            "max-redirs",
            "oauth2-bearer",
            "netrc-file",
            "limit-rate",
            "max-filesize",
            "noproxy",
            "parallel-max",
            "time-cond",
            "proxy-user",
            "keepalive-time",
            "continue-at",
            "speed-limit",
            "speed-time",
            "interface",
            "dns-servers",
            "aws-sigv4",
            "cert-type",
            "key-type",
            "pass",
            "ciphers",
            "expect100-timeout",
            "local-port",
            "tls-max",
            "proxy-header",
        ],
        exact: &[],
        num: false,
    };
    let Some(p) = flags_or_delete("curl", &F, args, false, v) else {
        return;
    };
    for w in p.values(&["-w", "--write-out"]) {
        if w.text.contains("%output{") || !w.is_static() {
            v.raise(Class::Delete, "curl --write-out %output{} writes a file");
        }
    }
    let upload = p.has(&["-T", "--upload-file"]);
    let sends = upload
        || p.has(&[
            "-d",
            "--data",
            "--data-raw",
            "--data-binary",
            "--data-urlencode",
            "--data-ascii",
            "--json",
            "-F",
            "--form",
            "--form-string",
        ]);
    let mut urls: Vec<&Word> = p.ops.clone();
    urls.extend(p.values(&["--url"]));
    if upload
        && urls
            .iter()
            .any(|u| !u.is_static() || u.text.to_ascii_lowercase().starts_with("file:"))
    {
        v.raise(Class::Delete, "curl -T to a file: URL writes a local file");
    }
    let method = p
        .value(&["-X", "--request"])
        .map(|w| w.text.to_ascii_uppercase())
        .unwrap_or_else(|| {
            if sends && !p.has(&["-G", "--get"]) {
                "POST".into()
            } else {
                "GET".into()
            }
        });
    match method.as_str() {
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
    let output_dir = p.value(&["--output-dir"]);
    for w in p.values(&[
        "-o",
        "--output",
        "-c",
        "--cookie-jar",
        "-D",
        "--dump-header",
        "--trace",
        "--trace-ascii",
        "--stderr",
        "--etag-save",
        "--libcurl",
        "--hsts",
        "--alt-svc",
    ]) {
        if w.text == "-" && w.is_static() {
            continue;
        }
        // --output-dir prefixes relative -o names.
        let w = match output_dir {
            Some(d) if !Path::new(&paths::msys_drive(&w.text)).is_absolute() => Word {
                text: format!("{}/{}", d.text, w.text),
                dynamic: d.dynamic || w.dynamic,
                split: d.split || w.split,
                glob: d.glob || w.glob,
                brace: d.brace || w.brace,
                tilde: d.tilde,
                quoted: true,
            },
            _ => w.clone(),
        };
        write_target(env, &w, true, v);
    }
    // curl -O replaces a file of the same name: outside runs/ that is Delete.
    if p.has(&["-O", "--remote-name", "--remote-name-all"]) && !dest_scratch(env, output_dir) {
        v.raise(Class::Delete, "curl -O may replace a file outside runs/");
    }
}

fn wget(env: &Env, args: &[Word], v: &mut Verdict) {
    const F: Flags = Flags {
        short: "qvcS46dhV",
        short_val: "OPoaUtTwiQ",
        long: &[
            "quiet",
            "verbose",
            "no-verbose",
            "continue",
            "server-response",
            "spider",
            "no-clobber",
            "no-directories",
            "no-host-directories",
            "no-parent",
            "no-check-certificate",
            "content-disposition",
            "show-progress",
            "inet4-only",
            "inet6-only",
            "no-cookies",
            "https-only",
            "no-cache",
            "ignore-length",
            "debug",
            "keep-session-cookies",
            "help",
            "version",
        ],
        long_val: &[
            "output-document",
            "directory-prefix",
            "output-file",
            "append-output",
            "user-agent",
            "header",
            "tries",
            "timeout",
            "wait",
            "waitretry",
            "limit-rate",
            "post-data",
            "post-file",
            "body-data",
            "body-file",
            "method",
            "user",
            "password",
            "load-cookies",
            "save-cookies",
            "max-redirect",
            "read-timeout",
            "connect-timeout",
            "dns-timeout",
            "input-file",
            "referer",
            "quota",
            "progress",
        ],
        exact: &["-nv", "-nc", "-nd", "-nH", "-np"],
        num: false,
    };
    let Some(p) = flags_or_delete("wget", &F, args, false, v) else {
        return;
    };
    if p.has(&["--post-data", "--post-file", "--body-data", "--body-file"]) {
        v.raise(Class::Submit, "wget sends data");
    }
    if let Some(m) = p.value(&["--method"]) {
        match m.text.to_ascii_uppercase().as_str() {
            "GET" | "HEAD" | "OPTIONS" if m.is_static() => {}
            "DELETE" => v.raise(Class::Delete, "wget sends an HTTP DELETE"),
            _ => v.raise(Class::Submit, "wget sends data"),
        }
    }
    for w in p.values(&["-o", "--output-file", "--save-cookies"]) {
        write_target(env, w, true, v);
    }
    for w in p.values(&["-a", "--append-output"]) {
        write_target(env, w, false, v);
    }
    match (
        p.value(&["-O", "--output-document"]),
        p.value(&["-P", "--directory-prefix"]),
    ) {
        (Some(o), _) if o.text == "-" && o.is_static() => {}
        (Some(o), _) => write_target(env, o, true, v),
        (None, Some(d)) => create_target(env, d, "wget downloads into", v),
        (None, None) => {
            let bookkeeping = env
                .cwds
                .as_ref()
                .is_some_and(|c| c.iter().any(|d| env.roots.place(d) == Place::Bookkeeping));
            match env.cwd_scratch() {
                Some(true) => {}
                Some(false) if !bookkeeping => {
                    v.raise(Class::Fill, "wget saves a file in the working folder")
                }
                _ => v.raise(
                    Class::Delete,
                    "wget saves a file where the classifier cannot tell",
                ),
            }
        }
    }
}

fn find(args: &[Word], v: &mut Verdict) {
    const NO_ARG: &[&str] = &[
        "-print",
        "-print0",
        "-ls",
        "-prune",
        "-quit",
        "-true",
        "-false",
        "-empty",
        "-readable",
        "-writable",
        "-executable",
        "-depth",
        "-xdev",
        "-mount",
        "-follow",
        "-nowarn",
        "-warn",
        "-daystart",
        "-noleaf",
        "-ignore_readdir_race",
        "-o",
        "-or",
        "-a",
        "-and",
        "-not",
        "!",
        "(",
        ")",
        ",",
        "-L",
        "-H",
        "-P",
        "-nouser",
        "-nogroup",
    ];
    const ONE_ARG: &[&str] = &[
        "-name",
        "-iname",
        "-path",
        "-ipath",
        "-wholename",
        "-iwholename",
        "-regex",
        "-iregex",
        "-regextype",
        "-type",
        "-xtype",
        "-maxdepth",
        "-mindepth",
        "-mtime",
        "-mmin",
        "-atime",
        "-amin",
        "-ctime",
        "-cmin",
        "-newer",
        "-anewer",
        "-cnewer",
        "-size",
        "-perm",
        "-user",
        "-group",
        "-uid",
        "-gid",
        "-links",
        "-inum",
        "-samefile",
        "-used",
        "-printf",
        "-fstype",
        "-lname",
        "-ilname",
        "-newermt",
        "-newerct",
        "-newerat",
    ];
    let mut k = 0;
    while k < args.len() {
        let a = &args[k];
        let t = a.text.as_str();
        k += 1;
        if may_be_option(a) {
            v.raise(
                Class::Delete,
                "find with a word decided at run time (it could become -exec or -delete)",
            );
            return;
        }
        if matches!(
            t,
            "-exec"
                | "-execdir"
                | "-ok"
                | "-okdir"
                | "-delete"
                | "-fprint"
                | "-fprint0"
                | "-fprintf"
                | "-fls"
        ) {
            v.raise(
                Class::Delete,
                format!("find {t} runs a program, deletes or writes files"),
            );
            return;
        }
        if NO_ARG.contains(&t) {
            continue;
        }
        if ONE_ARG.contains(&t) {
            k += 1;
            continue;
        }
        if t.starts_with('-') {
            v.raise(
                Class::Delete,
                format!(
                    "find {} is not a test the classifier knows to be safe",
                    crate::text::one_line(t, 40)
                ),
            );
            return;
        }
        // A starting point: read only.
    }
}

// ---- git ----

const GIT_LOG: Flags = Flags {
    short: "psuzRDWaiEFPwbmctrgqMCBNh",
    short_val: "nSGLUOl",
    long: &[
        "oneline",
        "graph",
        "decorate",
        "no-decorate",
        "all",
        "branches",
        "tags",
        "remotes",
        "abbrev-commit",
        "no-abbrev-commit",
        "no-abbrev",
        "relative-date",
        "follow",
        "no-follow",
        "no-merges",
        "merges",
        "first-parent",
        "reverse",
        "cached",
        "staged",
        "merge-base",
        "no-index",
        "stat",
        "shortstat",
        "numstat",
        "name-only",
        "name-status",
        "summary",
        "raw",
        "patch",
        "no-patch",
        "patch-with-stat",
        "patch-with-raw",
        "full-index",
        "binary",
        "no-color",
        "color",
        "color-words",
        "color-moved",
        "word-diff",
        "minimal",
        "patience",
        "histogram",
        "ignore-all-space",
        "ignore-space-change",
        "ignore-space-at-eol",
        "ignore-blank-lines",
        "ignore-cr-at-eol",
        "ignore-submodules",
        "exit-code",
        "quiet",
        "check",
        "no-ext-diff",
        "no-textconv",
        "no-prefix",
        "default-prefix",
        "no-renames",
        "find-renames",
        "find-copies",
        "find-copies-harder",
        "irreversible-delete",
        "text",
        "dirstat",
        "cumulative",
        "compact-summary",
        "full-history",
        "ancestry-path",
        "simplify-by-decoration",
        "simplify-merges",
        "topo-order",
        "date-order",
        "author-date-order",
        "left-right",
        "left-only",
        "right-only",
        "cherry-pick",
        "cherry-mark",
        "cherry",
        "boundary",
        "not",
        "show-pulls",
        "all-match",
        "invert-grep",
        "regexp-ignore-case",
        "extended-regexp",
        "fixed-strings",
        "perl-regexp",
        "basic-regexp",
        "pickaxe-all",
        "pickaxe-regex",
        "no-walk",
        "do-walk",
        "count",
        "parents",
        "children",
        "source",
        "use-mailmap",
        "no-use-mailmap",
        "mailmap",
        "full-diff",
        "log-size",
        "no-notes",
        "show-notes",
        "standard-notes",
        "no-standard-notes",
        "merge",
        "remerge-diff",
        "combined",
        "cc",
        "dense-combined",
        "relative",
        "no-relative",
        "function-context",
        "indent-heuristic",
        "no-indent-heuristic",
        "walk-reflogs",
        "objects",
        "objects-edge",
        "numbered",
        "email",
        "committer",
        "stdout",
        "no-numbered",
        "cover-letter",
        "signoff",
        "keep-index",
        "include-untracked",
        "only-untracked",
        "index",
        "help",
    ],
    long_val: &[
        "format",
        "pretty",
        "decorate",
        "abbrev",
        "date",
        "since",
        "until",
        "after",
        "before",
        "author",
        "committer",
        "grep",
        "max-count",
        "skip",
        "min-parents",
        "max-parents",
        "stat",
        "dirstat",
        "diff-filter",
        "unified",
        "src-prefix",
        "dst-prefix",
        "line-prefix",
        "word-diff",
        "word-diff-regex",
        "color",
        "color-moved",
        "color-moved-ws",
        "relative",
        "find-renames",
        "find-copies",
        "break-rewrites",
        "inter-hunk-context",
        "diff-algorithm",
        "anchored",
        "ignore-matching-lines",
        "ignore-submodules",
        "submodule",
        "notes",
        "show-notes",
        "decorate-refs",
        "decorate-refs-exclude",
        "exclude",
        "glob",
        "branches",
        "tags",
        "remotes",
        "diff-merges",
        "encoding",
        "max-age",
        "min-age",
        "output-directory",
        "subject-prefix",
        "start-number",
        "suffix",
        "base",
        "group",
        "creation-factor",
    ],
    exact: &[],
    num: true,
};

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
        || key.starts_with("include")
        || key.ends_with("command")
        || key.ends_with(".cmd")
        || key.ends_with("textconv")
        || key.contains("hook")
        || key.contains("editor")
        || key.contains("pager")
        || key.contains("program")
        || key.contains("ssh")
        || key.contains("proxy")
        || key.contains("askpass")
}

fn git(outer: &Env, args: &[Word], v: &mut Verdict) {
    // `git -C dir` changes the folder for this command only.
    let mut env = outer.clone();
    env.cd_replaces = true;
    let env = &mut env;
    let mut k = 0;
    while k < args.len() && args[k].text.starts_with('-') {
        let a = &args[k];
        if may_be_option(a) {
            break;
        }
        match a.text.as_str() {
            "-C" => {
                match args.get(k + 1) {
                    Some(d) => cd(env, std::slice::from_ref(d)),
                    None => env.cwds = None,
                }
                k += 2;
            }
            "--no-pager"
            | "--no-optional-locks"
            | "--literal-pathspecs"
            | "--glob-pathspecs"
            | "--noglob-pathspecs"
            | "--icase-pathspecs"
            | "--no-replace-objects" => k += 1,
            "--version" | "--help" if k + 1 == args.len() => return,
            t => {
                v.raise(
                    Class::Delete,
                    format!(
                        "git {}: -c, --config-env, --exec-path, --git-dir, --work-tree and other global options can run programs or point git elsewhere",
                        crate::text::one_line(t, 40)
                    ),
                );
                return;
            }
        }
    }
    let Some(sub) = args.get(k) else { return };
    if !sub.is_static() || sub.text.starts_with('-') {
        v.raise(Class::Delete, "git subcommand decided at run time");
        return;
    }
    let s = sub.text.as_str();
    let rest = &args[k + 1..];
    let first_op = rest
        .iter()
        .find(|a| !a.text.starts_with('-'))
        .map(|a| a.text.as_str());
    let parse =
        |f: &Flags, v: &mut Verdict| flags_or_delete(&format!("git {s}"), f, rest, false, v);
    match s {
        "status" => {
            const F: Flags = Flags {
                short: "sbvzu",
                short_val: "",
                long: &[
                    "short",
                    "branch",
                    "long",
                    "verbose",
                    "show-stash",
                    "ahead-behind",
                    "no-ahead-behind",
                    "untracked-files",
                    "ignored",
                    "ignore-submodules",
                    "column",
                    "no-column",
                    "renames",
                    "no-renames",
                    "porcelain",
                    "null",
                    "find-renames",
                ],
                long_val: &[
                    "porcelain",
                    "untracked-files",
                    "ignored",
                    "ignore-submodules",
                    "column",
                    "find-renames",
                ],
                exact: &["-uno", "-uall", "-unormal"],
                num: false,
            };
            parse(&F, v);
        }
        "log" | "show" | "diff" | "whatchanged" | "shortlog" | "rev-list" | "range-diff" => {
            parse(&GIT_LOG, v);
        }
        "format-patch" => {
            if let Some(p) = parse(&GIT_LOG, v) {
                if let Some(d) = p.value(&["--output-directory"]) {
                    create_target(env, d, "git format-patch -o", v);
                }
                if !p.has(&["--stdout"]) {
                    v.raise(Class::Fill, "git format-patch writes patch files");
                }
            }
        }
        "rev-parse" => {
            // rev-parse only prints; its options take no file or program.
            if rest.iter().any(may_be_option) {
                v.raise(Class::Delete, "git rev-parse with run-time arguments");
            }
        }
        "ls-files" => {
            const F: Flags = Flags {
                short: "cdmoiksuvtfzeI",
                short_val: "xX",
                long: &[
                    "cached",
                    "deleted",
                    "modified",
                    "others",
                    "ignored",
                    "stage",
                    "unmerged",
                    "killed",
                    "directory",
                    "no-empty-directory",
                    "eol",
                    "full-name",
                    "recurse-submodules",
                    "abbrev",
                    "error-unmatch",
                    "exclude-standard",
                    "deduplicate",
                    "sparse",
                    "resolve-undo",
                    "debug",
                ],
                long_val: &[
                    "exclude",
                    "exclude-from",
                    "exclude-per-directory",
                    "with-tree",
                    "abbrev",
                    "format",
                ],
                exact: &[],
                num: false,
            };
            parse(&F, v);
        }
        "ls-tree" => {
            const F: Flags = Flags {
                short: "rdtlz",
                short_val: "",
                long: &[
                    "name-only",
                    "name-status",
                    "object-only",
                    "full-name",
                    "full-tree",
                    "long",
                ],
                long_val: &["abbrev", "format"],
                exact: &[],
                num: false,
            };
            parse(&F, v);
        }
        "ls-remote" => {
            const F: Flags = Flags {
                short: "htqb",
                short_val: "",
                long: &[
                    "heads",
                    "tags",
                    "refs",
                    "quiet",
                    "exit-code",
                    "get-url",
                    "symref",
                    "branches",
                ],
                long_val: &["sort", "server-option"],
                exact: &[],
                num: false,
            };
            parse(&F, v);
        }
        "cat-file" => {
            // No --textconv / --filters (they run drivers).
            const F: Flags = Flags {
                short: "tsep",
                short_val: "",
                long: &[
                    "batch",
                    "batch-check",
                    "batch-all-objects",
                    "batch-command",
                    "buffer",
                    "follow-symlinks",
                    "unordered",
                    "allow-unknown-type",
                    "mailmap",
                    "use-mailmap",
                ],
                long_val: &["batch", "batch-check"],
                exact: &[],
                num: false,
            };
            parse(&F, v);
        }
        "blame" => {
            const F: Flags = Flags {
                short: "blftsenwcpMC",
                short_val: "LS",
                long: &[
                    "porcelain",
                    "line-porcelain",
                    "incremental",
                    "root",
                    "show-stats",
                    "show-name",
                    "show-number",
                    "show-email",
                    "minimal",
                    "first-parent",
                    "progress",
                    "no-progress",
                    "score-debug",
                ],
                long_val: &[
                    "contents",
                    "date",
                    "encoding",
                    "ignore-rev",
                    "ignore-revs-file",
                    "abbrev",
                    "reverse",
                ],
                exact: &[],
                num: true,
            };
            parse(&F, v);
        }
        "grep" => {
            // No -O / --open-files-in-pager, no --textconv.
            const F: Flags = Flags {
                short: "ivwcnlLhHEFPrsezaIoq",
                short_val: "efABCm",
                long: &[
                    "cached",
                    "untracked",
                    "no-index",
                    "recurse-submodules",
                    "text",
                    "ignore-case",
                    "word-regexp",
                    "invert-match",
                    "full-name",
                    "extended-regexp",
                    "basic-regexp",
                    "fixed-strings",
                    "perl-regexp",
                    "line-number",
                    "column",
                    "files-with-matches",
                    "name-only",
                    "files-without-match",
                    "null",
                    "count",
                    "break",
                    "heading",
                    "show-function",
                    "function-context",
                    "all-match",
                    "quiet",
                    "exclude-standard",
                    "no-exclude-standard",
                    "only-matching",
                    "no-textconv",
                ],
                long_val: &[
                    "max-depth",
                    "threads",
                    "context",
                    "after-context",
                    "before-context",
                    "color",
                    "max-count",
                ],
                exact: &[],
                num: true,
            };
            parse(&F, v);
        }
        "show-ref" | "for-each-ref" | "describe" | "name-rev" | "merge-base" | "count-objects"
        | "check-ignore" | "var" | "version" => {
            const F: Flags = Flags {
                short: "dqsvHnz",
                short_val: "",
                long: &[
                    "head",
                    "heads",
                    "tags",
                    "branches",
                    "verify",
                    "dereference",
                    "hash",
                    "abbrev",
                    "exists",
                    "shell",
                    "perl",
                    "python",
                    "tcl",
                    "ignore-case",
                    "omit-empty",
                    "all",
                    "contains",
                    "long",
                    "always",
                    "first-parent",
                    "exact-match",
                    "debug",
                    "dirty",
                    "broken",
                    "name-only",
                    "annotate-stdin",
                    "undefined",
                    "octopus",
                    "independent",
                    "is-ancestor",
                    "fork-point",
                    "verbose",
                    "human-readable",
                    "quiet",
                    "non-matching",
                    "no-index",
                    "build-options",
                ],
                long_val: &[
                    "format",
                    "sort",
                    "count",
                    "points-at",
                    "merged",
                    "no-merged",
                    "contains",
                    "no-contains",
                    "exclude",
                    "abbrev",
                    "candidates",
                    "match",
                    "dirty",
                    "broken",
                    "refs",
                    "hash",
                ],
                exact: &[],
                num: false,
            };
            parse(&F, v);
        }
        "branch" => {
            const F: Flags = Flags {
                short: "avrldDmMcCfq",
                short_val: "u",
                long: &[
                    "all",
                    "remotes",
                    "verbose",
                    "list",
                    "show-current",
                    "delete",
                    "move",
                    "copy",
                    "force",
                    "quiet",
                    "track",
                    "no-track",
                    "unset-upstream",
                    "ignore-case",
                    "omit-empty",
                    "create-reflog",
                    "no-column",
                    "column",
                ],
                long_val: &[
                    "contains",
                    "no-contains",
                    "merged",
                    "no-merged",
                    "sort",
                    "format",
                    "column",
                    "color",
                    "points-at",
                    "set-upstream-to",
                    "abbrev",
                    "track",
                ],
                exact: &["-vv"],
                num: false,
            };
            let Some(p) = parse(&F, v) else { return };
            let list = p.has(&["-l", "--list", "--show-current"]);
            if p.has(&["-d", "-D", "--delete"]) || (p.has(&["-f", "--force"]) && !p.ops.is_empty())
            {
                v.raise(Class::Delete, "git branch -d/-f deletes or resets a branch");
            } else if p.has(&[
                "-m",
                "-M",
                "-c",
                "-C",
                "-u",
                "--move",
                "--copy",
                "--set-upstream-to",
                "--unset-upstream",
                "--track",
            ]) || (!p.ops.is_empty() && !list)
            {
                v.raise(
                    Class::Fill,
                    "git branch creates, renames or sets up a branch",
                );
            }
        }
        "tag" => {
            const F: Flags = Flags {
                short: "ldna",
                short_val: "mF",
                long: &[
                    "list",
                    "delete",
                    "annotate",
                    "ignore-case",
                    "column",
                    "no-column",
                ],
                long_val: &[
                    "sort",
                    "contains",
                    "no-contains",
                    "merged",
                    "no-merged",
                    "points-at",
                    "format",
                    "message",
                    "file",
                    "color",
                    "column",
                ],
                exact: &[],
                num: true,
            };
            let Some(p) = parse(&F, v) else { return };
            let list = p.has(&["-l", "--list"]) || p.ops.is_empty();
            if p.has(&["-d", "--delete"]) {
                v.raise(Class::Delete, "git tag -d deletes a tag");
            } else if p.has(&["-a", "--annotate"]) && !p.has(&["-m", "-F", "--message", "--file"]) {
                v.raise(Class::Delete, "git tag -a without -m opens an editor");
            } else if !list {
                v.raise(Class::Fill, "git tag creates a tag");
            }
        }
        "remote" => {
            const F: Flags = Flags {
                short: "vnf",
                short_val: "tm",
                long: &[
                    "verbose", "push", "all", "add", "delete", "tags", "no-tags", "dry-run",
                ],
                long_val: &["mirror"],
                exact: &[],
                num: false,
            };
            let Some(p) = parse(&F, v) else { return };
            match p.ops.first().map(|w| w.text.as_str()) {
                None | Some("show" | "get-url") => {}
                Some("remove" | "rm" | "prune") => {
                    v.raise(Class::Delete, "git remote remove/prune")
                }
                Some("add" | "set-url" | "rename" | "set-head" | "set-branches") => {
                    v.raise(Class::Account, "git remote changes repository settings")
                }
                Some(o) => v.raise(
                    Class::Delete,
                    format!(
                        "git remote {} is not known to be safe",
                        crate::text::one_line(o, 30)
                    ),
                ),
            }
        }
        "config" => git_config(rest, v),
        "stash" => {
            const F: Flags = Flags {
                short: "kuaSq",
                short_val: "m",
                long: &[
                    "keep-index",
                    "no-keep-index",
                    "include-untracked",
                    "all",
                    "quiet",
                    "staged",
                    "index",
                ],
                long_val: &["message", "pathspec-from-file"],
                exact: &[],
                num: false,
            };
            match first_op {
                Some("list" | "show") => {
                    parse(&GIT_LOG, v);
                }
                Some("drop" | "clear") => {
                    v.raise(Class::Delete, "git stash drop/clear deletes saved work")
                }
                None | Some("push" | "save" | "apply" | "pop" | "branch" | "create" | "store") => {
                    if parse(&F, v).is_some() {
                        v.raise(Class::Fill, "git stash changes the working tree");
                    }
                }
                Some(o) => v.raise(
                    Class::Delete,
                    format!(
                        "git stash {} is not known to be safe",
                        crate::text::one_line(o, 30)
                    ),
                ),
            }
        }
        "add" => {
            // No -i/-p/-e (interactive, editor).
            const F: Flags = Flags {
                short: "nvfAuN",
                short_val: "",
                long: &[
                    "dry-run",
                    "verbose",
                    "force",
                    "all",
                    "no-all",
                    "update",
                    "ignore-removal",
                    "no-ignore-removal",
                    "intent-to-add",
                    "refresh",
                    "ignore-errors",
                    "ignore-missing",
                    "renormalize",
                    "sparse",
                ],
                long_val: &["chmod", "pathspec-from-file"],
                exact: &[],
                num: false,
            };
            if parse(&F, v).is_some() {
                v.raise(Class::Fill, "git add changes the index");
            }
        }
        "commit" => {
            // No -e/--edit, -c, -S, -p, -i (editor, gpg, interactive).
            const F: Flags = Flags {
                short: "aqvnso",
                short_val: "mFC",
                long: &[
                    "all",
                    "quiet",
                    "verbose",
                    "no-verify",
                    "verify",
                    "signoff",
                    "no-signoff",
                    "amend",
                    "no-edit",
                    "allow-empty",
                    "allow-empty-message",
                    "only",
                    "dry-run",
                    "short",
                    "branch",
                    "porcelain",
                    "long",
                    "null",
                    "reset-author",
                    "status",
                    "no-status",
                    "no-post-rewrite",
                ],
                long_val: &[
                    "message",
                    "file",
                    "reuse-message",
                    "fixup",
                    "squash",
                    "author",
                    "date",
                    "cleanup",
                    "trailer",
                    "untracked-files",
                    "pathspec-from-file",
                ],
                exact: &[],
                num: false,
            };
            let Some(p) = parse(&F, v) else { return };
            if !p.has(&[
                "-m",
                "-F",
                "-C",
                "--message",
                "--file",
                "--reuse-message",
                "--no-edit",
                "--fixup",
                "--dry-run",
            ]) {
                v.raise(Class::Delete, "git commit without -m opens an editor");
            } else {
                v.raise(Class::Fill, "git commit changes the repository");
            }
        }
        "fetch" | "pull" => {
            // No -u / --upload-pack (runs a program), no -c / --config.
            const F: Flags = Flags {
                short: "aqvpPftnr46",
                short_val: "jsX",
                long: &[
                    "all",
                    "quiet",
                    "verbose",
                    "prune",
                    "prune-tags",
                    "force",
                    "tags",
                    "no-tags",
                    "dry-run",
                    "append",
                    "atomic",
                    "progress",
                    "no-progress",
                    "unshallow",
                    "update-shallow",
                    "no-recurse-submodules",
                    "ipv4",
                    "ipv6",
                    "set-upstream",
                    "multiple",
                    "keep",
                    "write-fetch-head",
                    "no-write-fetch-head",
                    "refetch",
                    "rebase",
                    "no-rebase",
                    "ff",
                    "no-ff",
                    "ff-only",
                    "squash",
                    "no-squash",
                    "commit",
                    "no-commit",
                    "autostash",
                    "no-autostash",
                    "stat",
                    "no-stat",
                    "no-edit",
                    "verify",
                    "no-verify",
                ],
                long_val: &[
                    "depth",
                    "deepen",
                    "shallow-since",
                    "shallow-exclude",
                    "jobs",
                    "refmap",
                    "negotiation-tip",
                    "filter",
                    "rebase",
                    "strategy",
                    "strategy-option",
                ],
                exact: &[],
                num: false,
            };
            if let Some(p) = parse(&F, v) {
                if p.value(&["--rebase"]).is_some_and(|r| {
                    matches!(r.text.as_str(), "i" | "interactive") || !r.is_static()
                }) {
                    v.raise(
                        Class::Delete,
                        "git pull --rebase=interactive opens an editor",
                    );
                } else {
                    v.raise(Class::Fill, format!("git {s} changes the repository"));
                }
            }
        }
        "merge" | "cherry-pick" | "revert" => {
            // No -e/--edit, -S, -x for merge... (editor, gpg).
            const F: Flags = Flags {
                short: "nqvsx",
                short_val: "mXs",
                long: &[
                    "no-ff",
                    "ff",
                    "ff-only",
                    "squash",
                    "no-squash",
                    "commit",
                    "no-commit",
                    "stat",
                    "no-stat",
                    "abort",
                    "continue",
                    "quit",
                    "skip",
                    "no-edit",
                    "verify",
                    "no-verify",
                    "autostash",
                    "no-autostash",
                    "allow-unrelated-histories",
                    "progress",
                    "no-progress",
                    "log",
                    "no-log",
                    "signoff",
                    "allow-empty",
                    "allow-empty-message",
                    "keep-redundant-commits",
                ],
                long_val: &[
                    "message",
                    "strategy",
                    "strategy-option",
                    "log",
                    "into-name",
                    "file",
                    "cleanup",
                    "mainline",
                    "empty",
                ],
                exact: &[],
                num: false,
            };
            if let Some(p) = parse(&F, v) {
                if s == "merge" && p.has(&["--abort"]) {
                    v.raise(
                        Class::Delete,
                        "git merge --abort can drop changes made before the merge",
                    );
                } else {
                    v.raise(Class::Fill, format!("git {s} changes the repository"));
                }
            }
        }
        "rebase" => {
            // No -i/--interactive, -x/--exec, --edit-todo (editor, programs).
            const F: Flags = Flags {
                short: "qvfnm",
                short_val: "sX",
                long: &[
                    "root",
                    "continue",
                    "skip",
                    "abort",
                    "quit",
                    "autostash",
                    "no-autostash",
                    "quiet",
                    "verbose",
                    "no-verify",
                    "verify",
                    "keep-empty",
                    "no-keep-empty",
                    "update-refs",
                    "no-update-refs",
                    "committer-date-is-author-date",
                    "ignore-date",
                    "reset-author-date",
                    "signoff",
                    "force-rebase",
                    "no-ff",
                    "fork-point",
                    "no-fork-point",
                    "stat",
                    "no-stat",
                    "merge",
                    "apply",
                    "rebase-merges",
                    "no-rebase-merges",
                    "reapply-cherry-picks",
                    "no-reapply-cherry-picks",
                ],
                long_val: &[
                    "onto",
                    "empty",
                    "rebase-merges",
                    "strategy",
                    "strategy-option",
                    "whitespace",
                ],
                exact: &[],
                num: false,
            };
            if parse(&F, v).is_some() {
                v.raise(Class::Fill, "git rebase rewrites local commits");
            }
        }
        "switch" => {
            // -f / --discard-changes throw away changes (S1).
            if rest.iter().any(|a| {
                matches!(
                    a.text.as_str(),
                    "-f" | "--force" | "--discard-changes" | "-C" | "--force-create"
                ) || a.text.starts_with("--force-create=")
            }) {
                v.raise(
                    Class::Delete,
                    "git switch -f/--discard-changes/-C throws away changes or resets a branch",
                );
                return;
            }
            const F: Flags = Flags {
                short: "dtqm",
                short_val: "c",
                long: &[
                    "detach",
                    "guess",
                    "no-guess",
                    "track",
                    "no-track",
                    "quiet",
                    "merge",
                    "progress",
                    "no-progress",
                    "ignore-other-worktrees",
                    "recurse-submodules",
                    "no-recurse-submodules",
                ],
                long_val: &["create", "orphan", "conflict", "track"],
                exact: &[],
                num: false,
            };
            if parse(&F, v).is_some() {
                v.raise(Class::Fill, "git switch changes branch");
            }
        }
        "checkout" => {
            const F: Flags = Flags {
                short: "fqmtdB",
                short_val: "b",
                long: &[
                    "force",
                    "quiet",
                    "merge",
                    "detach",
                    "track",
                    "no-track",
                    "guess",
                    "no-guess",
                    "progress",
                    "no-progress",
                    "ignore-other-worktrees",
                    "overwrite-ignore",
                    "ours",
                    "theirs",
                ],
                long_val: &["orphan", "conflict", "track"],
                exact: &[],
                num: false,
            };
            let discards = rest.iter().any(|a| a.text == "--" || a.text == ".");
            let Some(p) = parse(&F, v) else { return };
            if discards || p.has(&["-f", "--force", "-B"]) {
                v.raise(
                    Class::Delete,
                    "git checkout -f/--/-B discards working-tree changes or resets a branch",
                );
            } else if p.has(&["-b", "--orphan"]) {
                v.raise(Class::Fill, "git checkout -b creates a branch");
            } else {
                let paths_exist = p.ops.iter().any(|a| match &env.cwds {
                    Some(cwds) => cwds.iter().any(|cwd| {
                        paths::resolve(cwd, &a.text)
                            .map(|p| paths::exists(&p))
                            .unwrap_or(true)
                    }),
                    None => true,
                });
                if paths_exist {
                    v.raise(Class::Delete, "git checkout of a path discards its changes");
                } else {
                    v.raise(Class::Fill, "git checkout switches branch");
                }
            }
        }
        "restore" => {
            const F: Flags = Flags {
                short: "SWq",
                short_val: "s",
                long: &[
                    "staged",
                    "worktree",
                    "quiet",
                    "overlay",
                    "no-overlay",
                    "ignore-unmerged",
                    "ours",
                    "theirs",
                    "merge",
                    "progress",
                    "no-progress",
                ],
                long_val: &["source", "conflict", "pathspec-from-file"],
                exact: &[],
                num: false,
            };
            let Some(p) = parse(&F, v) else { return };
            if p.has(&["-S", "--staged"]) && !p.has(&["-W", "--worktree"]) {
                v.raise(Class::Fill, "git restore --staged unstages");
            } else {
                v.raise(Class::Delete, "git restore discards working-tree changes");
            }
        }
        "reset" => {
            const F: Flags = Flags {
                short: "qN",
                short_val: "",
                long: &[
                    "soft",
                    "mixed",
                    "hard",
                    "merge",
                    "keep",
                    "quiet",
                    "no-refresh",
                    "refresh",
                    "intent-to-add",
                ],
                long_val: &["pathspec-from-file"],
                exact: &[],
                num: false,
            };
            let Some(p) = parse(&F, v) else { return };
            if p.has(&["--hard", "--merge", "--keep"]) {
                v.raise(Class::Delete, "git reset --hard discards work");
            } else {
                v.raise(Class::Fill, "git reset moves the branch");
            }
        }
        "clean" => {
            const F: Flags = Flags {
                short: "nfdxXq",
                short_val: "e",
                long: &["dry-run", "force", "quiet"],
                long_val: &["exclude"],
                exact: &[],
                num: false,
            };
            match parse(&F, v) {
                Some(p) if p.has(&["-n", "--dry-run"]) => {}
                _ => v.raise(Class::Delete, "git clean deletes untracked files"),
            }
        }
        "rm" => {
            const F: Flags = Flags {
                short: "frnq",
                short_val: "",
                long: &[
                    "cached",
                    "force",
                    "dry-run",
                    "quiet",
                    "ignore-unmatch",
                    "sparse",
                ],
                long_val: &["pathspec-from-file"],
                exact: &[],
                num: false,
            };
            let Some(p) = parse(&F, v) else { return };
            if p.has(&["-n", "--dry-run"]) {
            } else if p.has(&["--cached"]) {
                v.raise(Class::Fill, "git rm --cached untracks files");
            } else {
                v.raise(Class::Delete, "git rm deletes files");
            }
        }
        "mv" => {
            const F: Flags = Flags {
                short: "fkvn",
                short_val: "",
                long: &["force", "verbose", "dry-run", "sparse"],
                long_val: &[],
                exact: &[],
                num: false,
            };
            let Some(p) = parse(&F, v) else { return };
            if p.has(&["-f", "--force"]) {
                v.raise(Class::Delete, "git mv -f replaces the destination");
            } else {
                v.raise(Class::Fill, "git mv moves files");
            }
        }
        "init" => {
            // No --template (installs hooks), no --separate-git-dir.
            const F: Flags = Flags {
                short: "qb",
                short_val: "",
                long: &["quiet", "bare"],
                long_val: &["initial-branch", "object-format", "shared"],
                exact: &[],
                num: false,
            };
            if parse(&F, v).is_some() {
                v.raise(Class::Fill, "git init creates a repository");
            }
        }
        "clone" => {
            // No -u / --upload-pack, --template, -c / --config (B8).
            const F: Flags = Flags {
                short: "lsqnv",
                short_val: "boj",
                long: &[
                    "local",
                    "no-local",
                    "no-hardlinks",
                    "shared",
                    "quiet",
                    "verbose",
                    "progress",
                    "no-checkout",
                    "bare",
                    "mirror",
                    "single-branch",
                    "no-single-branch",
                    "no-tags",
                    "recurse-submodules",
                    "shallow-submodules",
                    "no-shallow-submodules",
                    "remote-submodules",
                    "sparse",
                    "reject-shallow",
                    "no-reject-shallow",
                ],
                long_val: &[
                    "branch",
                    "origin",
                    "depth",
                    "shallow-since",
                    "shallow-exclude",
                    "jobs",
                    "filter",
                    "reference",
                    "reference-if-able",
                    "revision",
                ],
                exact: &[],
                num: false,
            };
            if let Some(p) = parse(&F, v) {
                if let Some(dest) = p.ops.get(1) {
                    create_target(env, dest, "git clone into", v);
                }
                v.raise(Class::Fill, "git clone creates a repository");
            }
        }
        "apply" | "am" => {
            const F: Flags = Flags {
                short: "Rv",
                short_val: "p",
                long: &["check", "stat", "numstat", "summary", "verbose"],
                long_val: &[],
                exact: &[],
                num: false,
            };
            match parse(&F, v) {
                Some(p)
                    if s == "apply" && p.has(&["--check", "--stat", "--numstat", "--summary"]) => {}
                _ => v.raise(
                    Class::Delete,
                    format!("git {s} rewrites files named in the patch"),
                ),
            }
        }
        "worktree" => match first_op {
            Some("list") => {}
            Some("add") => v.raise(Class::Fill, "git worktree add creates a working tree"),
            _ => v.raise(
                Class::Delete,
                "git worktree remove/prune/move deletes or moves a working tree",
            ),
        },
        "gc" => {
            const F: Flags = Flags {
                short: "q",
                short_val: "",
                long: &[
                    "aggressive",
                    "auto",
                    "quiet",
                    "no-prune",
                    "keep-largest-pack",
                ],
                long_val: &[],
                exact: &[],
                num: false,
            };
            if parse(&F, v).is_some() {
                v.raise(Class::Fill, "git gc repacks the repository");
            }
        }
        "reflog" => match first_op {
            None | Some("show") => {
                parse(&GIT_LOG, v);
            }
            _ => v.raise(Class::Delete, "git reflog expire/delete drops history"),
        },
        "submodule" => match first_op {
            Some("status" | "summary") => {}
            Some("init" | "update" | "sync")
                if !rest.iter().any(|a| a.text == "--force" || a.text == "-f") =>
            {
                v.raise(Class::Fill, "git submodule changes the repository")
            }
            _ => v.raise(
                Class::Delete,
                "git submodule foreach/deinit/--force runs programs or drops work",
            ),
        },
        "push" => {
            // No --receive-pack / --exec (run a program, B8), no --signed (gpg).
            const F: Flags = Flags {
                short: "nquvfd",
                short_val: "o",
                long: &[
                    "dry-run",
                    "quiet",
                    "verbose",
                    "set-upstream",
                    "all",
                    "branches",
                    "mirror",
                    "tags",
                    "follow-tags",
                    "no-verify",
                    "verify",
                    "atomic",
                    "no-atomic",
                    "progress",
                    "no-progress",
                    "porcelain",
                    "prune",
                    "delete",
                    "force",
                    "force-if-includes",
                    "no-force-with-lease",
                    "force-with-lease",
                    "thin",
                    "no-thin",
                    "ipv4",
                    "ipv6",
                ],
                long_val: &[
                    "force-with-lease",
                    "repo",
                    "push-option",
                    "recurse-submodules",
                ],
                exact: &[],
                num: false,
            };
            let Some(p) = parse(&F, v) else { return };
            let destructive = p.has(&[
                "-f",
                "-d",
                "--force",
                "--delete",
                "--mirror",
                "--prune",
                "--force-with-lease",
                "--force-if-includes",
            ]) || p
                .ops
                .iter()
                .any(|a| a.text.starts_with(':') || a.text.starts_with('+'));
            if destructive {
                v.raise(Class::Delete, "git push rewrites or deletes remote history");
            } else {
                v.raise(Class::Account, "git push publishes commits to a remote");
            }
        }
        "send-email" => {
            // No --to-cmd / --cc-cmd / --smtp-server (run programs), no --compose (editor).
            const F: Flags = Flags {
                short: "q",
                short_val: "",
                long: &[
                    "quiet",
                    "dry-run",
                    "no-annotate",
                    "suppress-from",
                    "no-signed-off-by-cc",
                    "thread",
                    "no-thread",
                    "validate",
                    "no-validate",
                ],
                long_val: &[
                    "to",
                    "cc",
                    "bcc",
                    "from",
                    "subject",
                    "in-reply-to",
                    "confirm",
                    "suppress-cc",
                ],
                exact: &[],
                num: false,
            };
            if parse(&F, v).is_some() {
                v.raise(Class::Send, "git send-email sends mail");
            }
        }
        _ => v.raise(
            Class::Delete,
            format!(
                "git {} is not known to be safe",
                crate::text::one_line(s, 40)
            ),
        ),
    }
}

/// `git config`: reads are Read; writes Account, or Delete for keys that run programs;
/// `--edit` opens an editor.
fn git_config(rest: &[Word], v: &mut Verdict) {
    const F: Flags = Flags {
        short: "lze",
        short_val: "f",
        long: &[
            "get",
            "get-all",
            "get-regexp",
            "get-urlmatch",
            "list",
            "show-origin",
            "show-scope",
            "name-only",
            "global",
            "local",
            "system",
            "worktree",
            "null",
            "includes",
            "no-includes",
            "add",
            "unset",
            "unset-all",
            "replace-all",
            "edit",
            "bool",
            "int",
            "path",
            "all",
        ],
        long_val: &[
            "type",
            "default",
            "file",
            "blob",
            "value",
            "rename-section",
            "remove-section",
        ],
        exact: &[],
        num: false,
    };
    let Some(p) = flags_or_delete("git config", &F, rest, false, v) else {
        return;
    };
    if p.has(&["-e", "--edit"]) {
        v.raise(Class::Delete, "git config --edit opens an editor");
        return;
    }
    let mut ops: Vec<&str> = p.ops.iter().map(|w| w.text.as_str()).collect();
    // Newer syntax: `git config get|list|set|unset ...`.
    let verb = match ops.first() {
        Some(
            &o @ ("get" | "list" | "set" | "unset" | "rename-section" | "remove-section" | "edit"),
        ) => {
            ops.remove(0);
            Some(o)
        }
        _ => None,
    };
    let reads = matches!(verb, Some("get" | "list"))
        || (verb.is_none()
            && (p.has(&[
                "--get",
                "--get-all",
                "--get-regexp",
                "--get-urlmatch",
                "--list",
                "-l",
            ]) || (ops.len() <= 1
                && !p.has(&[
                    "--add",
                    "--unset",
                    "--unset-all",
                    "--replace-all",
                    "--rename-section",
                    "--remove-section",
                ]))));
    if reads {
        return;
    }
    if verb == Some("edit") {
        v.raise(Class::Delete, "git config edit opens an editor");
    } else if ops.first().is_none_or(|k| git_config_runs(k)) {
        v.raise(Class::Delete, "git config sets a key that runs a program");
    } else {
        v.raise(Class::Account, "git config changes settings");
    }
}

// ---- gh ----

/// Flag tokens gh accepts here (with or without `=value`).
const GH_FLAGS: &[&str] = &[
    "--repo",
    "-R",
    "--json",
    "--jq",
    "-q",
    "--template",
    "-t",
    "--limit",
    "-L",
    "--state",
    "-s",
    "--author",
    "--assignee",
    "-a",
    "--label",
    "-l",
    "--search",
    "-S",
    "--base",
    "-B",
    "--head",
    "-H",
    "--branch",
    "-b",
    "--workflow",
    "-w",
    "--web",
    "--event",
    "-e",
    "--status",
    "--user",
    "-u",
    "--job",
    "-j",
    "--attempt",
    "--log",
    "--log-failed",
    "--comments",
    "-c",
    "--exit-status",
    "--interval",
    "-i",
    "--required",
    "--name-only",
    "--patch",
    "--color",
    "--verbose",
    "--method",
    "-X",
    "--field",
    "-f",
    "--raw-field",
    "-F",
    "--header",
    "--input",
    "--hostname",
    "--silent",
    "--include",
    "--cache",
    "--preview",
    "-p",
    "--paginate",
    "--slurp",
    "--title",
    "--body",
    "--body-file",
    "--draft",
    "-d",
    "--fill",
    "--fill-first",
    "--fill-verbose",
    "--milestone",
    "-m",
    "--reviewer",
    "-r",
    "--project",
    "--name",
    "-n",
    "--dir",
    "-D",
    "--pattern",
    "--archive",
    "--ref",
    "--commit",
    "--subject",
    "--match-head-commit",
    "--admin",
    "--auto",
    "--squash",
    "--merge",
    "--rebase",
    "--delete-branch",
    "--disable-auto",
    "--yes",
    "--prerelease",
    "--latest",
    "--notes",
    "--notes-file",
    "--generate-notes",
    "--target",
    "--public",
    "--private",
    "--internal",
    "--description",
    "--homepage",
    "--limit",
    "--sort",
    "--order",
    "--owner",
    "--language",
    "--created",
    "--updated",
    "--visibility",
    "--watch",
    "--force",
];

fn gh(args: &[Word], v: &mut Verdict) {
    if args.iter().any(may_be_option) {
        v.raise(
            Class::Delete,
            "gh with a word decided at run time (it could become an option)",
        );
        return;
    }
    for a in args {
        let t = a.text.as_str();
        if t.starts_with('-') && t.len() > 1 {
            let name = t.split('=').next().unwrap_or(t);
            if !GH_FLAGS.contains(&name) {
                v.raise(
                    Class::Delete,
                    format!(
                        "gh {} is not an option the classifier knows to be safe",
                        crate::text::one_line(t, 40)
                    ),
                );
                return;
            }
        }
    }
    let group = args.first().map(|a| a.text.as_str()).unwrap_or("");
    if group.is_empty() || group.starts_with('-') {
        if group.is_empty() || (args.len() == 1 && matches!(group, "--version" | "--help")) {
            return;
        }
        v.raise(Class::Delete, "gh with options before the command");
        return;
    }
    // Any other first word is an alias (`!cmd` runs a shell) or an extension (a program).
    if !matches!(
        group,
        "pr" | "issue"
            | "repo"
            | "run"
            | "workflow"
            | "release"
            | "gist"
            | "api"
            | "browse"
            | "search"
            | "status"
            | "label"
            | "cache"
            | "project"
            | "ruleset"
            | "org"
            | "auth"
            | "config"
            | "help"
            | "version"
    ) {
        v.raise(
            Class::Delete,
            format!(
                "gh {}: aliases and extensions run programs",
                crate::text::one_line(group, 30)
            ),
        );
        return;
    }
    if args.iter().any(|a| a.text == "--web") {
        v.raise(Class::Navigate, "gh --web opens a browser");
    }
    let second = args.get(1).map(|a| a.text.as_str()).unwrap_or("");
    if group == "auth" {
        match second {
            "status" => {}
            "token" => v.raise(Class::Delete, "gh auth token prints a credential"),
            _ => v.raise(Class::Account, "gh auth changes the GitHub login"),
        }
        return;
    }
    if group == "config" {
        match second {
            "get" | "list" => {}
            // editor, browser, pager: programs gh runs.
            _ => v.raise(Class::Delete, "gh config set can make gh run a program"),
        }
        return;
    }
    if group == "api" {
        let method = args
            .iter()
            .enumerate()
            .find_map(|(i, a)| {
                let t = a.text.as_str();
                if t == "-X" || t == "--method" {
                    args.get(i + 1).map(|w| w.text.clone())
                } else {
                    t.strip_prefix("--method=")
                        .or_else(|| t.strip_prefix("-X").filter(|r| !r.is_empty()))
                        .map(str::to_string)
                }
            })
            .map(|m| m.to_ascii_uppercase());
        let fields = args.iter().any(|a| {
            let n = a.text.split('=').next().unwrap_or("");
            matches!(n, "-f" | "-F" | "--field" | "--raw-field" | "--input")
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
    let action = args
        .get(1)
        .map(|a| a.text.as_str())
        .filter(|a| !a.starts_with('-'))
        .unwrap_or("");
    match action {
        "list" | "ls" | "view" | "status" | "diff" | "checks" | "watch" | "" => {}
        "download" | "clone" => {
            if args.iter().any(|a| a.text == "--clobber") {
                v.raise(
                    Class::Delete,
                    format!("gh {group} {action} --clobber replaces files"),
                );
            } else {
                v.raise(Class::Fill, format!("gh {group} {action}"));
            }
        }
        "checkout" => v.raise(Class::Fill, format!("gh {group} {action}")),
        "create" | "comment" | "review" | "edit" | "close" | "reopen" | "ready" | "lock"
        | "unlock" | "transfer" | "pin" | "unpin" | "develop" => v.raise(
            Class::Submit,
            format!("gh {group} {action} posts to GitHub"),
        ),
        "delete" | "remove" | "rm" | "purge" | "delete-asset" => {
            v.raise(Class::Delete, format!("gh {group} {action}"))
        }
        "merge" | "set" | "enable" | "disable" | "run" | "rerun" | "cancel" | "login"
        | "logout" | "upgrade" | "rename" | "archive" | "unarchive" | "sync" | "fork"
        | "upload" => v.raise(
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

#[cfg(test)]
mod tests;
