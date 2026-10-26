//! T-19 acceptance items 2 and 3 (D-52, D-430, D-432): quoting round trip, no step
//! injection through a quoted argument, and malformed programs give a `ParseError` with a
//! 1-based step and a byte offset, never a panic.

use sb_act::{parse, quote, ParseError, Step};
use sb_sheet::ElemRef;

/// Small deterministic PRNG (xorshift64*), so the fuzz is reproducible without a dependency.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

const ADVERSARIAL: &[&str] = &[
    "x; c#99",
    "x\"; c#99; \"",
    "\"",
    "\"\"",
    "\\",
    "\\\\",
    "a\\\"b",
    "trailing backslash \\",
    "\\n is not a newline",
    "line1\nline2",
    "\n",
    "\r\n; c#1",
    "\ttab",
    "  leading and trailing  ",
    ";",
    ";;;",
    "?s",
    "; ?s; c#5",
    "c#5",
    "t#2=evil",
    "@vault(pw)",
    "@vault(pw)\"; c#99",
    "unicode: \u{e9}\u{301} \u{4e2d}\u{6587} \u{1f600} \u{202e}rtl",
    "\u{0}nul",
    "=",
    "#f2.15",
    "",
];

fn assert_round_trip(v: &str) {
    let program = format!("t#1={}", quote(v));
    let steps = parse(&program).unwrap_or_else(|e| panic!("{v:?} -> {program:?}: {e}"));
    assert_eq!(
        steps,
        vec![Step::Type {
            target: ElemRef::main(1),
            text: v.to_string()
        }],
        "value {v:?} program {program:?}"
    );
}

/// The tool page API's planner template: `t#1=${q(arg)}; c#5; ?s`.
fn assert_no_injection(v: &str) {
    let program = format!("t#1={}; c#5; ?s", quote(v));
    let steps = parse(&program).unwrap_or_else(|e| panic!("{v:?} -> {program:?}: {e}"));
    assert_eq!(
        steps,
        vec![
            Step::Type {
                target: ElemRef::main(1),
                text: v.to_string()
            },
            Step::Click {
                target: ElemRef::main(5)
            },
            Step::StopHere,
        ],
        "value {v:?} program {program:?}"
    );
}

#[test]
fn d52_adversarial_values_round_trip_as_one_type_step() {
    for v in ADVERSARIAL {
        assert_round_trip(v);
    }
}

#[test]
fn d52_planner_template_never_gains_steps_adversarial() {
    for v in ADVERSARIAL {
        assert_no_injection(v);
    }
}

#[test]
fn d52_planner_template_never_gains_steps_random() {
    // Alphabet biased toward DSL metacharacters, plus multi-byte and control chars.
    let alphabet: Vec<char> =
        "; \"\\n#=?sctkuw()@vault.f0123456789\n\r\t\u{e9}\u{4e2d}\u{1f600}\u{0}\u{202e}"
            .chars()
            .collect();
    let mut rng = Rng(0x5eed_0019_ac70_0001);
    for _ in 0..5000 {
        let len = rng.below(24);
        let v: String = (0..len)
            .map(|_| {
                if rng.below(8) == 0 {
                    char::from_u32(rng.below(0x11_0000) as u32).unwrap_or('?')
                } else {
                    alphabet[rng.below(alphabet.len())]
                }
            })
            .collect();
        assert_round_trip(&v);
        assert_no_injection(&v);
    }
}

fn err(program: &str) -> ParseError {
    match parse(program) {
        Ok(steps) => panic!("{program:?} should not parse, got {steps:?}"),
        Err(e) => e,
    }
}

fn check_err_shape(program: &str, e: &ParseError) {
    assert!(e.step >= 1, "{program:?}: step {} is not 1-based", e.step);
    assert!(
        e.at <= program.len(),
        "{program:?}: at {} past end {}",
        e.at,
        program.len()
    );
    assert!(
        program.is_char_boundary(e.at),
        "{program:?}: at {} is not a char boundary",
        e.at
    );
    assert!(!e.msg.is_empty(), "{program:?}: empty msg");
    let shown = e.to_string();
    assert!(
        shown.contains(&format!("step {}", e.step)) && shown.contains(&format!("byte {}", e.at)),
        "{shown}"
    );
}

/// (program, expected 1-based step, byte range the offset must fall in).
const MALFORMED: &[(&str, usize, std::ops::RangeInclusive<usize>)] = &[
    ("x#1", 1, 0..=3),
    ("c#", 1, 0..=2),
    ("c#abc", 1, 0..=5),
    ("c14", 1, 0..=3),
    ("t#1=a; x#2", 2, 7..=10),
    ("t#1=a;; ;q#2", 2, 9..=12), // D-430: empty steps do not count
    ("t#1=\"abc", 1, 4..=8),     // unterminated quote
    ("t#1=\"a\\q\"", 1, 4..=9),  // unknown escape (D-432)
    ("t#1=\"a\" junk", 1, 4..=12),
    ("s#1=", 1, 0..=4), // empty select (D-432)
    ("k#1=", 1, 0..=4),
    ("u#1=", 1, 0..=4),
    ("c#1; w(3)", 2, 5..=9),   // missing unit (D-431)
    ("c#1; w(3h)", 2, 5..=10), // unknown unit
    ("c#1; w(s)", 2, 5..=9),
    ("sc(sideways)", 1, 0..=12),
    ("sc(down", 1, 0..=7),
    ("?x", 1, 0..=2),
    ("C#1", 1, 0..=3), // ops are lowercase only (D-432)
    ("c#1; c#2; t#\u{e9}=x", 3, 10..=16),
    ("t#f2=x", 1, 0..=6),
];

#[test]
fn malformed_programs_report_step_and_offset() {
    for (program, step, range) in MALFORMED {
        let e = err(program);
        check_err_shape(program, &e);
        assert_eq!(e.step, *step, "{program:?}: {e}");
        assert!(
            range.contains(&e.at),
            "{program:?}: at {} not in {range:?}: {e}",
            e.at
        );
    }
}

#[test]
fn fuzz_random_bytes_never_panic() {
    let mut rng = Rng(0xdead_beef_0019_0003);
    let mut errors = 0usize;
    for _ in 0..4000 {
        let len = rng.below(48);
        let bytes: Vec<u8> = (0..len).map(|_| rng.next() as u8).collect();
        let program = String::from_utf8_lossy(&bytes).into_owned();
        let r = std::panic::catch_unwind(|| parse(&program));
        match r {
            Err(_) => panic!("parse panicked on {program:?}"),
            Ok(Err(e)) => {
                errors += 1;
                check_err_shape(&program, &e);
            }
            Ok(Ok(_)) => {}
        }
    }
    assert!(errors > 0);
}

#[test]
fn fuzz_dsl_shaped_strings_never_panic() {
    let tokens: &[&str] = &[
        "t#",
        "s#",
        "c#",
        "k#",
        "u#",
        "sc(",
        "w(",
        "?s",
        "#f",
        "f",
        ".",
        "=",
        ";",
        " ",
        "\"",
        "\\",
        "\\n",
        "\\\"",
        ")",
        "(",
        "1",
        "2",
        "15",
        "999999999999",
        "s",
        "ms",
        "down",
        "up",
        "Enter",
        "@vault(pw)",
        "\u{e9}",
        "\u{4e2d}",
        "\u{1f600}",
        "\n",
        "\t",
        "-",
        "0.5",
        "1e9",
        "#",
        "?",
        "0",
    ];
    let mut rng = Rng(0x0019_feed_cafe_0004);
    let (mut ok, mut errs) = (0usize, 0usize);
    for _ in 0..6000 {
        let n = 1 + rng.below(12);
        let program: String = (0..n).map(|_| tokens[rng.below(tokens.len())]).collect();
        let r = std::panic::catch_unwind(|| parse(&program));
        match r {
            Err(_) => panic!("parse panicked on {program:?}"),
            Ok(Err(e)) => {
                errs += 1;
                check_err_shape(&program, &e);
                // The step number never exceeds the number of `;`-separated pieces.
                assert!(
                    e.step <= program.split(';').count(),
                    "{program:?}: step {} > pieces",
                    e.step
                );
            }
            Ok(Ok(_)) => ok += 1,
        }
    }
    assert!(errs > 0 && ok > 0, "ok {ok} errs {errs}");
}
