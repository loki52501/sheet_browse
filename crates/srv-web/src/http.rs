//! HTTP plumbing shared by search, fetch, api and feed (W-1..W-4): the clients, a filtering
//! DNS resolver and redirect follower (the private-address guard), one deadline per page
//! request, a capped body reader, charset decoding, a tag scan and error text that names
//! only the host.
//!
//! Error text never comes from `reqwest::Error`'s `Display`: that prints the whole URL
//! (query included) and the source chain. Every error here is our own sentence naming the
//! host, so nothing a caller put in a URL or a header can come back in an error.

use crate::config::Config;
use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use reqwest::header::{HeaderMap, ACCEPT, LOCATION};
use reqwest::redirect::Policy;
use reqwest::{Response, Url};
use std::fmt;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

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
    /// fetch and feed of a URL that is not written as a private address: no automatic
    /// redirects ([`get_page`] follows them), and a DNS resolver that drops loopback,
    /// private and link-local answers ([`PublicOnly`]), so a public name can never connect
    /// to the user's machine or network, whatever it resolves to and however often.
    pub page: reqwest::Client,
    /// fetch and feed of a URL written as a private address or `localhost` (the user asked
    /// for their own machine), and search (the SearXNG URL is operator config, D-31): no
    /// automatic redirects, the system resolver.
    pub page_local: reqwest::Client,
    /// api without a vault header, and page.api: redirects only within the same host and
    /// never from https down to http.
    pub api: reqwest::Client,
    /// api with a resolved `@vault` header: redirects only to the same scheme, host and
    /// port, so the header never leaves the origin it was meant for, in clear or otherwise
    /// (reqwest strips `Authorization` on a host change only, and never a custom header).
    pub api_vault: reqwest::Client,
}

pub(crate) fn build(cfg: &Config) -> Result<Clients, String> {
    build_with(cfg, PublicOnly::default())
}

fn build_with(cfg: &Config, resolver: PublicOnly) -> Result<Clients, String> {
    let base = || {
        let mut b = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .timeout(cfg.timeout)
            .connect_timeout(cfg.timeout);
        // Test DNS overrides are answered before any resolver runs, so they count as
        // public (D-507): they stand in for the public site they name.
        for (host, addr) in &cfg.resolve {
            b = b.resolve(host, *addr);
        }
        b
    };
    let fail = |_| "could not start the HTTP client (TLS backend)".to_string();
    let page = base()
        .redirect(Policy::none())
        .dns_resolver(Arc::new(resolver))
        .build()
        .map_err(fail)?;
    let page_local = base().redirect(Policy::none()).build().map_err(fail)?;
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
        page_local,
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

/// The DNS resolver of [`Clients::page`]: resolves a name once and hands the connector only
/// the public addresses, so there is no check-then-connect window (a TTL-0 rebinding name
/// cannot answer public to a check and private to the connect) and no extra lookup. A name
/// whose every answer is loopback, private, link-local or CGNAT fails with
/// [`PrivateAnswer`].
#[derive(Default)]
pub(crate) struct PublicOnly {
    /// Test seam: fixed answers by name instead of the system resolver.
    fixed: Vec<(String, Vec<IpAddr>)>,
}

impl Resolve for PublicOnly {
    fn resolve(&self, name: Name) -> Resolving {
        let host = name.as_str().trim_end_matches('.').to_ascii_lowercase();
        let fixed = self
            .fixed
            .iter()
            .find(|(h, _)| *h == host)
            .map(|(_, ips)| ips.clone());
        Box::pin(async move {
            let ips: Vec<IpAddr> = match fixed {
                Some(ips) => ips,
                None => tokio::net::lookup_host((host.as_str(), 0))
                    .await?
                    .map(|a| a.ip())
                    .collect(),
            };
            let public: Vec<SocketAddr> = ips
                .iter()
                .filter(|ip| !private_ip(**ip))
                .map(|ip| SocketAddr::new(*ip, 0))
                .collect();
            if public.is_empty() && !ips.is_empty() {
                return Err(Box::new(PrivateAnswer) as Box<dyn std::error::Error + Send + Sync>);
            }
            Ok(Box::new(public.into_iter()) as Addrs)
        })
    }
}

/// [`PublicOnly`] refused a name that resolves only to private addresses.
#[derive(Debug)]
pub(crate) struct PrivateAnswer;

impl fmt::Display for PrivateAnswer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("the name resolves only to loopback, private or link-local addresses")
    }
}

impl std::error::Error for PrivateAnswer {}

/// Did `e` come from [`PublicOnly`] refusing a private answer?
fn is_private_answer(e: &reqwest::Error) -> bool {
    let mut cur: Option<&(dyn std::error::Error + 'static)> = Some(e);
    while let Some(err) = cur {
        if err.downcast_ref::<PrivateAnswer>().is_some() {
            return true;
        }
        cur = err.source();
    }
    false
}

/// Why a request failed, in words that name only the host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum HttpError {
    Timeout(String),
    Connect(String),
    Redirect(String),
    /// A loopback, private or link-local target from a URL that was not written as one:
    /// a redirect `from` a public page, a link on a public page, or a name that resolves
    /// to a private address.
    Private {
        from: Option<String>,
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
            HttpError::Private {
                from: Some(from),
                to,
            } => write!(
                f,
                "{from} redirected to {to}, a loopback, private or link-local address; \
                 refused because the URL asked for was public"
            ),
            HttpError::Private { from: None, to } => write!(
                f,
                "{to} is or resolves to a loopback, private or link-local address; refused \
                 because the URL was not written as one (a private IP address or localhost) \
                 by the user"
            ),
            HttpError::Other(h) => write!(f, "request to {h} failed"),
        }
    }
}

/// GET `start` for fetch, feed and search, follow up to [`MAX_REDIRECTS`] redirects by hand,
/// and read the body up to `cfg.max_bytes`, all within one `cfg.timeout` deadline (W-2:
/// 15 s for the whole request, redirects and body included).
///
/// `allow_private` says whether the request may reach a loopback, private or link-local
/// address (127/8, 10/8, 172.16/12, 192.168/16, 169.254/16, 100.64/10, 198.18/15, 0/8,
/// ::1, fc00::/7, fe80::/10, IPv4-mapped and -compatible forms, `localhost`). Callers pass
/// [`written_private`] of the URL the user wrote (for `fetch("#id")`: of the page holding
/// the link). When it is false every hop must be public: a literal private address or a
/// localhost name is refused before any request, and a name goes through [`PublicOnly`].
/// Every hop must be http(s) without credentials.
pub(crate) async fn get_page(
    cfg: &Config,
    clients: &Clients,
    start: &Url,
    accept: &str,
    allow_private: bool,
) -> Result<Body, HttpError> {
    let client = if allow_private {
        &clients.page_local
    } else {
        &clients.page
    };
    let whole = async {
        let resp = follow(client, start, accept, allow_private).await?;
        read_capped(resp, cfg.max_bytes).await
    };
    tokio::time::timeout(cfg.timeout, whole)
        .await
        .map_err(|_| HttpError::Timeout(host_of(start)))?
}

async fn follow(
    client: &reqwest::Client,
    start: &Url,
    accept: &str,
    allow_private: bool,
) -> Result<Response, HttpError> {
    let mut url = start.clone();
    let mut from: Option<String> = None;
    for hop in 0..=MAX_REDIRECTS {
        let host = host_of(&url);
        if !allow_private && written_private_or_local(&url) {
            return Err(HttpError::Private { from, to: host });
        }
        let resp = client
            .get(url.clone())
            .header(ACCEPT, accept)
            .send()
            .await
            .map_err(|e| {
                if is_private_answer(&e) {
                    HttpError::Private {
                        from: from.clone(),
                        to: host.clone(),
                    }
                } else {
                    map_err(&e, &host)
                }
            })?;
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
        from = Some(host);
        url = next;
    }
    Err(HttpError::Redirect(host_of(&url)))
}

/// Loopback, private, link-local, CGNAT, benchmarking or unspecified (the ranges the page
/// client never connects to unless the user wrote such an address).
pub(crate) fn private_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || o[0] == 0
                // 100.64.0.0/10, CGNAT (Tailscale puts the user's own machines here).
                || (o[0] == 100 && (o[1] & 0xc0) == 64)
                // 198.18.0.0/15, benchmarking (some VPNs and fake-IP DNS use it).
                || (o[0] == 198 && (o[1] & 0xfe) == 18)
        }
        IpAddr::V6(v6) => {
            // IPv4-mapped (::ffff:a.b.c.d) and IPv4-compatible (::a.b.c.d, which also
            // covers :: and ::1).
            if let Some(v4) = v6.to_ipv4() {
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
        Err(_) => HostKind::Name(h.trim_end_matches('.').to_ascii_lowercase()),
    })
}

/// Is `url` written as a private address: a private IP literal or exactly `localhost`?
/// Only such a URL, typed by the user, may reach the user's machine or network
/// (`fetch("#id")` asks this of the page that holds the link). A name that merely resolves
/// to a private address does not count: the name's owner chose that answer.
pub(crate) fn written_private(url: &Url) -> bool {
    match host_kind(url) {
        Some(HostKind::Ip(ip)) => private_ip(ip),
        Some(HostKind::Name(d)) => d == "localhost",
        None => false,
    }
}

/// [`written_private`], or a `*.localhost` name: a hop the page client refuses without a
/// lookup when private addresses are not allowed.
fn written_private_or_local(url: &Url) -> bool {
    written_private(url)
        || matches!(host_kind(url), Some(HostKind::Name(d)) if d.ends_with(".localhost"))
}

/// Does a request to `url` stay on this machine? Loopback IP literals and exactly
/// `localhost`; any other name only through a config DNS override whose address is
/// loopback (tests; that is where the bytes go). `*.localhost` names do not count: some
/// resolvers send them to DNS. Used by api to allow `@vault` headers over plain http only
/// to loopback.
pub(crate) fn is_loopback(url: &Url, overrides: &[(String, SocketAddr)]) -> bool {
    match host_kind(url) {
        Some(HostKind::Ip(IpAddr::V4(ip))) => ip.is_loopback(),
        Some(HostKind::Ip(IpAddr::V6(ip))) => {
            ip.is_loopback() || ip.to_ipv4_mapped().is_some_and(|v4| v4.is_loopback())
        }
        Some(HostKind::Name(d)) => {
            d == "localhost"
                || overrides
                    .iter()
                    .any(|(h, a)| *h == d && a.ip().is_loopback())
        }
        None => false,
    }
}

/// Does `text` hold anything an HTML parser would read as markup: `<` followed by an ASCII
/// letter, `!`, `/` or `?` (a tag, an end tag, a comment, a doctype, a processing
/// instruction)? Run on decoded text: a body with none of these has no markup to hide.
pub(crate) fn has_tag(text: &str) -> bool {
    text.as_bytes()
        .windows(2)
        .any(|w| w[0] == b'<' && (w[1].is_ascii_alphabetic() || matches!(w[1], b'!' | b'/' | b'?')))
}

/// Is decoded `text` binary data rather than text: more than a tenth replacement
/// characters or control characters other than tab, newline, carriage return and form
/// feed?
pub(crate) fn looks_binary(text: &str) -> bool {
    let mut n = 0usize;
    let mut bad = 0usize;
    for c in text.chars().take(65_536) {
        n += 1;
        if c == '\u{FFFD}' || (c.is_control() && !matches!(c, '\t' | '\n' | '\r' | '\u{c}')) {
            bad += 1;
        }
    }
    n > 0 && bad * 10 > n
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

impl Body {
    /// The body as text: the header charset, else (for HTML) a `<meta charset>`, else a
    /// BOM, else UTF-8 ([`decode`]). Every decision about what the body is runs on this.
    pub fn text(&self, html: bool) -> String {
        decode(&self.bytes, self.charset.as_deref(), html)
    }
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

/// Decodes `bytes`: a BOM first (UTF-8, UTF-16LE, UTF-16BE), else the header charset, else
/// (for HTML) a `<meta charset>` in the first 2 KiB, else UTF-8. Malformed sequences
/// become U+FFFD.
pub(crate) fn decode(bytes: &[u8], charset: Option<&str>, html: bool) -> String {
    let label = charset
        .map(str::to_string)
        .or_else(|| if html { meta_charset(bytes) } else { None });
    let enc = label
        .and_then(|l| encoding_rs::Encoding::for_label(l.as_bytes()))
        .unwrap_or(encoding_rs::UTF_8);
    // `decode` sniffs a BOM and lets it win over the label.
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
/// Longest URL a tool hands the model as a value (a feed link, a search hit).
pub(crate) const URL_MAX: usize = 2000;

/// A URL from a page, feed or search backend as the model may see it: parsed by
/// [`parse_url`] and re-serialised (the URL standard percent-encodes `<`, `>`, quotes and
/// spaces, so no markup survives, rule 5), or `None` when it is not a valid http(s) URL or
/// is longer than [`URL_MAX`].
pub(crate) fn clean_url(s: &str) -> Option<String> {
    let u = parse_url(s).ok()?.to_string();
    (u.len() <= URL_MAX).then_some(u)
}

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
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[test]
    fn charsets() {
        let latin1 = b"caf\xe9";
        assert_eq!(decode(latin1, Some("iso-8859-1"), false), "café");
        let html = b"<html><head><meta charset=\"windows-1252\"></head>caf\xe9";
        assert!(decode(html, None, true).ends_with("café"));
        assert_eq!(decode("ok é".as_bytes(), None, true), "ok é");
        // A BOM wins over the label.
        assert_eq!(decode(b"\xff\xfeo\x00k\x00", Some("utf-8"), false), "ok");
        assert_eq!(decode(b"\xfe\xff\x00o\x00k", None, false), "ok");
    }

    #[test]
    fn urls() {
        assert!(parse_url("https://docs.rs/tokio").is_ok());
        assert!(parse_url("ftp://x.org/").is_err());
        assert!(parse_url("https://user:pw@x.org/").is_err());
        assert!(parse_url("not a url").is_err());
        assert!(parse_url("file:///etc/passwd").is_err());
        // review-T-12 closing round: a URL value never carries markup and is capped.
        let u = clean_url(
            "https://f.test/d/<script>stealCookies()</script><!DOCTYPE html><html>?q=<b>#<i>",
        )
        .unwrap();
        assert!(!u.contains('<') && !u.contains('>'), "{u}");
        assert!(clean_url(&format!("https://f.test/{}", "a".repeat(URL_MAX))).is_none());
        assert!(clean_url("javascript:alert(1)").is_none());
    }

    #[test]
    fn tag_scan_and_binary() {
        for yes in [
            "<p>",
            "x <!-- c",
            "a</b",
            "<?xml",
            "\u{200b}<!DOCTYPE  html>",
            "<my-el>",
            "<LI>",
        ] {
            assert!(has_tag(yes), "{yes}");
        }
        for no in ["a < b", "3<4", "<", "< p>", "plain", "", "<<>>", "<\u{e9}"] {
            assert!(!has_tag(no), "{no}");
        }
        assert!(looks_binary("\0\u{1}\u{2}"));
        assert!(looks_binary("\u{FFFD}\u{FFFD}ab"));
        assert!(!looks_binary("plain text\twith\ttabs\r\n"));
        assert!(!looks_binary(""));
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
            "100.64.0.1",
            "100.100.100.100",
            "100.127.255.255",
            "198.18.0.1",
            "198.19.255.255",
            "::1",
            "::",
            "fc00::1",
            "fd12::1",
            "fe80::1",
            "::ffff:127.0.0.1",
            "::7f00:1",
            "::a00:1",
        ] {
            assert!(private_ip(ip.parse().unwrap()), "{ip}");
        }
        for ip in [
            "8.8.8.8",
            "172.32.0.1",
            "100.63.255.255",
            "100.128.0.1",
            "198.17.255.255",
            "198.20.0.1",
            "2606:4700::1",
            "::ffff:1.1.1.1",
        ] {
            assert!(!private_ip(ip.parse().unwrap()), "{ip}");
        }
        let u = |s: &str| Url::parse(s).unwrap();
        let ov = vec![("api.test".to_string(), "127.0.0.1:9".parse().unwrap())];
        assert!(is_loopback(&u("http://127.0.0.2:8/"), &[]));
        assert!(is_loopback(&u("http://[::1]:8/"), &[]));
        assert!(is_loopback(&u("http://localhost/"), &[]));
        assert!(is_loopback(&u("http://LOCALHOST./"), &[]));
        assert!(is_loopback(&u("http://api.test/"), &ov));
        assert!(!is_loopback(&u("http://api.test/"), &[]));
        assert!(!is_loopback(&u("http://10.0.0.1/"), &[]));
        assert!(!is_loopback(&u("http://foo.localhost/"), &[]));
        assert!(!is_loopback(&u("http://localhost.evil.test/"), &[]));
        assert!(written_private(&u("http://192.168.1.4:8080/")));
        assert!(written_private(&u("http://localhost:3000/")));
        assert!(written_private(&u("http://[::1]/")));
        assert!(!written_private(&u("http://foo.localhost/")));
        assert!(!written_private(&u("http://docs.rs/")));
        assert!(written_private_or_local(&u("http://foo.localhost/")));
    }

    /// A one-route server on 127.0.0.1 that answers 200 and counts requests.
    async fn counting_server() -> (SocketAddr, Arc<AtomicUsize>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let hits = Arc::new(AtomicUsize::new(0));
        let h = hits.clone();
        tokio::spawn(async move {
            while let Ok((mut sock, _)) = listener.accept().await {
                h.fetch_add(1, Ordering::SeqCst);
                let mut tmp = [0u8; 1024];
                let _ = sock.read(&mut tmp).await;
                let _ = sock
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
                    .await;
            }
        });
        (addr, hits)
    }

    fn cfg_short() -> Config {
        Config {
            timeout: std::time::Duration::from_secs(5),
            ..Config::default()
        }
    }

    #[tokio::test]
    async fn a_public_name_answering_loopback_never_connects() {
        let (addr, hits) = counting_server().await;
        let resolver = PublicOnly {
            fixed: vec![
                ("rebind.test".into(), vec!["127.0.0.1".parse().unwrap()]),
                (
                    "mixed.test".into(),
                    vec!["10.0.0.1".parse().unwrap(), "100.64.1.1".parse().unwrap()],
                ),
                ("ok.test".into(), vec!["127.0.0.1".parse().unwrap()]),
            ],
        };
        let cfg = cfg_short();
        let clients = build_with(&cfg, resolver).unwrap();
        let port = addr.port();
        for host in ["rebind.test", "mixed.test"] {
            let url = Url::parse(&format!("http://{host}:{port}/")).unwrap();
            let err = get_page(&cfg, &clients, &url, "*/*", false)
                .await
                .err()
                .unwrap_or_else(|| panic!("{host} connected"));
            assert_eq!(
                err,
                HttpError::Private {
                    from: None,
                    to: host.into()
                },
                "{host}"
            );
            assert!(err.to_string().contains("loopback, private or link-local"));
        }
        // Literal private addresses and localhost are refused without a lookup.
        for lit in ["127.0.0.1", "localhost", "a.localhost"] {
            let url = Url::parse(&format!("http://{lit}:{port}/")).unwrap();
            let err = get_page(&cfg, &clients, &url, "*/*", false).await.err();
            assert!(matches!(err, Some(HttpError::Private { .. })), "{lit}");
        }
        assert_eq!(hits.load(Ordering::SeqCst), 0, "a private answer was used");
        // The same URL written as loopback by the user is allowed.
        let url = Url::parse(&format!("http://127.0.0.1:{port}/")).unwrap();
        let body = get_page(&cfg, &clients, &url, "*/*", true)
            .await
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(body.bytes, b"ok");
        assert_eq!(hits.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn dns_overrides_count_as_public() {
        let (addr, hits) = counting_server().await;
        let cfg = cfg_short().with_resolve("docs.test", addr);
        let clients = build(&cfg).unwrap();
        let url = Url::parse("http://docs.test/").unwrap();
        let body = get_page(&cfg, &clients, &url, "*/*", false)
            .await
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(body.bytes, b"ok");
        assert_eq!(hits.load(Ordering::SeqCst), 1);
    }
}
