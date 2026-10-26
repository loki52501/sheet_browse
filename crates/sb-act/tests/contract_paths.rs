//! I-10 contract paths: every item `agent-memory/interfaces.yaml` lists for sb-act, used at
//! exactly that path, so a rename or a moved item fails compilation (pr-ready step 4, D-74).

use std::time::Duration;

use sb_act::{
    check_planner, classify, classify_step, parse, quote, ActClass, ParseError, PlannerError,
    Scroll, Step, MAX_WAIT,
};
use sb_sheet::IndexRow;

#[test]
fn i10_paths_exist() {
    let _parse: fn(&str) -> Result<Vec<Step>, ParseError> = parse;
    let _quote: fn(&str) -> String = quote;
    let _classify: fn(&[Step], &[IndexRow]) -> ActClass = classify;
    let _classify_step: fn(&Step, &[IndexRow]) -> ActClass = classify_step;
    let _check: fn(&[Step], &[IndexRow]) -> Result<(), PlannerError> = check_planner;
    let _scroll: Option<Scroll> = None;
    let max: Duration = MAX_WAIT;
    assert_eq!(max, Duration::from_secs(30));
    let e = PlannerError::ChallengeTarget { step: 1 };
    assert!(matches!(e, PlannerError::ChallengeTarget { step: 1 }));
}
