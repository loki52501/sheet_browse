//! Shared test support: an in-process HTTP/1.1 server on 127.0.0.1 (no network), a harness
//! with MemStore, ScriptedAsker and an in-memory vault holding the canary, and `call`,
//! which drives a tool through `harness_core::mcp::dispatch` (classify, gate, run, scrub,
//! ledger), exactly as `serve` does.
#![allow(dead_code)]

use harness_core::memory::{ts_now, LedgerEvent, Run, RunStatus};
use harness_core::testing::{MemStore, ScriptedAsker};
use harness_core::vault::EntryKind;
use harness_core::{
    dispatch, Harness, MemoryStore, RunFolder, ToolError, ToolOutput, ToolRegistry, Vault,
};
use serde_json::Value;
use srv_web::Config;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

pub fn fixture(name: &str) -> Vec<u8> {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name);
    std::fs::read(&p).unwrap_or_else(|_| panic!("fixture {name} missing"))
}

/// The planted canary (SB_VAULT_CANARY when set). Tests compare against it with
/// `contains`/`==` inside `assert!`, never printing it.
pub fn canary() -> String {
    std::env::var("SB_VAULT_CANARY")
        .ok()
        .filter(|c| !c.is_empty())
        .unwrap_or_else(|| "srv-web-canary-not-a-secret-q7".to_owned())
}

/// What a route answers.
#[derive(Clone)]
pub enum Reply {
    Fixed {
        status: u16,
        headers: Vec<(String, String)>,
        body: Vec<u8>,
    },
    /// `{"echo": "<value of this request header>"}`.
    EchoHeader(String),
    /// `{"echo64": "<base64 of this request header>"}`.
    EchoHeaderBase64(String),
}

#[derive(Clone)]
pub struct Route {
    /// Host header without the port; `None` matches any host.
    pub host: Option<String>,
    /// Path (with query when the route's path has one); exact match.
    pub path: String,
    pub reply: Reply,
}

pub fn route(
    host: Option<&str>,
    path: &str,
    status: u16,
    ctype: &str,
    body: impl Into<Vec<u8>>,
) -> Route {
    Route {
        host: host.map(str::to_string),
        path: path.to_string(),
        reply: Reply::Fixed {
            status,
            headers: vec![("Content-Type".into(), ctype.into())],
            body: body.into(),
        },
    }
}

pub fn redirect(host: Option<&str>, path: &str, location: &str) -> Route {
    Route {
        host: host.map(str::to_string),
        path: path.to_string(),
        reply: Reply::Fixed {
            status: 302,
            headers: vec![("Location".into(), location.into())],
            body: Vec::new(),
        },
    }
}

/// A request the server saw.
#[derive(Clone, Debug)]
pub struct Seen {
    pub method: String,
    pub host: String,
    pub target: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Seen {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

pub struct Server {
    pub addr: SocketAddr,
    pub seen: Arc<Mutex<Vec<Seen>>>,
    /// The routes answered; [`Server::set_routes`] swaps them (a page that changes).
    routes: Arc<Mutex<Vec<Route>>>,
}

impl Server {
    pub async fn start(routes: Vec<Route>) -> Server {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        let seen: Arc<Mutex<Vec<Seen>>> = Arc::default();
        let routes = Arc::new(Mutex::new(routes));
        let log = seen.clone();
        let served = routes.clone();
        tokio::spawn(async move {
            while let Ok((sock, _)) = listener.accept().await {
                let routes = served.lock().unwrap().clone();
                let log = log.clone();
                tokio::spawn(async move {
                    let _ = serve_one(sock, &routes, &log).await;
                });
            }
        });
        Server { addr, seen, routes }
    }

    /// Answers with `routes` from the next connection on.
    pub fn set_routes(&self, routes: Vec<Route>) {
        *self.routes.lock().unwrap() = routes;
    }

    pub fn requests(&self) -> Vec<Seen> {
        self.seen.lock().unwrap().clone()
    }
}

async fn serve_one(
    mut sock: tokio::net::TcpStream,
    routes: &[Route],
    log: &Mutex<Vec<Seen>>,
) -> std::io::Result<()> {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 4096];
    let head_end = loop {
        let n = sock.read(&mut tmp).await?;
        if n == 0 {
            return Ok(());
        }
        buf.extend_from_slice(&tmp[..n]);
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break i;
        }
        if buf.len() > 1 << 20 {
            return Ok(());
        }
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
    let mut lines = head.split("\r\n");
    let first = lines.next().unwrap_or("");
    let mut parts = first.split(' ');
    let method = parts.next().unwrap_or("").to_string();
    let target = parts.next().unwrap_or("").to_string();
    let headers: Vec<(String, String)> = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
        .collect();
    let len: usize = headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, v)| v.parse().ok())
        .unwrap_or(0);
    let mut body = buf[head_end + 4..].to_vec();
    while body.len() < len {
        let n = sock.read(&mut tmp).await?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&tmp[..n]);
    }
    let host = headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("host"))
        .map(|(_, v)| v.split(':').next().unwrap_or("").to_ascii_lowercase())
        .unwrap_or_default();
    let seen = Seen {
        method,
        host: host.clone(),
        target: target.clone(),
        headers,
        body,
    };
    log.lock().unwrap().push(seen.clone());
    let path_only = target.split('?').next().unwrap_or("");
    let found = routes.iter().find(|r| {
        r.host.as_deref().is_none_or(|h| h == host)
            && (r.path == target || (!r.path.contains('?') && r.path == path_only))
    });
    let (status, extra, body): (u16, Vec<(String, String)>, Vec<u8>) = match found.map(|r| &r.reply)
    {
        None => (
            404,
            vec![("Content-Type".into(), "text/plain".into())],
            b"not found".to_vec(),
        ),
        Some(Reply::Fixed {
            status,
            headers,
            body,
        }) => (*status, headers.clone(), body.clone()),
        Some(Reply::EchoHeader(h)) => {
            let v = seen.header(h).unwrap_or("").to_string();
            (
                200,
                vec![("Content-Type".into(), "application/json".into())],
                serde_json::to_vec(&serde_json::json!({ "echo": v })).unwrap(),
            )
        }
        Some(Reply::EchoHeaderBase64(h)) => {
            let v = seen.header(h).unwrap_or("").to_string();
            (
                200,
                vec![("Content-Type".into(), "application/json".into())],
                serde_json::to_vec(&serde_json::json!({ "echo64": base64(v.as_bytes()) })).unwrap(),
            )
        }
    };
    let mut resp = format!(
        "HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n",
        body.len()
    );
    for (k, v) in extra {
        resp.push_str(&format!("{k}: {v}\r\n"));
    }
    resp.push_str("\r\n");
    sock.write_all(resp.as_bytes()).await?;
    sock.write_all(&body).await?;
    sock.shutdown().await
}

/// Standard base64 with padding (test-only; avoids a dev-dependency).
pub fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(T[((n >> (18 - 6 * i)) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

pub struct Env {
    pub _tmp: tempfile::TempDir,
    pub store: Arc<MemStore>,
    pub asker: Arc<ScriptedAsker>,
    pub harness: Harness,
    pub registry: ToolRegistry,
    pub run: RunFolder,
}

/// A config whose `hosts` all resolve to `server`, with tools from `tools_root`.
pub fn config(server: &Server, hosts: &[&str], tools_root: PathBuf) -> Config {
    let mut cfg = Config {
        tools_root,
        searxng_url: format!("http://search.local:{}", server.addr.port()),
        ..Config::default()
    };
    for h in hosts {
        cfg = cfg.with_resolve(h, server.addr);
    }
    cfg.with_resolve("search.local", server.addr)
}

pub async fn env(cfg: Config) -> Env {
    let tmp = tempfile::tempdir().expect("tempdir");
    let runs = tmp.path().join("runs");
    std::fs::create_dir_all(&runs).unwrap();
    let run = RunFolder::create(&runs, "srv-web-test", "srv-web test").expect("run folder");
    let store = Arc::new(MemStore::new());
    store
        .run_start(Run {
            id: run.id.clone(),
            started: ts_now(),
            ended: None,
            prompt: "srv-web test".into(),
            status: RunStatus::Running,
            folder: format!("runs/{}", run.id),
            parent_run: None,
            strict: false,
        })
        .await
        .expect("run_start");
    let vault = Arc::new(Vault::in_memory([(
        "gh_token".to_owned(),
        canary(),
        EntryKind::Secret,
    )]));
    let asker = Arc::new(ScriptedAsker::new());
    let harness = Harness {
        store: store.clone(),
        vault,
        asker: asker.clone(),
        runs_root: runs,
        run: Some(run.id.clone()),
    };
    let registry = ToolRegistry::new(srv_web::tools_with(cfg));
    Env {
        _tmp: tmp,
        store,
        asker,
        harness,
        registry,
        run,
    }
}

pub async fn call(env: &Env, tool: &str, args: Value) -> Result<ToolOutput, ToolError> {
    dispatch("web", &env.registry, &env.harness, tool, args).await
}

/// Text of an Ok output, or the error message.
pub fn text(r: &Result<ToolOutput, ToolError>) -> String {
    match r {
        Ok(o) => o.text.clone(),
        Err(e) => format!("error: {e}"),
    }
}

pub fn events(env: &Env) -> Vec<LedgerEvent> {
    let mut t = env.store.ledger();
    t.sort_by_key(|(id, _)| *id);
    t.into_iter().map(|(_, e)| e).collect()
}

/// Every ledger row as JSON plus the run's ledger.jsonl mirror.
pub fn ledger_dump(env: &Env) -> String {
    let rows = serde_json::to_string(&events(env)).unwrap();
    let jsonl = std::fs::read_to_string(env.run.ledger_jsonl()).unwrap_or_default();
    format!("{rows}\n{jsonl}")
}
