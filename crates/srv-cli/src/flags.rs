//! C-3 `flags(provider, env, method?, path?, body?)`: feature-flag providers over HTTP,
//! configured in `config/flags.toml` (B-540: shipped with no providers and a documented
//! example). A GET of the provider's `read` path is Read; POST, PUT and PATCH write
//! production config and are Account; DELETE is Delete (H-1).
//!
//! ```toml
//! [providers.example]
//! api = "https://flags.example.com/api"      # base URL; every path stays under it
//! auth = "Bearer @vault(example_flags)"      # header value template; must use @vault(..)
//! header = "Authorization"                   # optional, default Authorization
//! read = "/environments/{env}/flags"         # GET path when the call gives none
//! domain = "example.com"                     # optional gate domain, default the api host
//! ```
//!
//! The file is read on every call (edits apply without a restart) and again in `call`, so a
//! file changed after the gate approved the call cannot raise what it does.

use crate::{http, text, Config};
use async_trait::async_trait;
use harness_core::class::Class;
use harness_core::mcp::{Classified, ToolCtx, ToolError, ToolHandler, ToolOutput, ToolSpec};
use harness_core::vault::vault_refs;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

/// One provider, as configured.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Provider {
    pub api: String,
    pub auth: String,
    #[serde(default = "default_header")]
    pub header: String,
    pub read: String,
    #[serde(default)]
    pub domain: Option<String>,
}

fn default_header() -> String {
    "Authorization".into()
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FlagsFile {
    #[serde(default)]
    providers: BTreeMap<String, Provider>,
}

/// Parses and checks `flags.toml`: provider names are folded to ASCII lowercase (two names
/// that fold together are an error), every `auth` references a vault entry, every `api` is
/// an https URL (plain http only to a loopback host). A missing file is no providers.
pub fn load_providers(path: &Path) -> Result<BTreeMap<String, Provider>, ToolError> {
    let src = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(e) => {
            return Err(ToolError::Failed(format!(
                "flags: cannot read {}: {e}",
                path.display()
            )))
        }
    };
    parse_providers(&src).map_err(|e| ToolError::Failed(format!("flags: {}: {e}", path.display())))
}

fn parse_providers(src: &str) -> Result<BTreeMap<String, Provider>, String> {
    let file: FlagsFile = toml::from_str(src).map_err(|e| e.message().to_string())?;
    let mut out = BTreeMap::new();
    for (name, p) in file.providers {
        let key = name.to_ascii_lowercase();
        if key.is_empty()
            || !key
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            return Err(format!("provider name {name:?} must be [a-z0-9_-]"));
        }
        match vault_refs(&p.auth) {
            Ok(refs) if !refs.is_empty() => {}
            _ => {
                return Err(format!(
                "provider {key}: auth must reference a vault entry, e.g. \"Bearer @vault(name)\""
            ))
            }
        }
        if let Err(e) = http::secure_base(&p.api) {
            return Err(format!("provider {key}: {e}"));
        }
        if out.insert(key.clone(), p).is_some() {
            return Err(format!(
                "provider {key} is listed twice (names fold to lowercase)"
            ));
        }
    }
    Ok(out)
}

/// `flags` with [`Config::from_env`].
pub fn tools() -> Vec<ToolSpec> {
    tools_with(&Arc::new(Config::from_env()))
}

pub fn tools_with(cfg: &Arc<Config>) -> Vec<ToolSpec> {
    vec![ToolSpec {
        name: "flags".into(),
        description: "flags(provider, env, method?, path?, body?): feature flags of a provider \
                      in config/flags.toml. Without method/path it reads the env's flags; \
                      writes (POST/PUT/PATCH/DELETE) change production config and ask first."
            .into(),
        class: Class::Read,
        args: json!({
            "type": "object",
            "properties": {
                "provider": {"type": "string"},
                "env": {"type": "string", "description": "environment, e.g. production"},
                "method": {"type": "string", "enum": ["GET", "HEAD", "POST", "PUT", "PATCH", "DELETE"], "default": "GET"},
                "path": {"type": "string", "description": "API path under the provider's api; {env} is replaced; default the provider's read path"},
                "body": {"type": ["object", "array"]}
            },
            "required": ["provider", "env"]
        }),
        handler: Arc::new(Flags {
            cfg: cfg.clone(),
            client: http::Client::default(),
        }),
    }]
}

struct Flags {
    cfg: Arc<Config>,
    client: http::Client,
}

/// Everything a call needs, checked.
struct Plan {
    provider_name: String,
    provider: Provider,
    method: reqwest::Method,
    class: Class,
    path: String,
    url: url::Url,
}

fn str_arg<'a>(args: &'a Value, key: &str) -> Result<&'a str, ToolError> {
    args.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| ToolError::Args(format!("{key} is required")))
}

impl Flags {
    fn plan(&self, args: &Value) -> Result<Plan, ToolError> {
        let name = str_arg(args, "provider")?.trim().to_ascii_lowercase();
        let env = str_arg(args, "env")?.trim();
        if env.is_empty()
            || env.len() > 64
            || !env
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
            || env.contains("..")
        {
            return Err(ToolError::Args(
                "env must be [A-Za-z0-9_.-], at most 64".into(),
            ));
        }
        let providers = load_providers(&self.cfg.flags_file)?;
        let Some(provider) = providers.get(&name).cloned() else {
            let known: Vec<&str> = providers.keys().map(String::as_str).collect();
            return Err(ToolError::Args(format!(
                "provider {:?} is not in {} (configured: {})",
                text::line(&name, 64),
                self.cfg.flags_file.display(),
                if known.is_empty() {
                    "none".to_string()
                } else {
                    known.join(", ")
                }
            )));
        };
        let (method, class) = http::method(args)?;
        let template = match args.get("path") {
            None | Some(Value::Null) if class == Class::Read => provider.read.clone(),
            None | Some(Value::Null) => {
                return Err(ToolError::Args(format!(
                    "{method} needs a path (the read path is for GET)"
                )))
            }
            Some(Value::String(p)) => p.clone(),
            Some(_) => return Err(ToolError::Args("path must be a string".into())),
        };
        let path = template.replace("{env}", env);
        let url = http::join(&provider.api, &path)?;
        Ok(Plan {
            provider_name: name,
            provider,
            method,
            class,
            path,
            url,
        })
    }
}

#[async_trait]
impl ToolHandler for Flags {
    async fn classify(&self, _ctx: &ToolCtx, args: &Value) -> Result<Classified, ToolError> {
        let p = self.plan(args)?;
        let body = http::body(args)?;
        let domain = p
            .provider
            .domain
            .clone()
            .or_else(|| p.url.host_str().map(str::to_string));
        let mut values = vec![
            ("provider".to_string(), p.provider_name.clone()),
            ("method".to_string(), p.method.to_string()),
            ("path".to_string(), text::line(&p.path, 200)),
        ];
        if let Some(b) = body {
            values.push(("body".into(), text::line(&b.to_string(), 300)));
        }
        Ok(Classified {
            class: Some(p.class),
            domain,
            what: Some(format!(
                "flags {} {} {}",
                p.provider_name,
                p.method,
                text::line(&p.path, 200)
            )),
            values,
            ..Classified::default()
        })
    }

    async fn call(&self, ctx: &ToolCtx, args: Value) -> Result<ToolOutput, ToolError> {
        let p = self.plan(&args)?;
        if p.class > ctx.class {
            return Err(ToolError::Failed(
                "flags: the call's class is above what the gate approved; not sent".into(),
            ));
        }
        let body = http::body(&args)?;
        let label = format!(
            "flags {} {} {}",
            p.provider_name,
            p.method,
            text::line(&p.path, 200)
        );
        let reply = http::send(
            ctx,
            &self.client,
            p.method.clone(),
            p.url.clone(),
            &p.provider.header,
            &p.provider.auth,
            body,
            self.cfg.http_timeout,
        )
        .await?;
        let status = reply.status;
        let t = http::reply_text(&label, reply)?;
        Ok(ToolOutput {
            text: t,
            summary: Some(format!("{label}: HTTP {status}")),
            ..ToolOutput::default()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shipped_flags_toml_parses_with_no_providers() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config/flags.toml");
        let src = std::fs::read_to_string(&path).unwrap();
        assert!(parse_providers(&src).unwrap().is_empty());
        // The documented example (commented out in the file) parses once uncommented.
        let example: String = src
            .lines()
            .filter_map(|l| l.strip_prefix("# ").filter(|l| !l.starts_with(' ')))
            .filter(|l| l.starts_with('[') || l.contains(" = "))
            .collect::<Vec<_>>()
            .join("\n");
        let p = parse_providers(&example).unwrap();
        assert_eq!(p.len(), 1, "{example}");
    }

    #[test]
    fn auth_must_use_the_vault_and_names_fold() {
        let base = |auth: &str| {
            format!("[providers.X]\napi = \"https://f.example.com\"\nauth = \"{auth}\"\nread = \"/e/{{env}}\"\n")
        };
        assert!(parse_providers(&base("Bearer plain")).is_err());
        let ok = parse_providers(&base("Bearer @vault(f)")).unwrap();
        assert!(ok.contains_key("x"));
        let twice = format!(
            "{}{}",
            base("@vault(f)"),
            base("@vault(f)").replace("X", "x")
        );
        assert!(parse_providers(&twice).is_err());
        assert!(parse_providers(
            "[providers.y]\napi = \"ftp://x\"\nauth = \"@vault(f)\"\nread = \"/\"\n"
        )
        .is_err());
    }

    #[test]
    fn api_must_be_https_unless_loopback() {
        let p = |api: &str| {
            parse_providers(&format!(
                "[providers.y]\napi = \"{api}\"\nauth = \"@vault(f)\"\nread = \"/\"\n"
            ))
        };
        assert!(p("http://flags.example.com/api").is_err());
        assert!(p("http://10.0.0.5/api").is_err());
        assert!(p("https://flags.example.com/api").is_ok());
        assert!(p("http://127.0.0.1:8080/api").is_ok());
        assert!(p("http://[::1]:8080/api").is_ok());
        assert!(p("http://localhost:8080/api").is_ok());
    }
}
