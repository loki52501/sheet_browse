//! B-7 parser. Values per D-52, empty steps D-430, waits D-431, lexing D-432.

use crate::{ParseError, Scroll, Step, MAX_WAIT};
use sb_sheet::ElemRef;
use std::time::Duration;

pub(crate) fn parse(program: &str) -> Result<Vec<Step>, ParseError> {
    let mut p = Parser {
        src: program,
        pos: 0,
        step: 0,
    };
    let mut steps = Vec::new();
    loop {
        p.skip_ws();
        match p.peek() {
            None => break,
            Some(';') => {
                // Empty step (`;;`, leading or trailing `;`): skipped, not numbered (D-430).
                p.bump();
                continue;
            }
            Some(_) => {}
        }
        p.step = steps.len() + 1;
        steps.push(p.one_step()?);
        p.skip_ws();
        match p.peek() {
            None => break,
            Some(';') => {
                p.bump();
            }
            Some(c) => {
                return Err(p.err(
                    p.pos,
                    format!("unexpected {c:?} after the step; separate steps with ';'"),
                ))
            }
        }
    }
    Ok(steps)
}

struct Parser<'a> {
    src: &'a str,
    pos: usize,
    /// 1-based number of the step being parsed.
    step: usize,
}

fn describe(c: Option<char>) -> String {
    match c {
        None => "the end of the program".to_string(),
        Some(c) => format!("{c:?}"),
    }
}

impl<'a> Parser<'a> {
    fn rest(&self) -> &'a str {
        &self.src[self.pos..]
    }

    fn peek(&self) -> Option<char> {
        self.rest().chars().next()
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.pos += c.len_utf8();
        Some(c)
    }

    fn skip_ws(&mut self) {
        while self.peek().is_some_and(char::is_whitespace) {
            self.bump();
        }
    }

    fn take_while(&mut self, f: impl Fn(char) -> bool) -> &'a str {
        let start = self.pos;
        while self.peek().is_some_and(&f) {
            self.bump();
        }
        &self.src[start..self.pos]
    }

    fn err(&self, at: usize, msg: impl Into<String>) -> ParseError {
        ParseError {
            step: self.step,
            at,
            msg: msg.into(),
        }
    }

    fn expect(&mut self, want: char, what: &str) -> Result<(), ParseError> {
        self.skip_ws();
        match self.peek() {
            Some(c) if c == want => {
                self.bump();
                Ok(())
            }
            other => Err(self.err(
                self.pos,
                format!("expected '{want}' {what}, found {}", describe(other)),
            )),
        }
    }

    fn one_step(&mut self) -> Result<Step, ParseError> {
        let start = self.pos;
        if self.peek() == Some('?') {
            self.bump();
            return match self.peek() {
                Some('s') => {
                    self.bump();
                    Ok(Step::StopHere)
                }
                other => Err(self.err(
                    start,
                    format!("expected '?s', found '?' then {}", describe(other)),
                )),
            };
        }
        let op = self.take_while(|c| c.is_ascii_alphabetic());
        match op {
            "t" => {
                let target = self.elem_ref()?;
                let text = self.value("t", true)?;
                Ok(Step::Type { target, text })
            }
            "s" => {
                let target = self.elem_ref()?;
                let choice = self.value("s", false)?;
                Ok(Step::Select { target, choice })
            }
            "k" => {
                let target = self.elem_ref()?;
                let key = self.value("k", false)?;
                Ok(Step::Key { target, key })
            }
            "u" => {
                let target = self.elem_ref()?;
                let path = self.value("u", false)?;
                Ok(Step::Upload { target, path })
            }
            "c" => Ok(Step::Click {
                target: self.elem_ref()?,
            }),
            "sc" => self.scroll(),
            "w" => self.wait(),
            "" => Err(self.err(
                start,
                format!(
                    "expected a step (t s c k u sc w ?s), found {}",
                    describe(self.peek())
                ),
            )),
            other => Err(self.err(
                start,
                format!("unknown step {other:?}; expected t s c k u sc w ?s"),
            )),
        }
    }

    fn elem_ref(&mut self) -> Result<ElemRef, ParseError> {
        self.skip_ws();
        let start = self.pos;
        if self.peek() != Some('#') {
            return Err(self.err(
                start,
                format!(
                    "expected an element ref like #12 or #f2.15, found {}",
                    describe(self.peek())
                ),
            ));
        }
        self.bump();
        self.take_while(|c| c.is_ascii_alphanumeric() || c == '.');
        let text = &self.src[start..self.pos];
        text.parse().map_err(|_| {
            self.err(
                start,
                format!("bad element ref {text:?}; expected #12 or #f2.15"),
            )
        })
    }

    /// `=value` for t, s, k, u: quoted (D-52 escapes) or bare to the next `;`, trimmed.
    fn value(&mut self, op: &str, allow_empty: bool) -> Result<String, ParseError> {
        self.expect('=', &format!("after the element ref of '{op}'"))?;
        self.skip_ws();
        let start = self.pos;
        let value = if self.peek() == Some('"') {
            self.quoted()?
        } else {
            let end = self
                .rest()
                .find(';')
                .map_or(self.src.len(), |i| self.pos + i);
            let v = self.src[self.pos..end].trim().to_string();
            self.pos = end;
            v
        };
        if value.is_empty() && !allow_empty {
            return Err(self.err(start, format!("'{op}' needs a value")));
        }
        Ok(value)
    }

    fn quoted(&mut self) -> Result<String, ParseError> {
        let open = self.pos;
        self.bump();
        let mut out = String::new();
        loop {
            let at = self.pos;
            match self.bump() {
                None => return Err(self.err(open, "unterminated quoted value")),
                Some('"') => break,
                Some('\\') => match self.bump() {
                    Some('"') => out.push('"'),
                    Some('\\') => out.push('\\'),
                    Some('n') => out.push('\n'),
                    Some(c) => {
                        return Err(self.err(
                            at,
                            format!("unknown escape '\\{c}'; only \\\" \\\\ and \\n are allowed"),
                        ))
                    }
                    None => return Err(self.err(open, "unterminated quoted value")),
                },
                Some(c) => out.push(c),
            }
        }
        self.skip_ws();
        match self.peek() {
            None | Some(';') => Ok(out),
            Some(c) => Err(self.err(
                self.pos,
                format!("unexpected {c:?} after the closing quote; quote the whole value"),
            )),
        }
    }

    fn scroll(&mut self) -> Result<Step, ParseError> {
        self.expect('(', "after 'sc'")?;
        self.skip_ws();
        let scroll = if self.peek() == Some('#') {
            Scroll::To(self.elem_ref()?)
        } else {
            let start = self.pos;
            let word = self.take_while(|c| c.is_ascii_alphabetic());
            if word.eq_ignore_ascii_case("down") {
                Scroll::Down
            } else if word.eq_ignore_ascii_case("up") {
                Scroll::Up
            } else {
                let found = if word.is_empty() {
                    describe(self.peek())
                } else {
                    format!("{word:?}")
                };
                return Err(self.err(
                    start,
                    format!("sc() takes down, up or an element ref like #12, found {found}"),
                ));
            }
        };
        self.expect(')', "to close 'sc('")?;
        Ok(Step::Scroll(scroll))
    }

    fn wait(&mut self) -> Result<Step, ParseError> {
        self.expect('(', "after 'w'")?;
        self.skip_ws();
        let start = self.pos;
        let num = self.take_while(|c| c.is_ascii_digit() || c == '.');
        let n: f64 = match num.parse() {
            Ok(n) if num.bytes().any(|b| b.is_ascii_digit()) => n,
            _ => {
                let found = if num.is_empty() {
                    describe(self.peek())
                } else {
                    format!("{num:?}")
                };
                return Err(self.err(
                    start,
                    format!("w() takes a number and a unit like w(3s) or w(500ms), found {found}"),
                ));
            }
        };
        self.skip_ws();
        let unit_at = self.pos;
        let unit = self.take_while(|c| c.is_ascii_alphabetic());
        let ms = match unit {
            "ms" => n,
            "s" => n * 1000.0,
            "" => {
                return Err(self.err(unit_at, "w() needs a unit, ms or s, e.g. w(3s) or w(500ms)"))
            }
            other => {
                return Err(self.err(
                    unit_at,
                    format!("unknown unit {other:?} in w(); use ms or s"),
                ))
            }
        };
        self.expect(')', "to close 'w('")?;
        // Clamp (D-431). `n` is finite or +inf (digits only), never NaN or negative.
        let ms = ms.min(MAX_WAIT.as_millis() as f64);
        Ok(Step::Wait(Duration::from_micros(
            (ms * 1000.0).round() as u64
        )))
    }
}

#[cfg(test)]
mod tests {
    use crate::{parse, quote, ParseError, Scroll, Step, MAX_WAIT};
    use sb_sheet::ElemRef;
    use std::time::Duration;

    fn m(id: u32) -> ElemRef {
        ElemRef::main(id)
    }

    fn err(program: &str) -> ParseError {
        match parse(program) {
            Ok(steps) => panic!("{program:?} parsed to {steps:?}"),
            Err(e) => e,
        }
    }

    #[test]
    fn every_step_kind() {
        let steps = parse(
            "t#7=hello world; t#8=@vault(pw); s#12=Plus; c#14; k#7=Enter; \
             u#9=files/cv.pdf; sc(down); sc(up); sc(#31); w(3s); ?s",
        )
        .unwrap();
        assert_eq!(
            steps,
            vec![
                Step::Type {
                    target: m(7),
                    text: "hello world".into()
                },
                Step::Type {
                    target: m(8),
                    text: "@vault(pw)".into()
                },
                Step::Select {
                    target: m(12),
                    choice: "Plus".into()
                },
                Step::Click { target: m(14) },
                Step::Key {
                    target: m(7),
                    key: "Enter".into()
                },
                Step::Upload {
                    target: m(9),
                    path: "files/cv.pdf".into()
                },
                Step::Scroll(Scroll::Down),
                Step::Scroll(Scroll::Up),
                Step::Scroll(Scroll::To(m(31))),
                Step::Wait(Duration::from_secs(3)),
                Step::StopHere,
            ]
        );
    }

    #[test]
    fn frame_refs_work_everywhere() {
        let f = ElemRef { frame: 2, id: 15 };
        let steps =
            parse("t#f2.15=x; s#f2.15=y; c#f2.15; k#f2.15=Tab; u#f2.15=a.pdf; sc(#f2.15)").unwrap();
        let targets: Vec<ElemRef> = steps
            .iter()
            .map(|s| match s {
                Step::Type { target, .. }
                | Step::Select { target, .. }
                | Step::Click { target }
                | Step::Key { target, .. }
                | Step::Upload { target, .. }
                | Step::Scroll(Scroll::To(target)) => *target,
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(targets, vec![f; 6]);
    }

    #[test]
    fn whitespace_is_tolerated() {
        let steps =
            parse("  t #7 =  hello world  ;\n c #14 ;\tsc( DOWN ) ; w( 1.5 s ) ; ?s  ").unwrap();
        assert_eq!(
            steps,
            vec![
                Step::Type {
                    target: m(7),
                    text: "hello world".into()
                },
                Step::Click { target: m(14) },
                Step::Scroll(Scroll::Down),
                Step::Wait(Duration::from_millis(1500)),
                Step::StopHere,
            ]
        );
    }

    #[test]
    fn empty_steps_are_skipped_and_not_numbered() {
        assert_eq!(parse("").unwrap(), vec![]);
        assert_eq!(parse("  \n ").unwrap(), vec![]);
        assert_eq!(parse(";;; ;").unwrap(), vec![]);
        assert_eq!(
            parse("; c#1;; ;c#2;").unwrap(),
            vec![Step::Click { target: m(1) }, Step::Click { target: m(2) }]
        );
        // The error is in the second real step even with empty steps before it.
        assert_eq!(err(";c#1;;x#2").step, 2);
    }

    #[test]
    fn bare_values() {
        // Runs to the next ';' and is trimmed; quotes inside a bare value are literal.
        assert_eq!(
            parse("t#7=  5\" screen , ok  ;c#1").unwrap()[0],
            Step::Type {
                target: m(7),
                text: "5\" screen , ok".into()
            }
        );
        // Without quoting, a ';' in a value starts a new step (why planners use q(), D-52).
        assert_eq!(parse("t#1=x; c#99").unwrap().len(), 2);
        // A bare value may span lines up to the next ';'.
        assert_eq!(
            parse("t#1=a\nb").unwrap()[0],
            Step::Type {
                target: m(1),
                text: "a\nb".into()
            }
        );
        // An empty t clears the field (D-432).
        assert_eq!(
            parse("t#7=;t#8=\"\"").unwrap(),
            vec![
                Step::Type {
                    target: m(7),
                    text: String::new()
                },
                Step::Type {
                    target: m(8),
                    text: String::new()
                },
            ]
        );
    }

    #[test]
    fn quoted_values() {
        assert_eq!(
            parse(r#"t#7="a; b" ; s#8="say \"hi\"\n\\""#).unwrap(),
            vec![
                Step::Type {
                    target: m(7),
                    text: "a; b".into()
                },
                Step::Select {
                    target: m(8),
                    choice: "say \"hi\"\n\\".into()
                },
            ]
        );
        // Leading and trailing spaces inside quotes are kept.
        assert_eq!(
            parse(r#"t#7=  "  x  "  "#).unwrap()[0],
            Step::Type {
                target: m(7),
                text: "  x  ".into()
            }
        );
    }

    #[test]
    fn waits() {
        let w = |p: &str| match parse(p).unwrap()[0] {
            Step::Wait(d) => d,
            ref other => panic!("{other:?}"),
        };
        assert_eq!(w("w(3s)"), Duration::from_secs(3));
        assert_eq!(w("w(500ms)"), Duration::from_millis(500));
        assert_eq!(w("w(0.25s)"), Duration::from_millis(250));
        assert_eq!(w("w(.5s)"), Duration::from_millis(500));
        assert_eq!(w("w(2.5ms)"), Duration::from_micros(2500));
        assert_eq!(w("w(0s)"), Duration::ZERO);
        assert_eq!(w("w(30s)"), MAX_WAIT);
        assert_eq!(w("w(31s)"), MAX_WAIT);
        assert_eq!(w("w(99999999999999999999999999999999s)"), MAX_WAIT);
        assert_eq!(w("w(600000ms)"), MAX_WAIT);
    }

    #[test]
    fn errors_carry_step_offset_and_reason() {
        let cases: &[(&str, usize, usize, &str)] = &[
            ("x#1", 1, 0, "unknown step"),
            ("T#1=a", 1, 0, "unknown step"),
            ("tx#1=a", 1, 0, "unknown step"),
            ("c#1; #2", 2, 5, "expected a step"),
            ("c#1; =a", 2, 5, "expected a step"),
            ("?", 1, 0, "expected '?s'"),
            ("c#1;?x", 2, 4, "expected '?s'"),
            ("?sx", 1, 2, "after the step"),
            ("c14", 1, 1, "expected an element ref"),
            ("c", 1, 1, "expected an element ref"),
            ("c#", 1, 1, "bad element ref"),
            ("c#abc", 1, 1, "bad element ref"),
            ("c#f2", 1, 1, "bad element ref"),
            ("c#1.5", 1, 1, "bad element ref"),
            ("c#-1", 1, 1, "bad element ref"),
            ("c#14 x", 1, 5, "after the step"),
            ("c#14=x", 1, 4, "after the step"),
            ("t#7 hello", 1, 4, "expected '='"),
            ("t#7", 1, 3, "expected '='"),
            ("s#12=", 1, 5, "needs a value"),
            ("s#12=  ;c#1", 1, 7, "needs a value"),
            ("k#7=", 1, 4, "needs a value"),
            ("u#9=\"\"", 1, 4, "needs a value"),
            ("t#7=\"abc", 1, 4, "unterminated"),
            ("t#7=\"abc\\", 1, 4, "unterminated"),
            ("t#7=\"a\\tb\"", 1, 6, "unknown escape"),
            ("t#7=\"a\"b", 1, 7, "after the closing quote"),
            ("t#7=\"a\" \"b\"", 1, 8, "after the closing quote"),
            ("sc", 1, 2, "expected '('"),
            ("sc(left)", 1, 3, "sc() takes"),
            ("sc()", 1, 3, "sc() takes"),
            ("sc(down", 1, 7, "expected ')'"),
            ("sc(#3 x)", 1, 6, "expected ')'"),
            ("w3s", 1, 1, "expected '('"),
            ("w()", 1, 2, "w() takes a number"),
            ("w(.s)", 1, 2, "w() takes a number"),
            ("w(1.2.3s)", 1, 2, "w() takes a number"),
            ("w(3)", 1, 3, "needs a unit"),
            ("w(3m)", 1, 3, "unknown unit"),
            ("w(3min)", 1, 3, "unknown unit"),
            ("w(3s", 1, 4, "expected ')'"),
            ("w(3s))", 1, 5, "after the step"),
        ];
        for &(program, step, at, msg) in cases {
            let e = err(program);
            assert_eq!((e.step, e.at), (step, at), "{program:?}: {e}");
            assert!(e.msg.contains(msg), "{program:?}: {e}");
        }
        assert_eq!(
            err("c#1; c#x").to_string(),
            "act parse error in step 2 at byte 6: bad element ref \"#x\"; expected #12 or #f2.15"
        );
    }

    #[test]
    fn offsets_are_bytes_not_chars() {
        // "é" is two bytes; the bad escape sits at byte 7.
        let e = err("t#7=\"é\\q\"");
        assert_eq!(e.at, 7);
    }

    fn round_trip(s: &str) {
        let program = format!("t#1={}", quote(s));
        assert_eq!(
            parse(&program),
            Ok(vec![Step::Type {
                target: ElemRef::main(1),
                text: s.to_string()
            }]),
            "{program:?}"
        );
        // Still exactly one step inside a larger program, with any spacing.
        let program = format!(" ;t #1 = {} ; c#2;", quote(s));
        let steps = parse(&program).unwrap();
        assert_eq!(steps.len(), 2, "{program:?}");
        assert_eq!(
            steps[0],
            Step::Type {
                target: ElemRef::main(1),
                text: s.to_string()
            }
        );
    }

    #[test]
    fn quote_round_trips_tricky_strings() {
        for s in [
            "",
            " ",
            ";",
            ";;",
            "x; c#99",
            "x\"; c#99; \"",
            "\"",
            "\\",
            "\\\"",
            "\\n",
            "\n",
            "\r\n\t",
            "a\\",
            "@vault(pw)",
            "?s",
            "  padded  ",
            "é 🦀 ñ",
            "\u{0}",
            "=#f2.15=",
            "w(3s); sc(#1)",
        ] {
            round_trip(s);
        }
    }

    #[test]
    fn quote_round_trip_property() {
        // Hand-rolled property loop: a fixed-seed LCG over an alphabet of separators,
        // quotes, escapes, whitespace and multi-byte chars.
        const ALPHABET: &[char] = &[
            ';', '"', '\\', '\n', '\r', '\t', ' ', 'a', 'n', 's', 'c', '#', '=', '(', ')', '?',
            '9', '@', '.', 'é', '🦀', '\u{0}', '\u{a0}',
        ];
        let mut state: u64 = 0x5eed_0b7e_c0de_1234;
        let mut next = move || {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (state >> 33) as usize
        };
        for _ in 0..3000 {
            let len = next() % 24;
            let s: String = (0..len)
                .map(|_| ALPHABET[next() % ALPHABET.len()])
                .collect();
            round_trip(&s);
        }
    }
}
