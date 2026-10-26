//! HTTP plumbing shared by search, fetch, api and feed (W-1..W-4): the clients, a redirect
//! follower with a private-address guard, a capped body reader, charset decoding, an HTML
//! sniffer and error text that names only the host.
//!
//! Error text never comes from `reqwest::Error`'s `Display`: that prints the whole URL
//! (query included) and the source chain. Every error here is our own sentence naming the
//! host, so nothing a caller put in a URL or a header can come back in an error.

use crate::config::Config;
use reqwest::header::{HeaderMap, ACCEPT, LOCATION};
use reqwest::redirect::Policy;
use reqwest::{Response, Url};
use std::fmt;
use std::net::{IpAddr, SocketAddr};

/// A normal desktop browser UA string (W-2). No other client hints or spoofing (D-5).
pub const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
     (KHTML, like Gecko) Chrome/130.0.0.0 Safari/537.36";

/// Redirects a page fetch follows.
const MAX_REDIRECTS: usize = 10;
/// Redirects an api call follows (same host only).
const MAX_API_REDIRECTS: usize = 5;

/// The clients one server process uses.
#[derive(Clone)]
pub(crate) struct Clients {
    /// fetch, feed, search: no automatic redirects; [`get_following`] follows them and
    /// checks every hop against the private-address guard.
    pub page: reqwest::Client,
    /// api without a vault header, and page.api: redirects only within the same host and
    /// never from https down to http.
    pub api: reqwest::Client,
    /// api with a resolved `@vault` header: redirects only to the same scheme, host and
    /// port, so the header never leaves the origin it was meant for, in clear or otherwise
    /// (reqwest strips `Authorization` on a host change only, and never a custom header).
    pub api_vault: reqwest::Client,
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
    let fail = |_| "could not start the HTTP client (TLS backend)".to_string();
    let page = base().redirect(Policy::none()).build().map_err(fail)?;
    let api_policy = |strict: bool| {
        Policy::custom(move |attempt| {
            let ok = attempt
                .previous()
                .first()
                .is_some_and(|first| api_redirect_ok(first, attempt.url(), strict));
            if attempt.previous().len() > MAX_API_REDIRECTS {
                attempt.error("too many redirects")
            } else if ok {
                attempt.follow()
            } else {
                attempt.error("redirect to another origin")
            }
        })
    };
    let api = base().redirect(api_policy(false)).build().map_err(fail)?;
    let api_vault = base().redirect(api_policy(true)).build().map_err(fail)?;
    Ok(Clients {
        page,
        api,
        api_vault,
    })
}

/// May an api request that started at `first` follow a redirect to `next`? Always the same
/// host and never https to http; with a vault header (`strict`) also the same scheme and
/// port.
pub(crate) fn api_redirect_ok(first: &Url, next: &Url, strict: bool) -> bool {
    if first.host_str() != next.host_str() {
        return false;
    }
    if first.scheme() == "https" && next.scheme() != "https" {
        return false;
    }
    !strict
        || (first.scheme() == next.scheme()
            && first.port_or_known_default() == next.port_or_known_default())
}

/// Why a request failed, in words that name only the host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum HttpError {
    Timeout(String),
    Connect(String),
    Redirect(String),
    /// A redirect from a public start URL into a loopback, private or link-local address.
    Private {
        from: String,
        to: String,
    },
    Other(String),
}

impl fmt::Display for HttpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HttpError::Timeout(h) => write!(f, "{h} did not answer in time"),
            HttpError::Connect(h) => write!(f, "could not connect to {h}"),
            HttpError::Redirect(h) => write!(
                f,
                "{h} redirected somewhere this tool does not follow (another origin, a \
                 non-http URL, or too many redirects)"
            ),
            HttpError::Private { from, to } => write!(
                f,
                "{from} redirected to {to}, a loopback, private or link-local address; \
                 refused because the URL asked for was public"
            ),
            HttpError::Other(h) => write!(f, "request to {h} failed"),
        }
    }
}

/// GET `start` and follow up to [`MAX_REDIRECTS`] redirects by hand (fetch, feed, search).
///
/// Every hop must be http(s) without credentials. A hop into a loopback, private or
/// link-local address (127/8, 10/8, 172.16/12, 192.168/16, 169.254/16, 0/8, ::1, fc00::/7,
/// fe80::/10, `localhost`) is refused unless the start URL was itself such an address, so
/// a public page cannot point the reader at the user's own machine or network. The hops
/// together are bounded by the config timeout.
pub(crate) async fn get_following(
    cfg: &Config,
    client: &reqwest::Client,
    start: &Url,
    accept: &str,
) -> Result<Response, HttpError> {
    let follow = async {
        let mut url = start.clone();
        let mut start_private: Option<bool> = None;
        for hop in 0..=MAX_REDIRECTS {
            let host = host_of(&url);
            let resp = client
                .get(url.clone())
                .header(ACCEPT, accept)
                .send()
                .await
                .map_err(|e| map_err(&e, &host))?;
            if !matches!(resp.status().as_u16(), 301 | 302 | 303 | 307 | 308) {
                return Ok(resp);
            }
            let Some(loc) = resp.headers().get(LOCATION).and_then(|v| v.to_str().ok()) else {
                return Ok(resp);
            };
            if hop == MAX_REDIRECTS {
                return Err(HttpError::Redirect(host));
            }
            let next = url
                .join(loc.trim())
                .ok()
                .and_then(|u| parse_url(u.as_str()).ok())
                .ok_or_else(|| HttpError::Redirect(host.clone()))?;
            if host_is_private(&next, &cfg.resolve).await {
                let public_start = match start_private {
                    Some(p) => !p,
                    None => {
                        let p = host_is_private(start, &cfg.resolve).await;
                        start_private = Some(p);
                        !p
                    }
                };
                if public_start {
                    return Err(HttpError::Private {
                        from: host,
                        to: host_of(&next),
                    });
                }
            }
            url = next;
        }
        Err(HttpError::Redirect(host_of(&url)))
    };
    tokio::time::timeout(cfg.timeout, follow)
        .await
        .map_err(|_| HttpError::Timeout(host_of(start)))?
}

/// Loopback, private, link-local or unspecified (the ranges [`get_following`] guards).
pub(crate) fn private_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            v4.is_loopback() || v4.is_private() || v4.is_link_local() || v4.octets()[0] == 0
        }
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return private_ip(IpAddr::V4(v4));
            }
            let seg = v6.segments()[0];
            v6.is_loopback()
                || v6.is_unspecified()
                || (seg & 0xfe00) == 0xfc00
                || (seg & 0xffc0) == 0xfe80
        }
    }
}

enum HostKind {
    Ip(IpAddr),
    Name(String),
}

/// `url`'s host as an IP literal or a name (`host_str` keeps an IPv6 literal's brackets).
fn host_kind(url: &Url) -> Option<HostKind> {
    let h = url.host_str().filter(|h| !h.is_empty())?;
    let bare = h.trim_start_matches('[').trim_end_matches(']');
    Some(match bare.parse::<IpAddr>() {
        Ok(ip) => HostKind::Ip(ip),
        Err(_) => HostKind::Name(h.to_string()),
    })
}

fn is_localhost_name(d: &str) -> bool {
    let d = d.trim_end_matches('.').to_ascii_lowercase();
    d == "localhost" || d.ends_with(".localhost")
}

/// Does `url`'s host point at a loopback, private or link-local address? An IP literal is
/// checked as written, `localhost` always is one, any other name is resolved and counts
/// when any of its addresses is one. A name in the config's DNS overrides counts as public:
/// an override stands in for the public site it names (tests serve `docs.rs` from a local
/// listener), and production has none. A failed lookup counts as public; the request then
/// fails to connect.
pub(crate) async fn host_is_private(url: &Url, overrides: &[(String, SocketAddr)]) -> bool {
    match host_kind(url) {
        Some(HostKind::Ip(ip)) => private_ip(ip),
        Some(HostKind::Name(d)) => {
            if is_localhost_name(&d) {
                return true;
            }
            let d = d.trim_end_matches('.').to_ascii_lowercase();
            if overrides.iter().any(|(h, _)| *h == d) {
                return false;
            }
            let port = url.port_or_known_default().unwrap_or(80);
            let found = tokio::net::lookup_host((d.as_str(), port)).await;
            found.is_ok_and(|mut addrs| addrs.any(|a| private_ip(a.ip())))
        }
        None => false,
    }
}

/// Does a request to `url` stay on this machine (127/8, ::1, `localhost`)? A name in the
/// config's DNS overrides counts by its override address, since that is where the bytes go.
/// Used by api to allow `@vault` headers over plain http only to loopback.
pub(crate) fn is_loopback(url: &Url, overrides: &[(String, SocketAddr)]) -> bool {
    match host_kind(url) {
        Some(HostKind::Ip(IpAddr::V4(ip))) => ip.is_loopback(),
        Some(HostKind::Ip(IpAddr::V6(ip))) => {
            ip.is_loopback() || ip.to_ipv4_mapped().is_some_and(|v4| v4.is_loopback())
        }
        Some(HostKind::Name(d)) => {
            if is_localhost_name(&d) {
                return true;
            }
            let d = d.trim_end_matches('.').to_ascii_lowercase();
            overrides
                .iter()
                .any(|(h, a)| *h == d && a.ip().is_loopback())
        }
        None => false,
    }
}

/// Tags an HTML document or fragment starts with, for [`looks_like_html`].
const HTML_START_TAGS: &[&str] = &[
    "html", "head", "body", "script", "meta", "title", "link", "style", "base", "noscript",
    "iframe", "frameset", "div", "p", "a", "span", "table", "form", "img", "br", "h1", "h2", "h3",
    "h4", "h5", "h6", "ul", "ol", "section", "article", "main", "nav", "header", "footer",
    "template", "center", "font", "b", "i", "button", "input", "svg",
];

/// Does `bytes` start like HTML, whatever the declared content type says (rule 5)? After a
/// BOM, whitespace, comments and `<?xml ...?>`: `<!doctype html`, or a start tag of a
/// common HTML element (`<html`, `<head`, `<body`, `<script`, `<div`...), any case.
pub(crate) fn looks_like_html(bytes: &[u8]) -> bool {
    let head = String::from_utf8_lossy(&bytes[..bytes.len().min(8192)]).to_ascii_lowercase();
    let mut s = head.trim_start_matches('\u{feff}');
    loop {
        s = s.trim_start();
        let (open, close) = if s.starts_with("<!--") {
            ("<!--", "-->")
        } else if s.starts_with("<?") {
            ("<?", "?>")
        } else {
            break;
        };
        match s[open.len()..].find(close) {
            Some(i) => s = &s[open.len() + i + close.len()..],
            None => return false,
        }
    }
    if s.starts_with("<!doctype html") {
        return true;
    }
    let Some(rest) = s.strip_prefix('<') else {
        return false;
    };
    let end = rest
        .find(|c: char| !c.is_ascii_alphanumeric())
        .unwrap_or(rest.len());
    let (name, after) = rest.split_at(end);
    let closed = after
        .chars()
        .next()
        .is_none_or(|c| c == '>' || c == '/' || c.is_whitespace());
    closed && HTML_START_TAGS.contains(&name)
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

    #[test]
    fn html_is_sniffed_whatever_the_content_type() {
        for yes in [
            "<!DOCTYPE html><html><body>x",
            "\u{feff}  \n<HTML lang=en>",
            "<!-- c --><?xml version=\"1.0\"?>\n<!doctype HTML>",
            "<head><title>t</title>",
            "<body>",
            "<script>alert(1)</script>",
            "<div class=x>hi</div>",
            "<p>",
        ] {
            assert!(looks_like_html(yes.as_bytes()), "{yes}");
        }
        for no in [
            "{\"a\": \"<html>\"}",
            "plain text <html>",
            "<?xml version=\"1.0\"?><rss version=\"2.0\">",
            "<feed xmlns=\"http://www.w3.org/2005/Atom\">",
            "<pre-x>",
            "<!-- never closed <html>",
            "",
        ] {
            assert!(!looks_like_html(no.as_bytes()), "{no}");
        }
    }

    #[test]
    fn api_redirects_keep_the_origin_with_a_vault_header() {
        let u = |s: &str| Url::parse(s).unwrap();
        let first = u("https://api.x.org/a");
        assert!(api_redirect_ok(&first, &u("https://api.x.org/b"), true));
        assert!(!api_redirect_ok(&first, &u("http://api.x.org/b"), true));
        assert!(!api_redirect_ok(&first, &u("http://api.x.org/b"), false));
        assert!(!api_redirect_ok(
            &first,
            &u("https://api.x.org:8443/b"),
            true
        ));
        assert!(api_redirect_ok(
            &first,
            &u("https://api.x.org:8443/b"),
            false
        ));
        assert!(!api_redirect_ok(&first, &u("https://other.org/b"), false));
        let plain = u("http://api.x.org/a");
        assert!(api_redirect_ok(&plain, &u("https://api.x.org/b"), false));
        assert!(!api_redirect_ok(&plain, &u("https://api.x.org/b"), true));
    }

    #[test]
    fn private_ranges_and_loopback() {
        for ip in [
            "127.0.0.1",
            "127.9.9.9",
            "10.1.2.3",
            "172.16.0.1",
            "172.31.255.255",
            "192.168.1.1",
            "169.254.169.254",
            "0.0.0.0",
            "::1",
            "fc00::1",
            "fd12::1",
            "fe80::1",
            "::ffff:127.0.0.1",
        ] {
            assert!(private_ip(ip.parse().unwrap()), "{ip}");
        }
        for ip in ["8.8.8.8", "172.32.0.1", "2606:4700::1", "::ffff:1.1.1.1"] {
            assert!(!private_ip(ip.parse().unwrap()), "{ip}");
        }
        let u = |s: &str| Url::parse(s).unwrap();
        let ov = vec![("api.test".to_string(), "127.0.0.1:9".parse().unwrap())];
        assert!(is_loopback(&u("http://127.0.0.2:8/"), &[]));
        assert!(is_loopback(&u("http://[::1]:8/"), &[]));
        assert!(is_loopback(&u("http://localhost/"), &[]));
        assert!(is_loopback(&u("http://api.test/"), &ov));
        assert!(!is_loopback(&u("http://api.test/"), &[]));
        assert!(!is_loopback(&u("http://10.0.0.1/"), &[]));
    }
}
