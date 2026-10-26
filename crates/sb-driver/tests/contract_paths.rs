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
