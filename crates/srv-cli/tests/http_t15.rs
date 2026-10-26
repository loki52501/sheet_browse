//! T-15 C-2 `cloudflare` and C-3 `flags` against an in-test HTTP server on 127.0.0.1.
//! The vault holds the canary as the API token. The server records the auth header it got
//! (so the test can prove the value reached the wire) and reflects it in its JSON body, the
//! worst case: the wrapper must scrub it from the returned text, errors and ledger.
//! D-541: GET is Read, POST/PUT/PATCH Account, DELETE Delete.

mod common;

use common::{assert_clean, canary, last_card_class, Env, Tmp};
use harness_core::class::Class;
use harness_core::gate::AskOutcome;
use harness_core::mcp::ToolError;
use harness_core::memory::LedgerMode;
use serde_json::json;
use srv_cli::Config;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

#[derive(Debug, Clone)]
struct Seen {
    method: String,
    target: String,
    /// The header named by the server's `auth_header`, raw.
    auth: Option<String>,
    body: String,
}

struct Server {
    base: String,
    seen: Arc<Mutex<Vec<Seen>>>,
    _task: tokio::task::JoinHandle<()>,
}

impl Server {
    fn seen(&self) -> Vec<Seen> {
        self.seen.lock().unwrap().clone()
    }
}

/// Answers every request with `status` and `{"success":..,"result":[..],"echo":<auth>}`.
async fn server(status: u16, auth_header: &'static str) -> Server {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let s2 = seen.clone();
    let task = tokio::spawn(async move {
        while let Ok((sock, _)) = listener.accept().await {
            let seen = s2.clone();
            tokio::spawn(async move {
                let _ = handle(sock, seen, status, auth_header).await;
            });
        }
    });
    Server {
        base: format!("http://{addr}"),
        seen,
        _task: task,
    }
}

async fn handle(
    mut sock: TcpStream,
    seen: Arc<Mutex<Vec<Seen>>>,
    status: u16,
    auth_header: &str,
) -> std::io::Result<()> {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 4096];
    let head_end = loop {
        let n = sock.read(&mut tmp).await?;
        if n == 0 {
            return Ok(());
        }
        buf.extend_from_slice(&tmp[..n]);
        if let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break p + 4;
        }
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).into_owned();
    let mut lines = head.split("\r\n");
    let mut first = lines.next().unwrap_or("").split(' ');
    let method = first.next().unwrap_or("").to_string();
    let target = first.next().unwrap_or("").to_string();
    let mut auth = None;
    let mut len = 0usize;
    for l in lines {
        if let Some((k, v)) = l.split_once(':') {
            if k.eq_ignore_ascii_case(auth_header) {
                auth = Some(v.trim().to_string());
            }
            if k.eq_ignore_ascii_case("content-length") {
                len = v.trim().parse().unwrap_or(0);
            }
        }
    }
    let mut body = buf[head_end..].to_vec();
    while body.len() < len {
        let n = sock.read(&mut tmp).await?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&tmp[..n]);
    }
    let reply = json!({
        "success": (200..300).contains(&status),
        "result": [{"id": "z1", "name": "example.com"}],
        "echo": auth.clone().unwrap_or_default(),
    })
    .to_string();
    seen.lock().unwrap().push(Seen {
        method,
        target,
        auth,
        body: String::from_utf8_lossy(&body).into_owned(),
    });
    let resp = format!(
        "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}",
        reply.len()
    );
    sock.write_all(resp.as_bytes()).await?;
    sock.shutdown().await
}

async fn cf_env(srv: &Server, answers: Vec<AskOutcome>) -> Env {
    let cfg = Config {
        cloudflare_api: format!("{}/client/v4", srv.base),
        ..Config::default()
    };
    Env::new(srv_cli::tools_with(cfg), "cf_token", answers).await
}

#[tokio::test]
async fn cloudflare_get_sends_the_vault_header_and_returns_scrubbed_json() {
    let srv = server(200, "authorization").await;
    let env = cf_env(&srv, vec![]).await;
    let out = env
        .call("cloudflare", json!({"path": "/zones?name=example.com"}))
        .await
        .expect("GET zones");

    let seen = srv.seen();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].method, "GET");
    assert_eq!(seen[0].target, "/client/v4/zones?name=example.com");
    let want = format!("Bearer {}", canary());
    assert!(
        seen[0].auth.as_deref() == Some(want.as_str()),
        "the resolved token did not reach the Authorization header"
    );

    assert_clean(&out.text, "returned text");
    assert!(out
        .text
        .starts_with("cloudflare GET /zones?name=example.com: HTTP 200\n"));
    assert!(out.text.contains("\"echo\":\"Bearer @vault(cf_token)\""));
    assert!(out.text.contains("\"name\":\"example.com\""));
    let e = env.last_tool_event();
    assert_eq!(e.class, Some(Class::Read));
    assert_eq!(e.mode, Some(LedgerMode::Allow));
    assert_clean(&env.everything_written(), "ledger or run folder");
    assert!(env.asker.cards().is_empty());
}

#[tokio::test]
async fn cloudflare_writes_ask_and_deletes_ask_as_delete() {
    let srv = server(200, "authorization").await;
    let env = cf_env(
        &srv,
        vec![AskOutcome::AnsweredProceed, AskOutcome::AnsweredSkip],
    )
    .await;
    let out = env
        .call(
            "cloudflare",
            json!({
                "method": "POST",
                "path": "/zones/z1/dns_records",
                "body": {"type": "TXT", "name": "_t.example.com", "content": "hello"}
            }),
        )
        .await
        .expect("approved POST");
    assert_clean(&out.text, "returned text");
    assert_eq!(last_card_class(&env.asker), Some(Class::Account));
    let card = env.asker.cards()[0].clone();
    assert_eq!(card.site.as_deref(), Some("cloudflare.com"));
    assert!(card
        .what
        .starts_with("cloudflare POST /zones/z1/dns_records"));
    let seen = srv.seen();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].method, "POST");
    assert!(
        seen[0].body.contains("\"content\":\"hello\""),
        "{}",
        seen[0].body
    );
    assert_eq!(env.last_tool_event().class, Some(Class::Account));

    let out = env
        .call(
            "cloudflare",
            json!({"method": "delete", "path": "/zones/z1/dns_records/r1"}),
        )
        .await
        .expect("skipped DELETE");
    assert!(out.text.starts_with("skipped:"), "{}", out.text);
    assert_eq!(last_card_class(&env.asker), Some(Class::Delete));
    assert_eq!(srv.seen().len(), 1, "a skipped DELETE sends nothing");
    assert_clean(&env.everything_written(), "ledger or run folder");
}

#[tokio::test]
async fn cloudflare_error_reflecting_the_token_is_scrubbed() {
    let srv = server(403, "authorization").await;
    let env = cf_env(&srv, vec![]).await;
    let err = env
        .call("cloudflare", json!({"path": "/user/tokens/verify"}))
        .await
        .expect_err("HTTP 403");
    let msg = err.to_string();
    assert_clean(&msg, "error message");
    assert!(msg.contains("HTTP 403"));
    assert!(msg.contains("@vault(cf_token)"));
    assert!(!env.last_tool_event().ok);
    assert_clean(&env.everything_written(), "ledger or run folder");
}

#[tokio::test]
async fn cloudflare_paths_cannot_leave_the_base_or_carry_vault() {
    let srv = server(200, "authorization").await;
    let env = cf_env(&srv, vec![]).await;
    for args in [
        json!({"path": "//evil.example/x"}),
        json!({"path": "/../x"}),
        json!({"path": "zones"}),
        json!({"path": "/zones", "method": "TRACE"}),
        json!({"path": "/zones", "method": "POST", "body": {"content": "@vault(cf_token)"}}),
    ] {
        let err = env
            .call("cloudflare", args.clone())
            .await
            .expect_err("refused");
        assert!(matches!(err, ToolError::Args(_)), "{args}: {err}");
    }
    assert!(srv.seen().is_empty());
    assert!(env.asker.cards().is_empty());
}

fn flags_file(dir: &Tmp, base: &str) -> std::path::PathBuf {
    let p = dir.0.join("flags.toml");
    std::fs::write(
        &p,
        format!(
            "[providers.Demo]\napi = \"{base}/api\"\nauth = \"Bearer @vault(flag_token)\"\n\
             header = \"X-Flags-Auth\"\nread = \"/environments/{{env}}/flags\"\ndomain = \"flags.example\"\n"
        ),
    )
    .unwrap();
    p
}

async fn flags_env(dir: &Tmp, srv: &Server, answers: Vec<AskOutcome>) -> Env {
    let cfg = Config {
        flags_file: flags_file(dir, &srv.base),
        ..Config::default()
    };
    Env::new(srv_cli::tools_with(cfg), "flag_token", answers).await
}

#[tokio::test]
async fn flags_read_uses_the_provider_header_and_is_scrubbed() {
    let dir = Tmp::new("flags");
    let srv = server(200, "x-flags-auth").await;
    let env = flags_env(&dir, &srv, vec![]).await;
    let out = env
        .call("flags", json!({"provider": "DEMO", "env": "production"}))
        .await
        .expect("flags read");
    let seen = srv.seen();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].method, "GET");
    assert_eq!(seen[0].target, "/api/environments/production/flags");
    let want = format!("Bearer {}", canary());
    assert!(
        seen[0].auth.as_deref() == Some(want.as_str()),
        "the resolved token did not reach the provider header"
    );
    assert_clean(&out.text, "returned text");
    assert!(out.text.contains("@vault(flag_token)"));
    assert_eq!(env.last_tool_event().class, Some(Class::Read));
    assert_clean(&env.everything_written(), "ledger or run folder");
}

#[tokio::test]
async fn flags_writes_ask_and_unknown_providers_are_refused() {
    let dir = Tmp::new("flagsw");
    let srv = server(200, "x-flags-auth").await;
    let env = flags_env(
        &dir,
        &srv,
        vec![AskOutcome::AnsweredSkip, AskOutcome::AnsweredSkip],
    )
    .await;
    let out = env
        .call(
            "flags",
            json!({"provider": "demo", "env": "production", "method": "PATCH",
                   "path": "/environments/{env}/flags/new-ui", "body": {"on": true}}),
        )
        .await
        .unwrap();
    assert!(out.text.starts_with("skipped:"), "{}", out.text);
    assert_eq!(last_card_class(&env.asker), Some(Class::Account));
    assert_eq!(env.asker.cards()[0].site.as_deref(), Some("flags.example"));
    let out = env
        .call(
            "flags",
            json!({"provider": "demo", "env": "production", "method": "DELETE",
                   "path": "/environments/{env}/flags/old"}),
        )
        .await
        .unwrap();
    assert!(out.text.starts_with("skipped:"), "{}", out.text);
    assert_eq!(last_card_class(&env.asker), Some(Class::Delete));
    assert!(srv.seen().is_empty());

    for args in [
        json!({"provider": "nope", "env": "production"}),
        json!({"provider": "demo", "env": "../x"}),
        json!({"provider": "demo", "env": "production", "method": "POST"}),
    ] {
        let err = env.call("flags", args.clone()).await.expect_err("refused");
        assert!(matches!(err, ToolError::Args(_)), "{args}: {err}");
    }
    assert!(srv.seen().is_empty());
}
