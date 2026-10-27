//! W-3 `api(url, method="GET", body?, headers?, pick?)` -> JSON, pretty-printed and capped.
//!
//! `pick` (D-566, B-560) projects the JSON onto dotted paths (`jobs[].title`) before it is
//! printed and capped, so a large answer fits in one call ([`crate::pick`]). A pick call
//! reads up to `Config::json_max_bytes` (32 MB, D-565) because only the projection reaches
//! the model; other calls keep the W-2 5 MB cap. A cut answer ends with a line naming its
//! top-level keys and array lengths and a `pick` example.
//!
//! Identity (B-562, D-567, W-2): the client always sends its own browser User-Agent. A
//! caller's `User-Agent` or `From` header is refused, and `Authorization`,
//! `Proxy-Authorization` and `Cookie` are refused unless the value uses `@vault(...)`. Any
//! other header whose literal text holds an email address raises the class to Send with the
//! host as the recipient (D-39), so personal data leaves only after an ask.
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
use crate::pick::{self, Pick};
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

/// B-562: identity headers the caller may never set, even from the vault: the client sends
/// its own User-Agent (W-2) and no one's contact details.
const IDENTITY_HEADERS: &[&str] = &["user-agent", "from"];

/// B-562: credential headers the caller may set only from the vault (`@vault(name)`).
const CREDENTIAL_HEADERS: &[&str] = &["authorization", "proxy-authorization", "cookie"];

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
            @vault(name), resolved only when the request is sent (Authorization and Cookie \
            only that way; User-Agent and From are refused). Returns the status and the JSON, \
            pretty-printed and capped at 20000 characters; for a large answer pass pick, \
            dotted paths with [] for arrays (e.g. [\"jobs[].title\", \"jobs[].jobUrl\"]), to \
            get only those fields in one call."
            .into(),
        class: Class::Read,
        args: json!({
            "type": "object",
            "properties": {
                "url": {"type": "string"},
                "method": {"type": "string", "enum": ["GET", "HEAD", "POST", "PUT", "PATCH", "DELETE"], "default": "GET"},
                "body": {"description": "JSON value, or a string sent as text"},
                "headers": {"type": "object", "additionalProperties": {"type": "string"}},
                "pick": {
                    "type": "array",
                    "items": {"type": "string"},
                    "maxItems": pick::MAX_PATHS,
                    "description": "keep only these fields: dotted paths, [] for arrays, e.g. jobs[].title"
                }
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
    /// Headers whose literal text holds an email address (B-562): the call is a Send.
    personal: Vec<HeaderName>,
    /// The url's path or query holds an email address: the call is a Send too.
    personal_url: bool,
    /// The `pick` projection (D-566).
    pick: Option<Pick>,
}

/// B-562: refuses a caller's identity header, and a credential header that does not come
/// from the vault. One line saying why, naming only the header, so the model does not retry
/// with another value and no value (vault or not) is echoed.
fn check_identity(name: &HeaderName, tmpl: &str) -> Result<(), ToolError> {
    let n = name.as_str();
    if IDENTITY_HEADERS.contains(&n) {
        return Err(ToolError::Args(format!(
            "header {n} is refused: api always sends its own browser User-Agent and never \
             sends who the user is (B-562); call again without it"
        )));
    }
    if CREDENTIAL_HEADERS.contains(&n) && !has_vault(tmpl) {
        return Err(ToolError::Args(format!(
            "header {n} is refused: credentials and cookies are sent only from the vault, \
             written as @vault(name) in the value (B-562)"
        )));
    }
    Ok(())
}

/// Is `c` a byte of an email address's local part?
fn local_byte(c: u8) -> bool {
    c.is_ascii_alphanumeric() || b".!#$%&'*+/=?^_`{|}~-".contains(&c)
}

/// Does `s` hold something shaped like an email address (`local@domain.tld`, also with the
/// `@` percent-encoded)? Errs on the side of yes: it decides an ask, not a refusal.
pub(crate) fn has_email(s: &str) -> bool {
    let s = s
        .replace("%2540", "@")
        .replace("%40", "@")
        .replace("%2E", ".")
        .replace("%2e", ".");
    let b = s.as_bytes();
    for (i, &c) in b.iter().enumerate() {
        if c != b'@' {
            continue;
        }
        let local = b[..i].iter().rev().take_while(|&&c| local_byte(c)).count();
        let local_ok = b[i - local..i].iter().any(|c| c.is_ascii_alphanumeric());
        let dom = &b[i + 1..];
        let n = dom
            .iter()
            .take_while(|c| c.is_ascii_alphanumeric() || **c == b'.' || **c == b'-')
            .count();
        let domain = std::str::from_utf8(&dom[..n])
            .unwrap_or("")
            .trim_end_matches('.');
        let labels: Vec<&str> = domain.split('.').collect();
        let domain_ok = labels.len() >= 2
            && labels.iter().all(|l| !l.is_empty())
            && labels
                .last()
                .is_some_and(|t| t.len() >= 2 && t.bytes().all(|c| c.is_ascii_alphabetic()));
        if local_ok && domain_ok {
            return true;
        }
    }
    false
}

/// The literal text of a header template: `@vault(name)` references removed, so a vault
/// reference never counts as an address. Only for templates [`check_vault_refs`] accepted.
fn literal_part(tmpl: &str) -> String {
    let mut out = String::new();
    let mut i = 0;
    while let Some(at) = tmpl[i..].find(VAULT_OPEN) {
        out.push_str(&tmpl[i..i + at]);
        match tmpl[i + at..].find(')') {
            Some(close) => i += at + close + 1,
            None => return out,
        }
    }
    out.push_str(&tmpl[i..]);
    out
}

/// The spelling core resolves (`harness_core::vault`, exact case).
const VAULT_OPEN: &str = "@vault(";

/// True when the template holds a reference core will resolve at the sink.
fn has_vault(s: &str) -> bool {
    s.contains(VAULT_OPEN)
}

/// Any spelling of `@vault(`, for the places where none is allowed.
fn mentions_vault(s: &str) -> bool {
    s.to_ascii_lowercase().contains(VAULT_OPEN)
}

/// A header template's vault references must parse the way core parses them at the sink:
/// a `@VAULT(` or an unclosed `@vault(name` would be sent as typed while this module
/// treated it as a reference (B-562 bypass), so it is refused.
fn check_vault_refs(name: &HeaderName, tmpl: &str) -> Result<(), ToolError> {
    if !mentions_vault(tmpl) {
        return Ok(());
    }
    let exact = tmpl.matches(VAULT_OPEN).count();
    let any_case = tmpl.to_ascii_lowercase().matches(VAULT_OPEN).count();
    let parsed = harness_core::vault::vault_refs(tmpl).map(|r| r.len());
    if exact != any_case || parsed.ok() != Some(exact) {
        return Err(ToolError::Args(format!(
            "header {}: write a vault reference exactly as @vault(name), lower case, the \
             name made of letters, digits, _ - .",
            name.as_str()
        )));
    }
    Ok(())
}

fn no_vault(s: &str, place: &str) -> Result<(), ToolError> {
    if mentions_vault(s) {
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
                check_vault_refs(&name, tmpl)?;
                check_identity(&name, tmpl)?;
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
    let personal: Vec<HeaderName> = headers
        .iter()
        .filter(|(_, t)| has_email(&literal_part(t)))
        .map(|(n, _)| n.clone())
        .collect();
    let personal_url = has_email(url.path()) || url.query().is_some_and(has_email);
    let pick = Pick::parse(args.get("pick")).map_err(ToolError::Args)?;
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
        personal,
        personal_url,
        pick,
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
            if has_vault(tmpl) || req.personal.contains(name) {
                // The template, never the value: `@vault(name)` as written.
                values.push((
                    format!("header {}", name.as_str()),
                    escape::cap_chars(tmpl, 300),
                ));
            }
        }
        if req.personal_url {
            values.push(("url".into(), escape::cap_chars(req.url.as_str(), 300)));
        }
        if let Some(b) = &req.body {
            let s = match b {
                Body::Json(v) => v.to_string(),
                Body::Text(t) => t.clone(),
            };
            values.push(("body".into(), escape::cap_chars(&s, 300)));
        }
        // B-562: an email address in a header leaves the machine only after an ask; the
        // recipient is the host it goes to (D-39).
        let mut places: Vec<String> = req
            .personal
            .iter()
            .map(|n| format!("header {}", n.as_str()))
            .collect();
        if req.personal_url {
            places.push("the url".into());
        }
        let (class, recipients, note) = if places.is_empty() {
            (req.class, Vec::new(), String::new())
        } else {
            (
                req.class.max(Class::Send),
                vec![host.clone()],
                format!(" (sends an email address in {})", places.join(", ")),
            )
        };
        Ok(Classified {
            class: Some(class),
            domain: Some(toolstore::registrable(&host)),
            recipients,
            what: Some(format!(
                "{} {}{}{note}",
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
            self.web.challenged(&req.url);
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
        // A vault header only follows redirects within the same scheme, host and port; a
        // loopback or private target never goes through a proxy (B-590, D-509).
        let client = clients
            .api_for(&req.url, vault, &self.web.cfg.resolve)
            .await;
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
        let resp = match rb.headers(hm).send().await {
            Ok(r) => r,
            // D-5, D-580: a redirect to a challenge URL stops the call; it is not followed.
            Err(e) => match http::challenge_redirect(&e) {
                Some(why) => {
                    self.web.challenged(&req.url);
                    return Ok(fetch::challenge_output(&req.url, &why));
                }
                None => {
                    return Err(ToolError::Failed(format!(
                        "api: {}",
                        http::map_err(&e, &host)
                    )))
                }
            },
        };
        // D-565: a pick call shows only its projection, so it reads up to the JSON cap;
        // every other call, and an answer pick cannot apply to (not JSON, untyped or
        // text/plain), keeps the W-2 page cap.
        let cfg = &self.web.cfg;
        let pick_cap = cfg.json_max_bytes.max(cfg.max_bytes);
        let (mime, _) = http::content_type(resp.headers());
        let pickable = fetch::is_json_type(&mime) || mime.is_empty() || mime == "text/plain";
        let cap = if req.pick.is_some() && pickable {
            pick_cap
        } else {
            cfg.max_bytes
        };
        let body = http::read_capped(resp, cap)
            .await
            .map_err(|e| ToolError::Failed(format!("api: {e}")))?;
        // D-5: a challenge stops the call, whatever the status and content type.
        // D-582: a challenge also drops the site's cached pages.
        let challenged = |stop: ToolOutput| {
            self.web.challenged(&req.url);
            self.web.challenged(&body.url);
            Ok(stop)
        };
        if let Some(stop) = fetch::challenge_by_response(&body) {
            return challenged(stop);
        }
        let text = match render(&body, &req.method, req.pick.as_ref(), cap, pick_cap) {
            Rendered::Shown(t) => t,
            Rendered::NotShown { line, text } => {
                // Only a body the model does not see is walked for a challenge: a JSON
                // string holding widget markup is data, not a page.
                if let Some(stop) = fetch::challenge_in_text(&body, &text, "api").await? {
                    return challenged(stop);
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
/// whose decoded text holds no tag-like sequence ([`http::has_tag`]), checked on the body and
/// again on what would be shown (D-510: JSON in a text/plain body, once its `<` escapes
/// are decoded). A top-level JSON string holding markup is a document, not data, under any
/// type. Everything else
/// (HTML, XML, other types, a missing type that is not JSON, text with tags) is one line:
/// status, type, size, not shown. Nothing here recognises HTML; the rule only lets through
/// what is known to be data or plain text.
///
/// `pick` (D-566) projects the parsed JSON first; a cut answer ends with a `truncated:` line
/// naming the top-level keys and array lengths and a `pick` example ([`pick::shape`]). `cap`
/// is the byte cap the body was read with, for the cut notes; `pick_cap` the cap a pick call
/// reads up to (D-565), offered when JSON was cut.
fn render(
    body: &http::Body,
    method: &Method,
    pick: Option<&Pick>,
    cap: usize,
    pick_cap: usize,
) -> Rendered {
    let mime = body.mime.as_str();
    // D-511: the header is server text; only a plain media type is shown as written.
    let ctype = if mime.is_empty() {
        "(no content type)".to_string()
    } else {
        http::shown_mime(mime)
    };
    let mut head = format!("HTTP {} {ctype}", body.status);
    if body.truncated {
        head.push_str(&format!(" (body cut at the {} cap)", http::cap_text(cap)));
    }
    if *method == Method::HEAD || body.bytes.is_empty() {
        return Rendered::Shown(head);
    }
    let text = body.text(false);
    let shown = if fetch::is_json_type(mime) || mime.is_empty() {
        // D-510: a top-level JSON string holding markup is a document, not data.
        as_json(&text, pick).filter(|j| !j.document)
    } else if mime == "text/plain" && !http::has_tag(&text) {
        if http::looks_binary(&text) {
            return Rendered::Shown(format!(
                "{head}\n({} bytes of binary data, not shown)",
                body.bytes.len()
            ));
        }
        match as_json(&text, pick) {
            // D-510: the tag check runs again on what would be shown. JSON escapes
            // (`<`) spell markup only once decoded, and text/plain is not a JSON type,
            // so no string in it is trusted to be data.
            Some(j) if j.document || http::has_tag(&j.text) => None,
            Some(j) => Some(j),
            None => {
                let mut s = Json::plain(escape::text_checked(&text));
                if pick.is_some() {
                    s.note = Some("pick ignored: the answer is not JSON".into());
                }
                Some(s)
            }
        }
    } else {
        None
    };
    let Some(shown) = shown else {
        let why = if body.truncated && (fetch::is_json_type(mime) || mime.is_empty()) {
            let mut w = format!(
                "not shown (the JSON was cut at the {} cap and cannot be parsed",
                http::cap_text(cap)
            );
            if pick.is_none() && pick_cap > cap {
                w.push_str(&format!(
                    "; call again with pick, which reads JSON up to {}",
                    http::cap_text(pick_cap)
                ));
            }
            w.push(')');
            w
        } else {
            NOT_SHOWN.to_string()
        };
        return Rendered::NotShown {
            line: format!("{head}, {} bytes, {why}", body.bytes.len()),
            text,
        };
    };
    let mut out = head;
    if let Some(note) = &shown.note {
        out.push('\n');
        out.push_str(note);
    }
    if shown.text.is_empty() {
        return Rendered::Shown(out);
    }
    let n = shown.text.chars().count();
    out.push('\n');
    out.push_str(&escape::cap_chars(&shown.text, API_MAX_CHARS));
    if n > API_MAX_CHARS {
        out.push_str(&format!(
            "\ntruncated: {} more characters not shown",
            n - API_MAX_CHARS
        ));
        let how = if pick.is_some() {
            "; pick fewer fields or a narrower path".to_string()
        } else if shown.shape.is_empty() {
            String::new()
        } else {
            format!("; {}", shown.shape)
        };
        out.push_str(&how);
    }
    Rendered::Shown(out)
}

/// What [`render`] shows of a body that is data.
struct Json {
    /// The text after the head line (pretty JSON or plain text); empty when nothing is shown.
    text: String,
    /// [`pick::shape`] of the whole answer, for the cut note; empty for plain text.
    shape: String,
    /// A line before the text (a pick that matched nothing, or was ignored).
    note: Option<String>,
    /// The whole answer is one JSON string holding markup: a document, not data (D-510).
    document: bool,
}

impl Json {
    fn plain(text: String) -> Json {
        Json {
            text,
            shape: String::new(),
            note: None,
            document: false,
        }
    }
}

/// `t` as pretty JSON, projected by `pick`; `None` when it does not parse.
fn as_json(t: &str, pick: Option<&Pick>) -> Option<Json> {
    let v = serde_json::from_str::<Value>(strip_xssi(t)).ok()?;
    let document = matches!(&v, Value::String(s) if http::has_tag(s));
    let shape = pick::shape(&v);
    let mut shown = match pick {
        None => v,
        Some(p) => match p.apply(&v) {
            Some(x) => x,
            None => {
                let mut note = "pick matched nothing in this answer".to_string();
                if !shape.is_empty() {
                    note.push_str("; ");
                    note.push_str(&shape);
                }
                return Some(Json {
                    text: String::new(),
                    shape,
                    note: Some(note),
                    document,
                });
            }
        },
    };
    escape::json_strings(&mut shown);
    Some(Json {
        text: serde_json::to_string_pretty(&shown).unwrap_or_default(),
        shape,
        note: None,
        document,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parsed(headers: Value) -> Result<Req, ToolError> {
        parse(
            &json!({"url": "https://crates.io/api/v1/crates/serde", "headers": headers}),
            &[],
        )
    }

    fn refusal(headers: Value) -> String {
        match parsed(headers) {
            Err(ToolError::Args(m)) => m,
            Err(e) => panic!("not an args error: {e}"),
            Ok(_) => panic!("accepted"),
        }
    }

    #[test]
    fn identity_headers_are_refused_even_from_the_vault() {
        for (name, value) in [
            ("User-Agent", "my-tool (someone@example.com)"),
            ("user-agent", "@vault(gh_token)"),
            ("From", "someone@example.com"),
            ("FROM", "@vault(gh_token)"),
        ] {
            let m = refusal(json!({ name: value }));
            assert!(m.contains("refused") && m.contains("B-562"), "{m}");
            assert!(m.contains("own browser User-Agent"), "{m}");
            assert!(!m.contains("someone@example.com"), "value echoed: {m}");
            assert!(!m.contains('\n'), "one line: {m}");
        }
    }

    #[test]
    fn credential_headers_need_a_vault_template() {
        for name in ["Authorization", "Proxy-Authorization", "Cookie"] {
            let m = refusal(json!({ name: "Bearer abc.def.ghi" }));
            assert!(m.contains("only from the vault"), "{m}");
            assert!(!m.contains("abc.def"), "value echoed: {m}");
            let ok = parsed(json!({ name: "token @vault(gh_token)" })).expect(name);
            assert_eq!(ok.headers.len(), 1);
            assert!(ok.personal.is_empty());
        }
    }

    #[test]
    fn an_email_in_any_other_header_marks_the_call_personal() {
        let r = parsed(json!({"X-Contact": "me <someone@example.com>", "Accept": "*/*"})).unwrap();
        assert_eq!(r.personal.len(), 1);
        assert_eq!(r.personal[0].as_str(), "x-contact");
        let r = parsed(json!({"X-Contact": "someone%40example.com"})).unwrap();
        assert_eq!(r.personal.len(), 1);
        // A vault reference is not an address, and plain values are not personal.
        let r =
            parsed(json!({"X-Api-Key": "@vault(gh_token)", "Accept": "application/json"})).unwrap();
        assert!(r.personal.is_empty());
    }

    #[test]
    fn email_shapes() {
        for yes in [
            "a@b.co",
            "first.last+tag@mail.example.org",
            "x (someone@example.com)",
            "someone%40example.com",
            "Someone@Example.COM.",
        ] {
            assert!(has_email(yes), "{yes}");
        }
        for no in [
            "@vault(x)",
            "token @vault(gh.token)",
            "user@localhost",
            "a@b.c",
            "@example.com",
            "a@.com",
            "foo@bar.123",
            "application/json",
            "",
        ] {
            assert!(!has_email(no), "{no}");
        }
    }

    #[test]
    fn literal_part_drops_vault_references() {
        assert_eq!(literal_part("token @vault(a.b-c_1)"), "token ");
        assert_eq!(literal_part("x@vault(k) y"), "x y");
        assert_eq!(literal_part("plain"), "plain");
    }

    #[test]
    fn vault_refs_must_parse_the_way_core_resolves_them() {
        let n = HeaderName::from_static("x-contact");
        for ok in ["plain", "Bearer @vault(tok)", "@vault(a) @vault(b.c)"] {
            assert!(check_vault_refs(&n, ok).is_ok(), "{ok}");
        }
        for bad in [
            "@VAULT(someone@example.com)",
            "@Vault(tok)",
            "@vault(tok) @VAULT(x)",
            "@vault(a.b@c.de)",
            "@vault(tok",
        ] {
            let e = check_vault_refs(&n, bad).unwrap_err().to_string();
            assert!(
                e.contains("x-contact") && !e.contains("example"),
                "{bad}: {e}"
            );
        }
    }

    #[test]
    fn pick_is_parsed_and_checked() {
        let r = parse(
            &json!({"url": "https://api.example.com/x", "pick": ["jobs[].title"]}),
            &[],
        )
        .unwrap();
        assert!(r.pick.is_some());
        let e = parse(
            &json!({"url": "https://api.example.com/x", "pick": ["jobs[0]"]}),
            &[],
        )
        .err()
        .unwrap();
        assert!(e.to_string().contains("like jobs[].title"), "{e}");
    }

    fn body(mime: &str, bytes: &[u8], truncated: bool) -> http::Body {
        http::Body {
            status: 200,
            url: Url::parse("https://api.example.com/x").unwrap(),
            mime: mime.into(),
            charset: None,
            headers: HeaderMap::new(),
            bytes: bytes.to_vec(),
            truncated,
        }
    }

    fn shown(r: Rendered) -> String {
        match r {
            Rendered::Shown(t) => t,
            Rendered::NotShown { line, .. } => panic!("not shown: {line}"),
        }
    }

    fn big_board(n: usize) -> Vec<u8> {
        let jobs: Vec<Value> = (0..n)
            .map(|i| {
                json!({"title": format!("Role {i}"), "jobUrl": format!("https://x/{i}"),
                       "descriptionHtml": "<p>".repeat(100)})
            })
            .collect();
        serde_json::to_vec(&json!({"apiVersion": "1", "jobs": jobs})).unwrap()
    }

    #[test]
    fn a_cut_answer_names_its_shape_and_suggests_pick() {
        let t = shown(render(
            &body("application/json", &big_board(200), false),
            &Method::GET,
            None,
            1 << 20,
            1 << 20,
        ));
        let last = t.lines().last().unwrap();
        assert!(last.starts_with("truncated: "), "{last}");
        assert!(last.contains("jobs (array of 200 objects)"), "{last}");
        assert!(last.contains("apiVersion (string)"), "{last}");
        assert!(
            last.contains(r#"pick: ["jobs[].title", "jobs[].jobUrl"]"#),
            "{last}"
        );
    }

    #[test]
    fn pick_fits_a_large_answer_in_one_call() {
        let p = Pick::parse(Some(&json!(["jobs[].title", "jobs[].jobUrl"])))
            .unwrap()
            .unwrap();
        let t = shown(render(
            &body("application/json", &big_board(200), false),
            &Method::GET,
            Some(&p),
            1 << 20,
            1 << 20,
        ));
        assert!(!t.contains("truncated:"), "{t}");
        assert!(t.contains("Role 199") && t.contains("https://x/199"), "{t}");
        assert!(!t.contains("descriptionHtml") && !t.contains("<p>"), "{t}");
        assert!(t.chars().count() <= API_MAX_CHARS + 100);
    }

    #[test]
    fn pick_that_matches_nothing_says_so_with_the_shape() {
        let p = Pick::parse(Some(&json!(["postings[].name"])))
            .unwrap()
            .unwrap();
        let t = shown(render(
            &body("application/json", &big_board(3), false),
            &Method::GET,
            Some(&p),
            1 << 20,
            1 << 20,
        ));
        assert!(t.contains("pick matched nothing"), "{t}");
        assert!(t.contains("jobs (array of 3 objects)"), "{t}");
        assert!(!t.contains("Role 0"), "{t}");
    }

    #[test]
    fn cut_json_names_the_cap() {
        let whole = big_board(10);
        let r = render(
            &body("application/json", &whole[..500], true),
            &Method::GET,
            None,
            500,
            32 << 20,
        );
        match r {
            Rendered::NotShown { line, .. } => {
                assert!(line.contains("body cut at the 500 byte cap"), "{line}");
                assert!(line.contains("cannot be parsed"), "{line}");
                assert!(
                    line.contains("call again with pick, which reads JSON up to 32 MB"),
                    "{line}"
                );
            }
            Rendered::Shown(t) => panic!("{t}"),
        }
    }

    fn not_shown(r: Rendered) -> String {
        match r {
            Rendered::NotShown { line, .. } => line,
            Rendered::Shown(t) => panic!("shown: {t}"),
        }
    }

    /// review-T-12 should-fix, D-510: markup that JSON escapes hide from the raw-text tag
    /// check is caught on what would be shown.
    #[test]
    fn escaped_markup_in_plain_text_or_a_json_string_document_is_not_shown() {
        let r = |mime: &str, b: &[u8]| {
            render(&body(mime, b, false), &Method::GET, None, 1 << 20, 1 << 20)
        };
        // T-12b round 2 (tester-1): a real JSON escape, a backslash built from its byte (so
        // no tool on the way into this file turns it back into `<`) and `u003c`. The bodies
        // hold no tag until decoded, so only D-510's decoded check can refuse them.
        let esc = |s: &str| s.replace('<', &format!("{}u003c", char::from(92u8)));
        for b in [
            esc(r#""<script>stealCookies()</script>""#),
            esc(r#"{"h": "<!DOCTYPE html><html>"}"#),
            esc(r#"["<p>x"]"#),
            esc(r#"{"a": [{"b": "</div><img src=x>"}]}"#),
        ] {
            assert!(!crate::http::has_tag(&b) && b.contains("u003c"), "{b}");
            let line = not_shown(r("text/plain", b.as_bytes()));
            assert!(line.contains("not shown") && !line.contains('<'), "{line}");
        }
        for mime in ["application/json", ""] {
            for b in [
                esc(r#""<html><body>Hi""#),
                r#""<html><body>Hi""#.to_string(),
            ] {
                let line = not_shown(r(mime, b.as_bytes()));
                assert!(!line.contains('<'), "{line}");
            }
            let line = not_shown(r(mime, br#"")]}'<p>x</p>""#));
            assert!(!line.contains("<p"), "{line}");
        }
        // Data stays data: nested HTML under a JSON type, and strings without markup.
        let t = shown(r("application/json", br#"{"body_html":"<b>hi"}"#));
        assert!(t.contains(r#""body_html": "<b>hi""#), "{t}");
        let t = shown(r("application/json", br#""a < b""#));
        assert!(t.contains(r#""a < b""#), "{t}");
        let t = shown(r("text/plain", br#"{"k": "v < w"}"#));
        assert!(t.contains(r#""k": "v < w""#), "{t}");
    }

    #[test]
    fn an_unusual_content_type_is_not_echoed() {
        let line = not_shown(render(
            &body("<script>stealcookies()</script>", b"x", false),
            &Method::GET,
            None,
            1 << 20,
            1 << 20,
        ));
        assert!(
            line.starts_with("HTTP 200 unusual content type, 1 bytes"),
            "{line}"
        );
        let t = shown(render(
            &body("text/<b>", b"", false),
            &Method::GET,
            None,
            1,
            1,
        ));
        assert_eq!(t, "HTTP 200 unusual content type");
    }

    #[test]
    fn pick_on_plain_text_is_ignored_with_a_note() {
        let p = Pick::parse(Some(&json!(["a"]))).unwrap().unwrap();
        let t = shown(render(
            &body("text/plain", b"hello there", false),
            &Method::GET,
            Some(&p),
            1 << 20,
            1 << 20,
        ));
        assert!(
            t.contains("pick ignored") && t.contains("hello there"),
            "{t}"
        );
    }
}
