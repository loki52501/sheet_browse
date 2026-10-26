//! Locates the D-34 Chromium (Chrome for Testing 156.0.8078.4, D-601) for the real-browser
//! tests. The same file lives in sb-cdp and sb-driver (sb-cdp cannot depend on sb-driver).
//!
//! Resolution, first hit wins:
//! 1. `SB_CHROMIUM`. When set it decides: a missing file fails the test (as D-601 does).
//! 2. The D-601 pinned path `<SB_HOME or workspace root>/chromium/156.0.8078.4/<platform>/<binary>`.
//! 3. Playwright's copy of the same build, chromium-1248, under `PLAYWRIGHT_BROWSERS_PATH` or
//!    Playwright's per-user default (`%LOCALAPPDATA%\ms-playwright` on Windows,
//!    `~/.cache/ms-playwright` on Linux, `~/Library/Caches/ms-playwright` on macOS).
//!
//! Skip policy: a real-browser test never passes silently. When no browser is found the test
//! FAILS, naming every path it tried. To skip on purpose, set `SB_CHROMIUM_TESTS=0`: then the
//! test prints `SKIP: <test>: ...` to stderr and returns, so the skip is a visible choice of
//! whoever runs the suite. On a machine with Playwright's chromium-1248 (the swarm machine)
//! the tests run by default with no variable set.

#![allow(dead_code)]

use std::path::PathBuf;

pub const CHROMIUM_VERSION: &str = "156.0.8078.4";
pub const PLAYWRIGHT_REVISION: &str = "chromium-1248";

fn platform_dir() -> &'static str {
    if cfg!(windows) {
        "chrome-win64"
    } else if cfg!(target_os = "macos") {
        if cfg!(target_arch = "aarch64") {
            "chrome-mac-arm64"
        } else {
            "chrome-mac-x64"
        }
    } else {
        "chrome-linux64"
    }
}

fn binary_name() -> &'static str {
    if cfg!(windows) {
        "chrome.exe"
    } else if cfg!(target_os = "macos") {
        "Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing"
    } else {
        "chrome"
    }
}

fn var(k: &str) -> Option<PathBuf> {
    std::env::var_os(k)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

fn playwright_root() -> Option<PathBuf> {
    if let Some(p) = var("PLAYWRIGHT_BROWSERS_PATH") {
        return Some(p);
    }
    if cfg!(windows) {
        var("LOCALAPPDATA").map(|d| d.join("ms-playwright"))
    } else if cfg!(target_os = "macos") {
        var("HOME").map(|h| h.join("Library/Caches/ms-playwright"))
    } else {
        var("HOME").map(|h| h.join(".cache/ms-playwright"))
    }
}

/// Candidate paths after `SB_CHROMIUM`, in order.
fn candidates() -> Vec<PathBuf> {
    let workspace = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let home = var("SB_HOME").unwrap_or(workspace);
    let mut v = vec![home
        .join("chromium")
        .join(CHROMIUM_VERSION)
        .join(platform_dir())
        .join(binary_name())];
    if let Some(root) = playwright_root() {
        v.push(
            root.join(PLAYWRIGHT_REVISION)
                .join(platform_dir())
                .join(binary_name()),
        );
    }
    v
}

/// The browser to test against. `None` only when `SB_CHROMIUM_TESTS=0` asks to skip (and a
/// `SKIP:` line names the test); otherwise a missing browser fails the test.
pub fn chromium() -> Option<PathBuf> {
    // libtest names each test's thread after the test; tokio::test runs the body on it.
    let current = std::thread::current();
    let test = current.name().unwrap_or("real-browser test");
    if let Some(p) = var("SB_CHROMIUM") {
        assert!(
            p.is_file(),
            "{test}: SB_CHROMIUM={} does not exist",
            p.display()
        );
        return Some(p);
    }
    let tried = candidates();
    if let Some(p) = tried.iter().find(|p| p.is_file()) {
        return Some(p.clone());
    }
    let list = tried
        .iter()
        .map(|p| p.display().to_string())
        .collect::<Vec<_>>()
        .join(", ");
    if std::env::var("SB_CHROMIUM_TESTS").as_deref() == Ok("0") {
        eprintln!("SKIP: {test}: no Chromium (SB_CHROMIUM_TESTS=0); tried {list}");
        return None;
    }
    panic!(
        "{test}: no Chromium {CHROMIUM_VERSION}: set SB_CHROMIUM, or install Playwright \
         {PLAYWRIGHT_REVISION}; tried {list}. Set SB_CHROMIUM_TESTS=0 to skip on purpose."
    );
}
