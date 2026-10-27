//! T-15b acceptance, adversarial cases for items 2, 3 and 4 (D-591, D-592, D-593), through
//! the H-6 wrapper (`dispatch`). Item 1 (D-590, the proxy) is in accept_t15b_proxy.rs: it sets
//! process-wide proxy variables, so it has its own binary.
//!
//! Fakes: tests/fixtures/fake-gh.* (echoes, `alias list` prints `<log>.aliases`),
//! fake-gh-swap.* (the alias list changes between classify and call) and fake-gh-page.*
//! (prints a page file, to stdout or as a failing call's stderr).

mod common;

use common::{assert_clean, canary, fixtures, last_card_class, Env, Tmp};
use harness_core::class::Class;
use harness_core::gate::AskOutcome;
use harness_core::mcp::ToolError;
use serde_json::json;
use srv_cli::Config;
use std::path::Path;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

fn fake(script: &str, arg: &Path) -> Vec<String> {
    let arg = arg.display().to_string();
    if cfg!(windows) {
        vec![
            fixtures()
                .join(format!("{script}.cmd"))
                .display()
                .to_string(),
            arg,
        ]
    } else {
        vec![
            "sh".into(),
            fixtures()
                .join(format!("{script}.sh"))
                .display()
                .to_string(),
            arg,
        ]
    }
}

async fn gh_env(dir: &Tmp, script: &str, aliases: &str, answers: Vec<AskOutcome>) -> Env {
    let log = dir.0.join("gh.log");
    std::fs::write(&log, "").unwrap();
    std::fs::write(dir.0.join("gh.log.aliases"), aliases).unwrap();
    let cfg = Config {
        gh: fake(script, &log),
        ..Config::default()
    };
    Env::new(srv_cli::tools_with(cfg), "gh_canary", answers).await
}

fn is_args(r: &Result<harness_core::mcp::ToolOutput, ToolError>) -> bool {
    matches!(r, Err(ToolError::Args(_)))
}

// ---------------------------------------------------------------- item 2: gh aliases

/// Aliases that reach `auth token` only through other aliases, `$N` placeholders or a
/// two-word name; aliases that loop or nest too deep; an alias of an api call to another host.
const TRICKY: &str = "a1: a2\na2: a3\na3: auth token\n\
ph: '$1 token'\n\
\"dq tok\": \"auth token\"\n\
ap: api $1\n\
ev: api https://evil.example/x\n\
l1: l2\nl2: l1\n\
b1: b2\nb2: b3\nb3: b4\nb4: b5\nb5: pr view\n\
c1: c2\nc2: c3\nc3: c4\nc4: pr view\n\
ml: |-\n    auth\n    token\n";

#[tokio::test]
async fn item2_adversarial_alias_chains_placeholders_and_loops_are_refused() {
    let dir = Tmp::new("acc-alias");
    let env = gh_env(&dir, "fake-gh", TRICKY, vec![AskOutcome::AnsweredSkip; 4]).await;
    // Refused when the alias runs, and nothing ran (no card, no ok tool event).
    for cmd in [
        "a1",      // alias -> alias -> alias -> auth token
        "ph auth", // '$1 token' + auth = auth token
        "dq tok",  // a two-word alias of auth token
        "ev",      // api to a host that is not GitHub
        "l1",      // a loop: refused at the depth cap, no hang
        "b1 5",    // five deep
        "ml",      // a block scalar: unreadable, fails closed
    ] {
        let r = env.call("gh", json!({ "args": cmd })).await;
        assert!(is_args(&r), "gh {cmd}: {:?}", r.as_ref().map(|o| &o.text));
    }
    assert!(env.asker.cards().is_empty(), "a refused alias never asks");
    assert!(env.events().iter().all(|e| !e.ok || e.server == "core"));

    // Four deep is still followed, and it runs the read it ends in.
    let out = env.call("gh", json!({"args": "c1 5"})).await.expect("c1");
    assert!(out.text.contains("fake gh: pr view 5"), "{}", out.text);
    assert_eq!(env.last_tool_event().class, Some(Class::Read));

    // An argument that adds words raises the class: `ap 'repos/o/r -X DELETE'` runs
    // `api repos/o/r -X DELETE`, so it asks Delete.
    let out = env
        .call("gh", json!({"args": "ap 'repos/o/r -X DELETE'"}))
        .await
        .expect("skipped");
    assert!(out.text.starts_with("skipped"), "{}", out.text);
    assert_eq!(last_card_class(&env.asker), Some(Class::Delete));

    // At `alias set`: an expansion that reaches auth through aliases is refused; one that is
    // `$1 token` cannot be known yet, so it asks (Account) and is refused when it runs (above).
    for cmd in [
        "alias set z a1",
        "alias set z2 'dq tok'",
        "alias set z3 ev",
        "alias set -- z4 'auth token'",
        "alias set --clobber z5 '  auth   token'",
        "alias set z6 l1",
    ] {
        let r = env.call("gh", json!({ "args": cmd })).await;
        assert!(is_args(&r), "gh {cmd}: {:?}", r.as_ref().map(|o| &o.text));
    }
    let n = env.asker.cards().len();
    let out = env
        .call("gh", json!({"args": "alias set z7 '$1 token'"}))
        .await
        .expect("asks");
    assert!(out.text.starts_with("skipped"), "{}", out.text);
    assert_eq!(env.asker.cards().len(), n + 1);
    assert!(last_card_class(&env.asker) >= Some(Class::Account));

    // `alias import` (a file of aliases, unchecked here) asks, and `alias delete` is Delete.
    env.call("gh", json!({"args": "alias import x.yml"}))
        .await
        .expect("skipped");
    assert!(last_card_class(&env.asker) >= Some(Class::Account));
    env.call("gh", json!({"args": "alias delete co"}))
        .await
        .expect("skipped");
    assert_eq!(last_card_class(&env.asker), Some(Class::Delete));
}

/// The alias changes between classify (a read, allowed without a card) and call: the call
/// plans again and refuses, and the fake gh never ran the command.
#[tokio::test]
async fn item2_adversarial_alias_swapped_after_classify_does_not_run() {
    for (second, want_args) in [
        ("x: auth token\n", true),
        ("x: repo delete o/r --yes\n", false),
    ] {
        let dir = Tmp::new("acc-swap");
        std::fs::write(dir.0.join("gh.log.aliases2"), second).unwrap();
        let env = gh_env(&dir, "fake-gh-swap", "x: pr view\n", vec![]).await;
        let r = env.call("gh", json!({"args": "x 5"})).await;
        match &r {
            Err(ToolError::Args(_)) => assert!(want_args, "{second}: {r:?}"),
            Err(ToolError::Failed(m)) => {
                assert!(
                    !want_args && m.contains("above what the gate approved"),
                    "{m}"
                )
            }
            other => panic!("{second}: the swapped alias ran: {other:?}"),
        }
        assert!(env.asker.cards().is_empty());
        assert!(
            !dir.0.join("gh.log.ran").exists(),
            "{second}: gh ran a command"
        );
    }
}

// ---------------------------------------------------------------- item 3: flag values

#[tokio::test]
async fn item3_adversarial_gh_repo_flag_forms_keep_delete() {
    let dir = Tmp::new("acc-repo");
    let env = gh_env(&dir, "fake-gh", "", vec![AskOutcome::AnsweredSkip; 16]).await;
    let cases: &[(&str, Class)] = &[
        ("run -R o/r delete 5", Class::Delete),
        ("run --repo o/r delete 5", Class::Delete),
        ("run --repo=o/r delete 5", Class::Delete),
        ("run -Ro/r delete 5", Class::Delete),
        ("run -R o/r -R x/y delete 5", Class::Delete),
        ("run --verbose delete 5", Class::Delete),
        ("run -R o/r --bogus delete 5", Class::Delete),
        ("repo -R o/r delete --yes", Class::Delete),
        ("release -R o/r delete v1", Class::Delete),
        ("pr -R o/r merge 5", Class::Account),
        ("pr --bogus view 5", Class::Account),
    ];
    for (i, (cmd, want)) in cases.iter().enumerate() {
        let out = env
            .call("gh", json!({ "args": cmd }))
            .await
            .unwrap_or_else(|e| panic!("{cmd}: {e}"));
        assert!(out.text.starts_with("skipped"), "{cmd}: {}", out.text);
        assert_eq!(env.asker.cards().len(), i + 1, "{cmd}");
        assert_eq!(last_card_class(&env.asker), Some(*want), "{cmd}");
    }
    // True negatives: -R's value is the repo even when it is a subcommand's name.
    let n = env.asker.cards().len();
    for cmd in [
        "run -R delete view 5",
        "pr -R o/r view 5",
        "pr view 5 -R o/r",
    ] {
        env.call("gh", json!({ "args": cmd }))
            .await
            .unwrap_or_else(|e| panic!("{cmd}: {e}"));
        assert_eq!(env.last_tool_event().class, Some(Class::Read), "{cmd}");
    }
    assert_eq!(env.asker.cards().len(), n);
    // A host in -R (separate or in a cluster) is refused before anything runs.
    for cmd in [
        "run -R evil.example/o/r delete 5",
        "pr view -cRevil.example/o/r 5",
    ] {
        let r = env.call("gh", json!({ "args": cmd })).await;
        assert!(is_args(&r), "{cmd}");
    }
}

/// The shipped config/cli-allow.toml, with dig run as the `cargo` that runs these tests (dig
/// is not on every machine; cargo answers a bad flag with a non-zero exit and changes
/// nothing). The git and dig rules are the shipped ones, unchanged.
fn shipped_allow(dir: &Tmp) -> std::path::PathBuf {
    let shipped = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config/cli-allow.toml");
    let src = std::fs::read_to_string(shipped)
        .unwrap()
        .replace("\r\n", "\n");
    assert!(src.contains("[programs.dig]\n"));
    let cargo = env!("CARGO").replace('\\', "/");
    let src = src.replace(
        "[programs.dig]\n",
        &format!("[programs.dig]\nbin = '{cargo}'\n"),
    );
    let p = dir.0.join("cli-allow.toml");
    std::fs::write(&p, src).unwrap();
    p
}

#[tokio::test]
async fn item3_cli_value_flags_run_as_read_and_letters_after_them_stay_refused() {
    let dir = Tmp::new("acc-cli");
    let cfg = Config {
        cli_allow_file: shipped_allow(&dir),
        ..Config::default()
    };
    let env = Env::new(srv_cli::tools_with(cfg), "cli_canary", vec![]).await;

    // Runs as Read with no card. git really runs (in this repository); dig runs as cargo,
    // which rejects the flag, so the call fails after the gate let it through as Read.
    for cmd in [
        "git log -Sconfig -n 1 --oneline",
        "git log -pSconfig -n 1 --oneline",
        "git log -GSc -n 1 --oneline",
    ] {
        let out = env
            .call("cli", json!({ "cmd": cmd }))
            .await
            .unwrap_or_else(|e| panic!("{cmd}: {e}"));
        assert!(out.text.contains(": exit 0"), "{cmd}: {}", out.text);
        let e = env.last_tool_event();
        assert_eq!(e.class, Some(Class::Read), "{cmd}");
        assert!(e.ok, "{cmd}");
    }
    for cmd in ["dig -qkey.example.com", "dig -4qfile.example", "dig -qf"] {
        let r = env.call("cli", json!({ "cmd": cmd })).await;
        assert!(
            !matches!(r, Err(ToolError::Args(_))),
            "{cmd} refused: {r:?}"
        );
        assert_eq!(env.last_tool_event().class, Some(Class::Read), "{cmd}");
    }
    assert!(env.asker.cards().is_empty());

    // Refused before anything runs.
    let before = env.events().len();
    let refused = [
        "git log -pc",
        "git log -S x -c",
        "git log -cS x",
        "git log -pSconfig -c",
        "git log -S x -pc",
        "git log -Sx -C.",
        "git log -Sconfig --output=x",
        "git log -Sconfig --output x",
        "git -Sconfig log",
        "git -cSx log",
        "dig -q -fnames.txt",
        "dig -qx.example.com -kkey.conf",
        "dig -4bq -y hmac:n:s",
    ];
    for cmd in refused {
        let r = env.call("cli", json!({ "cmd": cmd })).await;
        assert!(is_args(&r), "{cmd} not refused: {r:?}");
    }
    let events = env.events();
    assert_eq!(
        events.len() - before,
        refused.len(),
        "one event per refusal"
    );
    assert!(events[before..].iter().all(|e| !e.ok && e.mode.is_none()));
    assert!(env.asker.cards().is_empty(), "a refusal never asks");
}

// ---------------------------------------------------------------- item 4: HTML pages

/// A server on 127.0.0.1 answering `status`, `content_type` and `page(auth)`, where `auth`
/// is the Authorization / X-Flags-Auth value it got (reflected: the worst case).
async fn page_server(status: u16, content_type: &'static str, page: fn(&str) -> String) -> String {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((mut sock, _)) = l.accept().await {
            tokio::spawn(async move {
                let mut buf = Vec::new();
                let mut tmp = [0u8; 4096];
                while let Ok(n) = sock.read(&mut tmp).await {
                    if n == 0 {
                        break;
                    }
                    buf.extend_from_slice(&tmp[..n]);
                    if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                }
                let head = String::from_utf8_lossy(&buf).into_owned();
                let auth = head
                    .split("\r\n")
                    .filter_map(|l| l.split_once(':'))
                    .find(|(k, _)| {
                        k.eq_ignore_ascii_case("authorization")
                            || k.eq_ignore_ascii_case("x-flags-auth")
                    })
                    .map(|(_, v)| v.trim().to_string())
                    .unwrap_or_default();
                let body = page(&auth);
                let resp = format!(
                    "HTTP/1.1 {status} X\r\nContent-Type: {content_type}\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.shutdown().await;
            });
        }
    });
    format!("http://{addr}")
}

async fn cf_env(base: &str) -> Env {
    let cfg = Config {
        cloudflare_api: format!("{base}/client/v4"),
        ..Config::default()
    };
    Env::new(srv_cli::tools_with(cfg), "cf_token", vec![]).await
}

async fn flags_env(dir: &Tmp, base: &str) -> Env {
    let p = dir.0.join("flags.toml");
    std::fs::write(
        &p,
        format!(
            "[providers.Demo]\napi = \"{base}/api\"\nauth = \"Bearer @vault(flag_token)\"\n\
             header = \"X-Flags-Auth\"\nread = \"/environments/{{env}}/flags\"\n\
             domain = \"flags.example\"\n"
        ),
    )
    .unwrap();
    let cfg = Config {
        flags_file: p,
        ..Config::default()
    };
    Env::new(srv_cli::tools_with(cfg), "flag_token", vec![]).await
}

/// Raw markup that must never reach the model.
fn assert_no_markup(t: &str, what: &str) {
    for raw in [
        "<",
        "FORM",
        "form",
        "script",
        "SCRIPT",
        "\u{feff}",
        "secret-field",
    ] {
        assert!(!t.contains(raw), "{what}: {raw:?} reached the model: {t}");
    }
}

fn bom_upper(_: &str) -> String {
    "\u{feff}<HTML><HEAD><TITLE>Sign In</TITLE></HEAD>\n<BODY><FORM ACTION=/login>\
     <INPUT NAME=secret-field></FORM><SCRIPT>x()</SCRIPT></BODY></HTML>"
        .into()
}

fn three_tags(_: &str) -> String {
    "  \n<div class=\"secret-field\"><p>Access denied</p><a href=\"/x\">retry</a></div>".into()
}

fn mixed_doctype(_: &str) -> String {
    "\r\n<!DocType HTML><title>Just a moment...</title><form id=secret-field></form>".into()
}

fn reflect_in_title(auth: &str) -> String {
    format!(
        "<!doctype html><html><head><title>token {auth}</title></head><body>\
         <form>secret-field</form></body></html>"
    )
}

#[tokio::test]
async fn item4_adversarial_html_under_json_or_text_labels_is_not_shown() {
    // Cloudflare, 200, labelled JSON, uppercase tags behind a BOM, no doctype.
    let env = cf_env(&page_server(200, "application/json", bom_upper).await).await;
    let out = env
        .call("cloudflare", json!({"path": "/zones"}))
        .await
        .expect("200");
    assert!(out.text.contains("the body is HTML"), "{}", out.text);
    assert!(out.text.contains("\"Sign In\""), "{}", out.text);
    assert_no_markup(&out.text, "cloudflare json-labelled page");
    assert_clean(&env.everything_written(), "ledger and run folder");

    // Flags, 200, text/plain, three plain tags and no document marker.
    let dir = Tmp::new("acc-flags-html");
    let env = flags_env(&dir, &page_server(200, "text/plain", three_tags).await).await;
    let out = env
        .call("flags", json!({"provider": "demo", "env": "production"}))
        .await
        .expect("200");
    assert!(out.text.contains("the body is HTML"), "{}", out.text);
    assert_no_markup(&out.text, "flags text/plain tags");

    // Flags, 200, `application/problem+json`, a mixed-case doctype after CRLF.
    let dir = Tmp::new("acc-flags-doc");
    let env = flags_env(
        &dir,
        &page_server(200, "application/problem+json", mixed_doctype).await,
    )
    .await;
    let out = env
        .call("flags", json!({"provider": "demo", "env": "production"}))
        .await
        .expect("200");
    assert!(out.text.contains("the body is HTML"), "{}", out.text);
    assert_no_markup(&out.text, "flags +json doctype");

    // Cloudflare, 403, labelled JSON, the page reflects the token in its <title>: an
    // error, no markup, and the canary scrubbed from the error, the ledger and the run.
    let env = cf_env(&page_server(403, "application/json", reflect_in_title).await).await;
    let err = env
        .call("cloudflare", json!({"path": "/zones"}))
        .await
        .expect_err("403");
    let m = err.to_string();
    assert!(matches!(err, ToolError::Failed(_)));
    assert!(
        m.contains("HTTP 403") && m.contains("the body is HTML"),
        "{m}"
    );
    assert_clean(&m, "the 403 error");
    assert_no_markup(&m, "cloudflare 403 page");
    assert_clean(&env.everything_written(), "ledger and run folder");

    // True negative: JSON whose strings hold HTML is JSON, and is shown.
    fn json_with_html(_: &str) -> String {
        "{\"result\":{\"html\":\"<html><body>x</body></html>\"}}".into()
    }
    let env = cf_env(&page_server(200, "text/plain", json_with_html).await).await;
    let out = env
        .call("cloudflare", json!({"path": "/zones"}))
        .await
        .expect("200");
    assert!(
        out.text.contains("<html><body>x</body></html>"),
        "{}",
        out.text
    );
}

async fn page_gh(dir: &Tmp, page: &str, fail: bool) -> Env {
    let p = dir.0.join("page.txt");
    std::fs::write(&p, page.as_bytes()).unwrap();
    if fail {
        std::fs::write(dir.0.join("page.txt.fail"), "").unwrap();
    }
    let cfg = Config {
        gh: fake("fake-gh-page", &p),
        ..Config::default()
    };
    Env::new(srv_cli::tools_with(cfg), "gh_canary", vec![]).await
}

#[tokio::test]
async fn item4_adversarial_gh_api_and_gh_errors_hide_html_pages() {
    let page = format!(
        "\u{feff}<HTML><HEAD><TITLE>GHE login {}</TITLE></HEAD><BODY><FORM>secret-field\
         </FORM><SCRIPT>x()</SCRIPT></BODY></HTML>\n",
        canary()
    );
    // gh api prints a GHE login page (BOM, uppercase): not shown, title scrubbed.
    let dir = Tmp::new("acc-gh-page");
    let env = page_gh(&dir, &page, false).await;
    let out = env
        .call("gh", json!({"args": "api repos/o/r"}))
        .await
        .expect("gh api");
    assert!(out.text.contains("HTML page"), "{}", out.text);
    assert!(out.text.contains("GHE login"), "{}", out.text);
    assert_no_markup(&out.text, "gh api page");
    assert_clean(&out.text, "gh api text");
    assert_clean(&env.everything_written(), "ledger and run folder");

    // A failing gh call whose stderr is the page: an error without the page.
    let dir = Tmp::new("acc-gh-fail");
    let env = page_gh(&dir, &page, true).await;
    let err = env
        .call("gh", json!({"args": "pr view 5"}))
        .await
        .expect_err("exit 1");
    let m = err.to_string();
    assert!(m.contains("HTML page"), "{m}");
    assert_no_markup(&m, "gh error page");
    assert_clean(&m, "gh error");
    assert_clean(&env.everything_written(), "ledger and run folder");

    // True negative: a PR body quoting a few tags (no document marker) is shown.
    let dir = Tmp::new("acc-gh-details");
    let env = page_gh(
        &dir,
        "title: x\n<details><summary>log</summary>ok</details>\n",
        false,
    )
    .await;
    let out = env
        .call("gh", json!({"args": "pr view 5"}))
        .await
        .expect("pr view");
    assert!(out.text.contains("<details>"), "{}", out.text);
}

/// D-591 fails closed: when `gh alias list` exits non-zero or prints what cannot be read,
/// a call that may name an alias is refused and gh never runs it.
#[tokio::test]
async fn item2_adversarial_unreadable_alias_list_refuses() {
    // Non-zero exit (not "no aliases configured").
    let dir = Tmp::new("acc-alias-fail");
    std::fs::write(dir.0.join("page.txt.aliasfail"), "").unwrap();
    let env = page_gh(&dir, "ran\n", false).await;
    let err = env
        .call("gh", json!({"args": "tok"}))
        .await
        .expect_err("alias list failed");
    assert!(
        err.to_string()
            .contains("cannot tell whether this names a gh alias"),
        "{err}"
    );
    assert!(env.asker.cards().is_empty());
    assert!(env.events().iter().all(|e| !e.ok || e.server == "core"));
    // A read on the allowlist does not need the list, so it still runs.
    let out = env
        .call("gh", json!({"args": "pr view 5"}))
        .await
        .expect("read");
    assert!(out.text.contains("ran"), "{}", out.text);

    // Output that is not `name: expansion`.
    let dir = Tmp::new("acc-alias-garbage");
    let env = gh_env(&dir, "fake-gh", "  indented first\nx: pr view\n", vec![]).await;
    let err = env
        .call("gh", json!({"args": "x 5"}))
        .await
        .expect_err("unreadable list");
    assert!(
        err.to_string()
            .contains("cannot tell whether this names a gh alias"),
        "{err}"
    );
    assert!(env.events().iter().all(|e| !e.ok || e.server == "core"));
}
