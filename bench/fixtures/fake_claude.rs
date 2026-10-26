//! A stand-in for the `claude` executable in T-08 acceptance tests (CLAUDE.md rule 1: no
//! model is ever called). crates/harness/tests/accept_t08.rs and accept_t08_r2.rs compile it
//! with plain `rustc` and point `SB_CLAUDE` (or `ClaudeLauncher::print`) at the binary.
//!
//! It records what it was started with into `$FAKE_CLAUDE_DIR/<SB_RUN_ID>.{argv,stdin,env,pid}`
//! (or, when `FAKE_CLAUDE_DIR` is unset and a file named `record` sits next to the
//! executable, into the executable's folder): argv one argument per line, stdin as read (only
//! in `-p` mode, like `claude -p`), the `SB_RUN_ID` / `SB_RUNS_ROOT` it saw, and its process
//! id. Its stdout is `fake claude: <stdin>` plus, in `-p` mode with `FAKE_CLAUDE_LEAK=1`, the
//! value of `SB_VAULT_CANARY` (a session that leaked a resolved vault value, which the
//! launcher must scrub out of files/answer.md). It exits with `FAKE_CLAUDE_EXIT` (default 0).
//!
//! Directives in the `-p` prompt (stdin), for T-08 round 2 (D-124):
//! * `[[hang]]`: after recording, writes `<run>.hanging` and never exits (a hung session).
//! * `[[emit A P]]`: stdout is exactly A bytes `a`, then `SB_VAULT_CANARY`, then P bytes `b`
//!   (the canary comes from the environment, so no file ever holds it).

use std::io::{Read, Write};

fn emit_directive(input: &str) -> Option<(usize, usize)> {
    let start = input.find("[[emit ")? + "[[emit ".len();
    let rest = &input[start..];
    let end = rest.find("]]")?;
    let mut it = rest[..end].split_whitespace();
    let a = it.next()?.parse().ok()?;
    let p = it.next()?.parse().ok()?;
    Some((a, p))
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let print_mode = args.iter().any(|a| a == "-p");
    let run = std::env::var("SB_RUN_ID").unwrap_or_else(|_| "no-run-id".to_owned());
    let root = std::env::var("SB_RUNS_ROOT").unwrap_or_default();
    let mut input = String::new();
    if print_mode {
        let _ = std::io::stdin().read_to_string(&mut input);
    }
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()));
    let record_dir = match std::env::var("FAKE_CLAUDE_DIR") {
        Ok(d) => Some(std::path::PathBuf::from(d)),
        Err(_) => exe_dir.filter(|d| d.join("record").is_file()),
    };
    if let Some(dir) = &record_dir {
        let _ = std::fs::create_dir_all(dir);
        let _ = std::fs::write(dir.join(format!("{run}.argv")), args.join("\n"));
        if print_mode {
            let _ = std::fs::write(dir.join(format!("{run}.stdin")), &input);
        }
        let _ = std::fs::write(
            dir.join(format!("{run}.env")),
            format!("SB_RUN_ID={run}\nSB_RUNS_ROOT={root}\n"),
        );
        let _ = std::fs::write(
            dir.join(format!("{run}.pid")),
            std::process::id().to_string(),
        );
    }
    if print_mode && input.contains("[[hang]]") {
        if let Some(dir) = &record_dir {
            let _ = std::fs::write(
                dir.join(format!("{run}.hanging")),
                std::process::id().to_string(),
            );
        }
        loop {
            std::thread::park();
        }
    }
    let mut out = std::io::stdout();
    if let (true, Some((a, p))) = (print_mode, emit_directive(&input)) {
        let c = std::env::var("SB_VAULT_CANARY").unwrap_or_default();
        let _ = out.write_all(&vec![b'a'; a]);
        let _ = out.write_all(c.as_bytes());
        let _ = out.write_all(&vec![b'b'; p]);
        let _ = out.flush();
        std::process::exit(0);
    }
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
