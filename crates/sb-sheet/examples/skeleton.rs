//! Smoke test without a browser (B-200):
//!
//! ```text
//! cargo run -p sb-sheet --example skeleton -- crates/sb-sheet/examples/register.index.json
//! ```
//!
//! The file is either an `IndexRow[]` array or
//! `{"rows": [...], "text": [...], "url": "...", "title": "...", "site_tools": "...", "dialog": "..."}`
//! (every key but `rows` optional). Prints `skeleton()`.

use sb_sheet::{skeleton, IndexRow, PageMeta, SkeletonOpts, TextBlock};
use serde::Deserialize;
use std::process::ExitCode;

#[derive(Deserialize)]
struct Page {
    rows: Vec<IndexRow>,
    #[serde(default)]
    text: Vec<TextBlock>,
    #[serde(default)]
    url: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    site_tools: Option<String>,
    #[serde(default)]
    dialog: Option<String>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Input {
    Rows(Vec<IndexRow>),
    Page(Page),
}

fn main() -> ExitCode {
    let Some(path) = std::env::args().nth(1) else {
        eprintln!("usage: skeleton <index.json>");
        return ExitCode::from(2);
    };
    let raw = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("cannot read {path}: {e}");
            return ExitCode::FAILURE;
        }
    };
    let page = match serde_json::from_str::<Input>(&raw) {
        Ok(Input::Rows(rows)) => Page {
            rows,
            text: Vec::new(),
            url: String::new(),
            title: String::new(),
            site_tools: None,
            dialog: None,
        },
        Ok(Input::Page(p)) => p,
        Err(e) => {
            eprintln!("{path}: not an IndexRow array or a page object: {e}");
            return ExitCode::FAILURE;
        }
    };
    let opts = SkeletonOpts {
        meta: PageMeta {
            url: page.url,
            title: page.title,
            site_tools: page.site_tools,
            dialog: page.dialog,
        },
    };
    println!("{}", skeleton(&page.rows, &page.text, &opts));
    ExitCode::SUCCESS
}
