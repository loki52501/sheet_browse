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
use std::net::SocketAddr;
use std::time::Duration;
use tokio::sync::OnceCell;
use url::Url;
use zeroize::Zeroizing;

/// Response body kept, in bytes.
const MAX_BODY: usize = 1024 * 1024;
/// Error body shown, in chars.
const ERR_CHARS: usize = 1_500;

/// Lazily built clients shared by a handler's calls: one for loopback hosts that never uses
/// a proxy, one for every other host that follows the system proxy settings (D-590).
#[derive(Default)]
pub struct Client {
    /// Loopback bases: no proxy (env or OS settings), `localhost` pinned to 127.0.0.1.
    direct: OnceCell<reqwest::Client>,
    /// https bases: the system proxy, if any, sees only a CONNECT tunnel.
    system: OnceCell<reqwest::Client>,
}

impl Client {
    /// The client for `url`. A loopback base may be plain http ([`secure_base`]) because the
    /// request never leaves the machine; a system proxy (`HTTP_PROXY`, `ALL_PROXY`, the
    /// Windows proxy settings) would carry that cleartext request, vault header included, to
    /// another host, so loopback requests are never proxied.
    async fn for_url(&self, url: &Url) -> Result<&reqwest::Client, ToolError> {
        let fail = |e: reqwest::Error| ToolError::Failed(format!("http client: {e}"));
        let builder = || reqwest::Client::builder().redirect(reqwest::redirect::Policy::none());
        if is_loopback(url) {
            self.direct
                .get_or_try_init(|| async {
                    builder()
                        .no_proxy()
                        // The port in the address is ignored; the URL's port is used.
                        .resolve("localhost", SocketAddr::from(([127, 0, 0, 1], 0)))
                        .build()
                        .map_err(fail)
                })
                .await
        } else {
            self.system
                .get_or_try_init(|| async { builder().build().map_err(fail) })
                .await
        }
    }
}

/// 127.0.0.0/8, ::1 or `localhost` (any case).
fn is_loopback(u: &Url) -> bool {
    match u.host() {
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        Some(url::Host::Domain(d)) => d.eq_ignore_ascii_case("localhost"),
        None => false,
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
    let base_url = secure_base(base).map_err(ToolError::Failed)?;
    let url = Url::parse(&format!("{}{}", base.trim_end_matches('/'), path))
        .map_err(|_| bad("is not a valid URL path"))?;
    let prefix = base_url.path().trim_end_matches('/');
    // At a segment boundary: `/client/v4x` is not under `/client/v4`.
    let under = url.path() == prefix || url.path().starts_with(&format!("{prefix}/"));
    if url.origin() != base_url.origin() || !under {
        return Err(bad("leaves the API base"));
    }
    Ok(url)
}

/// An API base that may carry a vault-resolved header: https, or plain http only to a
/// loopback host (127.0.0.0/8, ::1, localhost; local test servers), so the credential never
/// crosses a network in cleartext or through a plain-http proxy.
pub fn secure_base(base: &str) -> Result<Url, String> {
    let u = Url::parse(base).map_err(|e| format!("API base {base:?} is not a URL: {e}"))?;
    if u.host().is_none() {
        return Err(format!("API base {base:?} has no host"));
    }
    // Loopback requests never go through a proxy ([`Client::for_url`]), so plain http to
    // them stays on this machine.
    let loopback = is_loopback(&u);
    match u.scheme() {
        "https" => Ok(u),
        "http" if loopback => Ok(u),
        _ => Err(format!(
            "API base {base:?} must be https (plain http only to a loopback host): it carries a \
             vault credential"
        )),
    }
}

/// What came back.
pub struct Reply {
    pub status: u16,
    /// The `Content-Type` header, capped.
    pub content_type: Option<String>,
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
    let client = client.for_url(&url).await?;
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
    let content_type = resp
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|s| text::line(s.trim(), 100));
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
        content_type,
        body: String::from_utf8_lossy(&kept).into_owned(),
        cut,
    })
}

/// JSON (`application/json`, `application/*+json`) or `text/plain`: a body the model may
/// read. Anything else (an HTML error page from a proxy or WAF, a binary) is not shown.
fn readable(content_type: Option<&str>) -> bool {
    let Some(ct) = content_type else {
        return false;
    };
    let essence = ct
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    essence == "application/json"
        || essence == "text/plain"
        || (essence.starts_with("application/") && essence.ends_with("+json"))
}

/// `HTTP <status>` and the body, neutral and capped; a non-2xx status is an error. A body
/// that is not JSON or plain text is replaced by its content type (rule 5: no raw HTML).
pub fn reply_text(label: &str, r: Reply) -> Result<String, ToolError> {
    let ok = (200..300).contains(&r.status);
    if !r.body.trim().is_empty() && !readable(r.content_type.as_deref()) {
        let t = format!(
            "{label}: HTTP {}, content type {}: body not shown (not JSON or plain text)",
            r.status,
            r.content_type.as_deref().unwrap_or("(none)")
        );
        return if ok { Ok(t) } else { Err(ToolError::Failed(t)) };
    }
    if !ok {
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
            "/%2e%2e/v4x",
        ] {
            assert!(join(base, bad).is_err(), "{bad} accepted");
        }
        assert!(join(base, "/").is_ok());
    }

    #[test]
    fn vault_bases_are_https_unless_loopback() {
        for bad in [
            "http://api.example.com/client/v4",
            "http://10.0.0.1/x",
            "ftp://api.example.com/",
            "HTTP://api.example.com/",
        ] {
            assert!(join(bad, "/zones").is_err(), "{bad} accepted");
        }
        for ok in [
            "https://api.example.com/client/v4",
            "http://127.0.0.1:9/client/v4",
            "http://[::1]:9/x",
            "http://localhost:9/x",
        ] {
            assert!(join(ok, "/zones").is_ok(), "{ok} refused");
        }
    }

    fn reply(status: u16, ct: Option<&str>, body: &str) -> Reply {
        Reply {
            status,
            content_type: ct.map(str::to_string),
            body: body.into(),
            cut: false,
        }
    }

    #[test]
    fn only_json_and_plain_text_bodies_are_shown() {
        let html = "<html><body>blocked by WAF</body></html>";
        let t = reply_text("cf", reply(200, Some("text/html; charset=utf-8"), html)).unwrap();
        assert!(!t.contains("<html") && !t.contains("WAF"), "{t}");
        assert!(t.contains("text/html") && t.contains("HTTP 200"), "{t}");
        let Err(ToolError::Failed(e)) = reply_text("cf", reply(403, Some("text/html"), html))
        else {
            panic!("403 is an error")
        };
        assert!(!e.contains("<html") && e.contains("HTTP 403"), "{e}");
        let t = reply_text("cf", reply(200, None, html)).unwrap();
        assert!(!t.contains("<html") && t.contains("(none)"), "{t}");
        for ct in [
            "application/json",
            "application/json; charset=utf-8",
            "application/problem+json",
            "text/plain",
        ] {
            let t = reply_text("cf", reply(200, Some(ct), "{\"ok\":true}")).unwrap();
            assert!(t.contains("{\"ok\":true}"), "{ct}: {t}");
        }
        assert_eq!(
            reply_text("cf", reply(204, None, "")).unwrap(),
            "cf: HTTP 204"
        );
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
