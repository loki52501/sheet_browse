//! Contract paths (pr-ready step 4, D-74): every item agent-memory/interfaces.yaml lists for
//! toolstore under I-7 and I-12, at exactly that path, so a moved or renamed item fails to
//! compile here. `ToolStore::tool_path` and `ToolStore::archive_path` are deprecated (D-72)
//! and are pinned by `tests_review::deprecated_path_builders_return_the_empty_path_on_bad_input`
//! instead, so this file stays free of `#[allow(deprecated)]`.

#![allow(unused_imports)]

// I-7, root.
use toolstore::{
    check_repair, check_tool_id, fingerprint, planner_act_class, planner_class, relocate,
    skeleton_line, valid_domain, valid_name, FingerprintResult, LoadReport, PlannerCheck,
    RepairKind, RunOutcome, StoreError, SweepAction, Tool, ToolRunResult, ToolStore,
};
// I-7, modules.
use toolstore::archive::has_task_id;
use toolstore::domain::{normalize_host, registrable, SiteConfig};
use toolstore::fingerprint::{fingerprint_items, norm, role_matches};
use toolstore::header::{parse_fingerprint, patch_fingerprint, render_fingerprint};
use toolstore::invariants::{stop_count, vault_keys};
use toolstore::lifecycle::{
    ARCHIVE_DELETE_DAYS, CLEAN_RUNS_TO_SAVE, EPHEMERAL_IDLE_DAYS, EPHEMERAL_IDLE_VISITS,
    MISSES_TO_RETIRE,
};
use toolstore::loader::{glob_match, namespace, url_path};
use toolstore::runner::{table_json, ALLOWED_GLOBALS, FIND_ALL_K};
// Root re-exports of the header helpers.
use toolstore::{parse_fingerprint as _, patch_fingerprint as _, render_fingerprint as _};
// I-12.
use toolstore::runner::{
    run_planner, run_planner_with, run_reader, run_reader_with, ApiFetch, PageHost, RunError,
    RunOpts, SheetPage,
};
use toolstore::{parse_header, ArgSpec, FpItem, ToolHeader, ToolKind};

/// The I-7 `ToolStore` methods exist under these names (fn items, never called).
#[test]
fn toolstore_methods_exist() {
    let _ = ToolStore::load;
    let _ = ToolStore::scan;
    let _ = ToolStore::try_tool_path;
    let _ = ToolStore::try_archive_path;
    let _ = ToolStore::record;
    let _ = ToolStore::folder_for;
    let _ = ToolStore::archive;
    let _ = ToolStore::git_commit;
    let _ = ToolStore::health;
    let _ = ToolStore::register_forged;
    let _ = ToolStore::reforge;
    let _ = ToolStore::confirm;
    let _ = ToolStore::set_pinned;
    let _ = ToolStore::log_invariant;
    let _ = ToolStore::sweep;
}
