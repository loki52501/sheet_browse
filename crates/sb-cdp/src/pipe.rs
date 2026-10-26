//! `--remote-debugging-pipe` transport (B-3, D-600). Chromium reads NUL-terminated JSON on
//! fd 3 and writes it on fd 4. Anonymous pipes have no async I/O on Windows, so one blocking
//! OS thread per direction bridges the pipe to tokio channels.

use crate::{Transport, TransportError};
use async_trait::async_trait;
use bytes::Bytes;
use std::ffi::OsString;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;
use tokio::sync::mpsc;

#[cfg(windows)]
#[path = "pipe_windows.rs"]
mod sys;

#[cfg(unix)]
#[path = "pipe_unix.rs"]
mod sys;

/// How long dropping a [`Pipe`] waits for the browser's processes to exit. Teardown is
/// usually well under a second, but on a loaded Windows machine a terminated Chromium was
/// measured taking 8 to 20 s to be reaped; the profile directory cannot be removed before.
pub const DROP_KILL_CAP: Duration = Duration::from_secs(20);

const OUTBOUND_QUEUE: usize = 256;
const INBOUND_QUEUE: usize = 1024;

/// A Chromium launched with `--remote-debugging-pipe`, in its own Job object (Windows) or
/// process group (Unix). Dropping it kills every process of the browser.
pub struct Pipe {
    tx: mpsc::Sender<Bytes>,
    inbound: Mutex<Option<mpsc::Receiver<Bytes>>>,
    proc: sys::Proc,
}

impl Pipe {
    /// Spawn `program` with `args` (which must include `--remote-debugging-pipe`), fd 3 and
    /// fd 4 wired to this transport, stdin/stdout/stderr on the null device.
    pub fn launch(program: &Path, args: &[OsString]) -> Result<Pipe, TransportError> {
        // Chromium reads `child_in` (fd 3) and writes `child_out` (fd 4).
        let (child_in, ours_out) = std::io::pipe()?;
        let (ours_in, child_out) = std::io::pipe()?;
        // Takes the child ends and closes the parent's copies, so EOF follows the browser.
        let proc = sys::spawn(program, args, child_in, child_out)?;
        let (tx, out_rx) = mpsc::channel::<Bytes>(OUTBOUND_QUEUE);
        let (in_tx, in_rx) = mpsc::channel::<Bytes>(INBOUND_QUEUE);
        std::thread::Builder::new()
            .name("sb-cdp-pipe-write".into())
            .spawn(move || write_thread(ours_out, out_rx))?;
        std::thread::Builder::new()
            .name("sb-cdp-pipe-read".into())
            .spawn(move || read_thread(ours_in, in_tx))?;
        Ok(Pipe {
            tx,
            inbound: Mutex::new(Some(in_rx)),
            proc,
        })
    }

    /// The browser process id.
    pub fn pid(&self) -> u32 {
        self.proc.pid()
    }

    /// Every live process of the browser (the Job's list on Windows; the leader on Unix).
    pub fn pids(&self) -> Vec<u32> {
        self.proc.pids()
    }

    /// Kill every process of the browser and wait, at most `cap`, for each to exit. True when
    /// all of them are gone. Blocking; safe to call more than once.
    pub fn kill(&self, cap: Duration) -> bool {
        self.proc.kill(cap)
    }
}

impl Drop for Pipe {
    fn drop(&mut self) {
        self.proc.kill(DROP_KILL_CAP);
    }
}

#[async_trait]
impl Transport for Pipe {
    async fn send(&self, msg: Bytes) -> Result<(), TransportError> {
        self.tx.send(msg).await.map_err(|_| TransportError::Closed)
    }

    fn recv(&self) -> mpsc::Receiver<Bytes> {
        self.inbound
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
            .unwrap_or_else(|| mpsc::channel(1).1)
    }
}

fn write_thread(mut w: impl Write, mut rx: mpsc::Receiver<Bytes>) {
    while let Some(msg) = rx.blocking_recv() {
        let mut framed = Vec::with_capacity(msg.len() + 1);
        framed.extend_from_slice(&msg);
        framed.push(0);
        if w.write_all(&framed).is_err() {
            return;
        }
    }
}

/// Splits the byte stream on NUL. Ends (closing the inbound channel, which closes the
/// client) on EOF, on a read error, or when the client is gone.
fn read_thread(r: impl Read, tx: mpsc::Sender<Bytes>) {
    let mut r = BufReader::with_capacity(1 << 16, r);
    let mut buf = Vec::new();
    loop {
        buf.clear();
        match r.read_until(0, &mut buf) {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
        if buf.last() == Some(&0) {
            buf.pop();
        } else {
            // EOF in the middle of a message: the browser died mid-write.
            return;
        }
        if buf.is_empty() {
            continue;
        }
        if tx
            .blocking_send(Bytes::from(std::mem::take(&mut buf)))
            .is_err()
        {
            return;
        }
    }
}

/// True while process `pid` exists (a diagnostic for tests: did the browser really exit).
pub fn process_running(pid: u32) -> bool {
    sys::process_running(pid)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_thread_splits_on_nul_and_ends_on_eof() {
        let (tx, mut rx) = mpsc::channel(8);
        let input: &[u8] = b"{\"a\":1}\0\0{\"b\":2}\0{\"partial\"";
        read_thread(input, tx);
        assert_eq!(rx.try_recv().unwrap(), Bytes::from_static(b"{\"a\":1}"));
        assert_eq!(rx.try_recv().unwrap(), Bytes::from_static(b"{\"b\":2}"));
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn write_thread_terminates_each_message_with_nul() {
        let (tx, rx) = mpsc::channel(8);
        tx.try_send(Bytes::from_static(b"{\"id\":1}")).unwrap();
        tx.try_send(Bytes::from_static(b"{\"id\":2}")).unwrap();
        drop(tx);
        let mut out = Vec::new();
        write_thread(&mut out, rx);
        assert_eq!(out, b"{\"id\":1}\0{\"id\":2}\0");
    }

    #[test]
    fn launch_of_a_missing_program_is_an_io_error() {
        let r = Pipe::launch(
            Path::new("Z:/definitely/not/here/chrome.exe"),
            &["--remote-debugging-pipe".into()],
        );
        assert!(matches!(r, Err(TransportError::Io(_))));
    }

    #[test]
    fn current_process_is_running_and_a_bogus_pid_is_not() {
        assert!(process_running(std::process::id()));
        assert!(!process_running(u32::MAX - 1));
    }
}
