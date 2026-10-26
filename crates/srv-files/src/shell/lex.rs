//! A bash-subset lexer for classification (F-2). It never executes anything.
//!
//! It splits a command line into simple commands (at `;` `&&` `||` `|` `|&` `&`,
//! newlines and parentheses), unquotes words (`'..'`, `".."`, backslash escapes), and
//! records for each word whether its value is decided only at run time (`$x`, `${x}`,
//! `$(..)`, backticks, `$((..))`, `$'..'`), whether it is a glob or brace expansion, and
//! whether it starts with `~`. Command substitutions (`$(..)`, backticks, `<(..)`, `>(..)`)
//! are returned as their own sources so the classifier checks them too. Redirections come
//! out with their targets; here-document bodies are skipped (an unquoted body that
//! substitutes a command is refused).
//!
//! Anything it does not understand is an error, and an error classifies as Delete
//! (fail closed, D-520): `case`'s `;;`, an unterminated quote or substitution, a
//! here-document without its end line.

/// One unquoted word.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Word {
    /// The literal text with quotes removed (dynamic parts are left out).
    pub text: String,
    /// Some part is decided only at run time.
    pub dynamic: bool,
    /// Contains an unquoted `*`, `?` or `[`.
    pub glob: bool,
    /// Contains an unquoted brace expansion (`{a,b}`, `{1..3}`).
    pub brace: bool,
    /// Starts with an unquoted `~`.
    pub tilde: bool,
    /// Some part was quoted.
    pub quoted: bool,
}

impl Word {
    /// True when the text is exactly what the shell will pass.
    pub fn is_static(&self) -> bool {
        !self.dynamic && !self.glob && !self.brace && !self.tilde
    }
}

/// A redirection: the operator and, for file redirections, the target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Redir {
    pub op: String,
    pub target: Option<Word>,
}

/// A simple command: its words and redirections.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Simple {
    pub words: Vec<Word>,
    pub redirs: Vec<Redir>,
    /// The operator after this command: `;` `\n` `&&` `||` `|` `&` `(` `)`, or empty at
    /// the end of the line.
    pub next_sep: &'static str,
}

/// The lexed source.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Parsed {
    pub commands: Vec<Simple>,
    /// Bodies of command and process substitutions.
    pub subs: Vec<String>,
    /// The line has a subshell, a pipe, `||` or a background `&`: commands may run in
    /// another folder than the line order suggests.
    pub structured: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Tok {
    Word(Word),
    Sep(&'static str),
    Redir(String),
    /// A here-document operator whose delimiter was consumed.
    HereDoc(String),
}

struct HereDoc {
    delim: String,
    strip_tabs: bool,
    quoted: bool,
}

struct Lexer {
    c: Vec<char>,
    i: usize,
    toks: Vec<Tok>,
    subs: Vec<String>,
    cur: Option<Word>,
    heredocs: Vec<HereDoc>,
}

/// Parses `src` (see the module docs).
pub fn parse(src: &str) -> Result<Parsed, String> {
    let mut lx = Lexer {
        c: src.chars().collect(),
        i: 0,
        toks: Vec::new(),
        subs: Vec::new(),
        cur: None,
        heredocs: Vec::new(),
    };
    lx.run()?;
    let structured = lx.toks.iter().any(|t| {
        matches!(
            t,
            Tok::Sep("(") | Tok::Sep(")") | Tok::Sep("|") | Tok::Sep("||") | Tok::Sep("&")
        )
    });
    let commands = split(lx.toks)?;
    Ok(Parsed {
        commands,
        subs: lx.subs,
        structured,
    })
}

impl Lexer {
    fn peek(&self, k: usize) -> Option<char> {
        self.c.get(self.i + k).copied()
    }

    fn word(&mut self) -> &mut Word {
        self.cur.get_or_insert_with(Word::default)
    }

    fn finish(&mut self) {
        if let Some(w) = self.cur.take() {
            self.toks.push(Tok::Word(w));
        }
    }

    fn sep(&mut self, op: &'static str) {
        self.finish();
        self.toks.push(Tok::Sep(op));
    }

    fn run(&mut self) -> Result<(), String> {
        while let Some(ch) = self.peek(0) {
            match ch {
                ' ' | '\t' | '\r' => {
                    self.finish();
                    self.i += 1;
                }
                '\n' => {
                    self.sep("\n");
                    self.i += 1;
                    self.heredoc_bodies()?;
                }
                '#' if self.cur.is_none() => {
                    while let Some(c) = self.peek(0) {
                        if c == '\n' {
                            break;
                        }
                        self.i += 1;
                    }
                }
                '\\' => match self.peek(1) {
                    Some('\n') => self.i += 2,
                    Some(c) => {
                        let w = self.word();
                        w.text.push(c);
                        w.quoted = true;
                        self.i += 2;
                    }
                    None => self.i += 1,
                },
                '\'' => {
                    let end = self.find_char(self.i + 1, '\'').ok_or("unterminated '")?;
                    let lit: String = self.c[self.i + 1..end].iter().collect();
                    let w = self.word();
                    w.text.push_str(&lit);
                    w.quoted = true;
                    self.i = end + 1;
                }
                '"' => self.double_quoted()?,
                '$' => self.dollar()?,
                '`' => self.backtick()?,
                '<' | '>' => self.redirect()?,
                '&' => {
                    if self.peek(1) == Some('&') {
                        self.sep("&&");
                        self.i += 2;
                    } else if self.peek(1) == Some('>') {
                        self.finish();
                        let op = if self.peek(2) == Some('>') {
                            "&>>"
                        } else {
                            "&>"
                        };
                        self.i += op.len();
                        self.toks.push(Tok::Redir(op.into()));
                    } else {
                        self.sep("&");
                        self.i += 1;
                    }
                }
                '|' => {
                    if self.peek(1) == Some('|') {
                        self.sep("||");
                        self.i += 2;
                    } else {
                        self.sep("|");
                        self.i += if self.peek(1) == Some('&') { 2 } else { 1 };
                    }
                }
                ';' => {
                    if self.peek(1) == Some(';') || self.peek(1) == Some('&') {
                        return Err("case syntax is not parsed".into());
                    }
                    self.sep(";");
                    self.i += 1;
                }
                '(' => {
                    self.sep("(");
                    self.i += 1;
                }
                ')' => {
                    self.sep(")");
                    self.i += 1;
                }
                '*' | '?' | '[' => {
                    let w = self.word();
                    w.glob = true;
                    w.text.push(ch);
                    self.i += 1;
                }
                '{' => {
                    if self.brace_expansion() {
                        self.word().brace = true;
                    }
                    self.word().text.push('{');
                    self.i += 1;
                }
                '~' if self.cur.is_none() => {
                    let w = self.word();
                    w.tilde = true;
                    w.text.push('~');
                    self.i += 1;
                }
                c => {
                    self.word().text.push(c);
                    self.i += 1;
                }
            }
        }
        self.finish();
        if !self.heredocs.is_empty() {
            return Err("here-document without its end line".into());
        }
        Ok(())
    }

    fn find_char(&self, from: usize, ch: char) -> Option<usize> {
        (from..self.c.len()).find(|&k| self.c[k] == ch)
    }

    /// `{` at self.i starts a brace expansion when a `,` or `..` comes before the matching
    /// `}` on the same word.
    fn brace_expansion(&self) -> bool {
        let mut depth = 0;
        let mut k = self.i;
        let mut saw = false;
        while let Some(&c) = self.c.get(k) {
            match c {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        return saw;
                    }
                }
                ',' => saw = true,
                '.' if self.c.get(k + 1) == Some(&'.') => saw = true,
                ' ' | '\t' | '\n' | ';' | '|' | '&' | '<' | '>' => return false,
                _ => {}
            }
            k += 1;
        }
        false
    }

    fn double_quoted(&mut self) -> Result<(), String> {
        self.word().quoted = true;
        self.i += 1;
        loop {
            let Some(ch) = self.peek(0) else {
                return Err("unterminated \"".into());
            };
            match ch {
                '"' => {
                    self.i += 1;
                    return Ok(());
                }
                '\\' => match self.peek(1) {
                    Some(c @ ('$' | '`' | '"' | '\\')) => {
                        self.word().text.push(c);
                        self.i += 2;
                    }
                    Some('\n') => self.i += 2,
                    _ => {
                        self.word().text.push('\\');
                        self.i += 1;
                    }
                },
                '$' => self.dollar()?,
                '`' => self.backtick()?,
                c => {
                    self.word().text.push(c);
                    self.i += 1;
                }
            }
        }
    }

    /// Index just after the `)` matching the `(` at `open`, and the body between them.
    fn balanced(&self, open: usize) -> Result<(usize, String), String> {
        let mut depth = 0usize;
        let mut k = open;
        while let Some(&c) = self.c.get(k) {
            match c {
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        return Ok((k + 1, self.c[open + 1..k].iter().collect()));
                    }
                }
                '\'' => {
                    k = self.find_char(k + 1, '\'').ok_or("unterminated '")?;
                }
                '"' => {
                    let mut j = k + 1;
                    loop {
                        match self.c.get(j) {
                            None => return Err("unterminated \"".into()),
                            Some('\\') => j += 2,
                            Some('"') => break,
                            Some(_) => j += 1,
                        }
                    }
                    k = j;
                }
                '\\' => k += 1,
                _ => {}
            }
            k += 1;
        }
        Err("unterminated (".into())
    }

    fn dollar(&mut self) -> Result<(), String> {
        match self.peek(1) {
            Some('(') => {
                if self.peek(2) == Some('(') {
                    // $(( arithmetic )): no command runs, but the value is dynamic.
                    let (end, _) = self.balanced(self.i + 1)?;
                    self.i = end;
                } else {
                    let (end, body) = self.balanced(self.i + 1)?;
                    self.subs.push(body);
                    self.i = end;
                }
                self.word().dynamic = true;
            }
            Some('{') => {
                let end = self.find_char(self.i + 2, '}').ok_or("unterminated ${")?;
                let body: String = self.c[self.i + 2..end].iter().collect();
                // ${x:-$(cmd)} and friends hide a substitution inside.
                if body.contains("$(") || body.contains('`') {
                    return Err("command substitution inside ${..} is not parsed".into());
                }
                self.word().dynamic = true;
                self.i = end + 1;
            }
            Some('\'') => {
                let mut j = self.i + 2;
                loop {
                    match self.c.get(j) {
                        None => return Err("unterminated $'".into()),
                        Some('\\') => j += 2,
                        Some('\'') => break,
                        Some(_) => j += 1,
                    }
                }
                self.word().dynamic = true;
                self.i = j + 1;
            }
            Some('"') => self.i += 1,
            Some(c) if c.is_ascii_alphanumeric() || c == '_' => {
                self.i += 1;
                while matches!(self.peek(0), Some(c) if c.is_ascii_alphanumeric() || c == '_') {
                    self.i += 1;
                }
                self.word().dynamic = true;
            }
            Some('@' | '*' | '#' | '?' | '$' | '!' | '-') => {
                self.i += 2;
                self.word().dynamic = true;
            }
            _ => {
                self.word().text.push('$');
                self.i += 1;
            }
        }
        Ok(())
    }

    fn backtick(&mut self) -> Result<(), String> {
        let mut j = self.i + 1;
        let mut body = String::new();
        loop {
            match self.c.get(j) {
                None => return Err("unterminated `".into()),
                Some('\\') => {
                    if let Some(&n) = self.c.get(j + 1) {
                        body.push(n);
                    }
                    j += 2;
                }
                Some('`') => break,
                Some(&c) => {
                    body.push(c);
                    j += 1;
                }
            }
        }
        self.subs.push(body);
        self.word().dynamic = true;
        self.i = j + 1;
        Ok(())
    }

    fn redirect(&mut self) -> Result<(), String> {
        let ch = self.peek(0).unwrap_or('>');
        // Process substitution <(..) / >(..): a command whose output is a path.
        if self.peek(1) == Some('(') {
            let (end, body) = self.balanced(self.i + 1)?;
            self.subs.push(body);
            self.word().dynamic = true;
            self.i = end;
            return Ok(());
        }
        // A word of digits right before the operator is a file descriptor, not an argument.
        if let Some(w) = &self.cur {
            if !w.quoted
                && w.is_static()
                && !w.text.is_empty()
                && w.text.chars().all(|c| c.is_ascii_digit())
            {
                self.cur = None;
            }
        }
        self.finish();
        let rest: String = self.c[self.i..self.c.len().min(self.i + 3)]
            .iter()
            .collect();
        let ops: &[&str] = if ch == '>' {
            &[">>", ">|", ">&", ">"]
        } else {
            &["<<<", "<<-", "<<", "<&", "<>", "<"]
        };
        let op = ops
            .iter()
            .find(|o| rest.starts_with(**o))
            .copied()
            .unwrap_or(">");
        self.i += op.len();
        if op == "<<" || op == "<<-" {
            while matches!(self.peek(0), Some(' ' | '\t')) {
                self.i += 1;
            }
            let start_toks = self.toks.len();
            // Read the delimiter as a word.
            let save_cur = self.cur.take();
            let mut delim = Word::default();
            while let Some(c) = self.peek(0) {
                match c {
                    ' ' | '\t' | '\n' | ';' | '&' | '|' | '<' | '>' | '(' | ')' => break,
                    '\'' => {
                        let end = self.find_char(self.i + 1, '\'').ok_or("unterminated '")?;
                        delim.text.extend(&self.c[self.i + 1..end]);
                        delim.quoted = true;
                        self.i = end + 1;
                    }
                    '"' => {
                        let end = self.find_char(self.i + 1, '"').ok_or("unterminated \"")?;
                        delim.text.extend(&self.c[self.i + 1..end]);
                        delim.quoted = true;
                        self.i = end + 1;
                    }
                    '\\' => {
                        if let Some(n) = self.peek(1) {
                            delim.text.push(n);
                        }
                        delim.quoted = true;
                        self.i += 2;
                    }
                    c => {
                        delim.text.push(c);
                        self.i += 1;
                    }
                }
            }
            self.cur = save_cur;
            debug_assert_eq!(start_toks, self.toks.len());
            if delim.text.is_empty() {
                return Err("here-document without a delimiter".into());
            }
            self.heredocs.push(HereDoc {
                delim: delim.text,
                strip_tabs: op == "<<-",
                quoted: delim.quoted,
            });
            self.toks.push(Tok::HereDoc(op.into()));
        } else {
            self.toks.push(Tok::Redir(op.into()));
        }
        Ok(())
    }

    /// After a newline: skips the bodies of pending here-documents.
    fn heredoc_bodies(&mut self) -> Result<(), String> {
        let docs = std::mem::take(&mut self.heredocs);
        for d in docs {
            loop {
                if self.i >= self.c.len() {
                    return Err("here-document without its end line".into());
                }
                let end = self.find_char(self.i, '\n').unwrap_or(self.c.len());
                let line: String = self.c[self.i..end].iter().collect();
                let line = line.strip_suffix('\r').unwrap_or(&line).to_string();
                self.i = (end + 1).min(self.c.len());
                let check = if d.strip_tabs {
                    line.trim_start_matches('\t')
                } else {
                    line.as_str()
                };
                if check == d.delim {
                    break;
                }
                if !d.quoted && (line.contains("$(") || line.contains('`')) {
                    return Err("here-document substitutes a command".into());
                }
                if end >= self.c.len() {
                    return Err("here-document without its end line".into());
                }
            }
        }
        Ok(())
    }
}

/// Groups tokens into simple commands.
fn split(toks: Vec<Tok>) -> Result<Vec<Simple>, String> {
    let mut out = Vec::new();
    let mut cur = Simple::default();
    let mut it = toks.into_iter().peekable();
    while let Some(t) = it.next() {
        match t {
            Tok::Word(w) => cur.words.push(w),
            Tok::Sep(op) => {
                if !cur.words.is_empty() || !cur.redirs.is_empty() {
                    cur.next_sep = op;
                    out.push(std::mem::take(&mut cur));
                }
            }
            Tok::HereDoc(op) => cur.redirs.push(Redir { op, target: None }),
            Tok::Redir(op) => {
                let target = match it.next() {
                    Some(Tok::Word(w)) => w,
                    _ => return Err(format!("{op} without a target")),
                };
                let fd_dup = (op == ">&" || op == "<&")
                    && target.is_static()
                    && (target.text == "-" || target.text.chars().all(|c| c.is_ascii_digit()));
                cur.redirs.push(Redir {
                    op,
                    target: if fd_dup { None } else { Some(target) },
                });
            }
        }
    }
    if !cur.words.is_empty() || !cur.redirs.is_empty() {
        out.push(cur);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(src: &str) -> Vec<Vec<String>> {
        parse(src)
            .unwrap()
            .commands
            .iter()
            .map(|c| c.words.iter().map(|w| w.text.clone()).collect())
            .collect()
    }

    #[test]
    fn splits_compound_commands() {
        assert_eq!(
            words("ls -la; rm a && echo 'x y' || cat \"b c\" | wc -l & true"),
            vec![
                vec!["ls", "-la"],
                vec!["rm", "a"],
                vec!["echo", "x y"],
                vec!["cat", "b c"],
                vec!["wc", "-l"],
                vec!["true"],
            ]
        );
        assert_eq!(
            words("(cd x; rm y)"),
            vec![vec!["cd", "x"], vec!["rm", "y"]]
        );
        assert_eq!(words("r\\m a\\ b"), vec![vec!["rm", "a b"]]);
        let p = parse("cd x && rm y; ls").unwrap();
        let seps: Vec<&str> = p.commands.iter().map(|c| c.next_sep).collect();
        assert_eq!(seps, vec!["&&", ";", ""]);
        assert!(!p.structured);
        assert!(parse("(cd x); rm y").unwrap().structured);
        assert!(parse("cd x | rm y").unwrap().structured);
        assert!(parse("cd x || rm y").unwrap().structured);
        assert!(parse("cd x & rm y").unwrap().structured);
    }

    #[test]
    fn substitutions_are_returned() {
        let p = parse("echo $(rm -rf /) `del x` <(curl -X POST u)").unwrap();
        assert_eq!(p.subs, vec!["rm -rf /", "del x", "curl -X POST u"]);
        assert!(p.commands[0].words[1].dynamic);
        let p = parse("echo \"a $(rm b) c\"").unwrap();
        assert_eq!(p.subs, vec!["rm b"]);
    }

    #[test]
    fn redirections_and_fds() {
        let p = parse("echo hi > out.txt 2>&1 2> err.log >> app.log < in &> all").unwrap();
        let r: Vec<(String, Option<String>)> = p.commands[0]
            .redirs
            .iter()
            .map(|r| (r.op.clone(), r.target.as_ref().map(|t| t.text.clone())))
            .collect();
        assert_eq!(
            r,
            vec![
                (">".into(), Some("out.txt".into())),
                (">&".into(), None),
                (">".into(), Some("err.log".into())),
                (">>".into(), Some("app.log".into())),
                ("<".into(), Some("in".into())),
                ("&>".into(), Some("all".into())),
            ]
        );
        assert_eq!(words("echo hi>x"), vec![vec!["echo", "hi"]]);
    }

    #[test]
    fn heredocs_are_skipped() {
        let p = parse("cat <<EOF > f.txt\nrm -rf /\nEOF\nls").unwrap();
        assert_eq!(p.commands.len(), 2);
        assert_eq!(p.commands[1].words[0].text, "ls");
        assert!(parse("cat <<'E'\n$(rm x)\nE\n").is_ok());
        assert!(parse("cat <<E\n$(rm x)\nE\n").is_err());
        assert!(parse("cat <<E\nno end").is_err());
    }

    #[test]
    fn flags_on_words() {
        let p = parse("rm runs/* ~/x {a,b} $HOME/y ${Z}").unwrap();
        let w = &p.commands[0].words;
        assert!(w[1].glob && w[2].tilde && w[3].brace && w[4].dynamic && w[5].dynamic);
        assert!(!parse("rm '*'").unwrap().commands[0].words[1].glob);
    }

    #[test]
    fn refuses_what_it_cannot_parse() {
        assert!(parse("echo 'x").is_err());
        assert!(parse("echo $(ls").is_err());
        assert!(parse("case x in a) rm y;; esac").is_err());
        assert!(parse("echo >").is_err());
    }
}
