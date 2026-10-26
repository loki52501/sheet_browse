//! Locates the D-34 Chromium for the real-browser tests: `SB_CHROMIUM`, else the local
//! Playwright copy of the same build (chromium-1248 = Chrome for Testing 156.0.8078.4, D-601).

use std::path::PathBuf;

pub const PLAYWRIGHT_CHROMIUM: &str =
    r"C:\Users\Admin\AppData\Local\ms-playwright\chromium-1248\chrome-win64\chrome.exe";

/// The browser to test against, or `None` (and a printed reason) when none is installed.
pub fn chromium() -> Option<PathBuf> {
    let candidates = std::env::var_os("SB_CHROMIUM")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .into_iter()
        .chain(Some(PathBuf::from(PLAYWRIGHT_CHROMIUM)));
    for c in candidates {
        if c.is_file() {
            return Some(c);
        }
    }
    eprintln!(
        "SKIP: no Chromium (set SB_CHROMIUM or install Playwright chromium-1248 at {PLAYWRIGHT_CHROMIUM})"
    );
    None
}
