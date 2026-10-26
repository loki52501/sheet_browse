//! C-2 `cloudflare(method, path, body)`: the Cloudflare v4 API with `Authorization: Bearer
//! @vault(cf_token)` resolved at the header write (H-2, D-541). GET and HEAD are Read;
//! POST, PUT and PATCH change production DNS or settings and are Account; DELETE is Delete.

use crate::{http, text, Config};
use async_trait::async_trait;
use harness_core::class::Class;
use harness_core::mcp::{Classified, ToolCtx, ToolError, ToolHandler, ToolOutput, ToolSpec};
use serde_json::{json, Value};
use std::sync::Arc;

/// `cloudflare` with [`Config::from_env`].
pub fn tools() -> Vec<ToolSpec> {
    tools_with(&Arc::new(Config::from_env()))
}

pub fn tools_with(cfg: &Arc<Config>) -> Vec<ToolSpec> {
    vec![ToolSpec {
        name: "cloudflare".into(),
        description: "cloudflare(path, method?, body?): Cloudflare API v4, e.g. path \
                      \"/zones?name=example.com\" or \"/zones/<id>/dns_records\". GET reads; \
                      POST/PUT/PATCH/DELETE change production DNS and ask first."
            .into(),
        class: Class::Read,
        args: json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "API path under /client/v4, starting with /; may carry a query"},
                "method": {"type": "string", "enum": ["GET", "HEAD", "POST", "PUT", "PATCH", "DELETE"], "default": "GET"},
                "body": {"description": "JSON body for POST/PUT/PATCH", "type": ["object", "array"]}
            },
            "required": ["path"]
        }),
        handler: Arc::new(Cloudflare {
            cfg: cfg.clone(),
            client: http::Client::default(),
        }),
    }]
}

struct Cloudflare {
    cfg: Arc<Config>,
    client: http::Client,
}

fn path(args: &Value) -> Result<&str, ToolError> {
    args.get("path")
        .and_then(Value::as_str)
        .ok_or_else(|| ToolError::Args("path is required".into()))
}

#[async_trait]
impl ToolHandler for Cloudflare {
    async fn classify(&self, _ctx: &ToolCtx, args: &Value) -> Result<Classified, ToolError> {
        let (method, class) = http::method(args)?;
        let p = path(args)?;
        http::join(&self.cfg.cloudflare_api, p)?;
        let body = http::body(args)?;
        let mut values = vec![
            ("method".to_string(), method.to_string()),
            ("path".to_string(), text::line(p, 200)),
        ];
        if let Some(b) = body {
            values.push(("body".into(), text::line(&b.to_string(), 300)));
        }
        Ok(Classified {
            class: Some(class),
            domain: Some(self.cfg.cloudflare_domain.clone()),
            what: Some(format!("cloudflare {method} {}", text::line(p, 200))),
            values,
            ..Classified::default()
        })
    }

    async fn call(&self, ctx: &ToolCtx, args: Value) -> Result<ToolOutput, ToolError> {
        let (method, class) = http::method(&args)?;
        if class > ctx.class {
            return Err(ToolError::Failed(
                "cloudflare: the call's class is above what the gate approved; not sent".into(),
            ));
        }
        let p = path(&args)?;
        let url = http::join(&self.cfg.cloudflare_api, p)?;
        let body = http::body(&args)?;
        let label = format!("cloudflare {method} {}", text::line(p, 200));
        let reply = http::send(
            ctx,
            &self.client,
            method,
            url,
            "authorization",
            &self.cfg.cloudflare_auth,
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
