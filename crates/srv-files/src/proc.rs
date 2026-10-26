//! Child processes with a timeout, a process-tree kill, and capped capture (F-2, F-3).
//!
//! No fixed sleeps (rule 6): every wait here is on a condition (exit, EOF) with a cap.

use std::collections::VecDeque;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, Command};

/// How long to wait for a killed tree to be reaped.
const KILL_WAIT: Duration = Duration::from_secs(10);
/// How long to wait for the output pipes to reach EOF once the process exited: a
/// background grandchild can keep them open forever.
const DRAIN_CAP: Duration = Duration::from_secs(2);

/// Keeps the first and last `keep` bytes of a stream, counts the rest, and optionally
/// writes everything to a log file.
pub struct Tap {
    keep: usize,
    head: Vec<u8>,
    tail: VecDeque<u8>,
    total: u64,
    log: Option<std::fs::File>,
    log_error: Option<String>,
}

impl Tap {
    pub fn new(keep: usize, log: Option<std::fs::File>) -> Tap {
        Tap {
            keep,
            head: Vec::new(),
            tail: VecDeque::new(),
            total: 0,
            log,
            log_error: None,
        }
    }

    pub fn shared(keep: usize, log: Option<std::fs::File>) -> Arc<Mutex<Tap>> {
        Arc::new(Mutex::new(Tap::new(keep, log)))
    }

    fn push(&mut self, b: &[u8]) {
        self.total += b.len() as u64;
        if let Some(f) = &mut self.log {
            if let Err(e) = f.write_all(b) {
                self.log_error = Some(e.to_string());
                self.log = None;
            }
        }
        let room = self.keep.saturating_sub(self.head.len());
        let (h, rest) = b.split_at(room.min(b.len()));
        self.head.extend_from_slice(h);
        for &c in rest {
            if self.tail.len() == self.keep {
                self.tail.pop_front();
            }
            self.tail.push_back(c);
        }
    }

    /// Bytes seen.
    pub fn total(&self) -> u64 {
        self.total
    }

    /// True when bytes between the head and the tail were dropped.
    pub fn cut(&self) -> bool {
        self.total > (self.head.len() + self.tail.len()) as u64
    }

    /// Everything kept (head then tail), lossy UTF-8.
    pub fn text(&self) -> String {
        let mut all = self.head.clone();
        all.extend(self.tail.iter());
        String::from_utf8_lossy(&all).into_owned()
    }

    /// (head, tail) as lossy UTF-8.
    pub fn parts(&self) -> (String, String) {
        let tail: Vec<u8> = self.tail.iter().copied().collect();
        (
            String::from_utf8_lossy(&self.head).into_owned(),
            String::from_utf8_lossy(&tail).into_owned(),
        )
    }

    /// Flushes the log; the first write error, if any.
    pub fn finish_log(&mut self) -> Option<String> {
        if let Some(f) = &mut self.log {
            if let Err(e) = f.flush() {
                self.log_error.get_or_insert(e.to_string());
            }
        }
        self.log = None;
        self.log_error.clone()
    }
}

/// How a process ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Exit {
    /// Exit code; `None` when killed or ended by a signal.
    pub code: Option<i32>,
    pub timed_out: bool,
}

/// Runs `cmd` to completion or `timeout`, feeding `stdin` and pumping stdout into `out`
/// and stderr into `err` (pass the same tap twice to merge them in arrival order). On
/// timeout the whole process tree is killed.
pub async fn run(
    mut cmd: Command,
    stdin: Option<Vec<u8>>,
    timeout: Duration,
    out: Arc<Mutex<Tap>>,
    err: Arc<Mutex<Tap>>,
) -> io::Result<Exit> {
    cmd.stdin(if stdin.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    })
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .kill_on_drop(true);
    #[cfg(unix)]
    cmd.process_group(0);
    // Started suspended so it joins its job before it can start anything (see `winjob`).
    #[cfg(windows)]
    cmd.creation_flags(winjob::CREATE_NO_WINDOW | winjob::CREATE_SUSPENDED);
    let mut child = cmd.spawn()?;
    let tree = Tree::attach(&child);
    if let (Some(data), Some(mut pipe)) = (stdin, child.stdin.take()) {
        tokio::spawn(async move {
            // A child that does not read its stdin closes the pipe; that is not an error.
            let _ = pipe.write_all(&data).await;
            let _ = pipe.shutdown().await;
        });
    }
    let mut r1 = tokio::spawn(pump(child.stdout.take(), out));
    let mut r2 = tokio::spawn(pump(child.stderr.take(), err));

    let exit = match tokio::time::timeout(timeout, child.wait()).await {
        Ok(status) => status.map(|s| Exit {
            code: s.code(),
            timed_out: false,
        }),
        Err(_) => {
            tree.kill(&mut child).await;
            let _ = tokio::time::timeout(KILL_WAIT, child.wait()).await;
            Ok(Exit {
                code: None,
                timed_out: true,
            })
        }
    };
    // A call's processes end with the call, on every path (a failed wait included):
    // anything it left running in the background (`cmd &`, a pipeline stage Git Bash
    // does not parent to bash) is killed too, so no orphan keeps the output pipes open.
    tree.kill(&mut child).await;
    let drained = tokio::time::timeout(DRAIN_CAP, async {
        let _ = (&mut r1).await;
        let _ = (&mut r2).await;
    })
    .await;
    if drained.is_err() {
        r1.abort();
        r2.abort();
    }
    exit
}

async fn pump<R: AsyncRead + Unpin>(src: Option<R>, tap: Arc<Mutex<Tap>>) {
    let Some(mut src) = src else { return };
    let mut buf = vec![0u8; 16 * 1024];
    loop {
        match src.read(&mut buf).await {
            Ok(0) | Err(_) => return,
            Ok(n) => tap
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(&buf[..n]),
        }
    }
}

/// Everything a child started: a Job Object on Windows, the process group on Unix.
struct Tree {
    #[cfg(windows)]
    job: Option<winjob::Job>,
    #[cfg(unix)]
    pgid: Option<u32>,
}

impl Tree {
    /// Puts a just-spawned child in its own job (Windows; the child was created suspended
    /// and is resumed here whether or not that worked) or records its process group.
    fn attach(child: &Child) -> Tree {
        #[cfg(windows)]
        {
            Tree {
                job: child.raw_handle().and_then(winjob::Job::adopt),
            }
        }
        #[cfg(unix)]
        {
            Tree { pgid: child.id() }
        }
    }

    /// Kills the child and everything it started.
    async fn kill(&self, child: &mut Child) {
        #[cfg(windows)]
        {
            match &self.job {
                Some(job) => job.terminate(),
                None => {
                    if let Some(pid) = child.id() {
                        let mut k = Command::new("taskkill");
                        k.args(["/F", "/T", "/PID", &pid.to_string()])
                            .stdin(Stdio::null())
                            .stdout(Stdio::null())
                            .stderr(Stdio::null())
                            .creation_flags(winjob::CREATE_NO_WINDOW);
                        let _ = tokio::time::timeout(KILL_WAIT, k.status()).await;
                    }
                }
            }
        }
        #[cfg(unix)]
        {
            if let Some(pgid) = self.pgid {
                let mut k = Command::new("kill");
                k.args(["-KILL", "--", &format!("-{pgid}")])
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null());
                let _ = tokio::time::timeout(KILL_WAIT, k.status()).await;
            }
        }
        let _ = child.start_kill();
    }
}

/// Windows Job Objects. Git Bash (MSYS2) starts pipeline stages and background commands
/// as processes whose Windows parent is not the bash that ran them, so `taskkill /T` on
/// bash misses them; a job holds every process created inside it, however it was started.
/// The job is set to kill its processes when its last handle closes, so a crashed server
/// leaves no tree behind (S10).
#[cfg(windows)]
mod winjob {
    use std::ffi::c_void;
    use std::os::windows::io::RawHandle;

    pub const CREATE_SUSPENDED: u32 = 0x0000_0004;
    pub const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    const JOB_OBJECT_EXTENDED_LIMIT_INFORMATION: i32 = 9;
    const JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE: u32 = 0x0000_2000;

    type Handle = *mut c_void;

    #[repr(C)]
    #[derive(Default)]
    struct BasicLimitInformation {
        per_process_user_time_limit: i64,
        per_job_user_time_limit: i64,
        limit_flags: u32,
        minimum_working_set_size: usize,
        maximum_working_set_size: usize,
        active_process_limit: u32,
        affinity: usize,
        priority_class: u32,
        scheduling_class: u32,
    }

    #[repr(C)]
    #[derive(Default)]
    struct IoCounters {
        read_operation_count: u64,
        write_operation_count: u64,
        other_operation_count: u64,
        read_transfer_count: u64,
        write_transfer_count: u64,
        other_transfer_count: u64,
    }

    /// JOBOBJECT_EXTENDED_LIMIT_INFORMATION.
    #[repr(C)]
    #[derive(Default)]
    struct ExtendedLimitInformation {
        basic: BasicLimitInformation,
        io: IoCounters,
        process_memory_limit: usize,
        job_memory_limit: usize,
        peak_process_memory_used: usize,
        peak_job_memory_used: usize,
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn CreateJobObjectW(attributes: *mut c_void, name: *const u16) -> Handle;
        fn SetInformationJobObject(
            job: Handle,
            class: i32,
            info: *const c_void,
            length: u32,
        ) -> i32;
        fn AssignProcessToJobObject(job: Handle, process: Handle) -> i32;
        fn TerminateJobObject(job: Handle, exit_code: u32) -> i32;
        fn CloseHandle(handle: Handle) -> i32;
    }

    #[link(name = "ntdll")]
    extern "system" {
        /// Resumes every thread of a process (exported by ntdll since Windows XP).
        fn NtResumeProcess(process: Handle) -> i32;
    }

    pub struct Job(Handle);

    // SAFETY: a job handle is a kernel handle, usable from any thread.
    unsafe impl Send for Job {}
    unsafe impl Sync for Job {}

    impl Job {
        /// Creates a job, assigns the suspended `process` to it and resumes the process.
        /// `None` if the job could not be made or assigned (the process is resumed anyway;
        /// the caller then falls back to `taskkill /T`).
        pub fn adopt(process: RawHandle) -> Option<Job> {
            let process = process as Handle;
            // SAFETY: plain Win32 calls on a live process handle owned by the tokio Child;
            // a null job handle is checked before use and the job handle is closed on drop.
            unsafe {
                let job = CreateJobObjectW(std::ptr::null_mut(), std::ptr::null());
                if !job.is_null() {
                    let mut info = ExtendedLimitInformation::default();
                    info.basic.limit_flags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
                    // Best effort: without the limit the explicit kills still run.
                    SetInformationJobObject(
                        job,
                        JOB_OBJECT_EXTENDED_LIMIT_INFORMATION,
                        (&info as *const ExtendedLimitInformation).cast(),
                        std::mem::size_of::<ExtendedLimitInformation>() as u32,
                    );
                }
                let job = if job.is_null() {
                    None
                } else if AssignProcessToJobObject(job, process) == 0 {
                    CloseHandle(job);
                    None
                } else {
                    Some(Job(job))
                };
                NtResumeProcess(process);
                job
            }
        }

        pub fn terminate(&self) {
            // SAFETY: self.0 is a valid job handle until drop.
            unsafe {
                TerminateJobObject(self.0, 1);
            }
        }
    }

    impl Drop for Job {
        fn drop(&mut self) {
            // SAFETY: closes the handle this Job owns, once.
            unsafe {
                CloseHandle(self.0);
            }
        }
    }
}

/// First match for `name` on PATH (with PATHEXT on Windows), skipping any path for
/// which `skip` is true.
pub fn find_program(name: &str, skip: impl Fn(&Path) -> bool) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    let exts: Vec<String> = if cfg!(windows) {
        let pathext = std::env::var("PATHEXT").unwrap_or_else(|_| ".EXE;.CMD;.BAT".into());
        let mut v: Vec<String> = pathext
            .split(';')
            .filter(|e| !e.is_empty())
            .map(|e| e.to_ascii_lowercase())
            .collect();
        if Path::new(name).extension().is_some() {
            v.insert(0, String::new());
        }
        v
    } else {
        vec![String::new()]
    };
    for dir in std::env::split_paths(&path) {
        for ext in &exts {
            let cand = dir.join(format!("{name}{ext}"));
            if cand.is_file() && !skip(&cand) {
                return Some(cand);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// S10: closing the job (the server dropping it, or dying) kills what runs in it.
    #[cfg(windows)]
    #[tokio::test]
    async fn closing_the_job_kills_the_tree() {
        let mut cmd = Command::new("ping");
        cmd.args(["-n", "60", "127.0.0.1"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(winjob::CREATE_NO_WINDOW | winjob::CREATE_SUSPENDED);
        let mut child = cmd.spawn().unwrap();
        let tree = Tree::attach(&child);
        assert!(tree.job.is_some(), "job created and assigned");
        drop(tree);
        // ping alone would run for about 60 s; the cap only bounds a failing test.
        let waited = tokio::time::timeout(Duration::from_secs(20), child.wait()).await;
        // Kill-on-close reports exit code 0, so the proof is that it ended long before 60 s.
        waited.expect("the job's close killed the process").unwrap();
    }

    #[test]
    fn tap_keeps_head_and_tail() {
        let mut t = Tap::new(4, None);
        t.push(b"abcdefghij");
        assert_eq!(t.total(), 10);
        assert!(t.cut());
        assert_eq!(t.parts(), ("abcd".to_string(), "ghij".to_string()));
        let mut t = Tap::new(4, None);
        t.push(b"abc");
        t.push(b"de");
        assert!(!t.cut());
        assert_eq!(t.text(), "abcde");
    }
}
