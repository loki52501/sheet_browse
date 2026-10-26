//! Classifier tests. `round1_blocking_probes_are_delete` holds every Blocking probe of
//! review-T-14 round 1 (B1-B9); `read_only_controls_stay_below_delete` holds the review's
//! controls, so failing closed does not cost a turn on everyday reads (D-523).

use super::*;

struct Fx {
    _t: tempfile::TempDir,
    roots: Roots,
}

fn fx() -> Fx {
    let t = tempfile::tempdir().unwrap();
    let run = t.path().join("runs").join("2026-10-25T00-00-00-t");
    std::fs::create_dir_all(run.join("files")).unwrap();
    std::fs::create_dir_all(run.join("asks")).unwrap();
    std::fs::create_dir_all(t.path().join("runs").join("x")).unwrap();
    std::fs::write(t.path().join("file.txt"), "keep").unwrap();
    std::fs::write(t.path().join("x"), "x").unwrap();
    std::fs::write(t.path().join("runs").join("x").join("a.txt"), "a").unwrap();
    let roots = Roots::from_run_dir(&run).unwrap();
    Fx { _t: t, roots }
}

fn class(f: &Fx, cmd: &str) -> Class {
    classify(&f.roots, &f.roots.base, cmd, false).class
}

fn assert_all(f: &Fx, cmds: &[&str], want: Class) {
    let wrong: Vec<String> = cmds
        .iter()
        .map(|c| (c, classify(&f.roots, &f.roots.base, c, false)))
        .filter(|(_, v)| v.class != want)
        .map(|(c, v)| format!("{c:?} is {:?} ({})", v.class, v.why()))
        .collect();
    assert!(wrong.is_empty(), "want {want:?}:\n{}", wrong.join("\n"));
}

#[test]
fn round1_blocking_probes_are_delete() {
    let f = fx();
    assert_all(
        &f,
        &[
            // B1: arithmetic hides a substitution.
            "echo $(( $(rm file.txt) + 1 ))",
            "echo \"$(( $(rm file.txt) + 1 ))\"",
            "(( x = $(rm file.txt) ))",
            "echo $[ $(rm file.txt) ]",
            // B2: subscripts in static strings are arithmetic.
            "let 'x[$(rm file.txt)]=1'",
            "test -v 'x[$(rm file.txt)]'",
            "[ -v 'x[$(rm file.txt)]' ]",
            "[[ -v 'x[$(rm file.txt)]' ]]",
            "declare -i x; x='y[$(rm file.txt)]'",
            "declare -i 'x[$(rm file.txt)]=1'",
            "typeset 'x[$(rm file.txt)]=1'",
            "local 'x[$(rm file.txt)]=1'",
            "export 'x[$(rm file.txt)]=1'",
            "readonly 'x[$(rm file.txt)]=1'",
            "printf -v 'x[$(rm file.txt)]' %s 1",
            "read 'x[$(rm file.txt)]' <<< 1",
            "unset 'x[$(rm file.txt)]'",
            "mapfile 'x[$(rm file.txt)]' < runs/x/a.txt",
            // B2 through a variable's value: [[ -eq ]] and ${x:N} evaluate it.
            "x=$(cat runs/x/a.txt); [[ $x -eq 1 ]]",
            "x=$(cat runs/x/a.txt); echo ${s:$x}",
            "echo ${x@P}",
            "echo ${!x}",
            "echo ${a[$i]}",
            // B3
            "exec -a ls rm file.txt",
            // B4
            "history -w file.txt",
            // B5: run-time arguments under xargs and find -exec.
            "echo file.txt | xargs truncate -s 0",
            "echo file.txt | xargs gzip",
            "echo file.txt | xargs sed -i s/a/b/",
            "find . -exec sed -i s/a/b/ {} +",
            "find . -exec truncate -s0 {} +",
            // B6: placeholders escape runs/.
            "echo .. | xargs -I X find runs/X -maxdepth 1 -name file.txt -delete",
            "echo ../file.txt | xargs -I X sh -c 'rm runs/X'",
            "find .. -maxdepth 0 -exec sh -c 'rm -f runs/{}/file.txt' \\;",
            // B7: links alias outside files into runs/.
            "ln file.txt runs/x/h",
            "cp -l file.txt runs/x/h",
            "cp -s file.txt runs/x/h",
            "cp --link file.txt runs/x/h",
            "ln file.txt runs/x/h && : > runs/x/h",
            "ln -s .. runs/x/up && rm -f runs/x/up/file.txt",
            "mklink /H runs\\x\\h file.txt",
            "cp -r runs/x runs/y && echo hi > runs/y/a.txt",
            "echo hi > runs/y/a.txt & cp -a runs/x runs/y",
            "tar -xf runs/x/a.tar -C runs/x && rm -rf runs/x/l/file.txt",
            // B8: git options that run programs.
            "git ls-remote --upload-pack='rm file.txt' .",
            "git fetch --upload-pack='rm file.txt' origin",
            "git fetch -u 'rm file.txt' origin",
            "git pull --upload-pack='rm file.txt' origin",
            "git pull -u 'rm file.txt' origin",
            "git clone --upload-pack='rm file.txt' a b",
            "git clone -u 'rm file.txt' a b",
            "git push --receive-pack='rm file.txt' origin",
            "git push --exec='rm file.txt' origin",
            "git clone --template=runs/x a b",
            "git init --template=runs/x",
            "git -c core.fsmonitor='rm file.txt' status",
            "git -c user.name=x status",
            "git -c alias.x='!rm -rf /' x",
            "git --exec-path=runs/x status",
            "git log --ext-diff",
            "git diff --textconv",
            "git log --output=file.txt",
            "git grep -O'rm file.txt' x",
            "git rebase -x 'rm file.txt' main",
            "git submodule foreach rm file.txt",
            "git cat-file --textconv HEAD:a",
        ],
        Class::Delete,
    );
}

#[test]
fn round1_b9_cmd_exe_runs_only_plain_commands() {
    let f = fx();
    let cmd = |s: &str| classify(&f.roots, &f.roots.base, s, true);
    for probe in [
        "echo ' & del file.txt & '",
        "echo \" & del file.txt & \"",
        "echo %PATH%",
        "echo hi ^& del file.txt",
        "dir && del file.txt",
        "git status > runs/x/s.txt",
        "X=1 git status",
    ] {
        let v = cmd(probe);
        assert_eq!(v.class, Class::Delete, "{probe}");
        assert!(v.why().contains("Git Bash"), "{probe}: {}", v.why());
    }
    assert_eq!(cmd("git status").class, Class::Read);
    assert_eq!(cmd("rm runs/x/a.txt").class, Class::Read);
    assert_eq!(cmd("rm file.txt").class, Class::Delete);
    assert_eq!(
        plain_argv("git log -n 3 --oneline"),
        Some(vec![
            "git".to_string(),
            "log".into(),
            "-n".into(),
            "3".into(),
            "--oneline".into()
        ])
    );
    assert_eq!(plain_argv("echo 'a'"), None);
    assert_eq!(plain_argv("a=1 b"), None);
}

#[test]
fn read_only_controls_stay_below_delete() {
    let f = fx();
    assert_all(
        &f,
        &[
            "ls runs",
            "ls -la runs/x",
            "cat x",
            "cat runs/x/a.txt | head -5",
            "grep -r foo .",
            "grep -rn --include='*.rs' foo crates",
            "rg -n foo",
            "git status",
            "git status -sb",
            "git log --oneline",
            "git log --oneline -5",
            "git log -n 3 --format='%h %s' -- src",
            "git diff --stat HEAD~1",
            "git show HEAD:Cargo.toml",
            "git branch -a",
            "git remote -v",
            "git config --get user.name",
            "rm -rf runs/x",
            "rm -rf runs/x/*",
            "[ -f x ] && cat x",
            "[[ -f x ]] && cat x",
            "test -d runs && ls runs",
            "[ \"$x\" = y ] && echo same",
            "echo hello",
            "echo \"$HOME\"",
            "seq 1 30000",
            "sleep 60 | cat",
            "(sleep 3; echo late > runs/late1.txt) & sleep 60",
            "echo hi > runs/x/out.txt",
            "echo hi 2>&1 | tr a-z A-Z | tr A-Z a-z",
            "ls 2>/dev/null",
            "cd runs/x && rm -rf a.txt",
            "cd runs && rm -rf x",
            "find . -name '*.rs' -type f",
            "find runs/x -maxdepth 1 -print",
            "wc -l runs/x/a.txt",
            "sort runs/x/a.txt | uniq -c",
            "sort -o runs/x/b.txt runs/x/a.txt",
            "sed -n 1,5p x",
            "sed -i s/a/b/ runs/x/a.txt",
            "awk '{print $1}' x",
            "curl https://example.com",
            "curl -sSL https://example.com",
            "curl -o runs/x/p.html https://x",
            "cd runs/x && curl -O https://x/a",
            "tar -tf a.tar",
            "tar -xf a.tar -C runs/x",
            "gzip -c x",
            "mkdir -p runs/x/y",
            "touch runs/x/new.txt",
            "cp x runs/x/",
            "mv runs/x/a.txt runs/x/b.txt",
            "jq .a runs/x/a.txt",
            "date +%Y-%m-%d",
            "set -euo pipefail; ls",
            "for f in a b; do echo $f; done",
            "f=runs/x/a.txt; cat \"$f\"",
            "cargo --version",
            "python --version",
            "gh pr list --state open",
            "gh run view 123 --log",
            "gh api repos/a/b",
        ],
        Class::Read,
    );
    assert_all(
        &f,
        &[
            "echo hi >> file.txt",
            "echo hi > new.txt",
            "mkdir -p newdir",
            "touch file.txt",
            "cp runs/x/a.txt new.txt",
            "cp -n runs/x/a.txt file.txt",
            "mv runs/x/a.txt new.txt",
            "git commit -m x",
            "git add -A",
            "git checkout -b feat",
            "git switch main",
            "git fetch origin",
            "ls | tee -a file.txt",
        ],
        Class::Fill,
    );
}

#[test]
fn acceptance_rm() {
    let f = fx();
    assert_eq!(class(&f, "rm -rf runs/x"), Class::Read);
    assert_eq!(class(&f, "rm file.txt"), Class::Delete);
    assert_eq!(class(&f, "rm -rf runs/../file.txt"), Class::Delete);
    assert_eq!(
        class(&f, "rm -rf runs/*"),
        Class::Delete,
        "includes the run folder"
    );
    assert_eq!(class(&f, "rm -rf runs"), Class::Delete);
    assert_eq!(
        class(&f, "rm -rf runs/2026-10-25T00-00-00-t/asks"),
        Class::Delete
    );
    assert_eq!(
        class(&f, "rm -rf runs/2026-10-25T00-00-00-t/files"),
        Class::Read
    );
    assert_eq!(class(&f, "rm $X"), Class::Delete);
    assert_eq!(class(&f, "rm ~/x"), Class::Delete);
    assert_eq!(class(&f, "rm runs/{x,y}"), Class::Delete);
    assert_eq!(
        class(&f, "cd runs/x && rm -rf ../../file.txt"),
        Class::Delete
    );
    assert_eq!(class(&f, "cd $D && rm -rf x"), Class::Delete);
    assert_eq!(class(&f, "rm -- -rf"), Class::Delete);
    assert_eq!(class(&f, "/usr/bin/rm -f runs/x/a.txt"), Class::Delete);
    assert_eq!(class(&f, "./rm runs/x/a.txt"), Class::Delete);
    assert_eq!(class(&f, "\\rm file.txt"), Class::Delete);
    assert_eq!(class(&f, "RM.EXE file.txt"), Class::Delete);
    assert_eq!(class(&f, "rm --no-such-flag runs/x"), Class::Delete);
    assert_eq!(class(&f, "rm *"), Class::Delete);
}

#[test]
fn unknown_programs_wrappers_and_shells_are_delete() {
    let f = fx();
    assert_all(
        &f,
        &[
            "ls; rm file.txt",
            "ls && rm file.txt",
            "false || rm file.txt",
            "cat file.txt | rm file.txt",
            "echo $(rm file.txt)",
            "echo `rm file.txt`",
            "bash -c 'ls runs'",
            "sh -c 'ls'",
            "eval ls",
            "source runs/x/a.txt",
            ". runs/x/a.txt",
            "env FOO=1 ls",
            "nohup ls",
            "time ls",
            "command ls",
            "builtin echo",
            "sudo ls",
            "timeout 5 ls",
            "trap 'rm file.txt' EXIT",
            "alias ls='rm file.txt'",
            "f() { rm runs/x/a.txt; }; cd ..; f",
            "function f { ls; }",
            "python -c 'print(1)'",
            "node x.js",
            "make",
            "$CMD file.txt",
            "echo 'unterminated",
            "if true; then rm file.txt; fi",
            "for f in a b; do rm $f; done",
            "for PATH in runs/x; do ls; done",
            "while cd ..; do :; done; rm -rf runs/x",
            "frobnicate",
            "ls --frobnicate",
            "PATH=runs/x ls",
            "IFS=/ ls",
            "PS4='$(rm file.txt)'; set -x; ls",
            "FOO=1 ls",
            "shopt -s dotglob; rm -rf runs/x/*",
            "set -f",
            "cat <<EOF\n$(rm file.txt)\nEOF",
            "cat <<EOF\n${x@P}\nEOF",
            "echo hi > /dev/tcp/example.com/80",
            "printf \"$fmt\" x",
            "awk -f prog.awk x",
            "awk 'BEGIN{system(\"rm x\")}'",
            "awk '@load \"x\"'",
            "rg --pre cat pattern",
            "sort --compress-program=sh x",
            "tree -o file.txt",
            "date -s 2020-01-01",
            "date 0101000020",
            "hostname evil",
            "find . -delete",
            "find runs/x -delete",
            "find . -fprint file.txt",
            "find $D -name x",
            "find * -name x",
        ],
        Class::Delete,
    );
}

#[test]
fn build_tools_run_project_code_s2() {
    let f = fx();
    assert_all(
        &f,
        &[
            "cargo test -p srv-files",
            "cargo build",
            "cargo run",
            "RUST_LOG=debug cargo test",
            "npm test",
            "npm run build",
            "npm install",
            "cd runs/x && npm test",
            "make test",
            "python x.py",
            "pytest",
            "go test ./...",
            "cargo publish",
        ],
        Class::Delete,
    );
    assert_eq!(class(&f, "cargo --version"), Class::Read);
    assert_eq!(class(&f, "go version"), Class::Read);
    assert_eq!(class(&f, "make --version"), Class::Read);
}

#[test]
fn git_classes() {
    let f = fx();
    assert_eq!(class(&f, "git push"), Class::Account);
    assert_eq!(class(&f, "git push --force origin main"), Class::Delete);
    assert_eq!(class(&f, "git push origin +main"), Class::Delete);
    assert_eq!(class(&f, "git reset --hard HEAD~1"), Class::Delete);
    assert_eq!(class(&f, "git reset HEAD~1"), Class::Fill);
    assert_eq!(class(&f, "git clean -fdx"), Class::Delete);
    assert_eq!(class(&f, "git clean -n"), Class::Read);
    assert_eq!(class(&f, "git checkout -- file.txt"), Class::Delete);
    assert_eq!(class(&f, "git checkout -f main"), Class::Delete);
    assert_eq!(class(&f, "git stash drop"), Class::Delete);
    assert_eq!(class(&f, "git stash clear"), Class::Delete);
    assert_eq!(class(&f, "git stash"), Class::Fill);
    assert_eq!(class(&f, "git stash list"), Class::Read);
    assert_eq!(class(&f, "git frobnicate"), Class::Delete);
    // S1
    assert_eq!(class(&f, "git switch -f main"), Class::Delete);
    assert_eq!(
        class(&f, "git switch --discard-changes main"),
        Class::Delete
    );
    assert_eq!(class(&f, "git switch -c feat"), Class::Fill);
    // An editor or a pager would run a configured program.
    assert_eq!(class(&f, "git commit"), Class::Delete);
    assert_eq!(class(&f, "git commit -e -m x"), Class::Delete);
    assert_eq!(class(&f, "git add -p"), Class::Delete);
    assert_eq!(class(&f, "git rebase -i main"), Class::Delete);
    assert_eq!(class(&f, "git config --edit"), Class::Delete);
    assert_eq!(class(&f, "git config user.name x"), Class::Account);
    assert_eq!(class(&f, "git config core.pager 'rm x'"), Class::Delete);
    assert_eq!(class(&f, "git -P log"), Class::Delete);
    assert_eq!(class(&f, "git log $REV"), Class::Delete);
    assert_eq!(class(&f, "git send-email --to=a@b.c x.patch"), Class::Send);
    assert_eq!(
        class(&f, "git send-email --to-cmd=x x.patch"),
        Class::Delete
    );
    // git -C is for that command only.
    assert_eq!(
        class(&f, "git -C runs/x status && rm -rf a.txt"),
        Class::Delete
    );
    assert_eq!(class(&f, "git -C runs/x status && cat x"), Class::Delete);
    assert_eq!(class(&f, "git status && cat x"), Class::Read);
}

/// Review-T-14 closing Blocking 3 (D-525): a repository inside runs/ can be planted by
/// Read-class steps (an archive, a copy, a download), and its `.git/config` runs on the
/// next git call there; so git in runs/ is Delete, except creating a repository.
#[test]
fn git_in_runs_is_delete_one_line_and_two_calls() {
    let f = fx();
    // One line: extract, then git in the extracted folder.
    assert_all(
        &f,
        &[
            "tar -xf runs/x/a.tar -C runs/x/r && git -C runs/x/r status",
            "unzip -o runs/x/a.zip -d runs/x/r && git -C runs/x/r diff",
            "cp -r runs/x/payload runs/x/r && cd runs/x/r && git status",
            "cd runs/x && git log",
            "cd runs/x && gh pr list",
            "cd $D && git status",
        ],
        Class::Delete,
    );
    // Two calls: the extract is Read, the git call after it is Delete.
    assert_eq!(class(&f, "tar -xf runs/x/a.tar -C runs/x/r"), Class::Read);
    let r = f.roots.runs.join("x").join("r");
    std::fs::create_dir_all(r.join(".git")).unwrap();
    std::fs::write(
        r.join(".git").join("config"),
        "[core]\n\tfsmonitor = rm -rf ../../file.txt\n",
    )
    .unwrap();
    for line in [
        "git -C runs/x/r status",
        "git -C runs/x/r diff",
        "git -C runs/x/r log --oneline",
    ] {
        let v = classify(&f.roots, &f.roots.base, line, false);
        assert_eq!(v.class, Class::Delete, "{line}");
        assert!(v.why().contains("inside runs/"), "{line}: {}", v.why());
    }
    let in_r = classify(&f.roots, &r, "git status", false);
    assert_eq!(in_r.class, Class::Delete);
    // Creating a repository there is fine: clone and init never run repo config.
    assert!(class(&f, "git clone https://example.com/a.git runs/x/r2") < Class::Delete);
    assert!(class(&f, "cd runs/x && git init fresh") < Class::Delete);
    assert!(class(&f, "git --version") < Class::Delete);
    // A `.git` file outside runs/ pointing into runs/ is the same repository.
    let sub = f.roots.base.join("sub");
    std::fs::create_dir_all(&sub).unwrap();
    std::fs::write(
        sub.join(".git"),
        format!(
            "gitdir: {}\n",
            paths::plain(&r.join(".git")).replace('\\', "/")
        ),
    )
    .unwrap();
    assert_eq!(class(&f, "git -C sub status"), Class::Delete);
    // Outside runs/ git is unchanged.
    assert_eq!(class(&f, "git status"), Class::Read);
}

/// D-525: `git add` that can stage a planted repository in runs/ as a submodule (a later
/// `git status` then runs inside it) is Delete unless runs/ is ignored.
#[test]
fn git_add_reaching_runs_is_delete() {
    let f = fx();
    std::fs::create_dir_all(f.roots.base.join(".git")).unwrap();
    assert_eq!(class(&f, "git add -A"), Class::Delete);
    assert_eq!(class(&f, "git add ."), Class::Delete);
    assert_eq!(class(&f, "git add runs/x"), Class::Delete);
    assert_eq!(class(&f, "git add x"), Class::Fill);
    std::fs::write(f.roots.base.join(".gitignore"), "target/\nruns/\n").unwrap();
    assert_eq!(class(&f, "git add -A"), Class::Fill);
    assert_eq!(class(&f, "git add ."), Class::Fill);
    assert_eq!(class(&f, "git add -f runs/x/r"), Class::Delete);
}

/// Review-T-14 closing Blocking 2 (D-525): a recursive copy writes SRC's tree, not
/// DST/<name>; into a folder outside runs/ it is Delete, inside runs/ it is not.
#[test]
fn recursive_copies_and_extracts_outside_runs_are_delete() {
    let f = fx();
    let payload = f.roots.runs.join("x").join("payload");
    std::fs::create_dir_all(payload.join(".git")).unwrap();
    std::fs::write(payload.join(".git").join("config"), "[core]\n").unwrap();
    std::fs::create_dir_all(f.roots.runs.join("x").join("home")).unwrap();
    std::fs::create_dir_all(f.roots.base.join("somerepo")).unwrap();
    let h = home(".");
    let lines: Vec<String> = vec![
        format!("cp -r runs/x/home/. {h}"),
        format!("cp -a runs/x/home/. {h}"),
        format!("cp -R runs/x/home/. {h}"),
        format!("cp -rT runs/x/home {h}"),
        format!("cp -r -T runs/x/home {h}"),
        "cp -r runs/x/payload/. somerepo".into(),
        "cp -r runs/x/payload somerepo".into(),
        "cp -r runs/x/payload newdir".into(),
        "cp -a runs/x/payload newdir".into(),
        "mv runs/x/payload newdir".into(),
        "tar -xf runs/x/a.tar -C somerepo".into(),
        "tar -xf runs/x/a.tar".into(),
        "unzip runs/x/a.zip -d somerepo".into(),
        "git clone https://example.com/a.git".into(),
        "git clone https://example.com/a.git somerepo2".into(),
        "git worktree add ../wt-x".into(),
        "gh repo clone a/b".into(),
        "gh repo clone a/b somerepo2".into(),
        "gh release download v1 -D somerepo".into(),
        "gh run download 1".into(),
        "wget --content-disposition https://example.com/x".into(),
        "curl -O https://example.com/a".into(),
    ];
    let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
    assert_all(&f, &refs, Class::Delete);
    // Inside runs/ a tree is scratch; a single file copy is checked by name.
    assert_all(
        &f,
        &[
            "cp -r runs/x/payload runs/x/r",
            "cp -r runs/x/payload/. runs/x/r",
            "cp -rT runs/x/payload runs/x/r",
            "tar -xf runs/x/a.tar -C runs/x/r",
            "unzip runs/x/a.zip -d runs/x/r",
            "mv runs/x/payload runs/x/moved",
            "wget https://example.com/a.csv -P runs/x",
        ],
        Class::Read,
    );
    assert!(class(&f, "gh release download v1 -D runs/x") < Class::Delete);
    assert!(class(&f, "git clone https://example.com/a.git runs/x/r") < Class::Delete);
    assert_eq!(class(&f, "cp -r runs/x/a.txt new.txt"), Class::Fill);
    assert_eq!(class(&f, "wget https://example.com/a.csv"), Class::Fill);
    // -T copies into the destination itself: a tree into a .git folder is Delete even
    // inside runs/.
    assert_eq!(
        class(&f, "cp -rT runs/x/home runs/x/payload/.git"),
        Class::Delete
    );
    assert_eq!(
        class(&f, "tar -xf runs/x/a.tar -C runs/x/payload/.git"),
        Class::Delete
    );
}

#[test]
fn network_classes() {
    let f = fx();
    assert_eq!(class(&f, "curl -X POST https://example.com"), Class::Submit);
    assert_eq!(class(&f, "curl -XPUT https://example.com"), Class::Submit);
    assert_eq!(class(&f, "curl --request DELETE https://x"), Class::Delete);
    assert_eq!(class(&f, "curl -d a=1 https://x"), Class::Submit);
    assert_eq!(class(&f, "curl --json '{}' https://x"), Class::Submit);
    assert_eq!(class(&f, "curl -o file.txt https://x"), Class::Delete);
    assert_eq!(class(&f, "curl -O https://x/a"), Class::Delete);
    assert_eq!(class(&f, "curl -K runs/x/rc https://x"), Class::Delete);
    assert_eq!(
        class(&f, "curl -T runs/x/a.txt file:///tmp/x"),
        Class::Delete
    );
    assert_eq!(
        class(&f, "curl -w '%output{file.txt}x' https://x"),
        Class::Delete
    );
    assert_eq!(class(&f, "curl \"$URL\""), Class::Delete);
    assert_eq!(
        class(&f, "cd runs && curl --output-dir .. -o file.txt https://x"),
        Class::Delete
    );
    assert_eq!(class(&f, "wget --post-data=a https://x"), Class::Submit);
    assert_eq!(class(&f, "wget -e robots=off https://x"), Class::Delete);
    assert_eq!(class(&f, "wget -O runs/x/p https://x"), Class::Read);
    assert_eq!(class(&f, "gh api -X POST repos/a/b/issues"), Class::Account);
    assert_eq!(
        class(&f, "gh api repos/a/b/issues -f title=x"),
        Class::Account
    );
    assert_eq!(class(&f, "gh pr merge 3"), Class::Account);
    assert_eq!(class(&f, "gh pr create --title x --body y"), Class::Submit);
    assert_eq!(class(&f, "gh repo delete a/b"), Class::Delete);
    assert_eq!(class(&f, "gh extension install x"), Class::Delete);
    // An alias (`!cmd`) or an extension runs a program.
    assert_eq!(class(&f, "gh co"), Class::Delete);
    assert_eq!(class(&f, "gh myext list"), Class::Delete);
    assert_eq!(class(&f, "gh alias set co '!rm x'"), Class::Delete);
    assert_eq!(class(&f, "gh config set editor x"), Class::Delete);
    assert_eq!(class(&f, "gh auth token"), Class::Delete);
    assert_eq!(class(&f, "gh auth status"), Class::Read);
}

/// An absolute path below the home folder, single-quoted for a command line (a `~` word
/// is Delete on its own, so tests that use it prove nothing about the path rules).
fn home(rel: &str) -> String {
    let h = paths::plain(&dirs::home_dir().unwrap()).replace('\\', "/");
    format!("'{h}/{rel}'")
}

#[test]
fn writes_to_config_that_runs_programs_are_delete() {
    let f = fx();
    let mut lines: Vec<String> = [
        "echo x > .git/hooks/pre-commit",
        "echo x >> .git/config",
        "echo '* diff=x' >> .gitattributes",
        "cp runs/x/a.txt .git/hooks/post-checkout",
        "touch runs/x/.git/hooks/pre-commit",
        "echo x > runs/x/repo/.git/config",
        "echo x >> .curlrc",
        "mv runs/x/a.txt .wgetrc",
        "echo x >> .cargo/config.toml",
        "echo x >> .envrc",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    // review-T-14 round 2 and closing round: XDG git config and attributes, startup and
    // rc files, by absolute path (D-525).
    for (pre, rel) in [
        ("echo x >>", ".config/git/config"),
        ("echo '* diff=x' >>", ".config/git/attributes"),
        ("echo x >>", ".bashrc"),
        ("echo x >>", ".bash_profile"),
        ("echo x >>", ".profile"),
        ("echo x >>", ".zshrc"),
        ("echo x >>", ".ssh/authorized_keys"),
        ("echo x >>", ".ssh/config"),
        ("echo x >>", ".npmrc"),
        ("echo x >>", ".netrc"),
        ("echo x >>", "_netrc"),
        ("echo x >>", ".cargo/config.toml"),
        (
            "echo x >>",
            "Documents/WindowsPowerShell/Microsoft.PowerShell_profile.ps1",
        ),
        (
            "echo x >",
            "AppData/Roaming/Microsoft/Windows/Start Menu/Programs/Startup/a.cmd",
        ),
        ("echo x >", "bin/jq"),
        ("tee -a", ".config/git/config"),
        ("touch", ".config/git/attributes"),
        ("cp runs/x/a.txt", ".gitconfig"),
        ("mv runs/x/a.txt", ".bashrc"),
        ("sed -i s/a/b/", ".bashrc"),
        ("mkdir -p", ".config/git/hooks"),
    ] {
        lines.push(format!("{pre} {}", home(rel)));
    }
    // Into the home folder itself: lands as ~/.gitconfig, ~/.bashrc.
    let h = home(".");
    lines.push(format!("cp runs/x/.gitconfig {h}"));
    lines.push(format!("mv runs/x/.bashrc {h}"));
    lines.push(format!("cp -t {h} runs/x/.gitconfig"));
    let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
    assert_all(&f, &refs, Class::Delete);
    // The reason names the path, not only git, curl or wget.
    let v = classify(
        &f.roots,
        &f.roots.base,
        &format!("echo x >> {}", home("bin/jq")),
        false,
    );
    assert!(
        v.why().contains("startup file or a program folder"),
        "{}",
        v.why()
    );
}

#[test]
fn writes_into_a_path_folder_are_delete() {
    let f = fx();
    let real = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .find(|d| d.is_absolute() && d.is_dir());
    if let Some(dir) = real {
        let target = dir.join("planted-by-test-git");
        assert!(
            paths::runs_code_on_use(&f.roots, &target),
            "{}",
            target.display()
        );
    }
    assert!(!paths::runs_code_on_use(
        &f.roots,
        &std::env::temp_dir().join("no-path-dir-here").join("a.txt")
    ));
    // ~/bin is on Git Bash's PATH whatever the harness PATH says (closing Blocking 4).
    let jq = dirs::home_dir().unwrap().join("bin").join("jq");
    assert!(paths::runs_code_on_use(&f.roots, &jq));
}

#[test]
fn redirections() {
    let f = fx();
    assert_eq!(class(&f, "echo hi > file.txt"), Class::Delete);
    assert_eq!(class(&f, "ls > $OUT"), Class::Delete);
    assert_eq!(class(&f, "ls > runs/x/*.txt"), Class::Delete);
    assert_eq!(
        class(&f, "echo yes > runs/2026-10-25T00-00-00-t/asks/1.answer"),
        Class::Delete
    );
    assert_eq!(
        class(&f, "cat <<EOF > runs/x/n.txt\nrm -rf /\nEOF"),
        Class::Read
    );
    assert_eq!(class(&f, "ls | tee file.txt"), Class::Delete);
    assert_eq!(class(&f, "sort -o file.txt file.txt"), Class::Delete);
}

#[test]
fn edits_moves_copies() {
    let f = fx();
    assert_eq!(class(&f, "sed -i s/a/b/ file.txt"), Class::Delete);
    assert_eq!(class(&f, "sed -i s/a/b/ runs/x/*.txt"), Class::Delete);
    assert_eq!(class(&f, "sed 's/a/b/w out' file.txt"), Class::Delete);
    assert_eq!(class(&f, "sed -e '1e rm x' file.txt"), Class::Delete);
    assert_eq!(class(&f, "sed -i'../*' s/a/b/ runs/x/a.txt"), Class::Delete);
    assert_eq!(class(&f, "sed -n 1p $f"), Class::Delete);
    assert_eq!(class(&f, "awk '{print > \"o\"}' file.txt"), Class::Delete);
    assert_eq!(class(&f, "mv file.txt runs/x/"), Class::Delete);
    // S4: moving away outside runs/ is Delete, as D-520 says.
    assert_eq!(class(&f, "mv file.txt other.txt"), Class::Delete);
    assert_eq!(class(&f, "mv runs/x/a.txt file.txt"), Class::Delete);
    assert_eq!(class(&f, "cp runs/x/a.txt file.txt"), Class::Delete);
    assert_eq!(class(&f, "truncate -s 0 file.txt"), Class::Delete);
    assert_eq!(class(&f, "gzip file.txt"), Class::Delete);
    assert_eq!(class(&f, "tar -xf a.tar"), Class::Delete);
    assert_eq!(class(&f, "tar cfz out.tgz x"), Class::Delete);
    assert_eq!(class(&f, "tar czf runs/x/out.tgz x"), Class::Read);
    assert_eq!(
        class(&f, "tar --to-command=sh -xf a.tar -C runs/x"),
        Class::Delete
    );
    assert_eq!(class(&f, "tar -I zstd -tf a.tar"), Class::Delete);
    assert_eq!(class(&f, "zip -m runs/x/a.zip runs/x/a.txt"), Class::Delete);
    assert_eq!(class(&f, "zip -TT 'rm a' runs/x/z.zip a"), Class::Delete);
    assert_eq!(class(&f, "zip -r runs/x/z runs/x/a.txt"), Class::Read);
    assert_eq!(class(&f, "unzip a.zip"), Class::Delete);
    assert_eq!(class(&f, "unzip a.zip -d runs/x"), Class::Read);
    assert_eq!(class(&f, "dd if=/dev/zero of=file.txt"), Class::Delete);
    assert_eq!(class(&f, "xxd runs/x/a.txt file.txt"), Class::Delete);
    assert_eq!(
        class(&f, "iconv -f latin1 -t utf8 -o file.txt a"),
        Class::Delete
    );
}

#[test]
fn hard_linked_files_are_outside_b7() {
    let f = fx();
    let h = f.roots.runs.join("x").join("h");
    if std::fs::hard_link(f.roots.base.join("file.txt"), &h).is_err() {
        eprintln!("skipped: no hard links on this file system");
        return;
    }
    assert!(paths::hard_linked(&h));
    assert_eq!(f.roots.place(&h), Place::Outside);
    assert_all(
        &f,
        &[
            ": > runs/x/h",
            "echo hi > runs/x/h",
            "echo hi >> runs/x/h",
            "sed -i s/k/K/ runs/x/h",
            "truncate -s 0 runs/x/h",
            "cp runs/x/a.txt runs/x/h",
            "rm runs/x/h",
        ],
        Class::Delete,
    );
    assert_eq!(class(&f, "cat runs/x/h"), Class::Read);
    assert!(!paths::hard_linked(&f.roots.runs.join("x").join("a.txt")));
}

#[test]
fn cd_is_followed_only_where_it_must_have_happened() {
    let f = fx();
    assert_eq!(class(&f, "cd runs/x && ls && rm -rf a.txt"), Class::Read);
    // `;` and newlines run on after a failed cd; a subshell or pipe keeps its own cd.
    assert_eq!(class(&f, "cd runs/x; rm -rf a.txt"), Class::Delete);
    assert_eq!(class(&f, "cd runs/x\nrm -rf a.txt"), Class::Delete);
    assert_eq!(class(&f, "(cd runs/x); rm -rf a.txt"), Class::Delete);
    assert_eq!(class(&f, "(cd runs/x && rm -rf a.txt)"), Class::Delete);
    assert_eq!(class(&f, "cd runs/x | rm -rf a.txt"), Class::Delete);
    assert_eq!(class(&f, "cd runs/x && rm a.txt; rm b.txt"), Class::Delete);
    assert_eq!(class(&f, "cd /c/Windows && rm -rf x"), Class::Delete);
    // A substitution runs where its command runs, not where the line started.
    assert_eq!(class(&f, "cd .. && echo $(rm -rf runs/x)"), Class::Delete);
    // A cd in a loop repeats.
    assert_eq!(
        class(&f, "for i in 1 2; do cd runs; done; rm -rf x"),
        Class::Delete
    );
}

#[test]
fn test_and_brackets_s5() {
    let f = fx();
    assert_eq!(class(&f, "[ -f x ] && cat x"), Class::Read);
    assert_eq!(class(&f, "[ -n \"$x\" ]"), Class::Read);
    assert_eq!(class(&f, "[[ $x == a* ]] && echo y"), Class::Read);
    assert_eq!(class(&f, "[[ 1 -eq 2 ]]"), Class::Read);
    assert_eq!(class(&f, "[ $x = y ]"), Class::Delete, "unquoted: splits");
    assert_eq!(class(&f, "[ \"$a\" \"$b\" ]"), Class::Delete);
    assert_eq!(class(&f, "[[ a && b ]]"), Class::Delete);
    assert_eq!(class(&f, "[ -f x"), Class::Delete);
}

#[test]
fn sed_script_check() {
    assert!(!sed_writes("s/a/b/g"));
    assert!(!sed_writes("1,5p"));
    assert!(!sed_writes("/x/d;s|a|b|"));
    assert!(!sed_writes("$!N;P;D"));
    assert!(sed_writes("s/a/b/w out"));
    assert!(sed_writes("/x/w out"));
    assert!(sed_writes("1e ls"));
    assert!(sed_writes("s/a/b/e"));
    assert!(sed_writes("1!G;h;$!d;Z"));
}

#[test]
fn every_forbidden_name_is_delete_anywhere() {
    let f = fx();
    for name in FORBIDDEN {
        for line in [
            format!("{name} x"),
            format!("ls && {name}"),
            format!("echo $({name} x)"),
        ] {
            assert_eq!(class(&f, &line), Class::Delete, "{line}");
        }
    }
}
