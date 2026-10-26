//! HTTP for `cloudflare` and `flags`: one request to a configured API base with one auth
//! header whose value template holds `@vault(name)`. The template is resolved inside the
//! header write (`ToolCtx::resolve_into`, H-2): the value exists only in the sink closure
//! and in the request's header map, marked sensitive. No redirect is followed, so the
//! header never goes to another host. Errors name the host and status, never the header.

use crate::text;
use harness_core::class::Class;
use harness_core::mcp::{ToolCtx, ToolError};
use reqwest::header::{HeaderName, HeaderValue, ACCEPT, CONTENT_TYPE};
use reqwest::Method;
use serde_json::Value;
use std::time::Duration;
use tokio::sync::OnceCell;
use url::Url;
use zeroize::Zeroizing;

/// Response body kept, in bytes.
const MAX_BODY: usize = 1024 * 1024;
/// Error body shown, in chars.
const ERR_CHARS: usize = 1_500;

/// A lazily built client shared by a handler's calls.
#[derive(Default)]
pub struct Client(OnceCell<reqwest::Client>);

impl Client {
    async fn get(&self) -> Result<&reqwest::Client, ToolError> {
        self.0
            .get_or_try_init(|| async {
                reqwest::Client::builder()
                    .redirect(reqwest::redirect::Policy::none())
                    .build()
                    .map_err(|e| ToolError::Failed(format!("http client: {e}")))
            })
            .await
    }
}

/// The method of a call: `GET` by default; GET and HEAD read, POST, PUT and PATCH are
/// Account (production config, H-1), DELETE is Delete. Anything else is refused.
pub fn method(args: &Value) -> Result<(Method, Class), ToolError> {
    let m = match args.get("method") {
        None | Some(Value::Null) => "GET".to_string(),
        Some(Value::String(s)) => s.trim().to_ascii_uppercase(),
        Some(_) => return Err(ToolError::Args("method must be a string".into())),
    };
    let class = match m.as_str() {
        "GET" | "HEAD" => Class::Read,
        "POST" | "PUT" | "PATCH" => Class::Account,
        "DELETE" => Class::Delete,
        other => {
            return Err(ToolError::Args(format!(
                "method {:?} is not one of GET, HEAD, POST, PUT, PATCH, DELETE",
                text::line(other, 20)
            )))
        }
    };
    let method =
        Method::from_bytes(m.as_bytes()).map_err(|_| ToolError::Args("bad method".into()))?;
    Ok((method, class))
}

/// `base` + `path` (which must start with `/` and may carry a query), refusing anything
/// that could leave the base: a scheme, `..`, `\`, `@`, `#`, whitespace or controls, or a
/// result whose origin or path prefix differs from the base.
pub fn join(base: &str, path: &str) -> Result<Url, ToolError> {
    let bad = |why: &str| ToolError::Args(format!("path {:?} {why}", text::line(path, 200)));
    if !path.starts_with('/') {
        return Err(bad("must start with /"));
    }
    if path.contains("://")
        || path.contains("..")
        || path.starts_with("//")
        || path
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || matches!(c, '\\' | '@' | '#'))
    {
        return Err(bad("must be a plain API path"));
    }
    let base_url =
        Url::parse(base).map_err(|e| ToolError::Failed(format!("API base is not a URL: {e}")))?;
    let url = Url::parse(&format!("{}{}", base.trim_end_matches('/'), path))
        .map_err(|_| bad("is not a valid URL path"))?;
    let prefix = base_url.path().trim_end_matches('/');
    if url.origin() != base_url.origin() || !url.path().starts_with(prefix) {
        return Err(bad("leaves the API base"));
    }
    Ok(url)
}

/// What came back.
pub struct Reply {
    pub status: u16,
    pub body: String,
    pub cut: bool,
}

/// One request with `header: <auth template resolved>`. `body` is sent as JSON.
#[allow(clippy::too_many_arguments)]
pub async fn send(
    ctx: &ToolCtx,
    client: &Client,
    method: Method,
    url: Url,
    header: &str,
    auth_template: &str,
    body: Option<&Value>,
    timeout: Duration,
) -> Result<Reply, ToolError> {
    let name = HeaderName::from_bytes(header.as_bytes())
        .map_err(|_| ToolError::Failed(format!("header name {header:?} is not valid")))?;
    // The sink: the value is copied only into a zeroized buffer and the header value.
    let value = ctx.resolve_into(auth_template, |r| {
        let bytes = Zeroizing::new(r.expose().as_bytes().to_vec());
        HeaderValue::from_bytes(&bytes).map(|mut h| {
            h.set_sensitive(true);
            h
        })
    })?;
    let value = value.map_err(|_| {
        ToolError::Failed(format!(
            "the {header} value is not a valid header value (check the vault entry)"
        ))
    })?;
    let host = url.host_str().unwrap_or("").to_string();
    let client = client.get().await?;
    let mut rb = client
        .request(method, url)
        .header(name, value)
        .header(ACCEPT, "application/json")
        .timeout(timeout);
    if let Some(b) = body {
        let bytes = serde_json::to_vec(b).map_err(|e| ToolError::Args(format!("body: {e}")))?;
        rb = rb.header(CONTENT_TYPE, "application/json").body(bytes);
    }
    let mut resp = rb.send().await.map_err(|e| {
        let why = if e.is_timeout() {
            format!("no answer within {} s", timeout.as_secs())
        } else if e.is_connect() {
            "cannot connect".to_string()
        } else {
            "request failed".to_string()
        };
        ToolError::Failed(format!("{host}: {why}"))
    })?;
    let status = resp.status().as_u16();
    let mut kept: Vec<u8> = Vec::new();
    let mut cut = false;
    loop {
        match resp.chunk().await {
            Ok(Some(c)) => {
                let room = MAX_BODY.saturating_sub(kept.len());
                if c.len() > room {
                    kept.extend_from_slice(&c[..room]);
                    cut = true;
                    break;
                }
                kept.extend_from_slice(&c);
            }
            Ok(None) => break,
            Err(_) => {
                return Err(ToolError::Failed(format!(
                    "{host}: HTTP {status}: body read failed"
                )))
            }
        }
    }
    Ok(Reply {
        status,
        body: String::from_utf8_lossy(&kept).into_owned(),
        cut,
    })
}

/// `HTTP <status>` and the body, neutral and capped; a non-2xx status is an error.
pub fn reply_text(label: &str, r: Reply) -> Result<String, ToolError> {
    if !(200..300).contains(&r.status) {
        return Err(ToolError::Failed(format!(
            "{label}: HTTP {}: {}",
            r.status,
            text::capped(r.body.trim(), ERR_CHARS, "")
        )));
    }
    let mut t = format!("{label}: HTTP {}", r.status);
    if !r.body.trim().is_empty() {
        t.push('\n');
        t.push_str(&text::capped(
            r.body.trim(),
            text::MAX_TEXT_CHARS,
            "narrow the request (filters, per_page)",
        ));
    }
    if r.cut {
        t.push_str(&format!("\ntruncated: the body was over {MAX_BODY} bytes"));
    }
    Ok(t)
}

/// A body argument: absent, or a JSON object/array. A `@vault(` literal is refused: vault
/// values are resolved only in the auth header.
pub fn body(args: &Value) -> Result<Option<&Value>, ToolError> {
    match args.get("body") {
        None | Some(Value::Null) => Ok(None),
        Some(b @ (Value::Object(_) | Value::Array(_))) => {
            let s = b.to_string();
            if s.contains("@vault(") {
                return Err(ToolError::Args(
                    "body must not contain @vault(..): vault values go only into the auth header"
                        .into(),
                ));
            }
            Ok(Some(b))
        }
        Some(_) => Err(ToolError::Args(
            "body must be a JSON object or array".into(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn join_stays_on_the_base() {
        let base = "https://api.example.com/client/v4";
        assert_eq!(
            join(base, "/zones?name=a.com").unwrap().as_str(),
            "https://api.example.com/client/v4/zones?name=a.com"
        );
        for bad in [
            "zones",
            "//evil.com/x",
            "/../x",
            "/x@evil.com",
            "/x#y",
            "/a b",
            "/x\\y",
            "/http://x",
        ] {
            assert!(join(base, bad).is_err(), "{bad} accepted");
        }
    }

    #[test]
    fn methods_classify() {
        let m = |s: &str| method(&serde_json::json!({ "method": s })).map(|(_, c)| c);
        assert_eq!(m("get").unwrap(), Class::Read);
        assert_eq!(method(&serde_json::json!({})).unwrap().1, Class::Read);
        assert_eq!(m("POST").unwrap(), Class::Account);
        assert_eq!(m("put").unwrap(), Class::Account);
        assert_eq!(m("PATCH").unwrap(), Class::Account);
        assert_eq!(m("DELETE").unwrap(), Class::Delete);
        assert!(m("TRACE").is_err());
    }
}
