//! Classify-to-call pinning (rule 4).
//!
//! `fetch(url="#12")` and site tools resolve "the last page" when they are classified. The
//! wrapper runs `classify`, gates the classified domain, then runs `call`; other calls can
//! land in between (serve_io runs calls concurrently), so resolving again in `call` could
//! act on a page whose domain the gate never saw. Instead `classify` pins what it resolved
//! and `call` takes exactly that.
//!
//! The key is the call's [`ToolCtx`]: `harness_core::dispatch` builds one per call and hands
//! the same one to `classify` and `call`, so its address names one call while that call is
//! in flight (a later call can reuse the address only after this one is gone, and its own
//! `classify` overwrites the slot). The arguments are part of the key too. A `call` that
//! finds no pin (evicted, or a host that skipped `classify`) is refused, never re-resolved.

use harness_core::{ToolCtx, ToolError};
use serde_json::Value;
use std::collections::VecDeque;
use std::sync::Mutex;

/// Pins kept at once; the oldest goes first (a pin is normally taken within one call).
const PINS_MAX: usize = 256;

/// The refusal when a call's pin is gone.
pub const CHANGED: &str = "the page changed since the call was checked; call again";

pub(crate) struct Pins<T> {
    inner: Mutex<VecDeque<(usize, String, T)>>,
}

impl<T> Default for Pins<T> {
    fn default() -> Self {
        Pins {
            inner: Mutex::new(VecDeque::new()),
        }
    }
}

fn key(ctx: &ToolCtx) -> usize {
    ctx as *const ToolCtx as usize
}

impl<T> Pins<T> {
    /// Pins `value` for the call `ctx` with `args` (from `classify`).
    pub fn put(&self, ctx: &ToolCtx, args: &Value, value: T) {
        let k = key(ctx);
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        g.retain(|(at, _, _)| *at != k);
        g.push_back((k, args.to_string(), value));
        while g.len() > PINS_MAX {
            g.pop_front();
        }
    }

    /// Takes the pin of the call `ctx` with `args` (from `call`); [`CHANGED`] when absent.
    pub fn take(&self, ctx: &ToolCtx, args: &Value) -> Result<T, ToolError> {
        let k = key(ctx);
        let a = args.to_string();
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let at = g
            .iter()
            .position(|(at, args, _)| *at == k && *args == a)
            .ok_or_else(|| ToolError::Failed(CHANGED.into()))?;
        g.remove(at)
            .map(|(_, _, v)| v)
            .ok_or_else(|| ToolError::Failed(CHANGED.into()))
    }
}
