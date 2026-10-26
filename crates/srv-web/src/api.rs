//! W-3 `api(url, method="GET", body?, headers?)` -> JSON, pretty-printed and capped.
//!
//! Class by method (D-500): GET and HEAD are Read, POST/PUT/PATCH are Submit, DELETE is
//! Delete; the classifier fills the registrable domain so `gate.rules` apply per site.
//!
//! Vault (rule 2, H-2): `@vault(k)` is allowed only inside header values and is resolved
//! with `ctx.resolve_into` straight into a sensitive `HeaderValue`, built inside the sink.
//! It is refused in the URL and the body (those are logged and echoed by servers), and on a
//! plain-http URL unless the host is this machine (127/8, ::1, localhost). Redirects are
//! followed only within the same host (never https to http); with a vault header only within
//! the same scheme, host and port, so a resolved header never travels anywhere else. Errors
//! are our own text naming only the host (never `reqwest::Error`'s message, which carries
//! the URL). The wrapper scrubs the returned text against every vault value.
//!
//! Rule 5 fails closed (D-508): the body reaches the model only as JSON or as tag-free
//! `text/plain`, decided on the decoded text; anything else is one "not shown" line with a
//! pointer to `fetch` ([`render`]). A challenge stops the call like fetch (D-5). JSON
//! strings cannot spell a `[text](#id)` link form ([`escape::json_strings`]).

use crate::http::{self, host_of};
use crate::{escape, fetch, html, opt_str, req_str, Web};
use async_trait::async_trait;
use harness_core::{Class, Classified, ToolCtx, ToolError, ToolHandler, ToolOutput, ToolSpec};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue, ACCEPT, CONTENT_TYPE};
use reqwest::{Method, Url};
use serde_json::{json, Value};
use std::net::SocketAddr;
use std::sync::Arc;

/// Response text shown to the model, in characters.
pub const API_MAX_CHARS: usize = 20_000;

/// Headers the caller may not set (the client owns them).
const FORBIDDEN_HEADERS: &[&str] = &[
    "host",
    "content-length",
    "transfer-encoding",
    "connection",
    "upgrade",
    "te",
    "trailer",
    "keep-alive",
    "proxy-connection",
];

pub(crate) fn spec(web: Arc<Web>) -> ToolSpec {
    ToolSpec {
        name: "api".into(),
        description: "Call a JSON HTTP API. method GET (default), HEAD, POST, PUT, PATCH or \
            DELETE; body is JSON (an object or array) or a string; header values may contain \
            @vault(name), resolved only when the request is sent. Returns the status and the \
            JSON, pretty-printed and capped."
            .into(),
        class: Class::Read,
        args: json!({
            "type": "object",
            "properties": {
                "url": {"type": "string"},
                "method": {"type": "string", "enum": ["GET", "HEAD", "POST", "PUT", "PATCH", "DELETE"], "default": "GET"},
                "body": {"description": "JSON value, or a string sent as text"},
                "headers": {"type": "object", "additionalProperties": {"type": "string"}}
            },
            "required": ["url"]
        }),
        handler: Arc::new(Api { web }),
    }
}

/// D-500: the class of an HTTP method.
pub fn method_class(method: &str) -> Option<Class> {
    match method {
        "GET" | "HEAD" => Some(Class::Read),
        "POST" | "PUT" | "PATCH" => Some(Class::Submit),
        "DELETE" => Some(Class::Delete),
        _ => None,
    }
}

enum Body {
    Json(Value),
    Text(String),
}

struct Req {
    url: Url,
    method: Method,
    class: Class,
    body: Option<Body>,
    /// (name, value template); a template may hold `@vault(k)`.
    headers: Vec<(HeaderName, String)>,
}

fn has_vault(s: &str) -> bool {
    s.to_ascii_lowercase().contains("@vault(")
}

fn no_vault(s: &str, place: &str) -> Result<(), ToolError> {
    if has_vault(s) {
        return Err(ToolError::Args(format!(
            "@vault(...) is allowed only in header values, not in the {place}"
        )));
    }
    Ok(())
}

/// `overrides` are the config's DNS overrides (for the loopback check of a vault header
/// over plain http).
fn parse(args: &Value, overrides: &[(String, SocketAddr)]) -> Result<Req, ToolError> {
    let url_s = req_str(args, "url")?;
    no_vault(url_s, "url")?;
    let url = http::parse_url(url_s).map_err(|e| ToolError::Args(format!("url: {e}")))?;
    let m = opt_str(args, "method")?
        .map(|m| m.trim().to_ascii_uppercase())
        .filter(|m| !m.is_empty())
        .unwrap_or_else(|| "GET".into());
    let class = method_class(&m).ok_or_else(|| {
        ToolError::Args(format!(
            "method must be GET, HEAD, POST, PUT, PATCH or DELETE, not {:?}",
            escape::name(&m, 20)
        ))
    })?;
    let method = Method::from_bytes(m.as_bytes())
        .map_err(|_| ToolError::Args("method is not a valid HTTP method".into()))?;
    let body = match args.get("body") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) => {
            no_vault(s, "body")?;
            Some(Body::Text(s.clone()))
        }
        Some(v) => {
            no_vault(&v.to_string(), "body")?;
            Some(Body::Json(v.clone()))
        }
    };
    if body.is_some() && class == Class::Read {
        return Err(ToolError::Args(format!("a {m} request has no body")));
    }
    let mut headers = Vec::new();
    match args.get("headers") {
        None | Some(Value::Null) => {}
        Some(Value::Object(map)) => {
            for (k, v) in map {
                let Value::String(tmpl) = v else {
                    return Err(ToolError::Args(format!(
                        "header {:?}: the value must be a string",
                        escape::name(k, 60)
                    )));
                };
                let name = HeaderName::from_bytes(k.trim().as_bytes()).map_err(|_| {
                    ToolError::Args(format!(
                        "header {:?} is not a valid header name",
                        escape::name(k, 60)
                    ))
                })?;
                if FORBIDDEN_HEADERS.contains(&name.as_str()) {
                    return Err(ToolError::Args(format!(
                        "header {} is set by the client, not by the caller",
                        name.as_str()
                    )));
                }
                if tmpl.chars().any(|c| c.is_control()) {
                    return Err(ToolError::Args(format!(
                        "header {}: control characters are not allowed",
                        name.as_str()
                    )));
                }
                if !has_vault(tmpl) && HeaderValue::from_str(tmpl).is_err() {
                    return Err(ToolError::Args(format!(
                        "header {}: not a valid header value",
                        name.as_str()
                    )));
                }
                headers.push((name, tmpl.clone()));
            }
        }
        Some(_) => {
            return Err(ToolError::Args(
                "headers must be an object of name: value strings".into(),
            ))
        }
    }
    // Rule 2: a resolved value never crosses a network in clear text.
    if headers.iter().any(|(_, t)| has_vault(t))
        && url.scheme() != "https"
        && !http::is_loopback(&url, overrides)
    {
        return Err(ToolError::Args(format!(
            "@vault(...) headers are sent only over https (or to this machine); {} is plain http",
            host_of(&url)
        )));
    }
    Ok(Req {
        url,
        method,
        class,
        body,
        headers,
    })
}

struct Api {
    web: Arc<Web>,
}

#[async_trait]
impl ToolHandler for Api {
    async fn classify(&self, _ctx: &ToolCtx, args: &Value) -> Result<Classified, ToolError> {
        let req = parse(args, &self.web.cfg.resolve)?;
        let host = host_of(&req.url);
        let mut values = Vec::new();
        for (name, tmpl) in &req.headers {
            if has_vault(tmpl) {
                // The template, never the value: `@vault(name)` as written.
                values.push((format!("header {}", name.as_str()), tmpl.clone()));
            }
        }
        if let Some(b) = &req.body {
            let s = match b {
                Body::Json(v) => v.to_string(),
                Body::Text(t) => t.clone(),
            };
            values.push(("body".into(), escape::cap_chars(&s, 300)));
        }
        Ok(Classified {
            class: Some(req.class),
            domain: Some(toolstore::registrable(&host)),
            what: Some(format!(
                "{} {}{}",
                req.method.as_str(),
                host,
                escape::cap_chars(req.url.path(), 120)
            )),
            values,
            ..Classified::default()
        })
    }

    async fn call(&self, ctx: &ToolCtx, args: Value) -> Result<ToolOutput, ToolError> {
        let req = parse(&args, &self.web.cfg.resolve)?;
        let host = host_of(&req.url);
        let clients = self.web.clients()?;
        if let Some(why) = html::challenge_url(&req.url) {
            return Ok(fetch::challenge_output(&req.url, &why));
        }
        let vault = req.headers.iter().any(|(_, t)| has_vault(t));
        let mut hm = HeaderMap::new();
        for (name, tmpl) in &req.headers {
            let value = if has_vault(tmpl) {
                // The sink: the resolved string becomes a sensitive header value here and
                // exists nowhere else.
                ctx.resolve_into(tmpl, |r| {
                    HeaderValue::from_str(r.expose()).ok().map(|mut v| {
                        v.set_sensitive(true);
                        v
                    })
                })?
                .ok_or_else(|| {
                    ToolError::Args(format!(
                        "header {}: the resolved value is not a valid header value",
                        name.as_str()
                    ))
                })?
            } else {
                HeaderValue::from_str(tmpl).map_err(|_| {
                    ToolError::Args(format!(
                        "header {}: not a valid header value",
                        name.as_str()
                    ))
                })?
            };
            hm.append(name.clone(), value);
        }
        if !hm.contains_key(ACCEPT) {
            hm.insert(
                ACCEPT,
                HeaderValue::from_static("application/json, text/plain;q=0.9, */*;q=0.5"),
            );
        }
        // A vault header only follows redirects within the same scheme, host and port.
        let client = if vault {
            &clients.api_vault
        } else {
            &clients.api
        };
        let mut rb = client.request(req.method.clone(), req.url.clone());
        match req.body {
            Some(Body::Json(v)) => {
                if !hm.contains_key(CONTENT_TYPE) {
                    hm.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
                }
                rb = rb.body(v.to_string());
            }
            Some(Body::Text(t)) => {
                if !hm.contains_key(CONTENT_TYPE) {
                    hm.insert(
                        CONTENT_TYPE,
                        HeaderValue::from_static("text/plain; charset=utf-8"),
                    );
                }
                rb = rb.body(t);
            }
            None => {}
        }
        let resp = rb
            .headers(hm)
            .send()
            .await
            .map_err(|e| ToolError::Failed(format!("api: {}", http::map_err(&e, &host))))?;
        let body = http::read_capped(resp, self.web.cfg.max_bytes)
            .await
            .map_err(|e| ToolError::Failed(format!("api: {e}")))?;
        // D-5: a challenge stops the call, whatever the status and content type.
        if let Some(stop) = fetch::challenge_by_response(&body) {
            return Ok(stop);
        }
        let text = match render(&body, &req.method) {
            Rendered::Shown(t) => t,
            Rendered::NotShown { line, text } => {
                // Only a body the model does not see is walked for a challenge: a JSON
                // string holding widget markup is data, not a page.
                if let Some(stop) = fetch::challenge_in_text(&body, &text, "api").await? {
                    return Ok(stop);
                }
                line
            }
        };
        Ok(ToolOutput {
            summary: Some(format!(
                "{} {} -> HTTP {}",
                req.method.as_str(),
                host,
                body.status
            )),
            text,
            consequences: json!({ "status": body.status, "host": host }),
            files: Vec::new(),
            stopped: None,
        })
    }
}

/// What [`render`] makes of a response.
enum Rendered {
    /// The text the model gets.
    Shown(String),
    /// The body is not shown: `line` is all the model gets; `text` is the decoded body, for
    /// the challenge check only.
    NotShown { line: String, text: String },
}

/// The suffix of the line for a body that is not shown.
const NOT_SHOWN: &str = "not shown (not JSON or plain text; use fetch to read pages)";

/// `text` without an XSSI guard line (`)]}'` and the rest of its line).
fn strip_xssi(text: &str) -> &str {
    let t = text.trim_start();
    match t.strip_prefix(")]}'") {
        Some(rest) => rest.split_once('\n').map_or("", |(_, r)| r),
        None => text,
    }
}

/// Rule 5, D-508: a body reaches the model only as JSON (a JSON content type, or none, and
/// the decoded text parses; JSON strings may hold HTML, which is data) or as `text/plain`
/// whose decoded text holds no tag-like sequence ([`http::has_tag`]). Everything else
/// (HTML, XML, other types, a missing type that is not JSON, text with tags) is one line:
/// status, type, size, not shown. Nothing here recognises HTML; the rule only lets through
/// what is known to be data or plain text.
fn render(body: &http::Body, method: &Method) -> Rendered {
    let mime = body.mime.as_str();
    let ctype = if mime.is_empty() {
        "(no content type)".to_string()
    } else {
        escape::name(mime, 80)
    };
    let mut head = format!("HTTP {} {ctype}", body.status);
    if body.truncated {
        head.push_str(" (body cut at the 5 MB cap)");
    }
    if *method == Method::HEAD || body.bytes.is_empty() {
        return Rendered::Shown(head);
    }
    let text = body.text(false);
    let as_json = |t: &str| {
        serde_json::from_str::<Value>(strip_xssi(t))
            .ok()
            .map(|mut v| {
                escape::json_strings(&mut v);
                serde_json::to_string_pretty(&v).unwrap_or_default()
            })
    };
    let shown = if fetch::is_json_type(mime) || mime.is_empty() {
        as_json(&text)
    } else if mime == "text/plain" && !http::has_tag(&text) {
        if http::looks_binary(&text) {
            return Rendered::Shown(format!(
                "{head}\n({} bytes of binary data, not shown)",
                body.bytes.len()
            ));
        }
        Some(as_json(&text).unwrap_or_else(|| escape::text(&text)))
    } else {
        None
    };
    let Some(shown) = shown else {
        return Rendered::NotShown {
            line: format!("{head}, {} bytes, {NOT_SHOWN}", body.bytes.len()),
            text,
        };
    };
    let n = shown.chars().count();
    let mut out = format!("{head}\n{}", escape::cap_chars(&shown, API_MAX_CHARS));
    if n > API_MAX_CHARS {
        out.push_str(&format!(
            "\ntruncated: {} more characters not shown",
            n - API_MAX_CHARS
        ));
    }
    Rendered::Shown(out)
}
