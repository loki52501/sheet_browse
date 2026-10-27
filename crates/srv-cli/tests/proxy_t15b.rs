//! T-15b item 1 (D-590): a loopback API base never goes through the system proxy. Plain http
//! is allowed only to a loopback base because the request stays on this machine
//! (`http::secure_base`); a proxy from `HTTP_PROXY`/`ALL_PROXY` would carry that cleartext
//! request, vault header included, to another host. This binary sets the proxy variables
//! for its own process, so it holds exactly one test (no other test reads the environment).
//!
//! The control half proves the variables are in effect: an https base on another host does
//! go to the proxy, as a CONNECT tunnel that carries no header of ours.

mod common;

use common::{assert_clean, canary, Env};
use serde_json::json;
use srv_cli::Config;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// What one listener got: the request head of every connection.
#[derive(Clone, Default)]
struct Heads(Arc<Mutex<Vec<String>>>);

impl Heads {
    fn all(&self) -> Vec<String> {
        self.0.lock().unwrap().clone()
    }
}

/// Reads one request head from `sock`.
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

/// A listener on 127.0.0.1 that records each request head and answers `reply`.
async fn listener(reply: &'static str) -> (u16, Heads) {
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
                let _ = sock.write_all(reply.as_bytes()).await;
                let _ = sock.shutdown().await;
            });
        }
    });
    (port, heads)
}

const ORIGIN_REPLY: &str = "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 11\r\nConnection: close\r\n\r\n{\"ok\":true}";
const PROXY_REPLY: &str =
    "HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";

#[tokio::test]
async fn loopback_bases_bypass_the_system_proxy_and_others_use_it() {
    let (proxy_port, proxy) = listener(PROXY_REPLY).await;
    let (origin_port, origin) = listener(ORIGIN_REPLY).await;
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

    for host in ["127.0.0.1", "localhost"] {
        let cfg = Config {
            cloudflare_api: format!("http://{host}:{origin_port}/client/v4"),
            ..Config::default()
        };
        let env = Env::new(srv_cli::tools_with(cfg), "cf_token", vec![]).await;
        let out = env
            .call("cloudflare", json!({"path": "/zones"}))
            .await
            .unwrap_or_else(|_| panic!("GET via {host} failed"));
        assert!(out.text.contains("HTTP 200"), "{host}: {}", out.text);
        assert_clean(&env.everything_written(), "ledger and run folder");
    }
    let got = origin.all();
    assert_eq!(
        got.len(),
        2,
        "the origin got both loopback requests directly"
    );
    let want = format!("authorization: bearer {}", canary()).to_ascii_lowercase();
    assert!(
        got.iter()
            .all(|h| h.to_ascii_lowercase().contains(&want) && h.starts_with("GET /client/v4/")),
        "the origin got the vault header on a direct GET"
    );
    assert!(
        proxy.all().is_empty(),
        "a loopback request went through the system proxy"
    );

    // Control: a base on another host uses the system proxy, as a CONNECT tunnel.
    let cfg = Config {
        cloudflare_api: "https://api.example.invalid/client/v4".into(),
        ..Config::default()
    };
    let env = Env::new(srv_cli::tools_with(cfg), "cf_token", vec![]).await;
    let r = env.call("cloudflare", json!({"path": "/zones"})).await;
    assert!(r.is_err(), "the proxy answers 502 to CONNECT");
    let seen = proxy.all();
    assert_eq!(seen.len(), 1, "the https request went to the proxy");
    assert!(
        seen[0].starts_with("CONNECT api.example.invalid:443 "),
        "a CONNECT tunnel, not a plain request"
    );
    assert_clean(
        &seen[0].clone(),
        "the proxy's view (the header stays inside TLS)",
    );
}
