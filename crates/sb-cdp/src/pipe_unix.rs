//! Unix launch for [`super::Pipe`] (D-600): the pipe ends are moved above fd 10 with
//! `F_DUPFD_CLOEXEC` (so they cannot collide with 3 and 4), then `dup2`'d onto fds 3 and 4
//! in the child, which runs in its own process group so `kill(-pgid)` reaches every helper.

use std::ffi::OsString;
use std::io;
use std::io::{PipeReader, PipeWriter};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::Duration;

pub(crate) struct Proc {
    child: Mutex<Option<Child>>,
    pid: u32,
}

fn dup_high(fd: RawFd) -> io::Result<OwnedFd> {
    // SAFETY: fcntl on a valid fd; the result is a fresh fd we own.
    let n = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 10) };
    if n < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `n` is a new, open fd owned by nobody else.
    Ok(unsafe { OwnedFd::from_raw_fd(n) })
}

pub(crate) fn spawn(
    program: &Path,
    args: &[OsString],
    child_in: PipeReader,
    child_out: PipeWriter,
) -> io::Result<Proc> {
    let r = dup_high(child_in.as_raw_fd())?;
    let w = dup_high(child_out.as_raw_fd())?;
    drop((child_in, child_out));
    let (rfd, wfd) = (r.as_raw_fd(), w.as_raw_fd());
    let mut cmd = Command::new(program);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0);
    // SAFETY: only async-signal-safe calls (dup2) between fork and exec. dup2 clears
    // FD_CLOEXEC on 3 and 4; the high copies close at exec.
    unsafe {
        cmd.pre_exec(move || {
            if libc::dup2(rfd, 3) < 0 || libc::dup2(wfd, 4) < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let child = cmd.spawn()?;
    drop((r, w));
    Ok(Proc {
        pid: child.id(),
        child: Mutex::new(Some(child)),
    })
}

fn group_alive(pgid: u32) -> bool {
    let Ok(pgid) = i32::try_from(pgid) else {
        return false;
    };
    // SAFETY: signal 0 only checks for existence.
    let r = unsafe { libc::kill(-pgid, 0) };
    r == 0 || io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

impl Proc {
    pub(crate) fn pid(&self) -> u32 {
        self.pid
    }

    pub(crate) fn pids(&self) -> Vec<u32> {
        if group_alive(self.pid) {
            vec![self.pid]
        } else {
            Vec::new()
        }
    }

    /// SIGKILL the whole group, then wait for the leader with a cap. A helper thread does
    /// the blocking `wait` so the cap is a channel timeout, not a poll.
    pub(crate) fn kill(&self, cap: Duration) -> bool {
        if let Ok(pgid) = i32::try_from(self.pid) {
            // SAFETY: the group was created for this child (process_group(0)).
            unsafe { libc::kill(-pgid, libc::SIGKILL) };
        }
        let child = self.child.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(mut child) = child {
            let (tx, rx) = std::sync::mpsc::channel();
            let waiter = std::thread::Builder::new()
                .name("sb-cdp-reap".into())
                .spawn(move || {
                    let _ = child.wait();
                    let _ = tx.send(());
                });
            if waiter.is_err() || rx.recv_timeout(cap).is_err() {
                return false;
            }
        }
        !group_alive(self.pid)
    }
}

pub(crate) fn process_running(pid: u32) -> bool {
    let Ok(pid) = i32::try_from(pid) else {
        return false;
    };
    // SAFETY: signal 0 only checks for existence.
    let r = unsafe { libc::kill(pid, 0) };
    r == 0 || io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}
