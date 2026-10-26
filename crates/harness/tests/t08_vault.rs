//! T-08 round 2, rule 2 (H-2) in the harness CLI, harnessd and ClaudeLauncher
//! (review-T-08 blocking item, should fix 1, 3, 4 and 7; B-192).
//!
//! The process-wide scrubber is global, so these tests live in their own test binary and
//! run one at a time (`LOCK`). The vault value is `SB_VAULT_CANARY` when it is set and fits
//! a run-id slug, else a fresh one. No assertion message prints it: texts are shown with it
//! replaced by `<CANARY>`. `claude` is a small fake program compiled from `FAKE_CLAUDE`
//! below (never a model, rule 1). No fixed sleeps: every wait is a condition with a cap.

use async_trait::async_trait;
use clap::Parser;
use harness::cli::{execute, Cli, Ctx};
use harness::daemon::{Daemon, ReopenVault};
use harness::launcher::{ClaudeLauncher, ANSWER_FILE, ANSWER_SCRUB_SLACK, DISALLOWED_TOOLS};
use harness::{CliError, Env};
use harness_core::memory::*;
use harness_core::run::{is_run_id, RunId};
use harness_core::sched::{
    self, LaunchError, LaunchExit, LaunchRequest, Launcher, ManualClock, RunSpec, Runner,
    SchedError, SystemClock,
};
use harness_core::testing::MemStore;
use harness_core::vault::{EntryKind, KeySource, Vault, VaultError};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use tokio::time::timeout;
use zeroize::Zeroizing;

const CAP: Duration = Duration::from_secs(60);
const LITERAL: &str = "@vault(test)";

static LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

// ------------------------------------------------------------------ fixtures

fn canary() -> &'static str {
    static C: OnceLock<String> = OnceLock::new();
    C.get_or_init(|| {
        std::env::var("SB_VAULT_CANARY")
            .ok()
            .filter(|c| c.len() >= 8 && is_run_id(&format!("2026-10-25T00-00-00-{c}")))
            .unwrap_or_else(|| {
                format!(
                    "sbcanary-t08r2-{}",
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap()
                        .as_nanos()
                )
            })
    })
}

fn redact(s: &str) -> String {
    s.replace(canary(), "<CANARY>")
}

/// Panics (without printing the value) when `text` holds the vault value; checks it shows
/// the literal instead when `literal` is set.
fn assert_clean(text: &str, literal: bool, what: &str) {
    assert!(
        !text.contains(canary()),
        "{what} holds the vault value: {}",
        redact(text)
    );
    if literal {
        assert!(text.contains(LITERAL), "{what} lacks {LITERAL}: {text}");
    }
}

fn env(tmp: &Path) -> Env {
    Env {
        database_url: "postgres://unused".to_owned(),
        runs_root: tmp.join("runs"),
        tools_root: tmp.join("tools"),
        vault_path: tmp.join("vault.toml.age"),
        vault_key: KeySource::IdentityFile(tmp.join("vault.key")),
        run_id: None,
        claude: tmp.join("no-claude-here"),
    }
}

fn ctx(env: Env, store: Arc<dyn MemoryStore>) -> Ctx {
    Ctx {
        env,
        store: Some(store),
        read_secret: Box::new(|_| Ok(Zeroizing::new(canary().to_owned()))),
        launcher: None,
    }
}

async fn run_cli(ctx: &Ctx, args: &[&str]) -> (Result<(), CliError>, String) {
    let mut argv = vec!["harness"];
    argv.extend_from_slice(args);
    let cli = Cli::try_parse_from(&argv).unwrap_or_else(|e| panic!("{argv:?}: {e}"));
    let mut out = Vec::new();
    let r = execute(cli, ctx, &mut out).await;
    (r, String::from_utf8(out).expect("utf-8 output"))
}

/// What harness's main prints for an error.
fn binary_error_line(e: &CliError) -> String {
    format!("harness: {}", harness_core::scrub(&format!("{e:#}")))
}

fn install_memory_vault() {
    Vault::in_memory([("test".to_owned(), canary().to_owned(), EntryKind::Secret)])
        .install_global();
}

/// Re-encrypts the vault at `path` with exactly `entries` (name, value, kind), as an older
/// build or a hand edit could have, with the identity `Vault::set` stored in `key_file`.
fn write_vault_file(path: &Path, key_file: &Path, entries: &[(&str, &str, &str)]) {
    let text = std::fs::read_to_string(key_file).unwrap();
    let line = text
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && !l.starts_with('#'))
        .unwrap();
    let id: age::x25519::Identity = line.parse().unwrap();
    let mut t = toml::Table::new();
    for (name, value, kind) in entries {
        let mut e = toml::Table::new();
        e.insert("value".into(), toml::Value::String((*value).into()));
        e.insert("kind".into(), toml::Value::String((*kind).into()));
        t.insert((*name).into(), toml::Value::Table(e));
    }
    let toml_text = toml::to_string(&t).unwrap();
    let ct = age::encrypt(&id.to_public(), toml_text.as_bytes()).unwrap();
    std::fs::write(path, ct).unwrap();
}

// ------------------------------------------------------------------ fake claude

/// A stand-in for `claude -p`: records argv, SB_RUN_ID / SB_RUNS_ROOT and stdin next to
/// itself, then prints `out.txt` and `pad.txt` (a count of `b` bytes), or with a `hang` file
/// waits forever after reading stdin.
const FAKE_CLAUDE: &str = r#"
use std::io::{Read, Write};
fn main() {
    let exe = std::env::current_exe().unwrap();
    let dir = exe.parent().unwrap().to_path_buf();
    let args: Vec<String> = std::env::args().skip(1).collect();
    std::fs::write(dir.join("args.txt"), args.join("\n")).unwrap();
    let env = format!(
        "{}\n{}",
        std::env::var("SB_RUN_ID").unwrap_or_default(),
        std::env::var("SB_RUNS_ROOT").unwrap_or_default()
    );
    std::fs::write(dir.join("env.txt"), env).unwrap();
    let mut input = Vec::new();
    std::io::stdin().read_to_end(&mut input).unwrap();
    std::fs::write(dir.join("stdin.txt"), &input).unwrap();
    if dir.join("hang").exists() {
        std::fs::write(dir.join("hanging"), b"").unwrap();
        loop {
            std::thread::park();
        }
    }
    let mut out = std::io::stdout().lock();
    if let Ok(b) = std::fs::read(dir.join("out.txt")) {
        out.write_all(&b).unwrap();
    }
    if let Ok(n) = std::fs::read_to_string(dir.join("pad.txt")) {
        let mut left: usize = n.trim().parse().unwrap();
        let chunk = vec![b'b'; 8192];
        while left > 0 {
            let k = left.min(chunk.len());
            out.write_all(&chunk[..k]).unwrap();
            left -= k;
        }
    }
    out.flush().unwrap();
}
"#;

fn fake_claude_build() -> PathBuf {
    static BIN: OnceLock<PathBuf> = OnceLock::new();
    BIN.get_or_init(|| {
        let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
            .join("t08_vault_fake_claude")
            .join(std::process::id().to_string());
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("fake_claude.rs");
        std::fs::write(&src, FAKE_CLAUDE).unwrap();
        let out = dir.join(format!("fake-claude{}", std::env::consts::EXE_SUFFIX));
        let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".into());
        let st = std::process::Command::new(rustc)
            .args(["--edition", "2021", "-o"])
            .arg(&out)
            .arg(&src)
            .status()
            .expect("run rustc for the fake claude");
        assert!(st.success(), "compiling the fake claude");
        out
    })
    .clone()
}

/// A copy of the fake in its own folder `dir` (it reads its files from there).
fn fake_claude(dir: &Path) -> PathBuf {
    std::fs::create_dir_all(dir).unwrap();
    let to = dir.join(format!("claude{}", std::env::consts::EXE_SUFFIX));
    std::fs::copy(fake_claude_build(), &to).unwrap();
    to
}

fn runner(store: Arc<MemStore>, launcher: Arc<dyn Launcher>, root: &Path) -> Runner {
    Runner {
        store,
        launcher,
        clock: Arc::new(SystemClock),
        runs_root: root.to_path_buf(),
    }
}

fn spec(prompt: &str) -> RunSpec<'_> {
    RunSpec {
        prompt,
        parent_run: None,
        strict: false,
        schedule: None,
    }
}

// ------------------------------------------------------------------ blocking item

/// Every command except `vault set` installs the vault: `schedule list` and `tools health`
/// print a vault value from the database as @vault(test), and the binary's error line for
/// `ask` is scrubbed. A vault that cannot be opened fails the command with nothing printed.
#[tokio::test]
async fn cli_output_and_errors_show_vault_values_as_literals() {
    let _g = LOCK.lock().await;
    // Another test may have left a vault installed; start from one without the value.
    Vault::in_memory(Vec::<(String, String, EntryKind)>::new()).install_global();
    let tmp = tempfile::tempdir().unwrap();
    let e = env(tmp.path());
    let store = Arc::new(MemStore::new());
    let c = ctx(e.clone(), store.clone());
    let v = canary();

    let (r, out) = run_cli(&c, &["vault", "set", "test"]).await;
    r.unwrap();
    assert_eq!(out, "vault: test set (secret)\n");

    // Rows written outside the CLI (a tool server, an older build) hold the raw value.
    store
        .schedule_add(Schedule {
            id: 0,
            cron: "0 9 * * *".into(),
            prompt: format!("log in as {v} and check mail"),
            kind: ScheduleKind::Cron,
            watch_target: None,
            enabled: true,
            last_run: None,
            run_id: None,
        })
        .await
        .unwrap();
    store
        .tool_health_set(ToolHealth {
            domain: "luma.com".into(),
            name: "register".into(),
            version: 1,
            status: ToolStatus::NeedsRepair,
            runs: 2,
            fails: 1,
            last_ok: None,
            last_fail: None,
            why: Some(format!("page said: wrong password {v}")),
            pinned: false,
        })
        .await
        .unwrap();

    let (r, list) = run_cli(&c, &["schedule", "list"]).await;
    r.unwrap();
    assert_clean(&list, true, "schedule list");
    let (r, health) = run_cli(&c, &["tools", "health", "luma.com"]).await;
    r.unwrap();
    assert_clean(&health, true, "tools health");

    // The error path, as main.rs prints it.
    let run = format!("2026-10-25T00-00-00-{v}");
    std::fs::create_dir_all(e.runs_root.join(&run)).unwrap();
    let (r, out) = run_cli(&c, &["ask", &run, "1", "yes"]).await;
    let err = r.expect_err("no ask card");
    assert!(out.is_empty());
    assert_clean(&binary_error_line(&err), true, "the ask error line");

    // Fail closed: a vault file that cannot be decrypted stops the command before output.
    std::fs::write(&e.vault_path, b"not an age file").unwrap();
    let (r, out) = run_cli(&c, &["schedule", "list"]).await;
    let err = r.expect_err("an unreadable vault fails the command");
    assert!(matches!(err, CliError::Vault(_)), "{err}");
    assert!(out.is_empty(), "nothing printed: {}", redact(&out));
    assert_clean(&binary_error_line(&err), false, "the vault error line");
}

// ------------------------------------------------------------------ should fix 1

/// A file `Vault::open` refuses (an entry under the D-118 floor) fails every command
/// closed, and `harness vault set <name>` (open_for_set) repairs it.
#[tokio::test]
async fn vault_set_repairs_a_short_entry_and_other_commands_fail_closed_until_then() {
    let _g = LOCK.lock().await;
    let tmp = tempfile::tempdir().unwrap();
    let e = env(tmp.path());
    let KeySource::IdentityFile(key_file) = &e.vault_key else {
        unreachable!()
    };
    let mut v = Vault::open(&e.vault_path, &e.vault_key).unwrap();
    v.set("test", canary(), EntryKind::Secret).unwrap();
    drop(v);
    write_vault_file(
        &e.vault_path,
        key_file,
        &[("test", canary(), "secret"), ("pin", "abc", "secret")],
    );
    assert!(matches!(
        Vault::open(&e.vault_path, &e.vault_key),
        Err(VaultError::ValueTooShort { ref name, .. }) if name == "pin"
    ));
    let c = ctx(e.clone(), Arc::new(MemStore::new()));

    let (r, out) = run_cli(&c, &["schedule", "list"]).await;
    let err = r.expect_err("open refuses the file");
    assert!(out.is_empty());
    let line = binary_error_line(&err);
    assert!(line.contains("harness vault set pin"), "{}", redact(&line));
    assert_clean(&line, false, "the refusal");

    let (r, out) = run_cli(&c, &["vault", "set", "pin"]).await;
    r.unwrap_or_else(|e| panic!("vault set pin: {}", redact(&e.to_string())));
    assert_eq!(out, "vault: pin set (secret)\n");
    let fixed = Vault::open(&e.vault_path, &e.vault_key).expect("repaired");
    assert!(fixed
        .resolve_into("@vault(pin)", |r| r.expose() == canary())
        .unwrap());
    let (r, out) = run_cli(&c, &["schedule", "list"]).await;
    r.unwrap();
    assert!(out.starts_with("id  kind"), "{out}");
}

// ------------------------------------------------------------------ should fix 3, B-192

/// A vault value in a prompt is @vault(test) in prompt.md, runs.prompt, the run id's slug
/// and the prompt `claude -p` reads on stdin; a value the session prints is scrubbed from
/// files/answer.md. The real argv carries --disallowedTools (B-192).
#[tokio::test]
async fn a_run_scrubs_prompt_slug_row_session_input_and_answer() {
    let _g = LOCK.lock().await;
    install_memory_vault();
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("runs");
    let fake_dir = tmp.path().join("fake");
    let program = fake_claude(&fake_dir);
    let v = canary();
    std::fs::write(
        fake_dir.join("out.txt"),
        format!("Logged in with {v}. Done."),
    )
    .unwrap();
    let store = Arc::new(MemStore::new());
    let r = runner(
        store.clone(),
        Arc::new(ClaudeLauncher::print(program)),
        &root,
    );
    let f = timeout(CAP, r.execute(spec(&format!("{v} please log in"))))
        .await
        .expect("run within the cap")
        .unwrap();
    assert_eq!(
        f.status,
        RunStatus::Done,
        "{:?}",
        f.launch_problem.as_deref().map(redact)
    );

    assert_clean(&f.run.0, false, "the run id");
    assert!(
        f.run.0.ends_with("-vault-test-please-log-in"),
        "{}",
        redact(&f.run.0)
    );
    let prompt_md = std::fs::read_to_string(f.folder.join("prompt.md")).unwrap();
    assert_clean(&prompt_md, true, "prompt.md");
    let row = store.run_get(&f.run).await.unwrap().unwrap();
    assert_clean(&row.prompt, true, "runs.prompt");
    assert_clean(&row.folder, false, "runs.folder");
    let stdin = std::fs::read_to_string(fake_dir.join("stdin.txt")).unwrap();
    // The run preamble (B-563, D-140 (10)), one blank line, then the scrubbed task.
    assert_clean(&stdin, true, "session stdin");
    assert!(
        stdin.starts_with(&format!("Harness run {}.\n", f.run.0)),
        "stdin lacks the run preamble"
    );
    assert_eq!(
        stdin.split_once("\n\n").map(|(_, t)| t),
        Some(format!("{LITERAL} please log in").as_str())
    );
    let answer = std::fs::read_to_string(f.folder.join("files").join(ANSWER_FILE)).unwrap();
    assert_clean(&answer, true, "answer.md");
    assert_eq!(answer, format!("Logged in with {LITERAL}. Done."));
    let report = std::fs::read_to_string(f.folder.join("report.md")).unwrap();
    assert_clean(&report, true, "report.md");

    // B-192 on the real process: -p, then the denied commands.
    let args = std::fs::read_to_string(fake_dir.join("args.txt")).unwrap();
    let mut want = vec!["-p", "--disallowedTools"];
    want.extend(DISALLOWED_TOOLS);
    assert_eq!(args.lines().collect::<Vec<_>>(), want);
    let env_seen = std::fs::read_to_string(fake_dir.join("env.txt")).unwrap();
    let (run_id, runs_root) = env_seen.split_once('\n').unwrap();
    assert_eq!(run_id, f.run.0);
    assert!(Path::new(runs_root).is_absolute(), "{runs_root}");
}

// ------------------------------------------------------------------ should fix 4

/// A hung `claude -p` session is killed at the cap; the run is failed, with the launch
/// event and the report saying why.
#[tokio::test]
async fn a_hung_session_is_killed_at_the_cap_and_the_run_fails() {
    let _g = LOCK.lock().await;
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("runs");
    let fake_dir = tmp.path().join("fake");
    let mut l = ClaudeLauncher::print(fake_claude(&fake_dir));
    std::fs::write(fake_dir.join("hang"), b"").unwrap();
    l.max_session = Duration::from_secs(2);
    let store = Arc::new(MemStore::new());
    let r = runner(store.clone(), Arc::new(l), &root);
    let f = timeout(CAP, r.execute(spec("wait forever")))
        .await
        .expect("the cap ended the session")
        .unwrap();
    assert!(fake_dir.join("hanging").is_file(), "the session did hang");
    assert_eq!(f.status, RunStatus::Failed);
    let why = f.launch_problem.clone().unwrap();
    assert!(
        why.contains("past its 2s cap and was killed"),
        "{}",
        redact(&why)
    );
    let row = store.run_get(&f.run).await.unwrap().unwrap();
    assert_eq!(row.status, RunStatus::Failed);
    assert!(row.ended.is_some());
    let ev = store.ledger_events(&f.run).await.unwrap();
    assert!(
        ev.iter()
            .any(|(_, e)| e.tool == sched::LAUNCH_TOOL && !e.ok && e.result_summary == why),
        "launch event"
    );
    let report = std::fs::read_to_string(f.folder.join("report.md")).unwrap();
    assert!(report.contains("Result: failed ("), "{report}");
}

/// stdout is capped: the answer keeps at most `max_answer_bytes`, and a vault value cut by
/// the cap is never written in part; one inside the kept part is scrubbed.
#[tokio::test]
async fn the_answer_is_capped_without_writing_part_of_a_value() {
    let _g = LOCK.lock().await;
    install_memory_vault();
    let v = canary();
    let cap = 1000;
    let keep = cap + ANSWER_SCRUB_SLACK;
    for (case, out, pad) in [
        // The value straddles the end of what is read before scrubbing.
        ("cut", format!("{}{v}", "a".repeat(keep - 4)), 100usize),
        // The value lies inside the kept part; far more output follows.
        ("kept", format!("{}{v}", "a".repeat(10)), keep + 10),
    ] {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("runs");
        let fake_dir = tmp.path().join("fake");
        let mut l = ClaudeLauncher::print(fake_claude(&fake_dir));
        l.max_answer_bytes = cap;
        std::fs::write(fake_dir.join("out.txt"), &out).unwrap();
        std::fs::write(fake_dir.join("pad.txt"), pad.to_string()).unwrap();
        let store = Arc::new(MemStore::new());
        let r = runner(store, Arc::new(l), &root);
        let f = timeout(CAP, r.execute(spec("print a lot")))
            .await
            .expect("run within the cap")
            .unwrap();
        assert_eq!(f.status, RunStatus::Done, "{case}");
        let answer = std::fs::read_to_string(f.folder.join("files").join(ANSWER_FILE)).unwrap();
        let total = out.len() + pad;
        let (body, note) = answer
            .rsplit_once("\n\n[answer cut: kept ")
            .expect("cut note");
        assert!(body.len() <= cap, "{case}: {} bytes kept", body.len());
        assert!(
            note.ends_with(&format!(" of {total} bytes]\n")),
            "{case}: {note}"
        );
        assert!(
            !answer.contains(&v[..4]),
            "{case}: part of the value written"
        );
        assert_clean(&answer, case == "kept", case);
    }
}

// ------------------------------------------------------------------ should fix 7

/// Records each request and exits 0. Never calls a model (rule 1).
#[derive(Default)]
struct FakeLauncher {
    seen: Mutex<Vec<(RunId, String)>>,
}

#[async_trait]
impl Launcher for FakeLauncher {
    async fn launch(&self, req: LaunchRequest<'_>) -> Result<LaunchExit, LaunchError> {
        self.seen
            .lock()
            .unwrap()
            .push((req.run.id.clone(), req.prompt.to_owned()));
        Ok(LaunchExit::OK)
    }
}

/// harnessd (ReopenVault) re-opens the vault per firing: a value added with `harness vault
/// set` after it started is scrubbed from the next firing, and a vault that cannot be
/// opened stops the firing before anything is written.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn harnessd_scrubs_a_value_added_after_it_started() {
    let _g = LOCK.lock().await;
    let tmp = tempfile::tempdir().unwrap();
    let e = env(tmp.path());
    let v = canary();
    // harnessd's startup install: the vault has no entries yet.
    e.install_vault().unwrap();
    assert_eq!(harness_core::scrub(v), v);

    let store = Arc::new(MemStore::new());
    let fake = Arc::new(FakeLauncher::default());
    let start = chrono::DateTime::parse_from_rfc3339("2026-10-25T12:00:30Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    let clock = Arc::new(ManualClock::new(start));
    let (tx, mut fired) = tokio::sync::mpsc::unbounded_channel();
    let daemon = Daemon::new(
        store.clone(),
        Arc::new(ReopenVault {
            env: e.clone(),
            inner: fake.clone(),
        }),
        clock.clone(),
        e.runs_root.clone(),
        Some(tx),
    )
    .unwrap();
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let harnessd = tokio::spawn(daemon.run(async move {
        let _ = stopped.await;
    }));

    // `harness vault set test` in another process: it never touches this process's scrubber.
    let c = ctx(e.clone(), store.clone());
    let (r, _) = run_cli(&c, &["vault", "set", "test"]).await;
    r.unwrap();
    assert_eq!(harness_core::scrub(v), v, "vault set installs nothing");
    // A row with the raw value (written before the value was in the vault).
    let id = store
        .schedule_add(Schedule {
            id: 0,
            cron: "*/1 * * * *".into(),
            prompt: format!("{v} check mail"),
            kind: ScheduleKind::Cron,
            watch_target: None,
            enabled: true,
            last_run: None,
            run_id: None,
        })
        .await
        .unwrap();
    sched::notify_changed(&e.runs_root).unwrap();

    let due = timeout(CAP, clock.next_deadline()).await.unwrap();
    clock.set(due);
    let f = timeout(CAP, fired.recv()).await.unwrap().unwrap();
    assert_eq!(f.schedule, id);
    let f = f.result.expect("firing succeeded");
    assert_clean(&f.run.0, false, "the run id");
    let prompt_md = std::fs::read_to_string(f.folder.join("prompt.md")).unwrap();
    assert_clean(&prompt_md, true, "prompt.md");
    let row = store.run_get(&f.run).await.unwrap().unwrap();
    assert_clean(&row.prompt, true, "runs.prompt");
    let seen = fake.seen.lock().unwrap().clone();
    assert_eq!(seen, vec![(f.run.clone(), format!("{LITERAL} check mail"))]);

    // The vault breaks: the next firing does not start, and writes nothing.
    std::fs::write(&e.vault_path, b"not an age file").unwrap();
    let runs_before = std::fs::read_dir(&e.runs_root).unwrap().count();
    let due = timeout(CAP, clock.next_deadline()).await.unwrap();
    clock.set(due);
    let f = timeout(CAP, fired.recv()).await.unwrap().unwrap();
    match f.result {
        Err(SchedError::BeforeRun(e)) => assert_clean(&e.0, false, "the refusal"),
        other => panic!("expected BeforeRun, got {:?}", other.map(|f| f.run)),
    }
    assert_eq!(
        std::fs::read_dir(&e.runs_root).unwrap().count(),
        runs_before
    );
    assert_eq!(fake.seen.lock().unwrap().len(), 1);

    stop.send(()).unwrap();
    timeout(CAP, harnessd).await.unwrap().unwrap().unwrap();
}
