//! HTTP plumbing shared by search, fetch, api and feed (W-1..W-4): two clients, a capped
//! body reader, charset decoding and error text that names only the host.
//!
//! Error text never comes from `reqwest::Error`'s `Display`: that prints the whole URL
//! (query included) and the source chain. Every error here is our own sentence naming the
//! host, so nothing a caller put in a URL or a header can come back in an error.

use crate::config::Config;
use reqwest::header::HeaderMap;
use reqwest::redirect::Policy;
use reqwest::{Response, Url};
use std::fmt;

/// A normal desktop browser UA string (W-2). No other client hints or spoofing (D-5).
pub const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
     (KHTML, like Gecko) Chrome/130.0.0.0 Safari/537.36";

/// Redirects a page fetch follows.
const MAX_REDIRECTS: usize = 10;

/// The clients one server process uses.
#[derive(Clone)]
pub(crate) struct Clients {
    /// fetch, feed, search: any redirect, up to [`MAX_REDIRECTS`].
    pub page: reqwest::Client,
    /// api and page.api: redirects only within the same host (a header may carry a vault
    /// value, and must never follow a redirect to another host).
    pub api: reqwest::Client,
}

pub(crate) fn build(cfg: &Config) -> Result<Clients, String> {
    let base = || {
        let mut b = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .timeout(cfg.timeout)
            .connect_timeout(cfg.timeout);
        for (host, addr) in &cfg.resolve {
            b = b.resolve(host, *addr);
        }
        b
    };
    let page = base()
        .redirect(Policy::limited(MAX_REDIRECTS))
        .build()
        .map_err(|_| "could not start the HTTP client (TLS backend)".to_string())?;
    let api = base()
        .redirect(Policy::custom(|attempt| {
            let first = attempt.previous().first().and_then(|u| u.host_str());
            if attempt.previous().len() > 5 {
                attempt.error("too many redirects")
            } else if first.is_some() && first == attempt.url().host_str() {
                attempt.follow()
            } else {
                attempt.error("redirect to another host")
            }
        }))
        .build()
        .map_err(|_| "could not start the HTTP client (TLS backend)".to_string())?;
    Ok(Clients { page, api })
}

/// Why a request failed, in words that name only the host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum HttpError {
    Timeout(String),
    Connect(String),
    Redirect(String),
    Other(String),
}

impl fmt::Display for HttpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HttpError::Timeout(h) => write!(f, "{h} did not answer in time"),
            HttpError::Connect(h) => write!(f, "could not connect to {h}"),
            HttpError::Redirect(h) => write!(
                f,
                "{h} redirected somewhere this tool does not follow (another host, or too many redirects)"
            ),
            HttpError::Other(h) => write!(f, "request to {h} failed"),
        }
    }
}

/// Maps a reqwest error to [`HttpError`] without reading its message.
pub(crate) fn map_err(e: &reqwest::Error, host: &str) -> HttpError {
    let host = host.to_string();
    if e.is_timeout() {
        HttpError::Timeout(host)
    } else if e.is_redirect() {
        HttpError::Redirect(host)
    } else if e.is_connect() {
        HttpError::Connect(host)
    } else {
        HttpError::Other(host)
    }
}

/// A response body read up to a cap.
pub(crate) struct Body {
    pub status: u16,
    /// Final URL after redirects.
    pub url: Url,
    /// Lowercased MIME type without parameters (`text/html`), empty when absent.
    pub mime: String,
    /// `charset=` parameter, if any.
    pub charset: Option<String>,
    pub headers: HeaderMap,
    pub bytes: Vec<u8>,
    /// The body was longer than the cap; the rest was not read.
    pub truncated: bool,
}

/// Reads `resp` up to `cap` bytes. Timeouts while reading map like request errors.
pub(crate) async fn read_capped(mut resp: Response, cap: usize) -> Result<Body, HttpError> {
    let url = resp.url().clone();
    let host = url.host_str().unwrap_or("the server").to_string();
    let status = resp.status().as_u16();
    let headers = resp.headers().clone();
    let (mime, charset) = content_type(&headers);
    let mut bytes: Vec<u8> = Vec::new();
    let mut truncated = false;
    loop {
        match resp.chunk().await {
            Ok(Some(chunk)) => {
                let room = cap.saturating_sub(bytes.len());
                if chunk.len() > room {
                    bytes.extend_from_slice(&chunk[..room]);
                    truncated = true;
                    break;
                }
                bytes.extend_from_slice(&chunk);
            }
            Ok(None) => break,
            Err(e) => return Err(map_err(&e, &host)),
        }
    }
    Ok(Body {
        status,
        url,
        mime,
        charset,
        headers,
        bytes,
        truncated,
    })
}

/// `(mime, charset)` from `Content-Type`.
fn content_type(headers: &HeaderMap) -> (String, Option<String>) {
    let Some(v) = headers
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
    else {
        return (String::new(), None);
    };
    let mut parts = v.split(';');
    let mime = parts.next().unwrap_or("").trim().to_ascii_lowercase();
    let charset = parts.find_map(|p| {
        let (k, val) = p.split_once('=')?;
        k.trim()
            .eq_ignore_ascii_case("charset")
            .then(|| val.trim().trim_matches('"').to_string())
    });
    (mime, charset)
}

/// Decodes `bytes`: the header charset, else (for HTML) a `<meta charset>` in the first
/// 2 KiB, else a BOM, else UTF-8. Malformed sequences become U+FFFD.
pub(crate) fn decode(bytes: &[u8], charset: Option<&str>, html: bool) -> String {
    let label = charset
        .map(str::to_string)
        .or_else(|| if html { meta_charset(bytes) } else { None });
    let enc = label
        .and_then(|l| encoding_rs::Encoding::for_label(l.as_bytes()))
        .unwrap_or(encoding_rs::UTF_8);
    let (text, _, _) = enc.decode(bytes);
    text.into_owned()
}

/// `<meta charset="x">` or `content="text/html; charset=x"` in the first 2 KiB.
fn meta_charset(bytes: &[u8]) -> Option<String> {
    let head = &bytes[..bytes.len().min(2048)];
    let s = String::from_utf8_lossy(head).to_ascii_lowercase();
    let at = s.find("charset=")?;
    let rest = &s[at + "charset=".len()..];
    let rest = rest.trim_start_matches(['"', '\'']);
    let end = rest
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '_'))
        .unwrap_or(rest.len());
    let label = &rest[..end];
    (!label.is_empty()).then(|| label.to_string())
}

/// Parses an http(s) URL from a tool argument. Credentials in the URL are refused (they
/// would be logged with the arguments; headers through `api` are the way to send them).
pub(crate) fn parse_url(s: &str) -> Result<Url, String> {
    let s = s.trim();
    let url = Url::parse(s).map_err(|_| "not a valid absolute URL".to_string())?;
    if url.scheme() != "http" && url.scheme() != "https" {
        return Err(format!(
            "only http and https URLs are supported, not {}:",
            url.scheme()
        ));
    }
    if url.host_str().is_none_or(str::is_empty) {
        return Err("the URL has no host".into());
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("credentials in the URL are not allowed".into());
    }
    Ok(url)
}

/// The host of `url` for messages and classification, lowercased.
pub(crate) fn host_of(url: &Url) -> String {
    url.host_str().unwrap_or("").to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn charsets() {
        let latin1 = b"caf\xe9";
        assert_eq!(decode(latin1, Some("iso-8859-1"), false), "café");
        let html = b"<html><head><meta charset=\"windows-1252\"></head>caf\xe9";
        assert!(decode(html, None, true).ends_with("café"));
        assert_eq!(decode("ok é".as_bytes(), None, true), "ok é");
    }

    #[test]
    fn urls() {
        assert!(parse_url("https://docs.rs/tokio").is_ok());
        assert!(parse_url("ftp://x.org/").is_err());
        assert!(parse_url("https://user:pw@x.org/").is_err());
        assert!(parse_url("not a url").is_err());
        assert!(parse_url("file:///etc/passwd").is_err());
    }
}
