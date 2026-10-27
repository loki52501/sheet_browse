//! W-2: "the index is cached per URL for the run so `find` and site tools can use it".
//! One server process serves one run, so the cache lives as long as the process: the last
//! [`CACHE_PAGES`] pages, the last page overall, and the last page per tool folder.

use crate::html::Doc;
use crate::http::host_of;
use reqwest::Url;
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

/// Pages kept for `cursor`, `region` and `find(url=...)`.
pub const CACHE_PAGES: usize = 16;

/// A fetched page.
#[derive(Debug)]
pub(crate) struct Page {
    /// The URL the caller asked for.
    pub requested: String,
    /// The final URL after redirects.
    pub url: String,
    pub doc: Doc,
    /// HTTP status of the final response.
    pub status: u16,
    /// The body was cut at the size cap.
    pub truncated: bool,
    /// Tool folder for the page's host (`ashbyhq.com`), `""` when none.
    pub folder: String,
    /// The T-3 skeleton line, when site tools were loaded.
    pub site_tools: Option<String>,
}

#[derive(Default)]
struct Inner {
    pages: VecDeque<Arc<Page>>,
    last: Option<Arc<Page>>,
    by_folder: HashMap<String, Arc<Page>>,
}

#[derive(Default)]
pub(crate) struct Cache {
    inner: Mutex<Inner>,
}

impl Cache {
    pub fn insert(&self, page: Arc<Page>) {
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        g.pages
            .retain(|p| p.requested != page.requested && p.url != page.url);
        g.pages.push_back(page.clone());
        while g.pages.len() > CACHE_PAGES {
            g.pages.pop_front();
        }
        if !page.folder.is_empty() {
            g.by_folder.insert(page.folder.clone(), page.clone());
        }
        g.last = Some(page);
    }

    /// The cached page for `url` (requested or final URL), newest first.
    pub fn get(&self, url: &str) -> Option<Arc<Page>> {
        let g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        g.pages
            .iter()
            .rev()
            .find(|p| p.requested == url || p.url == url)
            .cloned()
    }

    pub fn last(&self) -> Option<Arc<Page>> {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .last
            .clone()
    }

    /// Drops every cached page on registrable domain `site` (D-582): after a challenge on a
    /// site no site tool, `find` or cursor runs on an older page of it in this run. `last`
    /// falls back to the newest page left.
    pub fn drop_site(&self, site: &str) {
        let on_site = |p: &Page| {
            [&p.url, &p.requested].iter().any(|u| {
                Url::parse(u)
                    .ok()
                    .is_some_and(|u| toolstore::registrable(&host_of(&u)) == site)
            })
        };
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        g.pages.retain(|p| !on_site(p));
        g.by_folder.retain(|_, p| !on_site(p));
        if g.last.as_deref().is_some_and(on_site) {
            g.last = g.pages.back().cloned();
        }
    }

    /// The last page fetched on a host of `folder`; for `_generic`, the last page.
    pub fn for_folder(&self, folder: &str) -> Option<Arc<Page>> {
        let g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if folder == toolstore::GENERIC {
            return g.last.clone();
        }
        g.by_folder.get(folder).cloned()
    }
}
