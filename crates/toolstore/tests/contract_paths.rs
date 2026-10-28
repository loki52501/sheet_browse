//! Contract paths (pr-ready step 4, D-74): every item agent-memory/interfaces.yaml lists for
//! toolstore under I-7 and I-12, at exactly that path, so a moved or renamed item fails to
//! compile here. Signatures that callers implement or pass around (`ApiFetch::api`, the root
//! `fingerprint`) are pinned exactly. `ToolStore::tool_path` and `ToolStore::archive_path` are
//! deprecated (D-72) but still public: `deprecated_path_builders_stay_public` names them, with
//! `#[allow(deprecated)]` on that one test fn only (B-33 item 2).

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

use std::path::PathBuf;
use std::time::Duration;

/// The I-7 `ToolStore` methods exist under these names (fn items, never called).
#[test]
fn toolstore_methods_exist() {
    let _ = ToolStore::load;
    let _ = ToolStore::scan;
    // D-315, IC-305: the runtime loader that skips retired tools.health rows.
    let _ = ToolStore::scan_live;
    let _ = ToolStore::load_live;
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

/// D-72, B-33 item 2: the deprecated path builders stay public at these paths with these
/// signatures, and bad input still gives the empty path.
#[test]
#[allow(deprecated)]
fn deprecated_path_builders_stay_public() {
    let _: fn(&ToolStore, &str, &str) -> PathBuf = ToolStore::tool_path;
    let _: fn(&ToolStore, &str, &str, i32) -> PathBuf = ToolStore::archive_path;
    let dir = tempfile::tempdir().unwrap();
    let ts = ToolStore::new(dir.path());
    assert_eq!(ts.tool_path("luma.com", "../../evil"), PathBuf::new());
    assert_eq!(ts.archive_path("..", "x", 1), PathBuf::new());
}

/// I-12 `ApiFetch::api(&self, url, remaining)`: implemented outside the crate (srv-web) and
/// called through a trait object, so its exact signature is pinned both ways.
#[test]
fn api_fetch_signature() {
    struct Fixed;
    impl ApiFetch for Fixed {
        fn api(&self, url: &str, remaining: Duration) -> Result<serde_json::Value, String> {
            Ok(serde_json::json!({ "url": url, "ms": remaining.as_millis() as u64 }))
        }
    }
    let call: fn(&dyn ApiFetch, &str, Duration) -> Result<serde_json::Value, String> =
        |f, u, d| f.api(u, d);
    let v = call(&Fixed, "https://example.org/a", Duration::from_millis(5)).unwrap();
    assert_eq!(v["url"], "https://example.org/a");
    assert_eq!(v["ms"], 5);
}

/// I-7 root `fingerprint(tool, index)`, pinned with its exact signature.
#[test]
fn fingerprint_signature() {
    let _: fn(&Tool, &[sb_sheet::IndexRow]) -> FingerprintResult = toolstore::fingerprint;
}

/// T-28 (D-150, B-213): the recipe kind, at the paths harness-forge and servers T-27 use.
#[test]
fn recipe_paths_exist() {
    use toolstore::recipe::{MAX_DESCRIPTION_CHARS, MAX_RECIPE_BYTES, MAX_STEPS, TARGET_ACT_KINDS};
    use toolstore::{
        fill_placeholders, key_shaped, parse_recipe, placeholders, render_recipe, ArgType, Expects,
        Recipe, RecipeArg, RecipeReport, RecipeStep, Target, RECIPE_KIND, RECIPE_SUFFIX,
        RECIPE_TOOLS,
    };
    let _: fn(&str, &str, &str) -> Result<Recipe, StoreError> = parse_recipe;
    let _: fn(&Recipe) -> String = render_recipe;
    let _: fn(&str) -> Vec<&str> = placeholders;
    let _: fn(&str, &serde_json::Map<String, serde_json::Value>) -> String = fill_placeholders;
    let _: fn(&str) -> bool = key_shaped;
    let _: fn(&ToolStore, &str) -> RecipeReport = ToolStore::recipes;
    let _: fn(&ToolStore, &str, &str) -> Result<Option<Recipe>, StoreError> = ToolStore::recipe;
    let _: fn(&ToolStore, &str, &str) -> Result<PathBuf, StoreError> = ToolStore::try_recipe_path;
    let _ = ToolStore::recipes_live;
    let _ = (RECIPE_KIND, RECIPE_SUFFIX, RECIPE_TOOLS, TARGET_ACT_KINDS);
    let _ = (MAX_STEPS, MAX_DESCRIPTION_CHARS, MAX_RECIPE_BYTES);
    let _ = |a: RecipeArg, s: RecipeStep, t: Target, e: Expects, k: ArgType| (a, s, t, e, k);
    let _ = |e: StoreError| match e {
        StoreError::Recipe { path, msg } => (path, msg),
        _ => (PathBuf::new(), String::new()),
    };
}

/// T-28 (D-153, D-156, B-216): target nth, the recipe retire limit and the recipe file and
/// archive paths harness::judge and harness::forge use.
#[test]
fn recipe_lifecycle_paths_exist() {
    use toolstore::{Nth, Target, RECIPE_FAILS_TO_RETIRE};
    let _: i32 = RECIPE_FAILS_TO_RETIRE;
    let _ = |t: Target| match t.nth {
        Some(Nth::Index(n)) => n,
        Some(Nth::Arg(_)) | None => 1,
    };
    let _: fn(&ToolStore, &str, &str) -> Result<PathBuf, StoreError> = ToolStore::tool_file;
    let _: fn(&ToolStore, &str, &str, i32) -> Result<PathBuf, StoreError> =
        ToolStore::try_recipe_archive_path;
    let _: fn(&ToolStore, &str, &str, i32) -> Option<PathBuf> = ToolStore::archived_recipe;
}
