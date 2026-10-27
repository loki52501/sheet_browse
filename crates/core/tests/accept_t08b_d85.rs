//! T-08b acceptance, D-85: `harness_core::sched::SchedError` is `#[non_exhaustive]`.
//!
//! This file is its own crate, so `non_exhaustive` applies here. The match below names every
//! variant and also has a `_` arm. With `unreachable_patterns` denied, it compiles only while
//! the enum is `#[non_exhaustive]`: drop the attribute and the `_` arm becomes unreachable, so
//! this test binary stops compiling (red). The source check is a second guard for when a new
//! variant would make the `_` arm reachable anyway.
#![deny(unreachable_patterns)]

use harness_core::sched::SchedError;

fn kind(e: &SchedError) -> &'static str {
    match e {
        SchedError::BadCron { .. } => "bad_cron",
        SchedError::NotInPhase1(_) => "not_in_phase1",
        SchedError::Memory(_) => "memory",
        SchedError::Ledger(_) => "ledger",
        SchedError::Io(_) => "io",
        SchedError::BeforeRun(_) => "before_run",
        // Reachable from outside the crate only because SchedError is #[non_exhaustive].
        _ => "other",
    }
}

#[test]
fn d85_sched_error_needs_a_wildcard_arm_outside_the_crate() {
    let e = SchedError::NotInPhase1("watch");
    assert_eq!(kind(&e), "not_in_phase1");
    let e = SchedError::Io(std::io::Error::other("x"));
    assert_eq!(kind(&e), "io");
}

#[test]
fn d85_sched_error_source_carries_non_exhaustive() {
    let src = include_str!("../src/sched.rs");
    let at = src
        .find("pub enum SchedError")
        .expect("SchedError is declared in sched.rs");
    // The attribute block right above the declaration (doc comment and derives).
    let head = &src[..at];
    let attrs_start = head.rfind("\n\n").map_or(0, |i| i + 2).min(head.len());
    let attrs = &head[attrs_start..];
    assert!(
        attrs.contains("#[non_exhaustive]"),
        "SchedError must be #[non_exhaustive] (D-85); attributes found: {attrs:?}"
    );
}
