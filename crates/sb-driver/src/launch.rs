//! B-2 launch, pure parts (D-34, D-601): which Chromium, which profile directory, which flags.

use crate::{DriverError, Profile};
use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The D-34 pin (D-601): Chrome for Testing 156.0.8078.4, Playwright's chromium-1248.
pub const CHROMIUM_VERSION: &str = "156.0.8078.4";

/// The Chrome for Testing archive directory for this platform.
pub fn platform_dir() -> &'static str {
    if cfg!(windows) {
        if cfg!(target_pointer_width = "64") {
            "chrome-win64"
        } else {
            "chrome-win32"
        }
    } else if cfg!(target_os = "macos") {
        if cfg!(target_arch = "aarch64") {
            "chrome-mac-arm64"
        } else {
            "chrome-mac-x64"
        }
    } else {
        "chrome-linux64"
    }
}

/// The browser binary inside the platform directory.
pub fn binary_name() -> &'static str {
    if cfg!(windows) {
        "chrome.exe"
    } else if cfg!(target_os = "macos") {
        "Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing"
    } else {
        "chrome"
    }
}

/// What resolution and profile placement read from the process environment, captured once
/// so the functions using it stay pure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchEnv {
    /// `SB_CHROMIUM`; an empty value counts as unset.
    pub sb_chromium: Option<PathBuf>,
    /// `SB_HOME`; an empty value counts as unset.
    pub sb_home: Option<PathBuf>,
    pub cwd: PathBuf,
}

impl LaunchEnv {
    pub fn from_process() -> Result<LaunchEnv, DriverError> {
        let var = |k: &str| {
            std::env::var_os(k)
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
        };
        let cwd = std::env::current_dir()
            .map_err(|e| DriverError::Launch(format!("current directory: {e}")))?;
        Ok(LaunchEnv {
            sb_chromium: var("SB_CHROMIUM"),
            sb_home: var("SB_HOME"),
            cwd,
        })
    }

    /// `<SB_HOME or cwd>`.
    pub fn home(&self) -> &Path {
        self.sb_home.as_deref().unwrap_or(&self.cwd)
    }

    /// `<SB_HOME or cwd>/profiles`.
    pub fn profiles(&self) -> PathBuf {
        self.home().join("profiles")
    }

    /// `<SB_HOME or cwd>/chromium/<pin>/chrome-<platform>/<binary>`.
    pub fn pinned_chromium(&self) -> PathBuf {
        self.home()
            .join("chromium")
            .join(CHROMIUM_VERSION)
            .join(platform_dir())
            .join(binary_name())
    }
}

/// D-601 order: `LaunchOpts.chromium`, else `SB_CHROMIUM`, else the pinned path under
/// `<SB_HOME or cwd>`. The first source that is set decides: if it names a missing file that
/// is an error, never a fall-through to the next source.
pub fn resolve_chromium(
    explicit: Option<&Path>,
    env: &LaunchEnv,
    exists: impl Fn(&Path) -> bool,
) -> Result<PathBuf, DriverError> {
    if let Some(p) = explicit {
        return if exists(p) {
            Ok(p.to_path_buf())
        } else {
            Err(DriverError::Launch(format!(
                "LaunchOpts.chromium {} does not exist (it overrides SB_CHROMIUM and the pinned build)",
                p.display()
            )))
        };
    }
    if let Some(p) = env.sb_chromium.as_deref() {
        return if exists(p) {
            Ok(p.to_path_buf())
        } else {
            Err(DriverError::Launch(format!(
                "SB_CHROMIUM={} does not exist",
                p.display()
            )))
        };
    }
    let pinned = env.pinned_chromium();
    if exists(&pinned) {
        Ok(pinned)
    } else {
        Err(DriverError::Launch(format!(
            "Chromium {CHROMIUM_VERSION} not found at {}; set SB_CHROMIUM to a chrome binary",
            pinned.display()
        )))
    }
}

/// Exactly the D-601 flags, in order. Nothing that hides automation or changes the UA or
/// fingerprint (D-5, hard rule 3). Headless only for `bench`; `me` refuses it (D-30).
pub fn launch_args(
    profile: Profile,
    headless: bool,
    user_data_dir: &Path,
) -> Result<Vec<OsString>, DriverError> {
    if profile == Profile::Me && headless {
        return Err(DriverError::Launch(
            "profile `me` is never headless (B-2, D-30)".into(),
        ));
    }
    let mut dir = OsString::from("--user-data-dir=");
    dir.push(user_data_dir.as_os_str());
    let mut args: Vec<OsString> = vec![
        "--remote-debugging-pipe".into(),
        dir,
        "--no-first-run".into(),
        "--no-default-browser-check".into(),
    ];
    if headless {
        args.push("--headless=new".into());
    }
    args.push("about:blank".into());
    Ok(args)
}

/// A profile directory for one driver: `me` persists; `bench` is removed when dropped (B-2),
/// retrying while the browser's files are still held (D-630). Inside a [`crate::Driver`] it
/// is the last field, so it drops only after the browser was killed.
#[derive(Debug)]
pub(crate) enum ProfileDir {
    Me(PathBuf),
    Bench(PathBuf),
}

impl ProfileDir {
    pub(crate) fn prepare(profile: Profile, env: &LaunchEnv) -> Result<ProfileDir, DriverError> {
        let root = env.profiles();
        std::fs::create_dir_all(&root)
            .map_err(|e| DriverError::Launch(format!("create {}: {e}", root.display())))?;
        match profile {
            Profile::Me => {
                let dir = root.join("me");
                std::fs::create_dir_all(&dir)
                    .map_err(|e| DriverError::Launch(format!("create {}: {e}", dir.display())))?;
                Ok(ProfileDir::Me(dir))
            }
            // keep(): TempDir's own drop makes one silent removal attempt, which a file the
            // just-killed browser still holds defeats (B-199); our Drop retries instead.
            Profile::Bench => tempfile::Builder::new()
                .prefix("bench-")
                .tempdir_in(&root)
                .map(|t| ProfileDir::Bench(t.keep()))
                .map_err(|e| {
                    DriverError::Launch(format!("bench profile in {}: {e}", root.display()))
                }),
        }
    }

    pub(crate) fn path(&self) -> &Path {
        match self {
            ProfileDir::Me(p) | ProfileDir::Bench(p) => p,
        }
    }
}

impl Drop for ProfileDir {
    fn drop(&mut self) {
        if let ProfileDir::Bench(dir) = self {
            // This blocks the dropping thread for at most BENCH_REMOVE_CAP of waits, as
            // Driver's drop already blocked it up to DROP_KILL_CAP in kill(); no extra thread.
            // A failure is left behind for the stale-profile sweep (T-18c), never a panic.
            let _ = remove_dir_retrying(
                dir,
                |p: &Path| std::fs::remove_dir_all(p),
                &BENCH_REMOVE_BACKOFF,
                BENCH_REMOVE_CAP,
                std::thread::sleep,
            );
        }
    }
}

/// D-630: total time a bench profile removal may spend waiting between attempts. Measured
/// on a loaded Windows host: a held file (sharing violation) cleared within 5 s after the
/// browser was confirmed gone, but up to 16 s after `kill` hit its own 20 s cap. Only paid
/// while a file is still held; the usual removal succeeds on the first attempt.
pub(crate) const BENCH_REMOVE_CAP: Duration = Duration::from_secs(20);

/// D-630: waits before the 2nd, 3rd, ... removal attempt; the last repeats until the cap.
pub(crate) const BENCH_REMOVE_BACKOFF: [Duration; 5] = [
    Duration::ZERO,
    Duration::from_millis(50),
    Duration::from_millis(100),
    Duration::from_millis(250),
    Duration::from_millis(500),
];

/// An error a just-exited process's files can cause for a moment on Windows (Defender or
/// indexer scans, delete-pending handles): access denied (5), sharing violation (32), lock
/// violation (33), directory not empty (145). Anything else will not clear by waiting.
fn removal_retryable(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::PermissionDenied | io::ErrorKind::DirectoryNotEmpty
    ) || (cfg!(windows) && matches!(e.raw_os_error(), Some(5 | 32 | 33 | 145)))
}

/// Remove `dir` with `remove`, retrying a [`removal_retryable`] error after each wait in
/// `backoff` (its last entry repeating) while the waits so far stay within `cap`. NotFound
/// is success; any other error is returned at once.
pub(crate) fn remove_dir_retrying(
    dir: &Path,
    mut remove: impl FnMut(&Path) -> io::Result<()>,
    backoff: &[Duration],
    cap: Duration,
    mut sleep: impl FnMut(Duration),
) -> io::Result<()> {
    let mut waited = Duration::ZERO;
    let mut attempt = 0usize;
    loop {
        let err = match remove(dir) {
            Ok(()) => return Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(e) if !removal_retryable(&e) => return Err(e),
            Err(e) => e,
        };
        let wait = match backoff.get(attempt) {
            Some(w) => *w,
            // The last wait repeats; a zero one would spin, so it ends the retries instead.
            None => match backoff.last() {
                Some(w) if !w.is_zero() => *w,
                _ => return Err(err),
            },
        };
        if waited + wait > cap {
            return Err(err);
        }
        // Windows gives no event when another process releases a file lock, so there is no
        // condition to wait on (rule 6): back off on a schedule, capped in total.
        sleep(wait);
        waited += wait;
        attempt += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn env(sb_chromium: Option<&str>, sb_home: Option<&str>) -> LaunchEnv {
        LaunchEnv {
            sb_chromium: sb_chromium.map(PathBuf::from),
            sb_home: sb_home.map(PathBuf::from),
            cwd: PathBuf::from("/work"),
        }
    }

    fn only(paths: &[&str]) -> impl Fn(&Path) -> bool {
        let set: HashSet<PathBuf> = paths.iter().map(PathBuf::from).collect();
        move |p: &Path| set.contains(p)
    }

    #[test]
    fn bench_headless_argv_is_exactly_the_d601_flags() {
        let a = launch_args(Profile::Bench, true, Path::new("/p/bench-1")).unwrap();
        let want: Vec<OsString> = [
            "--remote-debugging-pipe",
            "--user-data-dir=/p/bench-1",
            "--no-first-run",
            "--no-default-browser-check",
            "--headless=new",
            "about:blank",
        ]
        .iter()
        .map(OsString::from)
        .collect();
        assert_eq!(a, want);
    }

    #[test]
    fn headed_argv_has_no_headless_flag() {
        for p in [Profile::Bench, Profile::Me] {
            let a = launch_args(p, false, Path::new("/p/x")).unwrap();
            assert_eq!(a.len(), 5);
            assert!(!a.iter().any(|f| f.to_string_lossy().contains("headless")));
            assert_eq!(a.last().unwrap(), "about:blank");
        }
    }

    #[test]
    fn me_headless_is_rejected() {
        let e = launch_args(Profile::Me, true, Path::new("/p/me")).unwrap_err();
        assert!(matches!(e, DriverError::Launch(ref m) if m.contains("never headless")));
    }

    #[test]
    fn argv_has_no_stealth_ua_or_automation_hiding_flags() {
        let a = launch_args(Profile::Bench, true, Path::new("/p")).unwrap();
        for f in &a {
            let f = f.to_string_lossy().to_ascii_lowercase();
            for banned in [
                "user-agent",
                "automationcontrolled",
                "disable-blink-features",
                "enable-automation",
                "stealth",
                "fingerprint",
                "excludeswitches",
            ] {
                assert!(!f.contains(banned), "{f}");
            }
        }
    }

    #[test]
    fn explicit_path_wins_over_env_and_pinned() {
        let e = env(Some("/env/chrome"), None);
        let p = resolve_chromium(
            Some(Path::new("/opt/chrome")),
            &e,
            only(&["/opt/chrome", "/env/chrome"]),
        )
        .unwrap();
        assert_eq!(p, PathBuf::from("/opt/chrome"));
    }

    #[test]
    fn missing_explicit_path_errors_instead_of_falling_through() {
        let e = env(Some("/env/chrome"), None);
        let err =
            resolve_chromium(Some(Path::new("/opt/gone")), &e, only(&["/env/chrome"])).unwrap_err();
        let m = err.to_string();
        assert!(m.contains("/opt/gone") && m.contains("SB_CHROMIUM"), "{m}");
    }

    #[test]
    fn env_is_used_when_no_explicit_path() {
        let e = env(Some("/env/chrome"), None);
        let p = resolve_chromium(None, &e, only(&["/env/chrome"])).unwrap();
        assert_eq!(p, PathBuf::from("/env/chrome"));
    }

    #[test]
    fn missing_env_path_errors_naming_sb_chromium() {
        let e = env(Some("/env/gone"), None);
        let pinned = e.pinned_chromium();
        let pinned = pinned.to_str().unwrap();
        let err = resolve_chromium(None, &e, only(&[pinned])).unwrap_err();
        let m = err.to_string();
        assert!(m.contains("SB_CHROMIUM=/env/gone"), "{m}");
    }

    #[test]
    fn pinned_path_under_sb_home_then_cwd() {
        let home = env(None, Some("/home/sb"));
        let want = PathBuf::from("/home/sb/chromium")
            .join(CHROMIUM_VERSION)
            .join(platform_dir())
            .join(binary_name());
        assert_eq!(home.pinned_chromium(), want);
        assert_eq!(
            resolve_chromium(None, &home, only(&[want.to_str().unwrap()])).unwrap(),
            want
        );
        let cwd = env(None, None);
        assert!(cwd
            .pinned_chromium()
            .starts_with("/work/chromium/156.0.8078.4"));
        if cfg!(windows) {
            assert!(cwd.pinned_chromium().ends_with("chrome-win64/chrome.exe"));
        }
        let err = resolve_chromium(None, &cwd, only(&[])).unwrap_err();
        assert!(err.to_string().contains("SB_CHROMIUM"));
    }

    #[test]
    fn profiles_live_under_home() {
        assert_eq!(
            env(None, Some("/h")).profiles(),
            PathBuf::from("/h/profiles")
        );
        assert_eq!(env(None, None).profiles(), PathBuf::from("/work/profiles"));
    }

    /// The error a file the browser still holds gives: ERROR_SHARING_VIOLATION on Windows.
    fn busy() -> io::Error {
        if cfg!(windows) {
            io::Error::from_raw_os_error(32)
        } else {
            io::Error::from(io::ErrorKind::PermissionDenied)
        }
    }

    /// Runs remove_dir_retrying with `results` as the remover's answers (then Ok), and a
    /// sleep that only records. Returns the result, the attempt count and the waits.
    fn run_removal(
        results: Vec<io::Result<()>>,
        backoff: &[Duration],
        cap: Duration,
    ) -> (io::Result<()>, usize, Vec<Duration>) {
        let mut results = results.into_iter();
        let mut attempts = 0;
        let mut waits = Vec::new();
        let r = remove_dir_retrying(
            Path::new("/p/bench-x"),
            |p| {
                assert_eq!(p, Path::new("/p/bench-x"));
                attempts += 1;
                results.next().unwrap_or(Ok(()))
            },
            backoff,
            cap,
            |d| waits.push(d),
        );
        (r, attempts, waits)
    }

    fn ms(v: &[u64]) -> Vec<Duration> {
        v.iter().map(|m| Duration::from_millis(*m)).collect()
    }

    #[test]
    fn removal_retries_a_held_file_on_the_d630_schedule_until_released() {
        let (r, attempts, waits) = run_removal(
            vec![Err(busy()), Err(busy()), Err(busy())],
            &BENCH_REMOVE_BACKOFF,
            BENCH_REMOVE_CAP,
        );
        assert!(r.is_ok(), "{r:?}");
        assert_eq!(attempts, 4);
        assert_eq!(waits, ms(&[0, 50, 100]));
    }

    #[test]
    fn removal_gives_up_at_the_d630_cap_with_the_last_error() {
        assert_eq!(BENCH_REMOVE_CAP, Duration::from_secs(20));
        assert_eq!(BENCH_REMOVE_BACKOFF.to_vec(), ms(&[0, 50, 100, 250, 500]));
        let always = (0..100).map(|_| Err(busy())).collect();
        let (r, attempts, waits) = run_removal(always, &BENCH_REMOVE_BACKOFF, BENCH_REMOVE_CAP);
        let e = r.expect_err("held past the cap");
        assert_eq!(e.raw_os_error(), busy().raw_os_error());
        assert_eq!(e.kind(), busy().kind());
        // 0+50+100+250+500 then 500 ms steps: 19.9 s waited, the next step would pass 20 s.
        let mut want = ms(&[0, 50, 100, 250, 500]);
        want.extend(ms(&[500; 38]));
        assert_eq!(waits, want);
        assert_eq!(waits.iter().sum::<Duration>(), Duration::from_millis(19900));
        assert_eq!(attempts, waits.len() + 1);
    }

    #[test]
    fn removal_not_found_is_success_and_other_errors_stop_at_once() {
        let gone = || Err(io::Error::from(io::ErrorKind::NotFound));
        let (r, attempts, waits) =
            run_removal(vec![gone()], &BENCH_REMOVE_BACKOFF, BENCH_REMOVE_CAP);
        assert!(r.is_ok() && attempts == 1 && waits.is_empty(), "{r:?}");
        let (r, attempts, _) = run_removal(
            vec![Err(busy()), gone(), Err(busy())],
            &BENCH_REMOVE_BACKOFF,
            BENCH_REMOVE_CAP,
        );
        assert!(r.is_ok() && attempts == 2, "{r:?}");

        let other = || Err(io::Error::from(io::ErrorKind::InvalidInput));
        let (r, attempts, waits) =
            run_removal(vec![other()], &BENCH_REMOVE_BACKOFF, BENCH_REMOVE_CAP);
        assert_eq!(r.unwrap_err().kind(), io::ErrorKind::InvalidInput);
        assert!(attempts == 1 && waits.is_empty());
        let (r, attempts, waits) = run_removal(
            vec![Err(busy()), other()],
            &BENCH_REMOVE_BACKOFF,
            BENCH_REMOVE_CAP,
        );
        assert_eq!(r.unwrap_err().kind(), io::ErrorKind::InvalidInput);
        assert_eq!((attempts, waits), (2, ms(&[0])));
    }

    #[test]
    fn removal_with_no_or_zero_schedule_does_not_spin() {
        let always = || (0..100).map(|_| Err(busy())).collect();
        let (r, attempts, waits) = run_removal(always(), &[], BENCH_REMOVE_CAP);
        assert!(r.is_err() && attempts == 1 && waits.is_empty());
        let (r, attempts, waits) = run_removal(always(), &[Duration::ZERO], BENCH_REMOVE_CAP);
        assert!(r.is_err() && attempts == 2);
        assert_eq!(waits, ms(&[0]));
    }

    #[cfg(windows)]
    #[test]
    fn removal_retries_each_windows_held_file_code() {
        // 5 access denied, 32 sharing violation, 33 lock violation, 145 dir not empty.
        for code in [5, 32, 33, 145] {
            let held = || Err(io::Error::from_raw_os_error(code));
            let (r, attempts, _) = run_removal(
                vec![held(), held()],
                &BENCH_REMOVE_BACKOFF,
                BENCH_REMOVE_CAP,
            );
            assert!(
                r.is_ok() && attempts == 3,
                "code {code}: {r:?} after {attempts}"
            );
        }
        // 3 is ERROR_PATH_NOT_FOUND (NotFound: success); 87 ERROR_INVALID_PARAMETER stops.
        let (r, attempts, _) = run_removal(
            vec![Err(io::Error::from_raw_os_error(87))],
            &BENCH_REMOVE_BACKOFF,
            BENCH_REMOVE_CAP,
        );
        assert!(r.is_err() && attempts == 1);
    }

    #[test]
    fn bench_profile_is_a_fresh_dir_removed_on_drop_and_me_persists() {
        let root = tempfile::tempdir().unwrap();
        let e = LaunchEnv {
            sb_chromium: None,
            sb_home: Some(root.path().to_path_buf()),
            cwd: PathBuf::from("/unused"),
        };
        let a = ProfileDir::prepare(Profile::Bench, &e).unwrap();
        let b = ProfileDir::prepare(Profile::Bench, &e).unwrap();
        assert_ne!(a.path(), b.path());
        let name = a.path().file_name().unwrap().to_string_lossy().into_owned();
        assert!(name.starts_with("bench-"), "{name}");
        assert_eq!(a.path().parent().unwrap(), root.path().join("profiles"));
        let gone = a.path().to_path_buf();
        std::fs::create_dir_all(gone.join("Default").join("Cache")).unwrap();
        std::fs::write(gone.join("Default").join("Cookies"), b"x").unwrap();
        drop(a);
        assert!(!gone.exists());
        let me = ProfileDir::prepare(Profile::Me, &e).unwrap();
        let me_path = me.path().to_path_buf();
        assert_eq!(me_path, root.path().join("profiles").join("me"));
        drop(me);
        assert!(me_path.is_dir());
    }
}
