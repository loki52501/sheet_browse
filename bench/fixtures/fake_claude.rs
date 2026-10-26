//! A stand-in for the `claude` executable in T-08 acceptance tests (CLAUDE.md rule 1: no
//! model is ever called). crates/harness/tests/accept_t08.rs compiles it with plain `rustc`
//! and points `SB_CLAUDE` (or `ClaudeLauncher::print`) at the binary.
//!
//! It records what it was started with into `$FAKE_CLAUDE_DIR/<SB_RUN_ID>.{argv,stdin,env}`:
//! argv one argument per line, stdin as read (only in `-p` mode, like `claude -p`), and the
//! `SB_RUN_ID` / `SB_RUNS_ROOT` it saw. Its stdout is `fake claude: <stdin>` plus, in `-p`
//! mode with `FAKE_CLAUDE_LEAK=1`, the value of `SB_VAULT_CANARY` (a session that leaked a
//! resolved vault value, which the launcher must scrub out of files/answer.md). It exits
//! with `FAKE_CLAUDE_EXIT` (default 0).

use std::io::{Read, Write};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let print_mode = args.iter().any(|a| a == "-p");
    let run = std::env::var("SB_RUN_ID").unwrap_or_else(|_| "no-run-id".to_owned());
    let root = std::env::var("SB_RUNS_ROOT").unwrap_or_default();
    let mut input = String::new();
    if print_mode {
        let _ = std::io::stdin().read_to_string(&mut input);
    }
    if let Ok(dir) = std::env::var("FAKE_CLAUDE_DIR") {
        let dir = std::path::PathBuf::from(dir);
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::fs::write(dir.join(format!("{run}.argv")), args.join("\n"));
        if print_mode {
            let _ = std::fs::write(dir.join(format!("{run}.stdin")), &input);
        }
        let _ = std::fs::write(
            dir.join(format!("{run}.env")),
            format!("SB_RUN_ID={run}\nSB_RUNS_ROOT={root}\n"),
        );
    }
    let mut out = std::io::stdout();
    let _ = write!(out, "fake claude: {input}");
    if print_mode && std::env::var("FAKE_CLAUDE_LEAK").as_deref() == Ok("1") {
        if let Ok(c) = std::env::var("SB_VAULT_CANARY") {
            let _ = write!(out, "\nleaked: {c}\n");
        }
    }
    let _ = out.flush();
    let code: i32 = std::env::var("FAKE_CLAUDE_EXIT")
        .ok()
        .and_then(|c| c.parse().ok())
        .unwrap_or(0);
    std::process::exit(code);
}
