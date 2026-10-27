//! W-1 `search(q, k=8)` -> `[{title, url, snippet}]` through the local SearXNG instance
//! (D-31: compose profile `search`, 127.0.0.1:8888, JSON output on, limiter off). No
//! scraping of search engine result pages.
//!
//! Upstream engines rate-limit SearXNG, so degraded results are reported honestly
//! (D-31): unresponsive engines are listed, zero results say so, and a backend that is
//! down is an error that says how to start it.

use crate::dom::Budget;
use crate::http;
use crate::{escape, opt_usize, req_str, Web};
use async_trait::async_trait;
use harness_core::{Class, Classified, ToolCtx, ToolError, ToolHandler, ToolOutput, ToolSpec};
use serde::Serialize;
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// W-1 default result count, and the most a caller can ask for.
pub const SEARCH_K: usize = 8;
pub const SEARCH_K_MAX: usize = 20;
const TITLE_MAX: usize = 200;
const SNIPPET_MAX: usize = 300;

pub(crate) fn spec(web: Arc<Web>) -> ToolSpec {
    ToolSpec {
        name: "search".into(),
        description: "Web search through the local SearXNG instance. Returns k results \
            (default 8, max 20) as [{title, url, snippet}], and names engines that did not \
            answer. Cite only what search and fetch returned in this run, not memory."
            .into(),
        class: Class::Read,
        args: json!({
            "type": "object",
            "properties": {
                "q": {"type": "string"},
                "k": {"type": "integer", "minimum": 1, "maximum": SEARCH_K_MAX, "default": SEARCH_K}
            },
            "required": ["q"]
        }),
        handler: Arc::new(Search { web }),
    }
}

/// One result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Hit {
    pub title: String,
    pub url: String,
    pub snippet: String,
}

/// What a SearXNG JSON answer holds, normalized.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Answer {
    pub hits: Vec<Hit>,
    /// `engine (reason)` for every engine that did not answer.
    pub unresponsive: Vec<String>,
}

/// Parses a SearXNG `format=json` answer. Results without an http(s) URL are dropped.
pub fn parse_answer(v: &Value) -> Answer {
    parse_answer_in(v, &Budget::unlimited())
}

/// [`parse_answer`] within `budget` (D-519): titles and snippets are bounded HTML parses
/// ([`crate::html::fragment_text_in`]); once the budget is spent the rest are empty and
/// [`Budget::spent`] says so.
pub(crate) fn parse_answer_in(v: &Value, budget: &Budget) -> Answer {
    let hits = v
        .get("results")
        .and_then(Value::as_array)
        .map(|rs| {
            rs.iter()
                .filter_map(|r| {
                    let url = http::clean_url(r.get("url")?.as_str()?)?;
                    let s = |k: &str| {
                        crate::html::fragment_text_in(
                            r.get(k).and_then(Value::as_str).unwrap_or(""),
                            budget,
                        )
                    };
                    Some(Hit {
                        title: escape::name(&s("title"), TITLE_MAX),
                        url,
                        snippet: escape::name(&s("content"), SNIPPET_MAX),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let unresponsive = v
        .get("unresponsive_engines")
        .and_then(Value::as_array)
        .map(|es| {
            es.iter()
                .filter_map(|e| match e {
                    Value::Array(parts) => {
                        let name = parts.first()?.as_str()?;
                        let why = parts.get(1).and_then(Value::as_str).unwrap_or("no answer");
                        Some(format!(
                            "{} ({})",
                            escape::name(name, 40),
                            escape::name(why, 60)
                        ))
                    }
                    Value::String(name) => Some(escape::name(name, 40)),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default();
    Answer { hits, unresponsive }
}

/// [`parse_answer_in`] off the async threads, capped at `max` like fetch's walk (D-517,
/// D-519): a backend that sends deep or huge markup in its fields cannot stall a worker.
async fn answer_off_thread(v: Value, max: Duration) -> Result<Answer, ToolError> {
    let deadline = Instant::now() + max;
    let task = tokio::task::spawn_blocking(move || {
        let budget = Budget::until(deadline);
        let answer = parse_answer_in(&v, &budget);
        (!budget.spent()).then_some(answer)
    });
    match tokio::time::timeout(max, task).await {
        Ok(Ok(Some(answer))) => Ok(answer),
        Ok(Ok(None)) | Err(_) => Err(ToolError::Failed(format!(
            "search: the SearXNG answer took longer than {} s to read",
            max.as_secs()
        ))),
        Ok(Err(_)) => Err(ToolError::Failed(
            "search: reading the SearXNG answer failed".into(),
        )),
    }
}

struct Search {
    web: Arc<Web>,
}

#[async_trait]
impl ToolHandler for Search {
    async fn classify(&self, _ctx: &ToolCtx, args: &Value) -> Result<Classified, ToolError> {
        let q = req_str(args, "q")?;
        Ok(Classified {
            class: Some(Class::Read),
            what: Some(format!("web search {:?}", escape::name(q, 120))),
            ..Classified::default()
        })
    }

    async fn call(&self, _ctx: &ToolCtx, args: Value) -> Result<ToolOutput, ToolError> {
        let q = req_str(&args, "q")?.trim().to_string();
        let k = match opt_usize(&args, "k")? {
            None => SEARCH_K,
            Some(0) => return Err(ToolError::Args("k must be at least 1".into())),
            Some(k) => k.min(SEARCH_K_MAX),
        };
        let base = self.web.cfg.searxng_url.trim_end_matches('/');
        let mut url = http::parse_url(&format!("{base}/search")).map_err(|e| {
            ToolError::Failed(format!("search: the SearXNG URL is not usable ({e})"))
        })?;
        url.query_pairs_mut()
            .append_pair("q", &q)
            .append_pair("format", "json")
            .append_pair("pageno", "1");
        let host = http::host_of(&url);
        let down = || {
            ToolError::Failed(format!(
                "search: the SearXNG backend at {host} is not answering; start it with \
                 `docker compose --profile search up -d searxng` (D-31)"
            ))
        };
        let clients = self.web.clients()?;
        // The SearXNG URL is operator config (D-31), usually loopback or a compose service
        // name on a private network: private addresses are allowed.
        let body = http::get_page(&self.web.cfg, clients, &url, "application/json", true)
            .await
            .map_err(|_| down())?;
        if body.status == 403 || body.status == 429 {
            return Err(ToolError::Failed(format!(
                "search: SearXNG refused the request (HTTP {}); JSON output must be on and the \
                 limiter off in config/searxng/settings.yml",
                body.status
            )));
        }
        if !(200..300).contains(&body.status) {
            return Err(ToolError::Failed(format!(
                "search: SearXNG answered HTTP {}",
                body.status
            )));
        }
        let v: Value = serde_json::from_slice(&body.bytes)
            .map_err(|_| ToolError::Failed("search: SearXNG did not answer with JSON".into()))?;
        let answer = answer_off_thread(v, crate::fetch::WALK_MAX).await?;
        let total = answer.hits.len();
        let hits: Vec<&Hit> = answer.hits.iter().take(k).collect();
        let mut text = format!(
            "search {:?}: {} result{}",
            escape::name(&q, 120),
            hits.len(),
            if hits.len() == 1 { "" } else { "s" }
        );
        if total == 0 {
            text.push_str(" (no engine returned anything for this query)");
        }
        if !answer.unresponsive.is_empty() {
            text.push_str(&format!(
                "\nunresponsive engines: {}",
                answer.unresponsive.join(", ")
            ));
        }
        text.push('\n');
        text.push_str(&serde_json::to_string_pretty(&hits).unwrap_or_else(|_| "[]".into()));
        Ok(ToolOutput {
            text,
            summary: Some(format!(
                "search: {} results, {} engines unresponsive",
                hits.len(),
                answer.unresponsive.len()
            )),
            consequences: Value::Null,
            files: Vec::new(),
            stopped: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// D-519: search's title and snippet parses run under a cap like fetch's walk.
    #[tokio::test]
    async fn a_search_answer_past_its_cap_fails_the_call() {
        let v = json!({"results": [
            {"url": "https://a.test/", "title": "<b>T</b>", "content": "<p>words</p>"}
        ]});
        let err = answer_off_thread(v.clone(), Duration::ZERO)
            .await
            .expect_err("a zero cap fails the call");
        assert!(err.to_string().contains("took longer than 0 s"), "{err}");
        let a = answer_off_thread(v, crate::fetch::WALK_MAX).await.unwrap();
        assert_eq!(a.hits.len(), 1);
        assert_eq!(a.hits[0].title, "T");
        assert_eq!(a.hits[0].snippet, "words");
    }
}
