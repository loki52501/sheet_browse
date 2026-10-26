//! T-05c acceptance (docs/01-HARNESS-CORE.md H-2; D-84, D-118 (3), I-4). Written by the
//! tester, independent of the coder's unit tests.
//!
//! Rows:
//! 1. `Vault::open_for_set` returns `harness_core::vault::VaultEditor`, not a `Vault`.
//! 2. The editor has no `resolve_into`, `scrub` or `install_global`, and no `Deref`,
//!    `AsRef`, `Borrow`, `Into` or `TryInto` to a `Vault`. Checked at run time with probe
//!    traits: an inherent method of the same name would shadow the probe's method, and an
//!    impl of a conversion trait switches the autoref probe, so adding any of them turns
//!    the test red instead of silently passing.
//! 3. `set` checks only the entry being set; the editor repairs a file `Vault::open`
//!    refuses (short entry, value inside a withheld marker); `remove` of an unknown name
//!    or on a missing file is `Unknown` and writes nothing; `save` fails on a file that no
//!    longer decrypts.
//! 5. No vault value in the editor's `Debug` or in any error message.
//!
//! The planted value is `SB_VAULT_CANARY` (or a fresh one when unset). No assertion
//! message formats a value. Every temp directory is removed when its test ends.

use std::any::TypeId;
use std::marker::PhantomData;
use std::path::{Path, PathBuf};

use harness_core::vault::{EntryKind, KeySource, Resolved, Vault, VaultEditor, VaultError};

/// A value inside the `[output withheld: vault scrubber unavailable]` marker (D-118 (3b)).
/// Distinctive, so an error message holding it would be caught.
const IN_MARKER: &str = "ber unavailab";

fn canary() -> String {
    std::env::var("SB_VAULT_CANARY")
        .ok()
        .filter(|c| c.chars().count() >= 8)
        .unwrap_or_else(|| {
            format!(
                "sbcanary-t05c-{}",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            )
        })
}

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> TempDir {
        let p = std::env::temp_dir().join(format!(
            "sb-t05c-accept-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&p).unwrap();
        TempDir(p)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn has_ci(hay: &str, needle: &str) -> bool {
    hay.to_ascii_lowercase()
        .contains(&needle.to_ascii_lowercase())
}

/// Row 5: neither the Display nor the Debug of `e` holds the canary or the in-marker value.
fn assert_clean_error(e: &VaultError, canary: &str, label: &str) {
    let text = format!("{e} | {e:?}");
    assert!(
        !has_ci(&text, canary),
        "{label}: canary in an error message"
    );
    assert!(
        !has_ci(&text, IN_MARKER),
        "{label}: planted value in an error message"
    );
}

fn identity(key_file: &Path) -> age::x25519::Identity {
    std::fs::read_to_string(key_file)
        .unwrap()
        .lines()
        .map(str::trim)
        .find(|l| l.starts_with("AGE-SECRET-KEY-"))
        .unwrap()
        .parse()
        .unwrap()
}

/// Writes `entries` (name, value, kind) into the vault file encrypted to `recipient`, as an
/// older build or a hand edit could have.
fn write_file(path: &Path, recipient: &age::x25519::Recipient, entries: &[(&str, &str, &str)]) {
    let mut body = String::new();
    for (name, value, kind) in entries {
        body.push_str(&format!(
            "{} = {{ value = {}, kind = \"{kind}\" }}\n",
            serde_json::to_string(name).unwrap(),
            serde_json::to_string(value).unwrap()
        ));
    }
    let ct = age::encrypt(recipient, body.as_bytes()).unwrap();
    std::fs::write(path, ct).unwrap();
}

/// A vault file `Vault::open` refuses: `test` = canary (valid), `st` = "CA" (under the
/// D-118 floor) and `mk` = a value inside the withheld marker.
fn broken_vault(tag: &str, canary: &str) -> (TempDir, PathBuf, KeySource) {
    let dir = TempDir::new(tag);
    let path = dir.0.join("vault.toml.age");
    let key_file = dir.0.join("vault.key");
    let key = KeySource::IdentityFile(key_file.clone());
    let mut v = Vault::open(&path, &key).unwrap();
    v.set("test", canary, EntryKind::Secret).unwrap();
    drop(v);
    write_file(
        &path,
        &identity(&key_file).to_public(),
        &[
            ("test", canary, "secret"),
            ("st", "CA", "personal"),
            ("mk", IN_MARKER, "personal"),
        ],
    );
    (dir, path, key)
}

fn listing(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

// ---------------------------------------------------------------------------------------
// Row 1

#[test]
fn r1_open_for_set_returns_a_vault_editor_not_a_vault() {
    // Compile-time: the signature names VaultEditor, at both paths.
    let f: fn(&Path, &KeySource) -> Result<VaultEditor, VaultError> = Vault::open_for_set;
    let dir = TempDir::new("r1");
    let path = dir.0.join("vault.toml.age");
    let key = KeySource::IdentityFile(dir.0.join("vault.key"));
    let editor = f(&path, &key).unwrap();
    fn type_of<T: 'static>(_: &T) -> TypeId {
        TypeId::of::<T>()
    }
    assert!(type_of(&editor) == TypeId::of::<VaultEditor>());
    assert!(type_of(&editor) != TypeId::of::<Vault>());
    assert!(TypeId::of::<harness_core::VaultEditor>() == TypeId::of::<VaultEditor>());
    assert!(std::any::type_name::<VaultEditor>().ends_with("vault::VaultEditor"));
    // A missing file opens as an empty editor and touches nothing (D-101).
    assert!(editor.names().is_empty());
    assert!(listing(&dir.0).is_empty(), "open_for_set wrote a file");
}

// ---------------------------------------------------------------------------------------
// Row 2: probes. If VaultEditor had an inherent method of the probed name, the inherent
// method would win over the probe trait's and the call would not return `Absent`.

struct Absent;

fn is_absent<T: 'static>(_: &T) -> bool {
    TypeId::of::<T>() == TypeId::of::<Absent>()
}

trait ProbeScrub {
    fn scrub(&self, _s: &str) -> Absent {
        Absent
    }
}
impl ProbeScrub for VaultEditor {}

trait ProbeInstall {
    fn install_global(&self) -> Absent {
        Absent
    }
}
impl ProbeInstall for VaultEditor {}

trait ProbeResolve {
    fn resolve_into<R>(&self, _t: &str, _sink: impl FnOnce(&Resolved<'_>) -> R) -> Absent {
        Absent
    }
}
impl ProbeResolve for VaultEditor {}

/// Autoref probes: `Probe<T>`'s impl applies only when `T` has the conversion; otherwise
/// the method resolves to the fallback on `&Probe<T>`.
struct Probe<T>(PhantomData<T>);

trait HasDeref {
    fn derefs(&self) -> bool {
        true
    }
}
impl<T: std::ops::Deref<Target = Vault>> HasDeref for Probe<T> {}
trait NoDeref {
    fn derefs(&self) -> bool {
        false
    }
}
impl<T> NoDeref for &Probe<T> {}

trait HasAsRef {
    fn as_refs(&self) -> bool {
        true
    }
}
impl<T: AsRef<Vault>> HasAsRef for Probe<T> {}
trait NoAsRef {
    fn as_refs(&self) -> bool {
        false
    }
}
impl<T> NoAsRef for &Probe<T> {}

trait HasBorrow {
    fn borrows(&self) -> bool {
        true
    }
}
impl<T: std::borrow::Borrow<Vault>> HasBorrow for Probe<T> {}
trait NoBorrow {
    fn borrows(&self) -> bool {
        false
    }
}
impl<T> NoBorrow for &Probe<T> {}

trait HasInto {
    fn intos(&self) -> bool {
        true
    }
}
impl<T: TryInto<Vault>> HasInto for Probe<T> {}
trait NoInto {
    fn intos(&self) -> bool {
        false
    }
}
impl<T> NoInto for &Probe<T> {}

#[test]
fn r2_editor_cannot_resolve_scrub_install_or_become_a_vault() {
    let c = canary();
    let (_dir, path, key) = broken_vault("r2", &c);
    let editor = Vault::open_for_set(&path, &key).unwrap();
    // Every probe runs; the failures are listed together.
    let mut has: Vec<&str> = Vec::new();
    if !is_absent(&editor.scrub("x")) {
        has.push("scrub");
    }
    if !is_absent(&editor.install_global()) {
        has.push("install_global");
    }
    if !is_absent(&editor.resolve_into("@vault(test)", |r| r.expose().len())) {
        has.push("resolve_into");
    }
    let p = Probe::<VaultEditor>(PhantomData);
    let p = &p;
    if p.derefs() {
        has.push("Deref<Target = Vault>");
    }
    if p.as_refs() {
        has.push("AsRef<Vault>");
    }
    if p.borrows() {
        has.push("Borrow<Vault>");
    }
    if p.intos() {
        has.push("Into/TryInto<Vault>");
    }
    assert!(has.is_empty(), "VaultEditor has {has:?}");

    // Positive controls: the probes see Vault's own methods.
    let v = Vault::in_memory([(
        "k".to_owned(),
        "a-control-value".to_owned(),
        EntryKind::Secret,
    )]);
    assert!(!is_absent(
        &v.resolve_into("@vault(k)", |r| r.expose().len())
    ));
    assert!(!is_absent(&v.scrub("x")));
    // Positive control: Vault is Borrow<Vault> and Into<Vault> (blanket impls).
    let q = Probe::<Vault>(PhantomData);
    let q = &q;
    assert!(q.borrows() && q.intos(), "autoref probe is broken");
    // Positive control: Box<Vault> derefs to and is AsRef<Vault>.
    let b = Probe::<Box<Vault>>(PhantomData);
    let b = &b;
    assert!(b.derefs() && b.as_refs(), "autoref probe is broken");

    // The process-wide scrubber is still the identity: nothing was installed.
    assert!(
        harness_core::vault::scrub(&c) == c,
        "a scrubber was installed"
    );
}

// ---------------------------------------------------------------------------------------
// Row 3

#[test]
fn r3a_set_checks_only_the_entry_being_set() {
    let c = canary();
    let (_dir, path, key) = broken_vault("r3a", &c);
    let e = Vault::open(&path, &key).unwrap_err();
    assert_clean_error(&e, &c, "open");
    assert!(matches!(e, VaultError::ValueTooShort { ref name, .. } if name == "st"));

    let mut ed = Vault::open_for_set(&path, &key).unwrap();
    // st and mk are still broken; a valid new entry is accepted anyway.
    ed.set("fresh", "a-valid-value-1", EntryKind::Secret)
        .expect("editor set refused a valid entry because of other entries");
    let names: Vec<String> = ed.names().into_iter().map(|(n, _)| n).collect();
    assert_eq!(names, ["fresh", "mk", "st", "test"]);
    // ...and the others are kept as they were: open still refuses, naming st.
    let e = Vault::open(&path, &key).unwrap_err();
    assert_clean_error(&e, &c, "open after fresh set");
    assert!(matches!(e, VaultError::ValueTooShort { ref name, .. } if name == "st"));

    let before = std::fs::read(&path).unwrap();
    // The entry being set is checked: under the floor.
    let e = ed.set("fresh2", "ab", EntryKind::Secret).unwrap_err();
    assert_clean_error(&e, &c, "set short");
    assert!(matches!(e, VaultError::ValueTooShort { ref name, min: 4 } if name == "fresh2"));
    // Inside the `scrub did not settle` marker.
    let e = ed
        .set("fresh3", "did not sett", EntryKind::Secret)
        .unwrap_err();
    assert_clean_error(&e, &c, "set in marker");
    assert!(matches!(e, VaultError::ValueInLiteral { ref name } if name == "fresh3"));
    // Inside `@vault(`.
    let e = ed.set("fresh4", "@VAULT", EntryKind::Secret).unwrap_err();
    assert_clean_error(&e, &c, "set in opener");
    assert!(matches!(e, VaultError::ValueInLiteral { ref name } if name == "fresh4"));
    // A name whose literal holds another entry's value involves the entry being set. (Here
    // the short `st` may hide in that literal too, so either entry may be the one named.)
    let bad_name = format!("x{c}");
    let e = ed
        .set(&bad_name, "another-valid-value", EntryKind::Secret)
        .unwrap_err();
    assert_clean_error(&e, &c, "set name holding a value");
    assert!(matches!(e, VaultError::ValueInLiteral { ref name } if name == "test" || name == "st"));
    assert!(
        std::fs::read(&path).unwrap() == before,
        "a refused set changed the file"
    );
    assert_eq!(ed.names().len(), 4, "a refused set changed the editor");

    // The same name rule on a vault whose only entry is the canary: `test` is named.
    let dir = TempDir::new("r3a-clean");
    let path = dir.0.join("vault.toml.age");
    let key = KeySource::IdentityFile(dir.0.join("vault.key"));
    let mut v = Vault::open(&path, &key).unwrap();
    v.set("test", &c, EntryKind::Secret).unwrap();
    drop(v);
    let mut ed = Vault::open_for_set(&path, &key).unwrap();
    let before = std::fs::read(&path).unwrap();
    let e = ed
        .set(&bad_name, "another-valid-value", EntryKind::Secret)
        .unwrap_err();
    assert_clean_error(&e, &c, "clean: set name holding a value");
    assert!(matches!(e, VaultError::ValueInLiteral { ref name } if name == "test"));
    assert!(
        std::fs::read(&path).unwrap() == before,
        "a refused set changed the file"
    );
}

#[test]
fn r3b_editor_repairs_a_file_open_refuses() {
    let c = canary();
    let (_dir, path, key) = broken_vault("r3b", &c);
    let mut ed = Vault::open_for_set(&path, &key).unwrap();
    ed.set("st", "California", EntryKind::Personal).unwrap();
    // The next problem is the value inside the withheld marker.
    let e = Vault::open(&path, &key).unwrap_err();
    assert_clean_error(&e, &c, "open after st repaired (pre-check)");
    assert!(matches!(e, VaultError::ValueInLiteral { ref name } if name == "mk"));
    assert!(
        e.to_string().contains("harness vault set mk"),
        "no repair hint"
    );
    assert_clean_error(&e, &c, "open after st repaired");
    ed.set("mk", "Mk-fixed-value", EntryKind::Personal).unwrap();
    ed.save().unwrap();

    let ok = Vault::open(&path, &key).expect("open still refuses the repaired file");
    let mut names = ok.names();
    names.sort_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(
        names,
        [
            ("mk".to_owned(), EntryKind::Personal),
            ("st".to_owned(), EntryKind::Personal),
            ("test".to_owned(), EntryKind::Secret),
        ]
    );
    assert!(
        ok.resolve_into("@vault(test)", |r| r.expose() == c)
            .unwrap(),
        "the repair lost the untouched entry"
    );
    assert_eq!(ok.scrub("from California"), "from @vault(st)");
    // The withheld marker is no longer a value.
    assert!(!ok.scrub("scrubber unavailable").contains("@vault("));
}

#[test]
fn r3c_remove_unknown_or_on_missing_file_writes_nothing() {
    let c = canary();
    // Missing file: Unknown, and no vault file, key file or lock file appears.
    let dir = TempDir::new("r3c-missing");
    let path = dir.0.join("vault.toml.age");
    let key = KeySource::IdentityFile(dir.0.join("vault.key"));
    let mut ed = Vault::open_for_set(&path, &key).unwrap();
    let e = ed.remove("nope").unwrap_err();
    assert_clean_error(&e, &c, "remove on missing file");
    assert!(matches!(e, VaultError::Unknown(ref n) if n == "nope"));
    assert!(
        listing(&dir.0).is_empty(),
        "remove on a missing file created a file"
    );
    assert!(ed.names().is_empty());
    ed.save().expect("save of an untouched empty editor");
    assert!(listing(&dir.0).is_empty(), "save created a file");

    // Existing file: Unknown, and the file is byte-for-byte and mtime unchanged.
    let (dir, path, key) = broken_vault("r3c", &c);
    let mut ed = Vault::open_for_set(&path, &key).unwrap();
    let before = std::fs::read(&path).unwrap();
    let mtime = std::fs::metadata(&path).unwrap().modified().unwrap();
    let files = listing(&dir.0);
    let e = ed.remove("nope").unwrap_err();
    assert_clean_error(&e, &c, "remove unknown");
    assert!(matches!(e, VaultError::Unknown(ref n) if n == "nope"));
    assert!(
        std::fs::read(&path).unwrap() == before,
        "remove of an unknown name rewrote the file"
    );
    assert_eq!(std::fs::metadata(&path).unwrap().modified().unwrap(), mtime);
    assert_eq!(listing(&dir.0), files);
    assert_eq!(ed.names().len(), 3);

    // Remove of a known entry writes through, and removing the broken ones repairs the file.
    ed.remove("st").unwrap();
    let e = Vault::open(&path, &key).unwrap_err();
    assert_clean_error(&e, &c, "open after st removed");
    assert!(matches!(e, VaultError::ValueInLiteral { ref name } if name == "mk"));
    ed.remove("mk").unwrap();
    let names: Vec<String> = ed.names().into_iter().map(|(n, _)| n).collect();
    assert_eq!(names, ["test"]);
    ed.save().unwrap();
    let ok = Vault::open(&path, &key).unwrap();
    assert_eq!(ok.names(), [("test".to_owned(), EntryKind::Secret)]);
}

#[test]
fn r3d_save_fails_on_a_file_that_no_longer_decrypts() {
    let c = canary();
    // Sanity: save on a good file succeeds and writes nothing.
    let (_d0, path, key) = broken_vault("r3d-ok", &c);
    let ed = Vault::open_for_set(&path, &key).unwrap();
    let before = std::fs::read(&path).unwrap();
    ed.save().unwrap();
    assert!(
        std::fs::read(&path).unwrap() == before,
        "save rewrote the file"
    );

    // Garbage bytes.
    let (_d1, path, key) = broken_vault("r3d-garbage", &c);
    let ed = Vault::open_for_set(&path, &key).unwrap();
    std::fs::write(&path, b"not an age file at all").unwrap();
    let e = ed.save().unwrap_err();
    assert_clean_error(&e, &c, "save garbage");
    assert!(matches!(e, VaultError::Decrypt), "garbage: {e}");

    // A well-formed age file for another key.
    let (_d2, path, key) = broken_vault("r3d-otherkey", &c);
    let mut ed = Vault::open_for_set(&path, &key).unwrap();
    ed.set("st", "California", EntryKind::Personal).unwrap();
    let other = age::x25519::Identity::generate();
    write_file(&path, &other.to_public(), &[("test", &c, "secret")]);
    let e = ed.save().unwrap_err();
    assert_clean_error(&e, &c, "save other key");
    assert!(matches!(e, VaultError::Decrypt), "other key: {e}");

    // The file was deleted after a set.
    let (_d3, path, key) = broken_vault("r3d-gone", &c);
    let mut ed = Vault::open_for_set(&path, &key).unwrap();
    ed.set("st", "California", EntryKind::Personal).unwrap();
    std::fs::remove_file(&path).unwrap();
    let e = ed.save().unwrap_err();
    assert_clean_error(&e, &c, "save gone");
    assert!(
        matches!(e, VaultError::Io(ref io) if io.kind() == std::io::ErrorKind::NotFound),
        "gone: {e}"
    );
}

// ---------------------------------------------------------------------------------------
// Row 5

#[test]
fn r5_editor_debug_and_names_hold_no_value() {
    let c = canary();
    let (_dir, path, key) = broken_vault("r5", &c);
    let mut ed = Vault::open_for_set(&path, &key).unwrap();
    ed.set("fresh", "Fresh-planted-value", EntryKind::Secret)
        .unwrap();
    for shown in [
        format!("{ed:?}"),
        format!("{ed:#?}"),
        format!("{:?}", Vault::open_for_set(&path, &key)),
        format!("{:?}", ed.names()),
    ] {
        assert!(!has_ci(&shown, &c), "canary in Debug or names");
        assert!(
            !has_ci(&shown, IN_MARKER),
            "planted value in Debug or names"
        );
        assert!(
            !has_ci(&shown, "Fresh-planted-value"),
            "set value in Debug or names"
        );
        assert!(!shown.contains("CA\""), "short value in Debug or names");
    }
}
