//! T-19 acceptance item 1 (docs/05 B-7, D-431, D-432): every B-7 form parses to the right
//! `Step`, through the public API only.

use sb_act::{parse, Scroll, Step, MAX_WAIT};
use sb_sheet::ElemRef;
use std::time::Duration;

fn one(program: &str) -> Step {
    let steps = parse(program).unwrap_or_else(|e| panic!("{program:?}: {e}"));
    assert_eq!(steps.len(), 1, "{program:?} -> {steps:?}");
    steps.into_iter().next().unwrap()
}

fn main_ref(id: u32) -> ElemRef {
    ElemRef::main(id)
}

#[test]
fn b7_type_bare_value_keeps_inner_space() {
    assert_eq!(
        one("t#7=hello world"),
        Step::Type {
            target: main_ref(7),
            text: "hello world".into()
        }
    );
}

#[test]
fn b7_type_vault_literal_is_kept_unresolved() {
    assert_eq!(
        one("t#7=@vault(pw)"),
        Step::Type {
            target: main_ref(7),
            text: "@vault(pw)".into()
        }
    );
    // Quoted form keeps the literal too.
    assert_eq!(
        one("t#7=\"@vault(pw)\""),
        Step::Type {
            target: main_ref(7),
            text: "@vault(pw)".into()
        }
    );
}

#[test]
fn b7_select() {
    assert_eq!(
        one("s#12=Plus"),
        Step::Select {
            target: main_ref(12),
            choice: "Plus".into()
        }
    );
}

#[test]
fn b7_click() {
    assert_eq!(
        one("c#14"),
        Step::Click {
            target: main_ref(14)
        }
    );
}

#[test]
fn b7_key() {
    assert_eq!(
        one("k#7=Enter"),
        Step::Key {
            target: main_ref(7),
            key: "Enter".into()
        }
    );
}

#[test]
fn b7_upload() {
    assert_eq!(
        one("u#9=files/cv.pdf"),
        Step::Upload {
            target: main_ref(9),
            path: "files/cv.pdf".into()
        }
    );
}

#[test]
fn b7_scroll_down_up_and_to_ref() {
    assert_eq!(one("sc(down)"), Step::Scroll(Scroll::Down));
    assert_eq!(one("sc(up)"), Step::Scroll(Scroll::Up));
    assert_eq!(one("sc(#31)"), Step::Scroll(Scroll::To(main_ref(31))));
}

#[test]
fn b7_wait() {
    assert_eq!(one("w(3s)"), Step::Wait(Duration::from_secs(3)));
    assert_eq!(one("w(250ms)"), Step::Wait(Duration::from_millis(250)));
    // D-431: clamped, not rejected.
    assert_eq!(one("w(600s)"), Step::Wait(MAX_WAIT));
}

#[test]
fn b7_stop_here() {
    assert_eq!(one("?s"), Step::StopHere);
}

#[test]
fn b7_frame_refs() {
    let f = ElemRef { frame: 2, id: 15 };
    assert_eq!(
        one("t#f2.15=x"),
        Step::Type {
            target: f,
            text: "x".into()
        }
    );
    assert_eq!(one("c#f2.15"), Step::Click { target: f });
    assert_eq!(
        one("s#f2.15=Pro"),
        Step::Select {
            target: f,
            choice: "Pro".into()
        }
    );
    assert_eq!(
        one("k#f2.15=Enter"),
        Step::Key {
            target: f,
            key: "Enter".into()
        }
    );
    assert_eq!(
        one("u#f2.15=files/cv.pdf"),
        Step::Upload {
            target: f,
            path: "files/cv.pdf".into()
        }
    );
    assert_eq!(one("sc(#f2.15)"), Step::Scroll(Scroll::To(f)));
}

#[test]
fn b7_full_program_in_order() {
    let steps =
        parse("t#7=hello world; t#8=@vault(pw); s#12=Plus; c#14; k#7=Enter; u#9=files/cv.pdf; sc(down); sc(up); sc(#3); w(3s); ?s")
            .unwrap();
    assert_eq!(
        steps,
        vec![
            Step::Type {
                target: main_ref(7),
                text: "hello world".into()
            },
            Step::Type {
                target: main_ref(8),
                text: "@vault(pw)".into()
            },
            Step::Select {
                target: main_ref(12),
                choice: "Plus".into()
            },
            Step::Click {
                target: main_ref(14)
            },
            Step::Key {
                target: main_ref(7),
                key: "Enter".into()
            },
            Step::Upload {
                target: main_ref(9),
                path: "files/cv.pdf".into()
            },
            Step::Scroll(Scroll::Down),
            Step::Scroll(Scroll::Up),
            Step::Scroll(Scroll::To(main_ref(3))),
            Step::Wait(Duration::from_secs(3)),
            Step::StopHere,
        ]
    );
}
