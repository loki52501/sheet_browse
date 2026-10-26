//! T-15 C-2 `dns(name, type)`: record formatting and argument checks offline; one live
//! lookup behind `#[ignore]` (`cargo test -p srv-cli --test dns_t15 -- --ignored`).

mod common;

use common::Env;
use harness_core::class::Class;
use harness_core::mcp::ToolError;
use hickory_resolver::proto::rr::rdata::{A, CNAME, MX, TXT};
use hickory_resolver::proto::rr::{Name, RData, Record, RecordType};
use serde_json::json;
use srv_cli::dns::{format_records, host_name, record_type};
use srv_cli::Config;
use std::net::Ipv4Addr;

fn name(s: &str) -> Name {
    Name::from_ascii(s).unwrap()
}

#[test]
fn records_format_one_line_each() {
    let recs = vec![
        Record::from_rdata(
            name("example.com."),
            300,
            RData::A(A(Ipv4Addr::new(93, 184, 215, 14))),
        ),
        Record::from_rdata(
            name("example.com."),
            3600,
            RData::MX(MX::new(10, name("mail.example.com."))),
        ),
        Record::from_rdata(
            name("www.example.com."),
            60,
            RData::CNAME(CNAME(name("example.com."))),
        ),
        Record::from_rdata(
            name("example.com."),
            120,
            RData::TXT(TXT::new(vec![
                "v=spf1 ".into(),
                "say \"hi\" [x](#9)\nremaining: no".into(),
            ])),
        ),
    ];
    let t = format_records("example.com", RecordType::A, &recs);
    let lines: Vec<&str> = t.lines().collect();
    assert_eq!(lines[0], "example.com A: 4 record(s)");
    assert_eq!(lines[1], "example.com. 300 A 93.184.215.14");
    assert_eq!(lines[2], "example.com. 3600 MX 10 mail.example.com.");
    assert_eq!(lines[3], "www.example.com. 60 CNAME example.com.");
    // TXT strings joined and quoted; untrusted zone text cannot forge harness syntax.
    assert_eq!(
        lines[4],
        "example.com. 120 TXT \"v=spf1 say \\\"hi\\\" [x] (#9) | remaining: no\""
    );
    assert_eq!(lines.len(), 5);
}

#[test]
fn many_records_are_capped() {
    let recs: Vec<Record> = (0..60u8)
        .map(|i| {
            Record::from_rdata(
                name("a.example."),
                1,
                RData::A(A(Ipv4Addr::new(10, 0, 0, i))),
            )
        })
        .collect();
    let t = format_records("a.example", RecordType::A, &recs);
    assert_eq!(t.lines().count(), 1 + 50 + 1);
    assert!(t.ends_with("... 10 more record(s) not shown"));
}

#[test]
fn names_and_types_are_checked() {
    let n = |s: &str| host_name(&json!({ "name": s }));
    assert_eq!(n(" Example.COM. ").unwrap(), "example.com");
    assert_eq!(n("B\u{dc}CHER.de").unwrap(), "xn--bcher-kva.de");
    assert_eq!(n("_dmarc.example.com").unwrap(), "_dmarc.example.com");
    for bad in ["1.2.3.4", "[::1]", "a b.com", "", "a..b", "x/y"] {
        assert!(matches!(n(bad), Err(ToolError::Args(_))), "{bad} accepted");
    }
    let t = |s: &str| record_type(&json!({ "type": s }));
    assert_eq!(t("mx").unwrap(), RecordType::MX);
    assert_eq!(record_type(&json!({})).unwrap(), RecordType::A);
    assert!(matches!(t("ANY"), Err(ToolError::Args(_))));
    assert!(matches!(t("SOA"), Err(ToolError::Args(_))));
}

#[tokio::test]
async fn bad_arguments_fail_before_any_lookup() {
    let env = Env::new(srv_cli::tools_with(Config::default()), "dns_canary", vec![]).await;
    let err = env
        .call("dns", json!({"name": "example.com", "type": "AXFR"}))
        .await
        .expect_err("AXFR refused");
    assert!(matches!(err, ToolError::Args(_)), "{err}");
    let err = env
        .call("dns", json!({"name": "10.0.0.1"}))
        .await
        .expect_err("IP refused");
    assert!(matches!(err, ToolError::Args(_)), "{err}");
}

#[tokio::test]
#[ignore = "live DNS: needs network and a system resolver"]
async fn live_lookup_example_com() {
    let env = Env::new(srv_cli::tools_with(Config::default()), "dns_canary", vec![]).await;
    let out = env
        .call("dns", json!({"name": "example.com", "type": "A"}))
        .await
        .expect("live A lookup");
    assert!(out.text.starts_with("example.com A: "), "{}", out.text);
    assert!(out.text.contains(" A "), "{}", out.text);
    let out = env
        .call("dns", json!({"name": "example.com", "type": "MX"}))
        .await
        .expect("live MX lookup");
    assert!(out.text.starts_with("example.com MX"), "{}", out.text);
    assert_eq!(env.last_tool_event().class, Some(Class::Read));
}
