//! Windows launch for [`super::Pipe`] (D-600). `std::process` cannot hand a child CRT fds 3
//! and 4, so this does what libuv and Playwright do: CreateProcessW with a CRT `lpReserved2`
//! block describing fds 0..=4, the inherited handles limited by
//! `PROC_THREAD_ATTRIBUTE_HANDLE_LIST`. The process starts suspended, joins a kill-on-close
//! Job object (so every renderer, GPU and crashpad child dies with it), then resumes.

use std::ffi::{c_void, OsStr, OsString};
use std::io;
use std::io::{PipeReader, PipeWriter};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::AsRawHandle;
use std::path::Path;
use std::ptr;
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::{
    CloseHandle, SetHandleInformation, HANDLE, HANDLE_FLAG_INHERIT, STILL_ACTIVE, WAIT_OBJECT_0,
};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JobObjectBasicProcessIdList,
    JobObjectExtendedLimitInformation, QueryInformationJobObject, SetInformationJobObject,
    TerminateJobObject, JOBOBJECT_BASIC_PROCESS_ID_LIST, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};
use windows_sys::Win32::System::Threading::{
    CreateProcessW, DeleteProcThreadAttributeList, GetExitCodeProcess,
    InitializeProcThreadAttributeList, OpenProcess, ResumeThread, TerminateProcess,
    UpdateProcThreadAttribute, WaitForSingleObject, CREATE_SUSPENDED, EXTENDED_STARTUPINFO_PRESENT,
    LPPROC_THREAD_ATTRIBUTE_LIST, PROCESS_INFORMATION, PROCESS_QUERY_LIMITED_INFORMATION,
    PROCESS_SYNCHRONIZE, PROC_THREAD_ATTRIBUTE_HANDLE_LIST, STARTF_USESTDHANDLES, STARTUPINFOEXW,
    STARTUPINFOW,
};

/// CRT `ioinfo` flags: FOPEN | FDEV for the null device, FOPEN | FPIPE for the pipes.
const CRT_DEV: u8 = 0x41;
const CRT_PIPE: u8 = 0x09;
/// Enough for any browser's process tree.
const MAX_JOB_PIDS: usize = 1024;

/// Owned process and job handles.
pub(crate) struct Proc {
    process: HANDLE,
    job: HANDLE,
    pid: u32,
}

// SAFETY: HANDLEs are process-wide kernel object references; every call made on them here
// is thread-safe in Win32.
unsafe impl Send for Proc {}
unsafe impl Sync for Proc {}

impl Drop for Proc {
    fn drop(&mut self) {
        // SAFETY: both handles are owned by this struct and closed exactly once. Closing the
        // kill-on-close job is the last-resort kill if `kill` timed out.
        unsafe {
            CloseHandle(self.job);
            CloseHandle(self.process);
        }
    }
}

fn check(ok: i32) -> io::Result<()> {
    if ok == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn wide(s: &OsStr) -> Vec<u16> {
    s.encode_wide().chain(Some(0)).collect()
}

/// MSVCRT argv quoting, as `std::process::Command` does it.
fn push_quoted(out: &mut Vec<u16>, arg: &OsStr) {
    let s: Vec<u16> = arg.encode_wide().collect();
    let needs = s.is_empty()
        || s.iter()
            .any(|&c| c == u16::from(b' ') || c == u16::from(b'\t') || c == u16::from(b'"'));
    if !needs {
        out.extend_from_slice(&s);
        return;
    }
    out.push(u16::from(b'"'));
    let mut backslashes = 0usize;
    for c in s {
        if c == u16::from(b'\\') {
            backslashes += 1;
        } else {
            if c == u16::from(b'"') {
                out.extend(std::iter::repeat_n(u16::from(b'\\'), backslashes + 1));
            }
            backslashes = 0;
        }
        out.push(c);
    }
    out.extend(std::iter::repeat_n(u16::from(b'\\'), backslashes));
    out.push(u16::from(b'"'));
}

fn command_line(program: &Path, args: &[OsString]) -> Vec<u16> {
    let mut cmd = Vec::new();
    push_quoted(&mut cmd, program.as_os_str());
    for a in args {
        cmd.push(u16::from(b' '));
        push_quoted(&mut cmd, a);
    }
    cmd.push(0);
    cmd
}

/// The CRT inheritance block: `int count; u8 flags[count]; HANDLE handles[count]`, packed.
fn crt_block(handles: &[(u8, HANDLE)]) -> Vec<u8> {
    let mut block = Vec::new();
    block.extend_from_slice(&(handles.len() as i32).to_le_bytes());
    for (flag, _) in handles {
        block.push(*flag);
    }
    for (_, h) in handles {
        block.extend_from_slice(&(*h as usize).to_ne_bytes());
    }
    block
}

/// A Win32 attribute list holding one `PROC_THREAD_ATTRIBUTE_HANDLE_LIST`.
struct AttrList {
    buf: Vec<usize>,
}

impl AttrList {
    fn new(handles: &[HANDLE]) -> io::Result<AttrList> {
        let mut size = 0usize;
        // SAFETY: a null list with a size pointer is the documented size query; it returns
        // FALSE with ERROR_INSUFFICIENT_BUFFER, which is expected.
        unsafe { InitializeProcThreadAttributeList(ptr::null_mut(), 1, 0, &mut size) };
        let mut list = AttrList {
            buf: vec![0usize; size.div_ceil(std::mem::size_of::<usize>())],
        };
        // SAFETY: the buffer is at least `size` bytes and usize-aligned.
        check(unsafe { InitializeProcThreadAttributeList(list.ptr(), 1, 0, &mut size) })?;
        // SAFETY: `handles` outlives CreateProcessW (the caller keeps it alive); the list
        // stores only the pointer.
        let ok = unsafe {
            UpdateProcThreadAttribute(
                list.ptr(),
                0,
                PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
                handles.as_ptr() as *const c_void,
                std::mem::size_of_val(handles),
                ptr::null_mut(),
                ptr::null(),
            )
        };
        if let Err(e) = check(ok) {
            // SAFETY: initialized above.
            unsafe { DeleteProcThreadAttributeList(list.ptr()) };
            list.buf.clear();
            return Err(e);
        }
        Ok(list)
    }

    fn ptr(&mut self) -> LPPROC_THREAD_ATTRIBUTE_LIST {
        self.buf.as_mut_ptr() as LPPROC_THREAD_ATTRIBUTE_LIST
    }
}

impl Drop for AttrList {
    fn drop(&mut self) {
        if !self.buf.is_empty() {
            // SAFETY: initialized in `new`, deleted once.
            unsafe { DeleteProcThreadAttributeList(self.ptr()) };
        }
    }
}

pub(crate) fn spawn(
    program: &Path,
    args: &[OsString],
    child_in: PipeReader,
    child_out: PipeWriter,
) -> io::Result<Proc> {
    let nul = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("NUL")?;
    let h_nul = nul.as_raw_handle() as HANDLE;
    let h_in = child_in.as_raw_handle() as HANDLE;
    let h_out = child_out.as_raw_handle() as HANDLE;
    let inherit = [h_nul, h_in, h_out];
    for h in inherit {
        // SAFETY: valid handles owned by the locals above. The handle list below keeps any
        // other concurrently spawned child from inheriting them through this process's spawn.
        check(unsafe { SetHandleInformation(h, HANDLE_FLAG_INHERIT, HANDLE_FLAG_INHERIT) })?;
    }
    let mut block = crt_block(&[
        (CRT_DEV, h_nul),
        (CRT_DEV, h_nul),
        (CRT_DEV, h_nul),
        (CRT_PIPE, h_in),
        (CRT_PIPE, h_out),
    ]);
    let mut attrs = AttrList::new(&inherit)?;
    let mut si = STARTUPINFOEXW::default();
    si.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
    si.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
    si.StartupInfo.hStdInput = h_nul;
    si.StartupInfo.hStdOutput = h_nul;
    si.StartupInfo.hStdError = h_nul;
    si.StartupInfo.cbReserved2 =
        u16::try_from(block.len()).map_err(|_| io::Error::other("CRT fd block too large"))?;
    si.StartupInfo.lpReserved2 = block.as_mut_ptr();
    si.lpAttributeList = attrs.ptr();
    let app = wide(program.as_os_str());
    let mut cmd = command_line(program, args);
    let mut pi = PROCESS_INFORMATION::default();
    // SAFETY: every pointer refers to a live local; `inherit`, `block` and `attrs` outlive
    // the call; `cmd` is mutable and NUL-terminated as CreateProcessW requires.
    let created = unsafe {
        CreateProcessW(
            app.as_ptr(),
            cmd.as_mut_ptr(),
            ptr::null(),
            ptr::null(),
            1,
            EXTENDED_STARTUPINFO_PRESENT | CREATE_SUSPENDED,
            ptr::null(),
            ptr::null(),
            &si as *const STARTUPINFOEXW as *const STARTUPINFOW,
            &mut pi,
        )
    };
    check(created)?;
    // The child holds its own copies now; ours close here so EOF tracks the browser.
    drop(attrs);
    drop((nul, child_in, child_out));
    match into_job(&pi) {
        Ok(job) => {
            // SAFETY: the thread handle is ours to close.
            unsafe { CloseHandle(pi.hThread) };
            Ok(Proc {
                process: pi.hProcess,
                job,
                pid: pi.dwProcessId,
            })
        }
        Err(e) => {
            // SAFETY: the suspended process never ran; terminate and release its handles.
            unsafe {
                TerminateProcess(pi.hProcess, 1);
                CloseHandle(pi.hThread);
                CloseHandle(pi.hProcess);
            }
            Err(e)
        }
    }
}

/// Create a kill-on-close job, put the suspended process in it, then let it run.
fn into_job(pi: &PROCESS_INFORMATION) -> io::Result<HANDLE> {
    // SAFETY: plain Win32 calls on handles we own; `info` is a live, correctly sized struct.
    unsafe {
        let job = CreateJobObjectW(ptr::null(), ptr::null());
        if job.is_null() {
            return Err(io::Error::last_os_error());
        }
        let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let r = check(SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            &info as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION as *const c_void,
            std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        ))
        .and_then(|()| check(AssignProcessToJobObject(job, pi.hProcess)))
        .and_then(|()| {
            if ResumeThread(pi.hThread) == u32::MAX {
                Err(io::Error::last_os_error())
            } else {
                Ok(())
            }
        });
        match r {
            Ok(()) => Ok(job),
            Err(e) => {
                CloseHandle(job);
                Err(e)
            }
        }
    }
}

impl Proc {
    pub(crate) fn pid(&self) -> u32 {
        self.pid
    }

    pub(crate) fn pids(&self) -> Vec<u32> {
        // Header (two u32) plus the id array, as usize slots so the struct is aligned.
        let mut buf = vec![0usize; 2 + MAX_JOB_PIDS];
        let len = std::mem::size_of_val(buf.as_slice()) as u32;
        let list = buf.as_mut_ptr() as *mut JOBOBJECT_BASIC_PROCESS_ID_LIST;
        // SAFETY: `buf` is large enough for the header plus MAX_JOB_PIDS ids and aligned
        // for the struct; we read only the count the kernel reports, bounded by capacity.
        unsafe {
            let ok = QueryInformationJobObject(
                self.job,
                JobObjectBasicProcessIdList,
                list as *mut c_void,
                len,
                ptr::null_mut(),
            );
            if ok == 0 {
                return Vec::new();
            }
            let n = ((*list).NumberOfProcessIdsInList as usize).min(MAX_JOB_PIDS);
            let ids = ptr::addr_of!((*list).ProcessIdList) as *const usize;
            (0..n).map(|i| *ids.add(i) as u32).collect()
        }
    }

    /// Terminate the job and wait, condition by condition (each process handle signalled),
    /// at most `cap` in total.
    pub(crate) fn kill(&self, cap: Duration) -> bool {
        let deadline = Instant::now() + cap;
        // SAFETY: the job handle is valid for our lifetime.
        unsafe { TerminateJobObject(self.job, 1) };
        if !wait_handle(self.process, deadline) {
            return false;
        }
        loop {
            let pids = self.pids();
            if pids.is_empty() {
                return true;
            }
            // SAFETY: as above; covers a child assigned while we were terminating.
            unsafe { TerminateJobObject(self.job, 1) };
            let mut opened = 0;
            for pid in pids {
                // SAFETY: OpenProcess returns null for a pid that is already gone.
                let h = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, pid) };
                if h.is_null() {
                    continue;
                }
                opened += 1;
                let exited = wait_handle(h, deadline);
                // SAFETY: opened above, closed once.
                unsafe { CloseHandle(h) };
                if !exited {
                    return false;
                }
            }
            if opened == 0 {
                // Listed but not openable: gone by now, or nothing we can wait on.
                return self.pids().is_empty();
            }
        }
    }
}

/// Block until `h` is signalled or `deadline` passes.
fn wait_handle(h: HANDLE, deadline: Instant) -> bool {
    let left = deadline.saturating_duration_since(Instant::now());
    let ms = u32::try_from(left.as_millis()).unwrap_or(u32::MAX - 1);
    // SAFETY: `h` is a valid waitable handle owned by the caller.
    unsafe { WaitForSingleObject(h, ms) == WAIT_OBJECT_0 }
}

pub(crate) fn process_running(pid: u32) -> bool {
    // SAFETY: OpenProcess returns null for a missing pid; the handle is closed once.
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if h.is_null() {
            return false;
        }
        let mut code = 0u32;
        let ok = GetExitCodeProcess(h, &mut code);
        CloseHandle(h);
        ok != 0 && code == STILL_ACTIVE as u32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[u16]) -> String {
        String::from_utf16_lossy(v)
    }

    #[test]
    fn command_line_quotes_like_msvcrt() {
        let cmd = command_line(
            Path::new(r"C:\Program Files\c\chrome.exe"),
            &[
                "--user-data-dir=F:\\a b\\".into(),
                "plain".into(),
                "say \"hi\"".into(),
                "".into(),
            ],
        );
        assert_eq!(
            s(&cmd[..cmd.len() - 1]),
            r#""C:\Program Files\c\chrome.exe" "--user-data-dir=F:\a b\\" plain "say \"hi\"" """#
        );
        assert_eq!(cmd.last(), Some(&0));
    }

    #[test]
    fn crt_block_layout_is_count_flags_handles() {
        let a = 0x10usize as HANDLE;
        let b = 0x20usize as HANDLE;
        let block = crt_block(&[(CRT_DEV, a), (CRT_PIPE, b)]);
        let w = std::mem::size_of::<usize>();
        assert_eq!(block.len(), 4 + 2 + 2 * w);
        assert_eq!(&block[..4], &2i32.to_le_bytes());
        assert_eq!(&block[4..6], &[0x41, 0x09]);
        assert_eq!(&block[6..6 + w], &0x10usize.to_ne_bytes());
        assert_eq!(&block[6 + w..], &0x20usize.to_ne_bytes());
    }
}
