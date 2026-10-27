//! HTTP plumbing shared by search, fetch, api and feed (W-1..W-4): the clients, a filtering
//! DNS resolver and redirect follower (the private-address guard), one deadline per page
//! request, a capped body reader, charset decoding, a tag scan and error text that names
//! only the host.
//!
//! Error text never comes from `reqwest::Error`'s `Display`: that prints the whole URL
//! (query included) and the source chain. Every error here is our own sentence naming the
//! host, so nothing a caller put in a URL or a header can come back in an error.

use crate::config::Config;
use hyper_util::client::proxy::matcher::Matcher;
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
///
/// B-590, D-509: a proxy (`HTTP(S)_PROXY`, `ALL_PROXY`, the Windows proxy settings) resolves
/// the target itself, so [`PublicOnly`] would never see the address, and a URL written as
/// `localhost` would reach the proxy's localhost. So the clients that may reach this machine
/// or the user's network never use a proxy, and a page request goes through the system proxy
/// only when the proxy applies to its URL and [`PublicOnly`] passes the name here first
/// ([`Clients::page_for`]); otherwise it goes direct, where [`PublicOnly`] decides at the
/// connect. An api request to a loopback or private address (the only place a `@vault`
/// header may travel over plain http) never goes through a proxy ([`Clients::api_for`]).
#[derive(Clone)]
pub(crate) struct Clients {
    /// fetch and feed of a URL that is not written as a private address, sent direct: no
    /// proxy, no automatic redirects ([`get_page`] follows them), and a DNS resolver that
    /// drops loopback, private and link-local answers ([`PublicOnly`]), so a public name can
    /// never connect to the user's machine or network, whatever it resolves to and however
    /// often.
    page: reqwest::Client,
    /// The same requests through the system proxy, for a URL the proxy applies to whose
    /// name [`PublicOnly`] passed ([`Clients::page_for`]). The system resolver, so a proxy
    /// named in the settings resolves (its address is the user's choice).
    page_proxy: reqwest::Client,
    /// fetch and feed of a URL written as a private address or `localhost` (the user asked
    /// for their own machine), and search (the SearXNG URL is operator config, D-31): no
    /// proxy, no automatic redirects, the system resolver.
    page_local: reqwest::Client,
    /// The resolver [`Clients::page`] connects through, shared with the pre-check of a
    /// proxied page request.
    resolver: Arc<PublicOnly>,
    /// The proxy settings, read once when the clients are built (as reqwest reads them for
    /// the proxied clients), to tell whether a URL goes through a proxy.
    proxy: Arc<Matcher>,
    /// api without a vault header, and page.api: redirects only within the same host and
    /// never from https down to http. The system proxy applies.
    api: reqwest::Client,
    /// api with a resolved `@vault` header: redirects only to the same scheme, host and
    /// port, so the header never leaves the origin it was meant for, in clear or otherwise
    /// (reqwest strips `Authorization` on a host change only, and never a custom header).
    /// The system proxy applies: https goes as a CONNECT tunnel, the header inside TLS.
    api_vault: reqwest::Client,
    /// [`Clients::api`] and [`Clients::api_vault`] without a proxy, for a URL written as a
    /// private address or one that stays on this machine ([`Clients::api_for`]).
    api_local: reqwest::Client,
    api_vault_local: reqwest::Client,
    /// Cap on [`Clients::api_for`]'s name lookup: [`LOOKUP_MAX`], or the request timeout
    /// when that is shorter (D-526).
    lookup_max: std::time::Duration,
}

impl Clients {
    /// The api client for `url`, with or without a vault header (`vault`): never a proxy for
    /// a private or loopback target. `overrides` are the config's DNS overrides: a host in
    /// them goes direct (the proxy would resolve the name itself and lose the pinned
    /// address, D-515). When the system proxy applies to a name, the name is resolved here
    /// first, like [`Clients::page_for`]: a name whose every answer is loopback, private or
    /// link-local goes direct (D-515), so a proxy never reaches the user's machine or
    /// network for it. A name with a public answer, no answer or a lookup error follows the
    /// proxy settings (api may need the proxy to resolve; the proxy's own network is the
    /// proxy owner's choice). An IP literal needs no lookup.
    pub(crate) async fn api_for(
        &self,
        url: &Url,
        vault: bool,
        overrides: &[(String, SocketAddr)],
    ) -> &reqwest::Client {
        let local = written_private_or_local(url)
            || is_loopback(url, overrides)
            || overridden(url, overrides)
            || (self.proxied(url) && self.resolves_private(url).await);
        match (vault, local) {
            (true, true) => &self.api_vault_local,
            (true, false) => &self.api_vault,
            (false, true) => &self.api_local,
            (false, false) => &self.api,
        }
    }

    /// Does `url`'s host name resolve only to loopback, private or link-local addresses
    /// ([`PublicOnly`]'s private answer)? An IP literal is not looked up (false: a private
    /// literal is [`written_private`]).
    async fn resolves_private(&self, url: &Url) -> bool {
        match host_kind(url) {
            Some(HostKind::Name(host)) => {
                // A lookup that does not finish in time is no answer.
                let answer = tokio::time::timeout(self.lookup_max, self.resolver.public(&host));
                matches!(answer.await, Ok(Err(e)) if e.downcast_ref::<PrivateAnswer>().is_some())
            }
            _ => false,
        }
    }

    /// Does the system proxy apply to `url`?
    fn proxied(&self, url: &Url) -> bool {
        url.as_str()
            .parse::<http::Uri>()
            .is_ok_and(|u| self.proxy.intercept(&u).is_some())
    }

    /// The client for one hop of a page request ([`get_page`]). A hop written as a private
    /// address or a localhost name, or whose host is a config DNS override (the proxy would
    /// resolve the name itself and lose the pinned address, D-515), is never proxied. With
    /// `allow_private` any other hop follows the proxy settings. Without it a hop goes
    /// through the proxy only when the proxy applies and either its host is a public IP
    /// literal (no lookup needed, D-515) or [`PublicOnly`] passes its name here; otherwise
    /// it goes direct, where [`PublicOnly`] decides at the connect (a private answer is
    /// refused, a name with no answer fails to connect). So a proxy never fetches a name
    /// that resolves to the user's machine or network here.
    async fn page_for(&self, cfg: &Config, url: &Url, allow_private: bool) -> &reqwest::Client {
        let direct = if allow_private {
            &self.page_local
        } else {
            &self.page
        };
        if written_private_or_local(url) || overridden(url, &cfg.resolve) || !self.proxied(url) {
            return direct;
        }
        if allow_private {
            return &self.page_proxy;
        }
        match host_kind(url) {
            // Not private: `written_private_or_local` said so above.
            Some(HostKind::Ip(_)) => &self.page_proxy,
            Some(HostKind::Name(host)) => match self.resolver.public(&host).await {
                Ok(addrs) if !addrs.is_empty() => &self.page_proxy,
                _ => direct,
            },
            None => direct,
        }
    }
}

/// Is `url`'s host one of the config's DNS overrides (tests; empty in production)? Such a
/// request goes direct: reqwest answers the override before any resolver, and a proxy would
/// resolve the name itself instead (D-515).
fn overridden(url: &Url, overrides: &[(String, SocketAddr)]) -> bool {
    match host_kind(url) {
        Some(HostKind::Name(host)) => overrides.iter().any(|(h, _)| *h == host),
        _ => false,
    }
}

pub(crate) fn build(cfg: &Config) -> Result<Clients, String> {
    build_with(cfg, PublicOnly::default(), None)
}

/// `proxy`: `None` reads the system proxy settings (reqwest reads the same ones for the
/// proxied clients); a test passes one proxy URL for every proxied client.
fn build_with(cfg: &Config, resolver: PublicOnly, proxy: Option<&str>) -> Result<Clients, String> {
    let fail = |_| "could not start the HTTP client (TLS backend)".to_string();
    let base = |proxied: bool| -> Result<reqwest::ClientBuilder, String> {
        let mut b = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .timeout(cfg.timeout)
            .connect_timeout(cfg.timeout);
        // Test DNS overrides are answered before any resolver runs, so they count as
        // public (D-507): they stand in for the public site they name.
        for (host, addr) in &cfg.resolve {
            b = b.resolve(host, *addr);
        }
        if !proxied {
            b = b.no_proxy();
        } else if let Some(p) = proxy {
            let p = reqwest::Proxy::all(p).map_err(|_| "the test proxy URL is not valid")?;
            b = b.proxy(p);
        }
        Ok(b)
    };
    let matcher = match proxy {
        None => Matcher::from_system(),
        Some(p) => Matcher::builder().all(p.to_string()).build(),
    };
    let resolver = Arc::new(resolver);
    let page = base(false)?
        .redirect(Policy::none())
        .dns_resolver(Arc::new(SharedPublicOnly(resolver.clone())))
        .build()
        .map_err(fail)?;
    let page_proxy = base(true)?.redirect(Policy::none()).build().map_err(fail)?;
    let page_local = base(false)?
        .redirect(Policy::none())
        .build()
        .map_err(fail)?;
    let api_policy = |strict: bool| {
        Policy::custom(move |attempt| {
            // D-5, D-580: a redirect to a challenge URL (another host or this one) is never
            // followed; the error carries the target so the call stops as a challenge.
            if let Some(why) = crate::html::challenge_url(attempt.url()) {
                return attempt.error(ChallengeRedirect { why });
            }
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
    let api_client = |proxied: bool, strict: bool| -> Result<reqwest::Client, String> {
        base(proxied)?
            .redirect(api_policy(strict))
            .build()
            .map_err(fail)
    };
    Ok(Clients {
        page,
        page_proxy,
        page_local,
        resolver,
        proxy: Arc::new(matcher),
        api: api_client(true, false)?,
        api_vault: api_client(true, true)?,
        api_local: api_client(false, false)?,
        api_vault_local: api_client(false, true)?,
        lookup_max: LOOKUP_MAX.min(cfg.timeout),
    })
}

/// D-526 (review-T-12b should-fix 2): the longest [`Clients::api_for`] waits for its name
/// lookup. A lookup that does not finish by then is no answer (the proxy settings apply),
/// and api's request gets what is left of its timeout.
pub(crate) const LOOKUP_MAX: std::time::Duration = std::time::Duration::from_secs(5);

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

type BoxError = Box<dyn std::error::Error + Send + Sync>;

impl PublicOnly {
    /// The public addresses of `host` (lowercase, no trailing dot): [`PrivateAnswer`] when
    /// every answer is private, an empty list when there is no answer.
    async fn public(&self, host: &str) -> Result<Vec<SocketAddr>, BoxError> {
        let fixed = self
            .fixed
            .iter()
            .find(|(h, _)| h == host)
            .map(|(_, ips)| ips.clone());
        let ips: Vec<IpAddr> = match fixed {
            Some(ips) => ips,
            None => tokio::net::lookup_host((host, 0))
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
            return Err(Box::new(PrivateAnswer));
        }
        Ok(public)
    }
}

/// [`PublicOnly`] as the direct page client's resolver, shared with the pre-check of a
/// proxied page request ([`Clients::page_for`]), so both ask the same resolver.
struct SharedPublicOnly(Arc<PublicOnly>);

impl Resolve for SharedPublicOnly {
    fn resolve(&self, name: Name) -> Resolving {
        let host = name.as_str().trim_end_matches('.').to_ascii_lowercase();
        let resolver = self.0.clone();
        Box::pin(async move {
            let public = resolver.public(&host).await?;
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

/// An api or page.api redirect to a challenge URL (D-5, D-580), refused by the redirect
/// policy before it is followed. `why` is [`crate::html::challenge_url`]'s reason for the
/// target (it names a challenge host; a challenge path is named by kind).
#[derive(Debug)]
pub(crate) struct ChallengeRedirect {
    why: String,
}

impl fmt::Display for ChallengeRedirect {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("redirect to a human check")
    }
}

impl std::error::Error for ChallengeRedirect {}

/// Why, when `e` is a redirect the api policy refused because it led to a challenge
/// ([`ChallengeRedirect`]): `redirect to <reason>`. reqwest's own `Error::url()` is the URL
/// that redirected, not the target, so the reason travels in the error.
pub(crate) fn challenge_redirect(e: &reqwest::Error) -> Option<String> {
    let mut cur: Option<&(dyn std::error::Error + 'static)> = Some(e);
    while let Some(err) = cur {
        if let Some(c) = err.downcast_ref::<ChallengeRedirect>() {
            return Some(format!("redirect to {}", c.why));
        }
        cur = err.source();
    }
    None
}

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
    let whole = async {
        let resp = follow(cfg, clients, start, accept, allow_private).await?;
        read_capped(resp, cfg.max_bytes).await
    };
    tokio::time::timeout(cfg.timeout, whole)
        .await
        .map_err(|_| HttpError::Timeout(host_of(start)))?
}

async fn follow(
    cfg: &Config,
    clients: &Clients,
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
        let client = clients.page_for(cfg, &url, allow_private).await;
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

/// A byte cap as the model reads it: `5 MB` for whole mebibytes, else `1500 byte`.
pub(crate) fn cap_text(cap: usize) -> String {
    const MB: usize = 1024 * 1024;
    if cap >= MB && cap.is_multiple_of(MB) {
        format!("{} MB", cap / MB)
    } else {
        format!("{cap} byte")
    }
}

/// `(mime, charset)` from `Content-Type`.
pub(crate) fn content_type(headers: &HeaderMap) -> (String, Option<String>) {
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

/// Longest content type shown as written.
const MIME_SHOWN_MAX: usize = 80;

/// The response's content type as the model may see it (D-511): `mime` itself when it is a
/// plain media type (`type/subtype`, each made of RFC 6838 restricted-name characters, at
/// most 80 characters in all), `no content type` when there is none, else `unusual content
/// type`. A header is server text: nothing tag-, link- or fence-shaped from it is shown.
pub(crate) fn shown_mime(mime: &str) -> String {
    if mime.is_empty() {
        return "no content type".into();
    }
    let name = |s: &str| {
        s.as_bytes().first().is_some_and(u8::is_ascii_alphanumeric)
            && s.bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"!#$&-^_.+".contains(&c))
    };
    match mime.split_once('/') {
        Some((t, sub)) if mime.len() <= MIME_SHOWN_MAX && name(t) && name(sub) => mime.to_string(),
        _ => "unusual content type".into(),
    }
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
        let clients = build_with(&cfg, resolver, None).unwrap();
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

    /// A listener on 127.0.0.1 that records the first line of each request and answers
    /// 200 `px` (a forward proxy's view: `GET http://host/path`).
    async fn recording_proxy() -> (SocketAddr, Arc<std::sync::Mutex<Vec<String>>>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let seen: Arc<std::sync::Mutex<Vec<String>>> = Arc::default();
        let s = seen.clone();
        tokio::spawn(async move {
            while let Ok((mut sock, _)) = listener.accept().await {
                let mut tmp = [0u8; 4096];
                let n = sock.read(&mut tmp).await.unwrap_or(0);
                let head = String::from_utf8_lossy(&tmp[..n]).into_owned();
                s.lock()
                    .unwrap()
                    .push(head.lines().next().unwrap_or("").to_string());
                let _ = sock
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\npx")
                    .await;
            }
        });
        (addr, seen)
    }

    /// B-590, D-509: with a proxy configured, a name that resolves private is never handed
    /// to the proxy (which would resolve and fetch it itself), a URL written as loopback
    /// goes direct, and a public name goes through the proxy.
    #[tokio::test]
    async fn a_proxy_never_fetches_a_private_target() {
        let (origin, hits) = counting_server().await;
        let (proxy_addr, proxied) = recording_proxy().await;
        let resolver = PublicOnly {
            fixed: vec![
                ("rebind.test".into(), vec!["127.0.0.1".parse().unwrap()]),
                ("lan.test".into(), vec!["192.168.1.10".parse().unwrap()]),
                ("nx.test".into(), vec![]),
                ("pub.test".into(), vec!["93.184.216.34".parse().unwrap()]),
            ],
        };
        let cfg = cfg_short();
        let proxy = format!("http://{proxy_addr}");
        let clients = build_with(&cfg, resolver, Some(&proxy)).unwrap();
        let port = origin.port();
        for host in ["rebind.test", "lan.test"] {
            let url = Url::parse(&format!("http://{host}:{port}/")).unwrap();
            let err = get_page(&cfg, &clients, &url, "*/*", false).await.err();
            assert!(matches!(err, Some(HttpError::Private { .. })), "{host}");
        }
        let url = Url::parse(&format!("http://nx.test:{port}/")).unwrap();
        let err = get_page(&cfg, &clients, &url, "*/*", false).await.err();
        assert!(
            matches!(err, Some(HttpError::Connect(_))),
            "nx.test: {err:?}"
        );
        assert!(
            proxied.lock().unwrap().is_empty(),
            "a private or unresolved name went to the proxy"
        );
        // Written as loopback by the user: direct to this machine, never the proxy.
        for lit in ["127.0.0.1", "localhost"] {
            let url = Url::parse(&format!("http://{lit}:{port}/a")).unwrap();
            let body = get_page(&cfg, &clients, &url, "*/*", true)
                .await
                .unwrap_or_else(|e| panic!("{lit}: {e}"));
            assert_eq!(body.bytes, b"ok", "{lit}");
        }
        assert_eq!(hits.load(Ordering::SeqCst), 2);
        assert!(
            proxied.lock().unwrap().is_empty(),
            "loopback went to the proxy"
        );
        // Control: a public name goes through the proxy.
        let url = Url::parse("http://pub.test/p").unwrap();
        let body = get_page(&cfg, &clients, &url, "*/*", false)
            .await
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(body.bytes, b"px");
        let seen = proxied.lock().unwrap().clone();
        assert_eq!(seen, ["GET http://pub.test/p HTTP/1.1"]);
        assert_eq!(hits.load(Ordering::SeqCst), 2);
        // api: a loopback target never uses the proxy; a public one does.
        let resp = clients
            .api_for(
                &Url::parse(&format!("http://127.0.0.1:{port}/")).unwrap(),
                true,
                &[],
            )
            .await
            .get(format!("http://127.0.0.1:{port}/"))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.text().await.unwrap(), "ok");
        let resp = clients
            .api_for(&Url::parse("http://pub.test/q").unwrap(), true, &[])
            .await
            .get("http://pub.test/q")
            .send()
            .await
            .unwrap();
        assert_eq!(resp.text().await.unwrap(), "px");
        assert_eq!(proxied.lock().unwrap().len(), 2);
        // D-515, review-T-12b should-fix 4: a public IPv6 literal needs no lookup and goes
        // through the proxy like a public name.
        let url = Url::parse("http://[2606:4700::1]/v6").unwrap();
        let body = get_page(&cfg, &clients, &url, "*/*", false)
            .await
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(body.bytes, b"px");
        assert_eq!(
            proxied.lock().unwrap().last().map(String::as_str),
            Some("GET http://[2606:4700::1]/v6 HTTP/1.1")
        );
        // The origin got the two page requests and the loopback api request only.
        assert_eq!(hits.load(Ordering::SeqCst), 3);
    }

    /// D-526 (review-T-12b round 2 should-fix 2): api's name lookup has its own short cap
    /// within the request budget, not the whole request timeout.
    #[test]
    fn the_api_lookup_cap_is_short() {
        let mut cfg = Config::default();
        assert_eq!(cfg.timeout, std::time::Duration::from_secs(15));
        let clients = build_with(&cfg, PublicOnly::default(), None).unwrap();
        assert_eq!(clients.lookup_max, LOOKUP_MAX);
        assert_eq!(LOOKUP_MAX, std::time::Duration::from_secs(5));
        cfg.timeout = std::time::Duration::from_secs(2);
        let clients = build_with(&cfg, PublicOnly::default(), None).unwrap();
        assert_eq!(clients.lookup_max, std::time::Duration::from_secs(2));
    }

    /// D-515 (B-590, review-T-12b should-fix 2 and 4): which api client a URL gets with a
    /// proxy configured. A name answering only private addresses, a private literal in any
    /// spelling and a config DNS override go direct, with or without a vault header; a
    /// public name, a public IP literal and a name with no answer follow the proxy.
    #[tokio::test]
    async fn api_never_hands_a_private_name_to_the_proxy() {
        let resolver = PublicOnly {
            fixed: vec![
                ("rebind.test".into(), vec!["127.0.0.1".parse().unwrap()]),
                ("lan.test".into(), vec!["192.168.1.10".parse().unwrap()]),
                (
                    "mixed.test".into(),
                    vec!["10.0.0.1".parse().unwrap(), "fe80::1".parse().unwrap()],
                ),
                ("nx.test".into(), vec![]),
                ("pub.test".into(), vec!["93.184.216.34".parse().unwrap()]),
            ],
        };
        let cfg = cfg_short();
        let clients = build_with(&cfg, resolver, Some("http://127.0.0.1:9")).unwrap();
        let ov: Vec<(String, SocketAddr)> =
            vec![("over.test".into(), "93.184.216.34:80".parse().unwrap())];
        for (u, local) in [
            ("http://rebind.test/a", true),
            ("https://rebind.test./a", true),
            ("http://LAN.test:8080/a", true),
            ("http://mixed.test/a", true),
            ("http://[::ffff:127.0.0.1]/a", true),
            ("http://[fd00::1]/a", true),
            ("http://10.1.2.3/a", true),
            ("http://over.test/a", true),
            ("http://pub.test/a", false),
            ("https://pub.test/a", false),
            ("http://nx.test/a", false),
            ("http://[2606:4700::1]/a", false),
            ("http://93.184.216.34/a", false),
        ] {
            let url = Url::parse(u).unwrap();
            for vault in [false, true] {
                let got = clients.api_for(&url, vault, &ov).await;
                let want = match (vault, local) {
                    (true, true) => &clients.api_vault_local,
                    (true, false) => &clients.api_vault,
                    (false, true) => &clients.api_local,
                    (false, false) => &clients.api,
                };
                assert!(
                    std::ptr::eq(got, want),
                    "{u} vault={vault}: want local={local}"
                );
            }
        }
    }

    /// A forward proxy on 127.0.0.1 that answers every request with a 302 to the target
    /// its path names (`/to-rebind`, `/to-mapped`), and records each request line.
    async fn redirecting_proxy(port: u16) -> (SocketAddr, Arc<std::sync::Mutex<Vec<String>>>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let seen: Arc<std::sync::Mutex<Vec<String>>> = Arc::default();
        let s = seen.clone();
        tokio::spawn(async move {
            while let Ok((mut sock, _)) = listener.accept().await {
                let mut tmp = [0u8; 4096];
                let n = sock.read(&mut tmp).await.unwrap_or(0);
                let head = String::from_utf8_lossy(&tmp[..n]).into_owned();
                let line = head.lines().next().unwrap_or("").to_string();
                let to = if line.contains("/to-mapped") {
                    format!("http://[::ffff:127.0.0.1]:{port}/")
                } else {
                    format!("http://rebind.test:{port}/")
                };
                s.lock().unwrap().push(line);
                let resp =
                    format!("HTTP/1.1 302 Found\r\nLocation: {to}\r\nContent-Length: 0\r\n\r\n");
                let _ = sock.write_all(resp.as_bytes()).await;
            }
        });
        (addr, seen)
    }

    /// review-T-12b should-fix 3 (D-509): a public URL that the proxy answers with a
    /// redirect to a private target (a name resolving to loopback, an IPv4-mapped loopback
    /// literal) is refused at that hop: one proxied request, nothing reaches this machine.
    #[tokio::test]
    async fn a_proxied_redirect_to_a_private_target_is_refused() {
        let (origin, hits) = counting_server().await;
        let (proxy_addr, proxied) = redirecting_proxy(origin.port()).await;
        let resolver = PublicOnly {
            fixed: vec![
                ("rebind.test".into(), vec!["127.0.0.1".parse().unwrap()]),
                ("pub.test".into(), vec!["93.184.216.34".parse().unwrap()]),
            ],
        };
        let cfg = cfg_short();
        let clients = build_with(&cfg, resolver, Some(&format!("http://{proxy_addr}"))).unwrap();
        for (path, to) in [
            ("/to-rebind", "rebind.test"),
            ("/to-mapped", "[::ffff:7f00:1]"),
        ] {
            let before = proxied.lock().unwrap().len();
            let url = Url::parse(&format!("http://pub.test{path}")).unwrap();
            let err = get_page(&cfg, &clients, &url, "*/*", false).await.err();
            assert_eq!(
                err,
                Some(HttpError::Private {
                    from: Some("pub.test".into()),
                    to: to.into()
                }),
                "{path}"
            );
            let seen = proxied.lock().unwrap().clone();
            assert_eq!(seen.len(), before + 1, "{path}: one proxied request");
            assert_eq!(seen[before], format!("GET http://pub.test{path} HTTP/1.1"));
        }
        assert_eq!(
            hits.load(Ordering::SeqCst),
            0,
            "a private target was reached"
        );
    }

    /// D-515 (review-T-12b should-fix 2): a host in the config's DNS overrides goes direct
    /// to its pinned address even with a proxy configured (the proxy would resolve the
    /// name itself).
    #[tokio::test]
    async fn dns_override_hosts_never_go_through_the_proxy() {
        let (origin, hits) = counting_server().await;
        let (proxy_addr, proxied) = recording_proxy().await;
        let cfg = cfg_short().with_resolve("over.test", origin);
        // The pre-check would pass the name (a public answer), so only the override rule
        // keeps it off the proxy.
        let resolver = PublicOnly {
            fixed: vec![("over.test".into(), vec!["93.184.216.34".parse().unwrap()])],
        };
        let clients = build_with(&cfg, resolver, Some(&format!("http://{proxy_addr}"))).unwrap();
        let url = Url::parse("http://over.test/x").unwrap();
        let body = get_page(&cfg, &clients, &url, "*/*", false)
            .await
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(body.bytes, b"ok");
        let resp = clients
            .api_for(&url, false, &cfg.resolve)
            .await
            .get(url.clone())
            .send()
            .await
            .unwrap();
        assert_eq!(resp.text().await.unwrap(), "ok");
        assert_eq!(hits.load(Ordering::SeqCst), 2);
        assert!(
            proxied.lock().unwrap().is_empty(),
            "an override host was proxied"
        );
    }

    #[test]
    fn content_types_are_shown_only_when_plain() {
        for ok in [
            "text/html",
            "application/vnd.github+json",
            "application/atom+xml",
            "image/svg+xml",
        ] {
            assert_eq!(shown_mime(ok), ok);
        }
        assert_eq!(shown_mime(""), "no content type");
        for bad in [
            "<script>alert(1)</script>",
            "text/<b>",
            "text/html<!--",
            "[x](#3)/y",
            "text/```",
            "text",
            "a/b/c",
            "/x",
            "text/ html",
            &format!("text/{}", "a".repeat(80)),
        ] {
            assert_eq!(shown_mime(bad), "unusual content type", "{bad}");
        }
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
