//! I-9 contract: every path `agent-memory/interfaces.yaml` lists for sb-driver resolves.
//! A renamed or moved item fails compilation here.

use sb_driver::{ActResult, Consequence, Driver, FrameInfo, Settled, StepOutcome, StepResult};

#[test]
fn i9_paths_resolve() {
    let _ = Driver::launch;
    let _ = Driver::open;
    let _ = Driver::act;
    let _ = Driver::settle;
    let _ = Driver::screenshot;
    let _ = Driver::frames;
    let _ = Driver::index;
    let _ = Driver::text;
    let _ = Driver::tabs;
    let _ = Driver::close;
    let _ = Driver::dialog;
    let _ = Driver::download;

    let step = StepResult {
        n: 1,
        step: "c#19".into(),
        outcome: StepOutcome::Ok,
        consequences: vec![Consequence::NoVisibleChange],
        settled: Some(true),
    };
    let act = ActResult {
        steps: vec![step],
        diff: None,
        stopped: None,
    };
    assert_eq!(act.steps.len(), 1);
    let s = Settled {
        settled: true,
        changed: 0,
    };
    assert!(s.settled);
    let f = FrameInfo {
        frame: 0,
        parent: None,
        url: "about:blank".into(),
        opaque: false,
    };
    assert_eq!(f.frame, 0);
}

/// B-604 (additive, pending in interface_changes): the paths it names resolve.
#[test]
fn b604_additive_paths_resolve() {
    use sb_driver::{ExecContext, LaunchEnv, Registry, Seed, SB_WORLD};
    let _ = Driver::launch_with;
    let _ = |d: &Driver| d.with_registry(|r| r.tabs().len());
    let _ = Driver::shutdown;
    let _ = sb_driver::launch_args;
    let _ = |e: &LaunchEnv| sb_driver::resolve_chromium(None, e, |_| false);
    let _ = sb_driver::platform_dir;
    let _ = sb_driver::binary_name;
    let _ = LaunchEnv::from_process;
    let mut r = Registry::new();
    r.seed_frame_tree("S1", &serde_json::json!({}));
    assert!(r.take_seeds().is_empty());
    assert!(!r.tracks("S1"));
    let _: Option<(Seed, ExecContext)> = None;
    assert_eq!(SB_WORLD, "sb");
    assert!(!sb_driver::CHROMIUM_VERSION.is_empty());
}
