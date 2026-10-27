//! T-15b acceptance item 1 (D-590), adversarial: with HTTP_PROXY/HTTPS_PROXY/ALL_PROXY set
//! (both cases), every loopback spelling the URL parser accepts (`[::1]`, `LOCALHOST`,
//! `127.0.0.2`, the integer form `2130706433`) goes straight to the origin, for cloudflare
//! and for flags; plain http to anything that is not loopback (`localhost.`, `[::ffff:..]`,
//! a `.nip.io` name, a real host) is refused before a request is made, so the proxy never
//! sees a vault header. Sets process-wide variables, so it is the only test in this binary.

mod common;

use common::{assert_clean, canary, Env, Tmp};
use serde_json::json;
use srv_cli::Config;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

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

async fn serve(l: TcpListener, reply: &'static str, heads: Heads) {
    while let Ok((mut sock, _)) = l.accept().await {
        let heads = heads.clone();
        tokio::spawn(async move {
            let head = read_head(&mut sock).await;
            heads.0.lock().unwrap().push(head);
            let _ = sock.write_all(reply.as_bytes()).await;
            let _ = sock.shutdown().await;
        });
    }
}

/// A listener on `ip:0` (None when the address cannot be bound here).
async fn listener(ip: &str, reply: &'static str) -> Option<(u16, Heads)> {
    let l = TcpListener::bind(format!("{ip}:0")).await.ok()?;
    let port = l.local_addr().unwrap().port();
    let heads = Heads::default();
    tokio::spawn(serve(l, reply, heads.clone()));
    Some((port, heads))
}

const ORIGIN_REPLY: &str = "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 11\r\nConnection: close\r\n\r\n{\"ok\":true}";
const PROXY_REPLY: &str =
    "HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";

async fn cf(base: String) -> Env {
    let cfg = Config {
        cloudflare_api: base,
        ..Config::default()
    };
    Env::new(srv_cli::tools_with(cfg), "cf_token", vec![]).await
}

#[tokio::test]
async fn item1_adversarial_loopback_spellings_bypass_the_proxy_and_plain_http_elsewhere_is_refused()
{
    let (proxy_port, proxy) = listener("127.0.0.1", PROXY_REPLY).await.unwrap();
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
    let want = format!("authorization: bearer {}", canary()).to_ascii_lowercase();

    // Loopback spellings, each to its own origin: the origin gets the header directly.
    let (p4, o4) = listener("127.0.0.1", ORIGIN_REPLY).await.unwrap();
    let mut bases = vec![
        (format!("http://LOCALHOST:{p4}/client/v4"), o4.clone()),
        (format!("http://2130706433:{p4}/client/v4"), o4.clone()),
        (format!("http://0x7f.1:{p4}/client/v4"), o4.clone()),
    ];
    let v6 = listener("[::1]", ORIGIN_REPLY).await;
    assert!(v6.is_some(), "::1 cannot be bound on this machine");
    let (p6, o6) = v6.unwrap();
    bases.push((format!("http://[::1]:{p6}/client/v4"), o6.clone()));
    let o2 = listener("127.0.0.2", ORIGIN_REPLY).await;
    if let Some((p2, o2)) = &o2 {
        bases.push((format!("http://127.0.0.2:{p2}/client/v4"), o2.clone()));
    }
    for (base, origin) in &bases {
        let before = origin.all().len();
        let env = cf(base.clone()).await;
        let out = env
            .call("cloudflare", json!({"path": "/zones"}))
            .await
            .unwrap_or_else(|e| panic!("GET {base} failed: {e}"));
        assert!(out.text.contains("HTTP 200"), "{base}: {}", out.text);
        assert_clean(&out.text, "returned text");
        assert_clean(&env.everything_written(), "ledger and run folder");
        let got = origin.all();
        assert_eq!(got.len(), before + 1, "{base}: the origin got the request");
        let head = got.last().unwrap().to_ascii_lowercase();
        assert!(
            head.starts_with("get /client/v4/zones") && head.contains(&want),
            "{base}: the origin did not get the vault header on a direct GET"
        );
    }
    assert!(
        proxy.all().is_empty(),
        "a loopback request went to the proxy"
    );

    // flags uses the same rule: its loopback provider is reached directly.
    let (pf, of) = listener("127.0.0.1", ORIGIN_REPLY).await.unwrap();
    let dir = Tmp::new("acc-proxy-flags");
    let flags = dir.0.join("flags.toml");
    std::fs::write(
        &flags,
        format!(
            "[providers.demo]\napi = \"http://localhost:{pf}/api\"\n\
             auth = \"Bearer @vault(flag_token)\"\nheader = \"X-Flags-Auth\"\n\
             read = \"/environments/{{env}}/flags\"\ndomain = \"flags.example\"\n"
        ),
    )
    .unwrap();
    let cfg = Config {
        flags_file: flags,
        ..Config::default()
    };
    let env = Env::new(srv_cli::tools_with(cfg), "flag_token", vec![]).await;
    env.call("flags", json!({"provider": "demo", "env": "production"}))
        .await
        .expect("flags via loopback");
    let want_f = format!("x-flags-auth: bearer {}", canary()).to_ascii_lowercase();
    assert!(of.all().len() == 1 && of.all()[0].to_ascii_lowercase().contains(&want_f));
    assert_clean(&env.everything_written(), "flags ledger and run folder");
    assert!(
        proxy.all().is_empty(),
        "the flags request went to the proxy"
    );

    // Plain http to a base that is not loopback: refused, nothing sent anywhere.
    for base in [
        format!("http://localhost.:{p4}/client/v4"),
        format!("http://[::ffff:127.0.0.1]:{p4}/client/v4"),
        format!("http://127.0.0.1.nip.io:{p4}/client/v4"),
        "http://api.cloudflare.com/client/v4".to_string(),
    ] {
        let before = o4.all().len();
        let env = cf(base.clone()).await;
        let err = env
            .call("cloudflare", json!({"path": "/zones"}))
            .await
            .expect_err(&base);
        assert!(err.to_string().contains("must be https"), "{base}: {err}");
        assert_clean(&err.to_string(), "the refusal");
        assert_eq!(o4.all().len(), before, "{base}: a request was sent");
    }
    assert!(proxy.all().is_empty(), "plain http went to the proxy");

    // Control: an https base does use the proxy, as a CONNECT carrying no header of ours.
    let env = cf("https://api.example.invalid/client/v4".into()).await;
    assert!(env
        .call("cloudflare", json!({"path": "/zones"}))
        .await
        .is_err());
    let seen = proxy.all();
    assert_eq!(seen.len(), 1, "the https request went to the proxy");
    assert!(seen[0].starts_with("CONNECT api.example.invalid:443 "));
    assert_clean(&seen[0], "the proxy's view");
    assert!(!seen[0].to_ascii_lowercase().contains("authorization"));
    if o2.is_none() {
        eprintln!("note: 127.0.0.2 could not be bound here; that spelling was not tried");
    }
}
