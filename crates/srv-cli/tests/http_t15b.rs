//! T-15b item 4 (D-593, rule 5) through the H-6 wrapper: an API that labels an HTML page
//! `text/plain` (a proxy, login or challenge page) does not get the page shown to the
//! model; a real text/plain answer next to it still is.

mod common;

use common::{assert_clean, Env};
use serde_json::json;
use srv_cli::Config;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// A server on 127.0.0.1 answering every request with `body` as `text/plain`.
async fn plain_server(body: &'static str) -> String {
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
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/plain; charset=utf-8\r\n\
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

async fn cf(base: &str) -> Env {
    let cfg = Config {
        cloudflare_api: format!("{base}/client/v4"),
        ..Config::default()
    };
    Env::new(srv_cli::tools_with(cfg), "cf_token", vec![]).await
}

const PAGE: &str = "<!DOCTYPE html>\n<html><head><title>Attention Required!</title></head>\n\
<body><form id=\"challenge-form\"><input name=\"r\"></form>\n<script>go()</script></body></html>";

#[tokio::test]
async fn html_labelled_text_plain_is_not_shown() {
    let env = cf(&plain_server(PAGE).await).await;
    let out = env
        .call("cloudflare", json!({"path": "/zones"}))
        .await
        .expect("a 200 answer");
    let t = &out.text;
    assert!(t.contains("the body is HTML"), "{t}");
    assert!(t.contains("page title \"Attention Required!\""), "{t}");
    for raw in ["<", "challenge-form", "go()", "<!DOCTYPE"] {
        assert!(!t.contains(raw), "{raw} reached the model: {t}");
    }
    assert_clean(&env.everything_written(), "ledger and run folder");

    // Plain text that mentions one tag is still plain text.
    let env = cf(&plain_server("zone ok: use <name> to pick one").await).await;
    let out = env
        .call("cloudflare", json!({"path": "/zones"}))
        .await
        .expect("a 200 answer");
    assert!(
        out.text.ends_with("zone ok: use <name> to pick one"),
        "{}",
        out.text
    );
}
