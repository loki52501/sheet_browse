//! C-2 `dns(name, type)`: A, AAAA, MX, TXT, CNAME or NS through hickory-resolver with the
//! system's resolver configuration (D-541). Read.

use crate::{text, Config};
use async_trait::async_trait;
use harness_core::class::Class;
use harness_core::mcp::{Classified, ToolCtx, ToolError, ToolHandler, ToolOutput, ToolSpec};
use hickory_resolver::proto::rr::{RData, Record, RecordType};
use hickory_resolver::TokioResolver;
use serde_json::{json, Value};
use std::fmt::Write as _;
use std::sync::Arc;
use tokio::sync::OnceCell;

/// Records shown per answer.
const MAX_RECORDS: usize = 50;
/// Chars per record line.
const RECORD_CHARS: usize = 600;

/// `dns` with [`Config::from_env`].
pub fn tools() -> Vec<ToolSpec> {
    tools_with(&Arc::new(Config::from_env()))
}

pub fn tools_with(cfg: &Arc<Config>) -> Vec<ToolSpec> {
    vec![ToolSpec {
        name: "dns".into(),
        description: "dns(name, type?): DNS records for a host name; type A (default), AAAA, \
                      MX, TXT, CNAME or NS. One line per record: name ttl type data."
            .into(),
        class: Class::Read,
        args: json!({
            "type": "object",
            "properties": {
                "name": {"type": "string", "description": "host name, e.g. example.com"},
                "type": {"type": "string", "enum": ["A", "AAAA", "MX", "TXT", "CNAME", "NS"], "default": "A"}
            },
            "required": ["name"]
        }),
        handler: Arc::new(Dns {
            cfg: cfg.clone(),
            resolver: OnceCell::new(),
        }),
    }]
}

struct Dns {
    cfg: Arc<Config>,
    resolver: OnceCell<TokioResolver>,
}

/// The record type argument (ASCII case-insensitive), default A.
pub fn record_type(args: &Value) -> Result<RecordType, ToolError> {
    let t = match args.get("type") {
        None | Some(Value::Null) => "A".to_string(),
        Some(Value::String(s)) => s.trim().to_ascii_uppercase(),
        Some(_) => return Err(ToolError::Args("type must be a string".into())),
    };
    Ok(match t.as_str() {
        "A" => RecordType::A,
        "AAAA" => RecordType::AAAA,
        "MX" => RecordType::MX,
        "TXT" => RecordType::TXT,
        "CNAME" => RecordType::CNAME,
        "NS" => RecordType::NS,
        other => {
            return Err(ToolError::Args(format!(
                "type {:?} is not one of A, AAAA, MX, TXT, CNAME, NS",
                text::line(other, 20)
            )))
        }
    })
}

/// The host name as an ASCII (Punycode) lowercase name without a trailing dot. IP
/// addresses and anything that is not a host name are refused.
pub fn host_name(args: &Value) -> Result<String, ToolError> {
    let raw = args
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| ToolError::Args("name is required".into()))?;
    let trimmed = raw.trim();
    let trimmed = trimmed.strip_suffix('.').unwrap_or(trimmed);
    let bad = || ToolError::Args(format!("{:?} is not a host name", text::line(raw, 100)));
    match url::Host::parse(trimmed) {
        Ok(url::Host::Domain(d))
            if !d.is_empty()
                && d.len() <= 253
                && d.split('.').all(|l| {
                    !l.is_empty()
                        && l.len() <= 63
                        && l.bytes()
                            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
                }) =>
        {
            Ok(d.to_ascii_lowercase())
        }
        _ => Err(bad()),
    }
}

/// One line per answer record: `<owner> <ttl> <TYPE> <data>`, under a header line. TXT data
/// is the record's strings joined, quoted; everything is neutral (untrusted zone text).
pub fn format_records(name: &str, rtype: RecordType, records: &[Record]) -> String {
    let mut out = format!("{name} {rtype}: {} record(s)", records.len());
    for r in records.iter().take(MAX_RECORDS) {
        let data = match &r.data {
            RData::TXT(t) => {
                let joined: String = t
                    .txt_data
                    .iter()
                    .map(|c| String::from_utf8_lossy(c).into_owned())
                    .collect();
                format!("\"{}\"", joined.replace('\\', "\\\\").replace('"', "\\\""))
            }
            other => {
                let mut s = String::new();
                if write!(s, "{other}").is_err() {
                    s = "(no presentation form)".into();
                }
                s
            }
        };
        out.push('\n');
        out.push_str(&text::line(
            &format!("{} {} {} {data}", r.name, r.ttl, r.record_type()),
            RECORD_CHARS,
        ));
    }
    if records.len() > MAX_RECORDS {
        out.push_str(&format!(
            "\n... {} more record(s) not shown",
            records.len() - MAX_RECORDS
        ));
    }
    out
}

impl Dns {
    async fn resolver(&self) -> Result<&TokioResolver, ToolError> {
        self.resolver
            .get_or_try_init(|| async {
                TokioResolver::builder_tokio()
                    .and_then(|b| b.build())
                    .map_err(|e| ToolError::Failed(format!("dns: resolver setup failed: {e}")))
            })
            .await
    }
}

#[async_trait]
impl ToolHandler for Dns {
    async fn classify(&self, _ctx: &ToolCtx, args: &Value) -> Result<Classified, ToolError> {
        let name = host_name(args)?;
        let t = record_type(args)?;
        Ok(Classified {
            domain: Some(name.clone()),
            what: Some(format!("look up {t} records of {name}")),
            ..Classified::default()
        })
    }

    async fn call(&self, _ctx: &ToolCtx, args: Value) -> Result<ToolOutput, ToolError> {
        let name = host_name(&args)?;
        let t = record_type(&args)?;
        let resolver = self.resolver().await?;
        // The trailing dot makes the name absolute: one query, no search-domain retries.
        let fqdn = format!("{name}.");
        let res = tokio::time::timeout(self.cfg.dns_timeout, resolver.lookup(fqdn, t))
            .await
            .map_err(|_| {
                ToolError::Failed(format!(
                    "dns: no answer for {name} {t} within {} s",
                    self.cfg.dns_timeout.as_secs()
                ))
            })?;
        let text = match res {
            Ok(lookup) => format_records(&name, t, lookup.answers()),
            Err(e) if e.is_nx_domain() => format!("{name} does not exist (NXDOMAIN)"),
            Err(e) if e.is_no_records_found() => format!("{name} {t}: no records"),
            Err(e) => {
                return Err(ToolError::Failed(format!(
                    "dns: lookup {name} {t} failed: {}",
                    text::line(&e.to_string(), 300)
                )))
            }
        };
        Ok(ToolOutput {
            summary: Some(text.lines().next().unwrap_or("").to_string()),
            text,
            ..ToolOutput::default()
        })
    }
}
