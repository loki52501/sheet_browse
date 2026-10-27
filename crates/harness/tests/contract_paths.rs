//! Contract paths (pr-ready section 4) for the harness crate: the public items T-16 adds and
//! files under interface changes B-189 (reflect, proposals), B-197 (session_prompt) and
//! B-198 (mem server, `harness mcp`) must exist at exactly these paths and signatures, so a
//! wrong path in blockers.yaml fails compilation here.

use harness::cli::Cmd;
use harness::launcher::{session_prompt, ClaudeLauncher};
use harness::mem_server::{
    self, DUE_DAYS_MAX, DUE_K_DEFAULT, DUE_K_MAX, SERVER, SHOW_MAX, STATE_MAX_BYTES, TARGET_MAX,
    TEXT_MAX, WATCH_FILE,
};
use harness::proposals::{
    arg_shape, default_proposals_file, merge_proposals, proposals_file_from_env, retire_proposal,
    role_sequence, write_proposals, MergeInput, Proposal, StaleWhy, Written, MERGE_MIN_DOMAINS,
    PROPOSALS_FILE_VAR, RUN_COPY,
};
use harness::reflect::{
    last_use, replay_prompt, tool_domains, tool_files, GoodCall, History, ReflectError,
    ReflectLauncher, Reflected, Replay, ToolFile, PROMPT_FILE, READERS_ALLOWED_TOOLS,
    REPLAY_MAX_CHARS, SUMMARY_FILE, SWEEP_TOOL, WEB_SERVER,
};
use harness::CliError;
use harness_core::mcp::{Harness, ToolSpec};
use harness_core::memory::{ToolHealth, Ts};
use harness_core::run::RunId;
use harness_core::sched::{LaunchRequest, Launcher};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use toolstore::ToolStore;

/// B-189: reflect's launcher, history and tool scan.
#[test]
fn reflect_paths() {
    fn _is_launcher(l: ReflectLauncher) -> Arc<dyn Launcher> {
        Arc::new(l)
    }
    let _read: fn(&Path, &ToolStore, Option<&RunId>) -> History = History::read;
    let _visits: fn(&History, &str, Option<Ts>) -> u32 = History::visits_since;
    let _good: for<'a> fn(&'a History, &str, &str) -> Option<&'a GoodCall> =
        History::last_good_call;
    let _domains: fn(&Path) -> Vec<String> = tool_domains;
    let _files: fn(&Path) -> Vec<ToolFile> = tool_files;
    let _ns: fn(&ToolFile) -> String = ToolFile::namespaced;
    let _last: fn(&ToolHealth, Option<&Path>) -> Option<Ts> = last_use;
    let _prompt: fn(&RunId, &[Replay]) -> String = replay_prompt;
    let _r: Option<(Reflected, ReflectError)> = None;
    let _: ([&str; 1], &str, &str, &str, &str, usize) = (
        READERS_ALLOWED_TOOLS,
        SWEEP_TOOL,
        WEB_SERVER,
        SUMMARY_FILE,
        PROMPT_FILE,
        REPLAY_MAX_CHARS,
    );
    // Reflect's pass is callable without starting a session.
    fn _reflect<'a>(
        l: &'a ReflectLauncher,
        req: &'a LaunchRequest<'a>,
    ) -> impl std::future::Future<Output = Result<Reflected, ReflectError>> + 'a {
        l.reflect(req)
    }
    let _ = _reflect;
}

/// B-189: the proposals file writer.
#[test]
fn proposals_paths() {
    let _merge: fn(&[MergeInput<'_>], &RunId, &str) -> Vec<Proposal> = merge_proposals;
    let _retire: fn(&str, &str, i32, &StaleWhy, &RunId, &str) -> Proposal = retire_proposal;
    let _write: fn(Option<&Path>, &Path, &RunId, &[Proposal]) -> std::io::Result<Written> =
        write_proposals;
    let _shape: fn(&[toolstore::ArgSpec]) -> String = arg_shape;
    let _roles: fn(&toolstore::ToolHeader) -> String = role_sequence;
    let _default: fn(&Path) -> PathBuf = default_proposals_file;
    let _env: fn(&Path) -> PathBuf = proposals_file_from_env;
    let _line: fn(&Proposal) -> String = Proposal::line;
    let _: (&str, &str, usize) = (PROPOSALS_FILE_VAR, RUN_COPY, MERGE_MIN_DOMAINS);
}

/// B-197, B-198: session preamble, `allowed_tools`, the mem server and `harness mcp`.
#[test]
fn launcher_and_mem_server_paths() {
    let _session: fn(&LaunchRequest<'_>) -> String = session_prompt;
    let l = ClaudeLauncher::print("claude");
    let _: &Vec<String> = &l.allowed_tools;
    let _tools: fn(&Harness) -> Vec<ToolSpec> = mem_server::tools;
    let _serve = mem_server::serve;
    let _: (&str, &str, usize, usize, usize, usize, usize, usize, i64) = (
        SERVER,
        WATCH_FILE,
        TEXT_MAX,
        TARGET_MAX,
        STATE_MAX_BYTES,
        SHOW_MAX,
        DUE_K_DEFAULT,
        DUE_K_MAX,
        DUE_DAYS_MAX,
    );
    assert_eq!(WATCH_FILE, harness_core::watch::WATCH_FILE);
    let _cmd = Cmd::Mcp;
    fn _serve_err(e: harness_core::mcp::ServeError) -> CliError {
        CliError::Serve(e)
    }
}
