//! Live variants of the docs/04 srv-web acceptance items. They need the network, so they
//! are ignored by default: `cargo test -p srv-web --test live -- --ignored`.

mod common;

use common::*;
use serde_json::json;
use std::path::PathBuf;

fn live_config() -> srv_web::Config {
    srv_web::Config {
        tools_root: PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/no-tools"),
        ..srv_web::Config::default()
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "network: fetches https://docs.rs/tokio"]
async fn live_docs_rs_tokio_read_and_find() {
    let e = env(live_config()).await;
    let out = call(&e, "fetch", json!({"url": "https://docs.rs/tokio"}))
        .await
        .expect("fetch");
    let read = out.text.split_once("\n\n").map(|(_, r)| r).unwrap_or("");
    let first = read.lines().find(|l| l.starts_with('#')).unwrap_or("");
    assert!(first.contains("tokio"), "first heading: {first}");
    assert!(!read.contains("Privacy policy"));
    let find = call(&e, "find", json!({"q": "search"}))
        .await
        .expect("find");
    assert!(
        find.text
            .lines()
            .next()
            .unwrap_or("")
            .contains("Find crate"),
        "{}",
        find.text
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "needs the SearXNG compose service: docker compose --profile search up -d searxng"]
async fn live_searxng_search() {
    let e = env(live_config()).await;
    let out = call(&e, "search", json!({"q": "tokio rust runtime"}))
        .await
        .expect("search");
    assert!(
        out.text.starts_with("search \"tokio rust runtime\": "),
        "{}",
        out.text
    );
    assert!(out.text.contains("\"url\": \"https://"), "{}", out.text);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "network: calls https://api.github.com without a token"]
async fn live_github_api_json() {
    let e = env(live_config()).await;
    let out = call(
        &e,
        "api",
        json!({"url": "https://api.github.com/repos/tokio-rs/tokio", "headers": {"X-Probe": "@vault(gh_token)"}}),
    )
    .await
    .expect("api");
    assert!(out.text.contains("\"full_name\": \"tokio-rs/tokio\""));
    assert!(
        !ledger_dump(&e).contains(&canary()),
        "vault value in the ledger"
    );
}
