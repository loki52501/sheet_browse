//! T-12b item 4 (B-590, D-509): with the system proxy set, srv-web never hands a request
//! that may reach this machine to the proxy. A fetch, feed or api call to a URL written as
//! loopback (`127.0.0.1`, `localhost`) goes direct, so a `@vault` header over plain http
//! (allowed only to loopback) never crosses to the proxy in clear. This binary sets the
//! proxy variables for its own process, so it holds exactly one test (no other test reads
//! the environment). The private-answer half (a public name that resolves to a private
//! address is not proxied) is the unit test `http::tests::a_proxy_never_fetches_a_private_target`,
//! which needs fixed DNS answers.
//!
//! The control half proves the variables are in effect: a public name (a config DNS
//! override, public by D-507) goes to the proxy, and an https api call goes as a CONNECT
//! tunnel that carries no header of ours.

mod common;

use common::*;
use serde_json::json;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// What the proxy got: the request head of every connection.
#[derive(Clone, Default)]
struct Heads(Arc<Mutex<Vec<String>>>);

impl Heads {
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
async fn proxy() -> (u16, Heads) {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    let heads = Heads::default();
    let h2 = heads.clone();
    tokio::spawn(async move {
        while let Ok((mut sock, _)) = l.accept().await {
            let heads = h2.clone();
            tokio::spawn(async move {
                let head = read_head(&mut sock).await;
                heads.0.lock().unwrap().push(head);
                let _ = sock
                    .write_all(
                        b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                    )
                    .await;
                let _ = sock.shutdown().await;
            });
        }
    });
    (port, heads)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn loopback_targets_bypass_the_system_proxy_and_public_ones_use_it() {
    let (proxy_port, proxy) = proxy().await;
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

    let origin = Server::start(vec![
        route(None, "/page", 200, "text/html", "<p>Local page words</p>"),
        route(
            None,
            "/feed",
            200,
            "application/rss+xml",
            "<?xml version=\"1.0\"?><rss version=\"2.0\"><channel><title>L</title>\
             <item><title>Local item</title><link>/i/1</link></item></channel></rss>",
        ),
        Route {
            host: None,
            path: "/echo".into(),
            reply: Reply::EchoHeader("authorization".into()),
        },
    ])
    .await;
    let port = origin.addr.port();
    let tools = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/no-tools");
    // The clients are built here, after the variables are set.
    let e = env(config(&origin, &["public.test"], tools)).await;
    let canary = canary();

    for host in ["127.0.0.1", "localhost"] {
        let t = call(
            &e,
            "fetch",
            json!({ "url": format!("http://{host}:{port}/page") }),
        )
        .await
        .unwrap_or_else(|err| panic!("fetch via {host}: {err}"))
        .text;
        assert!(t.contains("Local page words"), "{host}: {t}");
        let t = call(
            &e,
            "feed",
            json!({ "url": format!("http://{host}:{port}/feed") }),
        )
        .await
        .unwrap_or_else(|err| panic!("feed via {host}: {err}"))
        .text;
        assert!(t.contains("Local item"), "{host}: {t}");
        let out = call(
            &e,
            "api",
            json!({
                "url": format!("http://{host}:{port}/echo"),
                "headers": {"Authorization": "token @vault(gh_token)"}
            }),
        )
        .await
        .unwrap_or_else(|err| panic!("api via {host}: {err}"));
        assert!(out.text.contains("HTTP 200"), "{host}: {}", out.text);
        assert!(
            !out.text.contains(&canary),
            "the echoed vault value was not scrubbed"
        );
    }
    let seen = origin.requests();
    assert_eq!(
        seen.len(),
        6,
        "the origin got every loopback request directly"
    );
    let want = format!("token {canary}");
    assert!(
        seen.iter()
            .filter(|s| s.target == "/echo")
            .all(|s| s.header("authorization") == Some(want.as_str())),
        "the origin got the vault header on a direct request"
    );
    assert!(
        proxy.all().is_empty(),
        "a loopback request went through the system proxy"
    );
    assert!(
        !ledger_dump(&e).contains(&canary),
        "vault value in the ledger"
    );

    // Control: a public name goes through the proxy (a forward request, 502 back).
    let r = call(&e, "fetch", json!({"url": "http://public.test/page"})).await;
    let seen = proxy.all();
    assert_eq!(seen.len(), 1, "the public fetch went to the proxy: {r:?}");
    assert!(
        seen[0].starts_with("GET http://public.test/page HTTP/1.1"),
        "a forward request: {}",
        seen[0].lines().next().unwrap_or("")
    );
    assert_eq!(
        origin.requests().len(),
        6,
        "the origin was not asked directly"
    );

    // Control: an https api call with a vault header uses the proxy as a CONNECT tunnel.
    let r = call(
        &e,
        "api",
        json!({
            "url": "https://api.example.invalid/x",
            "headers": {"Authorization": "token @vault(gh_token)"}
        }),
    )
    .await;
    assert!(r.is_err(), "the proxy answers 502 to CONNECT");
    let seen = proxy.all();
    assert_eq!(seen.len(), 2, "the https request went to the proxy");
    assert!(
        seen[1].starts_with("CONNECT api.example.invalid:443 "),
        "a CONNECT tunnel, not a plain request"
    );
    assert!(
        !seen.iter().any(|h| h.contains(&canary)),
        "the proxy saw the vault value"
    );
}
