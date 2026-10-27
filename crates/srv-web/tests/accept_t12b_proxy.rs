//! Tester acceptance for T-12b item 4 (B-590, D-509): with HTTP_PROXY, HTTPS_PROXY and
//! ALL_PROXY set, fetch, feed, api (and search) to a target on this machine or the user's
//! network never go through the proxy; a public name does; a vault header never reaches
//! the proxy in clear.
//!
//! Targets: `localhost`, `127.0.0.2` (127/8 beyond .1), `[::1]`, other spellings of
//! loopback the URL parser normalises (`2130706433`, `127.1`, `localhost.`, `[::ffff:127.0.0.1]`),
//! private IP literals (`10.255.255.1`, `[fd00::1]`), and a name the system resolver answers
//! only with private addresses (this machine's own name, `COMPUTERNAME`/`HOSTNAME`; the test
//! checks that precondition with its own lookup first).
//!
//! This binary sets the proxy variables for its own process, so it holds exactly one test.

mod common;

use common::*;
use serde_json::json;
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

#[derive(Clone, Default)]
struct Log(Arc<Mutex<Vec<String>>>);

impl Log {
    fn all(&self) -> Vec<String> {
        self.0.lock().unwrap().clone()
    }
}

async fn read_head(sock: &mut TcpStream) -> String {
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
    String::from_utf8_lossy(&buf).into_owned()
}

/// A recording proxy on 127.0.0.1 that answers 502 to everything.
async fn proxy() -> (u16, Log) {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    let log = Log::default();
    let l2 = log.clone();
    tokio::spawn(async move {
        while let Ok((mut sock, _)) = l.accept().await {
            let log = l2.clone();
            tokio::spawn(async move {
                let head = read_head(&mut sock).await;
                log.0.lock().unwrap().push(head);
                let _ = sock
                    .write_all(
                        b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                    )
                    .await;
                let _ = sock.shutdown().await;
            });
        }
    });
    (port, log)
}

/// A direct origin on `bind`: answers by path, records each request head.
async fn origin(bind: &str) -> Option<(SocketAddr, Log)> {
    let l = TcpListener::bind(bind).await.ok()?;
    let addr = l.local_addr().unwrap();
    let log = Log::default();
    let l2 = log.clone();
    tokio::spawn(async move {
        while let Ok((mut sock, _)) = l.accept().await {
            let log = l2.clone();
            tokio::spawn(async move {
                let head = read_head(&mut sock).await;
                log.0.lock().unwrap().push(head.clone());
                let target = head.split(' ').nth(1).unwrap_or("").to_string();
                let (ct, body): (&str, String) = if target.starts_with("/feed") {
                    (
                        "application/rss+xml",
                        "<?xml version=\"1.0\"?><rss version=\"2.0\"><channel><title>D</title>\
                         <item><title>Direct item</title><link>/i/1</link></item></channel></rss>"
                            .into(),
                    )
                } else if target.starts_with("/api") || target.starts_with("/echo") {
                    ("application/json", r#"{"ok": "direct answer"}"#.into())
                } else if target.starts_with("/search") {
                    (
                        "application/json",
                        json!({"results": [{"title": "Direct hit", "url": "https://hit.test/",
                                "content": "snippet"}]})
                        .to_string(),
                    )
                } else {
                    ("text/html", "<p>Direct page words</p>".into())
                };
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: {ct}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.shutdown().await;
            });
        }
    });
    Some((addr, log))
}

/// This machine's own name, when the system resolver answers it only with private
/// addresses (the "name resolving private" case).
async fn private_name() -> Option<String> {
    let name = std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .ok()?
        .to_ascii_lowercase();
    let ips: Vec<IpAddr> = tokio::net::lookup_host((name.as_str(), 0))
        .await
        .ok()?
        .map(|a| a.ip())
        .collect();
    let private = |ip: &IpAddr| match ip {
        IpAddr::V4(v) => v.is_loopback() || v.is_private() || v.is_link_local(),
        IpAddr::V6(v) => {
            v.is_loopback()
                || (v.segments()[0] & 0xfe00) == 0xfc00
                || (v.segments()[0] & 0xffc0) == 0xfe80
        }
    };
    (!ips.is_empty() && ips.iter().all(private)).then_some(name)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn private_and_loopback_targets_never_use_the_proxy_public_ones_do() {
    let (proxy_port, proxied) = proxy().await;
    let proxy_url = format!("http://127.0.0.1:{proxy_port}");
    for k in [
        "HTTP_PROXY",
        "http_proxy",
        "HTTPS_PROXY",
        "https_proxy",
        "ALL_PROXY",
        "all_proxy",
    ] {
        std::env::set_var(k, &proxy_url);
    }
    for k in ["NO_PROXY", "no_proxy"] {
        std::env::remove_var(k);
    }

    let (o1, log1) = origin("127.0.0.1:0").await.expect("bind 127.0.0.1");
    let (o2, log2) = origin("127.0.0.2:0").await.expect("bind 127.0.0.2");
    let (o6, log6) = origin("[::1]:0").await.expect("bind [::1]");
    let name = private_name()
        .await
        .expect("precondition: this machine's name resolves only to private addresses");

    // The shared test server only stands in for the public name (a config override).
    let filler = Server::start(vec![]).await;
    let tools = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/no-tools");
    let mut cfg = config(&filler, &["public.test"], tools);
    cfg.searxng_url = format!("http://127.0.0.2:{}", o2.port());
    // Unreachable private literals fail fast instead of at 15 s.
    cfg.timeout = Duration::from_secs(4);
    let e = env(cfg).await;
    let canary = canary();

    // (1) Reachable loopback spellings: every call reaches the origin directly.
    let direct: Vec<(String, &Log)> = vec![
        (format!("http://localhost:{}", o1.port()), &log1),
        (format!("http://127.0.0.2:{}", o2.port()), &log2),
        (format!("http://[::1]:{}", o6.port()), &log6),
        (format!("http://2130706433:{}", o1.port()), &log1),
        (format!("http://127.1:{}", o1.port()), &log1),
    ];
    for (base, log) in &direct {
        let before = log.all().len();
        let t = text(&call(&e, "fetch", json!({ "url": format!("{base}/page") })).await);
        assert!(t.contains("Direct page words"), "fetch {base}: {t}");
        let t = text(&call(&e, "feed", json!({ "url": format!("{base}/feed") })).await);
        assert!(t.contains("Direct item"), "feed {base}: {t}");
        let t = text(&call(&e, "api", json!({ "url": format!("{base}/api") })).await);
        assert!(t.contains("direct answer"), "api {base}: {t}");
        let t = text(
            &call(
                &e,
                "api",
                json!({ "url": format!("{base}/echo"),
                        "headers": {"Authorization": "token @vault(gh_token)"} }),
            )
            .await,
        );
        assert!(t.contains("HTTP 200"), "api+vault {base}: {t}");
        assert!(!t.contains(&canary), "vault value in the api output");
        let heads = log.all();
        assert_eq!(
            heads.len(),
            before + 4,
            "{base}: the origin got all 4 calls"
        );
        assert!(
            heads[before..]
                .iter()
                .any(|h| h.contains(&format!("token {canary}"))),
            "{base}: the vault header reached the origin directly"
        );
    }
    let t = text(&call(&e, "search", json!({"q": "x"})).await);
    assert!(t.contains("Direct hit"), "search on 127.0.0.2: {t}");
    assert!(
        proxied.all().is_empty(),
        "a loopback request went to the proxy: {:?}",
        proxied
            .all()
            .iter()
            .map(|h| h.lines().next().unwrap_or("").to_string())
            .collect::<Vec<_>>()
    );

    // (2) Other loopback spellings and private literals: whatever the outcome, no proxy.
    let mut quiet: Vec<String> = vec![
        format!("http://localhost.:{}", o1.port()),
        format!("http://[::ffff:127.0.0.1]:{}", o1.port()),
        "http://10.255.255.1:9".into(),
        "http://192.168.255.254:9".into(),
        "http://[fd00::1]:9".into(),
    ];
    // (3) A name the system resolver answers only privately, over http and https.
    quiet.push(format!("http://{name}:{}", o1.port()));
    quiet.push(format!("https://{name}:{}", o1.port()));
    // Every violation is collected so one run reports them all; asserted at the end.
    let mut violations: Vec<String> = Vec::new();
    for base in &quiet {
        for (tool, path) in [("fetch", "/page"), ("feed", "/feed"), ("api", "/api")] {
            let before = proxied.all().len();
            let r = call(&e, tool, json!({ "url": format!("{base}{path}") })).await;
            let after = proxied.all();
            if after.len() != before {
                violations.push(format!(
                    "{tool} {base}{path} went through the proxy ({}): {}",
                    after.last().and_then(|h| h.lines().next()).unwrap_or(""),
                    text(&r)
                ));
            }
        }
        let before = proxied.all().len();
        let r = call(
            &e,
            "api",
            json!({ "url": format!("{base}/echo"),
                    "headers": {"Authorization": "token @vault(gh_token)"} }),
        )
        .await;
        let after = proxied.all();
        assert!(!text(&r).contains(&canary), "vault value in the output");
        assert!(
            !after.iter().any(|h| h.contains(&canary)),
            "the proxy saw the vault value in clear ({base})"
        );
        if after.len() != before {
            violations.push(format!(
                "api+vault {base}/echo went through the proxy ({}): {}",
                after.last().and_then(|h| h.lines().next()).unwrap_or(""),
                text(&r)
            ));
        }
    }

    // (4) Control: a public address goes through the proxy (forward request; 502 back), and
    // an https api call with a vault header is a CONNECT tunnel carrying no header of ours.
    // T-12b round 2 (D-515, review-T-12b should-fix 2): a config DNS override such as
    // public.test pins its address and now goes direct, so the public control is a public
    // IP literal (192.0.2.0/24, TEST-NET-1: public by srv-web's ranges, never routed).
    let before = proxied.all().len();
    let r = call(&e, "fetch", json!({"url": "http://public.test/page"})).await;
    assert_eq!(
        proxied.all().len(),
        before,
        "an override host went through the proxy: {}",
        text(&r)
    );
    let r = call(&e, "fetch", json!({"url": "http://192.0.2.10/page"})).await;
    let seen = proxied.all();
    assert_eq!(
        seen.len(),
        before + 1,
        "public fetch used the proxy: {}",
        text(&r)
    );
    assert!(
        seen.last()
            .unwrap()
            .starts_with("GET http://192.0.2.10/page HTTP/1.1"),
        "{}",
        seen.last().unwrap().lines().next().unwrap_or("")
    );
    let before = proxied.all().len();
    let _ = call(&e, "api", json!({"url": "http://api.example.invalid/api"})).await;
    assert_eq!(
        proxied.all().len(),
        before + 1,
        "public api (no override) used the proxy"
    );
    let before = proxied.all().len();
    let _ = call(
        &e,
        "api",
        json!({"url": "https://api.example.invalid/x",
               "headers": {"Authorization": "token @vault(gh_token)"}}),
    )
    .await;
    let seen = proxied.all();
    assert_eq!(seen.len(), before + 1, "https api went to the proxy");
    assert!(seen
        .last()
        .unwrap()
        .starts_with("CONNECT api.example.invalid:443 "));
    assert!(
        !seen.iter().any(|h| h.contains(&canary)),
        "the proxy saw the vault value"
    );
    // A vault header over plain http to a public name is refused before any request.
    let before = proxied.all().len();
    let r = call(
        &e,
        "api",
        json!({"url": "http://api.example.invalid/echo",
               "headers": {"Authorization": "token @vault(gh_token)"}}),
    )
    .await;
    assert!(
        r.is_err(),
        "plain-http vault to a public name: {}",
        text(&r)
    );
    assert_eq!(proxied.all().len(), before, "refused before the proxy");

    let dump = ledger_dump(&e);
    assert!(!dump.contains(&canary), "vault value in the ledger");
    let files = walk(&e.harness.runs_root);
    assert!(
        files.iter().all(|f| !std::fs::read_to_string(f)
            .unwrap_or_default()
            .contains(&canary)),
        "vault value in a run file"
    );
    assert!(
        violations.is_empty(),
        "private targets that went through the proxy:
{}",
        violations.join(
            "
"
        )
    );
}

fn walk(dir: &std::path::Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(dir) {
        for ent in rd.flatten() {
            let p = ent.path();
            if p.is_dir() {
                out.extend(walk(&p));
            } else {
                out.push(p);
            }
        }
    }
    out
}
