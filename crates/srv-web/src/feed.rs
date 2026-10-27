//! W-4 `feed(url)` -> normalized items `{title, url, date, summary}` from RSS (0.9x, 1.0,
//! 2.0), Atom, JSON Feed (feed-rs) and iCal (a small VEVENT reader below).
//!
//! Item text is untrusted: HTML in summaries is reduced to text, everything is escaped
//! ([`crate::escape`]) and capped, and the output is JSON, so a feed cannot forge harness
//! syntax.
//!
//! A challenge (the `cf-mitigated` header, a challenge URL or page) stops the call like
//! fetch does (D-5). A body that does not parse as a feed is refused as not-a-feed and never
//! echoed, whatever its content type (rule 5, D-508). The request goes through
//! [`http::get_page`] (one deadline, private-address guard).

use crate::http::{self, host_of};
use crate::{escape, fetch, html, opt_usize, req_str, Web};
use async_trait::async_trait;
use harness_core::{Class, Classified, ToolCtx, ToolError, ToolHandler, ToolOutput, ToolSpec};
use serde::Serialize;
use serde_json::{json, Value};
use std::sync::Arc;

/// Items returned by default, and the most a caller can ask for.
pub const FEED_LIMIT: usize = 30;
pub const FEED_LIMIT_MAX: usize = 100;
const TITLE_MAX: usize = 200;
const SUMMARY_MAX: usize = 300;

const ACCEPT_FEED: &str = "application/rss+xml, application/atom+xml, application/feed+json, \
     application/json;q=0.9, text/calendar;q=0.9, application/xml;q=0.8, text/xml;q=0.8, */*;q=0.5";

/// One normalized item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Item {
    pub title: String,
    pub url: String,
    /// RFC 3339 (`2026-10-25T12:00:00Z`) or a date (`2026-10-25`); empty when unknown.
    pub date: String,
    pub summary: String,
}

/// A parsed feed: its title, its kind and its items in source order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Parsed {
    pub title: String,
    pub kind: &'static str,
    pub items: Vec<Item>,
}

pub(crate) fn spec(web: Arc<Web>) -> ToolSpec {
    ToolSpec {
        name: "feed".into(),
        description: "Read an RSS, Atom, JSON Feed or iCal (.ics) URL as items {title, url, \
            date, summary}. limit items (default 30, max 100)."
            .into(),
        class: Class::Read,
        args: json!({
            "type": "object",
            "properties": {
                "url": {"type": "string"},
                "limit": {"type": "integer", "minimum": 1, "maximum": FEED_LIMIT_MAX, "default": FEED_LIMIT}
            },
            "required": ["url"]
        }),
        handler: Arc::new(Feed { web }),
    }
}

struct Feed {
    web: Arc<Web>,
}

#[async_trait]
impl ToolHandler for Feed {
    async fn classify(&self, _ctx: &ToolCtx, args: &Value) -> Result<Classified, ToolError> {
        let url = http::parse_url(req_str(args, "url")?)
            .map_err(|e| ToolError::Args(format!("url: {e}")))?;
        Ok(Classified {
            class: Some(Class::Read),
            domain: Some(toolstore::registrable(&host_of(&url))),
            what: Some(format!("read feed {}", escape::name(url.as_str(), 200))),
            ..Classified::default()
        })
    }

    async fn call(&self, _ctx: &ToolCtx, args: Value) -> Result<ToolOutput, ToolError> {
        let url = http::parse_url(req_str(&args, "url")?)
            .map_err(|e| ToolError::Args(format!("url: {e}")))?;
        let limit = match opt_usize(&args, "limit")? {
            None => FEED_LIMIT,
            Some(0) => return Err(ToolError::Args("limit must be at least 1".into())),
            Some(n) => n.min(FEED_LIMIT_MAX),
        };
        let host = host_of(&url);
        let clients = self.web.clients()?;
        if let Some(why) = html::challenge_url(&url) {
            self.web.challenged(&url);
            return Ok(fetch::challenge_output(&url, &why));
        }
        let body = http::get_page(
            &self.web.cfg,
            clients,
            &url,
            ACCEPT_FEED,
            http::written_private(&url),
        )
        .await
        .map_err(|e| ToolError::Failed(format!("feed: {e}")))?;
        // D-5 before anything else: a challenge (any status) stops the call.
        // D-582: and drops the challenged site's cached pages.
        let challenged = |stop: ToolOutput| {
            self.web.challenged(&url);
            self.web.challenged(&body.url);
            Ok(stop)
        };
        if let Some(stop) = fetch::challenge_by_response(&body) {
            return challenged(stop);
        }
        let ok = (200..300).contains(&body.status);
        let parsed = if ok {
            parse_with(&body.bytes, body.charset.as_deref())
        } else {
            None
        };
        let Some(parsed) = parsed else {
            // Not a feed: the body is never echoed (rule 5). A challenge page under any
            // status or type still stops the call (D-5).
            let text = body.text(true);
            if let Some(stop) = fetch::challenge_in_text(&body, &text, "feed").await? {
                return challenged(stop);
            }
            if !ok {
                return Err(ToolError::Failed(format!(
                    "feed: {host} answered HTTP {}",
                    body.status
                )));
            }
            return Err(ToolError::Failed(format!(
                "feed: {} did not answer with RSS, Atom, JSON Feed or iCal ({}); use \
                 fetch(url) to read web pages",
                host_of(&body.url),
                if body.mime.is_empty() {
                    "no content type".to_string()
                } else {
                    escape::name(&body.mime, 80)
                }
            )));
        };
        let total = parsed.items.len();
        let items: Vec<&Item> = parsed.items.iter().take(limit).collect();
        let mut text = format!(
            "{} feed {:?}: {} items{}\n",
            parsed.kind,
            parsed.title,
            total,
            if total > items.len() {
                format!(", first {} shown", items.len())
            } else {
                String::new()
            }
        );
        text.push_str(&serde_json::to_string_pretty(&items).unwrap_or_else(|_| "[]".into()));
        Ok(ToolOutput {
            text,
            summary: Some(format!("feed {host}: {total} items")),
            consequences: Value::Null,
            files: Vec::new(),
            stopped: None,
        })
    }
}

/// Parses a feed body of any supported kind; `None` when it is none of them.
pub fn parse(bytes: &[u8]) -> Option<Parsed> {
    parse_with(bytes, None)
}

/// [`parse`] with the response's `charset`. The iCal check runs on the decoded text (a BOM
/// or the charset honoured, so a UTF-16 calendar is one); XML and JSON feeds are parsed from
/// the bytes, whose own encoding declaration feed-rs reads.
pub(crate) fn parse_with(bytes: &[u8], charset: Option<&str>) -> Option<Parsed> {
    let text = http::decode(bytes, charset, false);
    let start = text.trim_start_matches('\u{feff}').trim_start();
    if start
        .as_bytes()
        .get(..15)
        .is_some_and(|h| h.eq_ignore_ascii_case(b"BEGIN:VCALENDAR"))
    {
        return Some(ical::parse(start));
    }
    let feed = feed_rs::parser::parse(bytes).ok()?;
    let kind = match feed.feed_type {
        feed_rs::model::FeedType::Atom => "atom",
        feed_rs::model::FeedType::JSON => "json",
        _ => "rss",
    };
    let items = feed
        .entries
        .iter()
        .map(|e| {
            let summary = e
                .summary
                .as_ref()
                .map(|t| t.content.clone())
                .or_else(|| e.content.as_ref().and_then(|c| c.body.clone()))
                .unwrap_or_default();
            Item {
                title: escape::name(
                    &e.title
                        .as_ref()
                        .map(|t| html_to_text(&t.content))
                        .unwrap_or_default(),
                    TITLE_MAX,
                ),
                url: e
                    .links
                    .iter()
                    .find(|l| l.rel.as_deref().is_none_or(|r| r == "alternate"))
                    .or_else(|| e.links.first())
                    .and_then(|l| http::clean_url(&l.href))
                    .unwrap_or_default(),
                date: e
                    .published
                    .or(e.updated)
                    .map(|d| d.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
                    .unwrap_or_default(),
                summary: escape::name(&html_to_text(&summary), SUMMARY_MAX),
            }
        })
        .collect();
    Some(Parsed {
        title: escape::name(
            &feed
                .title
                .map(|t| html_to_text(&t.content))
                .unwrap_or_default(),
            TITLE_MAX,
        ),
        kind,
        items,
    })
}

/// Text of an HTML fragment (feed summaries and calendar fields are often HTML), collapsed
/// ([`html::fragment_text`]: only text nodes, so no markup reaches the model).
fn html_to_text(s: &str) -> String {
    html::fragment_text(s)
}

/// A minimal RFC 5545 reader: VEVENTs with SUMMARY, URL, DTSTART, DESCRIPTION, LOCATION.
/// Every text field goes through [`html_to_text`]: Google and Outlook calendars put `<br>`,
/// `<a href>` and `<b>` in descriptions.
pub mod ical {
    use super::{escape, html_to_text, Item, Parsed, SUMMARY_MAX, TITLE_MAX};

    /// Parses a VCALENDAR text. Unknown properties and components are ignored.
    pub fn parse(text: &str) -> Parsed {
        let lines = unfold(text);
        let mut title = String::new();
        let mut items = Vec::new();
        let mut cur: Option<Event> = None;
        let mut depth_other = 0usize;
        for line in &lines {
            let Some((name, params, value)) = split(line) else {
                continue;
            };
            match (name.as_str(), value.trim()) {
                ("BEGIN", v) if v.eq_ignore_ascii_case("VEVENT") && cur.is_none() => {
                    cur = Some(Event::default());
                }
                ("BEGIN", v) if !v.eq_ignore_ascii_case("VCALENDAR") => depth_other += 1,
                ("END", v) if v.eq_ignore_ascii_case("VEVENT") && depth_other == 0 => {
                    if let Some(e) = cur.take() {
                        items.push(e.item());
                    }
                }
                ("END", v) if !v.eq_ignore_ascii_case("VCALENDAR") => {
                    depth_other = depth_other.saturating_sub(1)
                }
                _ if depth_other > 0 => {}
                ("X-WR-CALNAME", _) if cur.is_none() => title = text_value(&value),
                _ => {
                    if let Some(e) = cur.as_mut() {
                        match name.as_str() {
                            "SUMMARY" => e.summary = text_value(&value),
                            "URL" => e.url = value.trim().to_string(),
                            "DESCRIPTION" => e.description = text_value(&value),
                            "LOCATION" => e.location = text_value(&value),
                            "DTSTART" => e.start = date(&value, &params),
                            _ => {}
                        }
                    }
                }
            }
        }
        Parsed {
            title: escape::name(&title, TITLE_MAX),
            kind: "ical",
            items,
        }
    }

    #[derive(Default)]
    struct Event {
        summary: String,
        url: String,
        description: String,
        location: String,
        start: String,
    }

    impl Event {
        fn item(self) -> Item {
            let mut summary = self.description;
            if !self.location.trim().is_empty() {
                summary = if summary.trim().is_empty() {
                    format!("at {}", self.location)
                } else {
                    format!("at {}. {summary}", self.location)
                };
            }
            let url = crate::http::clean_url(&self.url).unwrap_or_default();
            Item {
                title: escape::name(&self.summary, TITLE_MAX),
                url,
                date: self.start,
                summary: escape::name(&summary, SUMMARY_MAX),
            }
        }
    }

    /// RFC 5545 3.1: a line starting with a space or tab continues the previous one.
    fn unfold(text: &str) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for raw in text.split('\n') {
            let line = raw.strip_suffix('\r').unwrap_or(raw);
            if let Some(rest) = line.strip_prefix([' ', '\t']) {
                if let Some(last) = out.last_mut() {
                    last.push_str(rest);
                    continue;
                }
            }
            out.push(line.to_string());
        }
        out
    }

    /// `NAME;PARAM=x:value` -> (NAME uppercased, params, value). Quoted params may hold `:`.
    fn split(line: &str) -> Option<(String, String, String)> {
        let mut in_quote = false;
        let mut colon = None;
        for (i, c) in line.char_indices() {
            match c {
                '"' => in_quote = !in_quote,
                ':' if !in_quote => {
                    colon = Some(i);
                    break;
                }
                _ => {}
            }
        }
        let colon = colon?;
        let (head, value) = (&line[..colon], &line[colon + 1..]);
        let (name, params) = head.split_once(';').unwrap_or((head, ""));
        Some((
            name.trim().to_ascii_uppercase(),
            params.to_string(),
            value.to_string(),
        ))
    }

    /// A TEXT value as the model may see it: unescaped, then reduced to text (rule 5).
    fn text_value(v: &str) -> String {
        html_to_text(&unescape(v))
    }

    /// TEXT unescaping (RFC 5545 3.3.11).
    fn unescape(v: &str) -> String {
        let mut out = String::with_capacity(v.len());
        let mut chars = v.chars();
        while let Some(c) = chars.next() {
            if c != '\\' {
                out.push(c);
                continue;
            }
            match chars.next() {
                Some('n' | 'N') => out.push('\n'),
                Some(other) => out.push(other),
                None => {}
            }
        }
        out
    }

    /// `20261025T120000Z` -> `2026-10-25T12:00:00Z`; `20261025` -> `2026-10-25`; a local
    /// time keeps its TZID: `2026-10-25T12:00:00 (Europe/Berlin)`.
    fn date(v: &str, params: &str) -> String {
        let v = v.trim();
        let digits = |s: &str| s.chars().all(|c| c.is_ascii_digit());
        let tzid = params
            .split(';')
            .find_map(|p| {
                p.split_once('=')
                    .filter(|(k, _)| k.eq_ignore_ascii_case("TZID"))
            })
            .map(|(_, z)| z.trim_matches('"').to_string());
        if v.len() == 8 && digits(v) {
            return format!("{}-{}-{}", &v[..4], &v[4..6], &v[6..8]);
        }
        // Byte checks first: once bytes 0..15 are ASCII, every slice below is on a boundary.
        let b = v.as_bytes();
        if b.len() >= 15
            && b[..8].iter().all(u8::is_ascii_digit)
            && b[8] == b'T'
            && b[9..15].iter().all(u8::is_ascii_digit)
        {
            let base = format!(
                "{}-{}-{}T{}:{}:{}",
                &v[..4],
                &v[4..6],
                &v[6..8],
                &v[9..11],
                &v[11..13],
                &v[13..15]
            );
            return match (v.ends_with('Z'), tzid) {
                (true, _) => format!("{base}Z"),
                (false, Some(z)) => format!("{base} ({})", escape::name(&z, 60)),
                (false, None) => base,
            };
        }
        escape::name(v, 40)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn events() {
            let ics = "BEGIN:VCALENDAR\r\nX-WR-CALNAME:Meetups\r\nBEGIN:VEVENT\r\nSUMMARY:Rust\r\n  meetup\r\nDTSTART:20261025T180000Z\r\nURL:https://example.com/e/1\r\nLOCATION:Hall\\, 2nd floor\r\nDESCRIPTION:Talks\\nand pizza\r\nBEGIN:VALARM\r\nSUMMARY:not an event\r\nEND:VALARM\r\nEND:VEVENT\r\nBEGIN:VEVENT\r\nSUMMARY:All day\r\nDTSTART;VALUE=DATE:20261101\r\nEND:VEVENT\r\nBEGIN:VEVENT\r\nSUMMARY:Local\r\nDTSTART;TZID=\"Europe/Berlin\":20261102T090000\r\nURL:javascript:alert(1)\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
            let p = parse(ics);
            assert_eq!(p.title, "Meetups");
            assert_eq!(p.items.len(), 3);
            assert_eq!(p.items[0].title, "Rust meetup");
            assert_eq!(p.items[0].date, "2026-10-25T18:00:00Z");
            assert_eq!(p.items[0].url, "https://example.com/e/1");
            assert_eq!(p.items[0].summary, "at Hall, 2nd floor. Talks and pizza");
            assert_eq!(p.items[1].date, "2026-11-01");
            assert_eq!(p.items[2].date, "2026-11-02T09:00:00 (Europe/Berlin)");
            assert_eq!(p.items[2].url, "");
        }

        #[test]
        fn garbage_does_not_panic() {
            for s in [
                "",
                "BEGIN:VCALENDAR",
                "BEGIN:VEVENT\nDTSTART:2026é\nEND:VEVENT",
                ":::\n;;;",
            ] {
                let _ = parse(s);
            }
            assert_eq!(date("2026101éT1", ""), "2026101éT1");
        }
    }
}
