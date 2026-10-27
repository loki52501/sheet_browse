//! Writes and deletes that act on the file that was classified (T-14b item 6, D-549).
//!
//! A call resolves its target once, every link and junction followed (D-520, D-572), and
//! rechecks the class before acting. Between that check and the syscall a component of
//! the path could still become a junction (another call extracts an archive or replaces
//! a folder). The functions here open first, then ask the OS which file the handle is
//! (GetFinalPathNameByHandleW on Windows, `/proc/self/fd` on Linux) and act only when it
//! is the planned path: a write goes through the verified handle, a new file created
//! somewhere else is removed through its own handle, a delete is done on the handle.
//! Folder removal and renames are checked on a handle just before the path-based call,
//! which leaves a window of one syscall (recorded in D-549).

use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};

/// Why an act was refused: the path now names another file than the classified one.
fn moved(p: &Path, now: Option<&Path>) -> io::Error {
    let now = now
        .map(crate::paths::plain)
        .unwrap_or_else(|| "a file it cannot name".into());
    io::Error::other(format!(
        "not done: {} now leads to {now} (a link or junction changed after the check); call it again",
        crate::paths::plain(p)
    ))
}

/// True when `a` and `b` name the same path (case-insensitively, verbatim prefix or not).
pub fn same_path(a: &Path, b: &Path) -> bool {
    crate::paths::lower_names(a) == crate::paths::lower_names(b)
}

/// The path the OS has for an open handle; `None` where it cannot say (not Windows or
/// Linux), in which case callers act on the path as before.
pub fn final_path(f: &File) -> Option<PathBuf> {
    imp::final_path(f)
}

/// Checks that `f` is the file at `planned`.
fn verify(f: &File, planned: &Path) -> io::Result<()> {
    match final_path(f) {
        Some(now) if same_path(&now, planned) => Ok(()),
        Some(now) => Err(moved(planned, Some(&now))),
        None if cfg!(any(windows, target_os = "linux")) => Err(moved(planned, None)),
        None => Ok(()),
    }
}

/// Opens `p` (already resolved by [`crate::paths::resolve`]) for writing, verified: an
/// existing file is opened without truncating, checked, then emptied unless `append`; a
/// missing one is created new (never replacing one that appeared meanwhile), checked,
/// and removed again through its own handle when it landed elsewhere.
pub fn open_write(p: &Path, append: bool) -> io::Result<File> {
    if crate::paths::exists(p) {
        let f = std::fs::OpenOptions::new()
            .write(!append)
            .append(append)
            .open(p)?;
        verify(&f, p)?;
        if !append {
            f.set_len(0)?;
        }
        return Ok(f);
    }
    let f = imp::create_new(p)?;
    if let Err(e) = verify(&f, p) {
        // Best effort: the empty file this call just made is removed by handle, so the
        // stray name cannot be a link to something else.
        let _ = imp::delete_by_handle(&f);
        return Err(e);
    }
    Ok(f)
}

/// Writes `bytes` to `p` through [`open_write`].
pub fn write(p: &Path, bytes: &[u8]) -> io::Result<()> {
    use std::io::Write;
    let mut f = open_write(p, false)?;
    f.write_all(bytes)?;
    f.flush()
}

/// Removes the file (or the link itself) at `p` through a verified handle.
pub fn remove_file(p: &Path) -> io::Result<()> {
    imp::remove_file(p)
}

/// Checks that the existing file or folder at `p` is still the one at `p` (opened with
/// every link followed, so a folder swapped for a link to elsewhere fails); the caller's
/// path-based syscall follows right after (D-549).
pub fn check_entry(p: &Path) -> io::Result<()> {
    let f = imp::open_entry(p)?;
    verify(&f, p)
}

/// [`check_entry`] for the folder that holds `p` (moves and new folders).
pub fn check_parent(p: &Path) -> io::Result<()> {
    match p.parent() {
        Some(d) if crate::paths::exists(d) => check_entry(d),
        _ => Ok(()),
    }
}

#[cfg(windows)]
mod imp {
    use super::{moved, verify};
    use std::fs::File;
    use std::io;
    use std::os::windows::ffi::OsStringExt;
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::AsRawHandle;
    use std::path::{Path, PathBuf};

    const GENERIC_WRITE: u32 = 0x4000_0000;
    const DELETE: u32 = 0x0001_0000;
    const FILE_READ_ATTRIBUTES: u32 = 0x0080;
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    /// FILE_INFO_BY_HANDLE_CLASS::FileDispositionInfo.
    const FILE_DISPOSITION_INFO: i32 = 4;

    #[link(name = "kernel32")]
    extern "system" {
        fn GetFinalPathNameByHandleW(
            file: *mut std::ffi::c_void,
            path: *mut u16,
            len: u32,
            flags: u32,
        ) -> u32;
        fn SetFileInformationByHandle(
            file: *mut std::ffi::c_void,
            class: i32,
            info: *mut std::ffi::c_void,
            size: u32,
        ) -> i32;
    }

    pub fn final_path(f: &File) -> Option<PathBuf> {
        let h = f.as_raw_handle();
        let mut buf = vec![0u16; 512];
        loop {
            // SAFETY: a live handle owned by `f` and a buffer of the length passed.
            let n = unsafe { GetFinalPathNameByHandleW(h, buf.as_mut_ptr(), buf.len() as u32, 0) }
                as usize;
            if n == 0 {
                return None;
            }
            if n < buf.len() {
                buf.truncate(n);
                return Some(PathBuf::from(std::ffi::OsString::from_wide(&buf)));
            }
            if n > 32_768 {
                return None;
            }
            buf = vec![0u16; n + 1];
        }
    }

    pub fn create_new(p: &Path) -> io::Result<File> {
        // DELETE so a file created in the wrong place can be removed by this handle.
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .access_mode(GENERIC_WRITE | DELETE | FILE_READ_ATTRIBUTES)
            .open(p)
    }

    pub fn delete_by_handle(f: &File) -> io::Result<()> {
        let mut delete: u8 = 1;
        // SAFETY: a live handle owned by `f`; FILE_DISPOSITION_INFO is one BOOLEAN.
        let ok = unsafe {
            SetFileInformationByHandle(
                f.as_raw_handle() as *mut _,
                FILE_DISPOSITION_INFO,
                (&mut delete as *mut u8).cast(),
                1,
            )
        };
        if ok == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    /// The file or folder `p` leads to (links followed; folders need backup semantics).
    pub fn open_entry(p: &Path) -> io::Result<File> {
        std::fs::OpenOptions::new()
            .access_mode(FILE_READ_ATTRIBUTES)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
            .open(p)
    }

    /// The name itself is opened (a link in the last component is removed, not its
    /// target, as DeleteFileW does); a link in an earlier component is followed, so a
    /// swapped folder shows in the final path.
    pub fn remove_file(p: &Path) -> io::Result<()> {
        let f = std::fs::OpenOptions::new()
            .access_mode(DELETE | FILE_READ_ATTRIBUTES)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS)
            .open(p)?;
        verify(&f, p)?;
        if f.metadata()?.is_dir() {
            return Err(moved(p, final_path(&f).as_deref()));
        }
        delete_by_handle(&f)
    }
}

#[cfg(not(windows))]
mod imp {
    use super::verify;
    use std::fs::File;
    use std::io;
    use std::path::{Path, PathBuf};

    pub fn final_path(f: &File) -> Option<PathBuf> {
        #[cfg(target_os = "linux")]
        {
            use std::os::unix::io::AsRawFd;
            std::fs::read_link(format!("/proc/self/fd/{}", f.as_raw_fd())).ok()
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = f;
            None
        }
    }

    pub fn create_new(p: &Path) -> io::Result<File> {
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(p)
    }

    pub fn delete_by_handle(f: &File) -> io::Result<()> {
        match final_path(f) {
            Some(now) => std::fs::remove_file(now),
            None => Ok(()),
        }
    }

    pub fn open_entry(p: &Path) -> io::Result<File> {
        use std::os::unix::fs::OpenOptionsExt;
        // O_PATH where it exists (a folder opens without read rights); links followed.
        #[cfg(target_os = "linux")]
        let flags = 0o10000000;
        #[cfg(not(target_os = "linux"))]
        let flags = 0;
        std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(flags)
            .open(p)
    }

    pub fn remove_file(p: &Path) -> io::Result<()> {
        // Unix has no delete-by-handle: the folder that holds the name is checked, then
        // the name is unlinked (a link in the last component is removed, not followed).
        if let Some(d) = p.parent() {
            let f = open_entry(d)?;
            verify(&f, d)?;
        }
        std::fs::remove_file(p)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn link(target: &Path, link: &Path) -> bool {
        #[cfg(windows)]
        {
            std::process::Command::new("cmd")
                .args(["/C", "mklink", "/J"])
                .arg(link.to_string_lossy().replace('/', "\\"))
                .arg(target.to_string_lossy().replace('/', "\\"))
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false)
        }
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(target, link).is_ok()
        }
    }

    /// A folder `d` resolved (as a plan would have it), then swapped for a junction to
    /// `outside` before the act.
    fn swapped() -> Option<(tempfile::TempDir, PathBuf, PathBuf)> {
        let t = tempfile::tempdir().unwrap();
        let base = std::fs::canonicalize(t.path()).unwrap();
        let d = base.join("runs").join("x").join("d");
        let outside = base.join("precious");
        std::fs::create_dir_all(&d).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("f.txt"), "keep").unwrap();
        std::fs::write(d.join("f.txt"), "scratch").unwrap();
        let planned = crate::paths::resolve(&base, "runs/x/d/f.txt").unwrap();
        std::fs::remove_dir_all(&d).unwrap();
        if !link(&outside, &d) {
            eprintln!("skipped: cannot create a junction or symlink here");
            return None;
        }
        Some((t, planned, outside))
    }

    #[test]
    fn write_through_a_swapped_folder_is_refused() {
        let Some((_t, planned, outside)) = swapped() else {
            return;
        };
        let e = write(&planned, b"overwrite").unwrap_err().to_string();
        assert!(e.contains("not done"), "{e}");
        assert_eq!(
            std::fs::read_to_string(outside.join("f.txt")).unwrap(),
            "keep"
        );
        // A new name through the swapped folder is not left behind outside.
        let new = planned.with_file_name("new.txt");
        assert!(write(&new, b"x").is_err());
        assert!(!outside.join("new.txt").exists());
        // Append is refused the same way.
        assert!(open_write(&planned, true).is_err());
        assert_eq!(
            std::fs::read_to_string(outside.join("f.txt")).unwrap(),
            "keep"
        );
    }

    #[test]
    fn delete_through_a_swapped_folder_is_refused() {
        let Some((_t, planned, outside)) = swapped() else {
            return;
        };
        assert!(remove_file(&planned).is_err());
        assert!(outside.join("f.txt").exists());
        assert!(check_entry(&planned).is_err());
        assert!(check_parent(&planned).is_err());
    }

    #[test]
    fn plain_writes_and_deletes_still_work() {
        let t = tempfile::tempdir().unwrap();
        let base = std::fs::canonicalize(t.path()).unwrap();
        let p = crate::paths::resolve(&base, "a/b.txt").unwrap();
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        write(&p, b"one two").unwrap();
        write(&p, b"three").unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "three");
        {
            use std::io::Write;
            let mut f = open_write(&p, true).unwrap();
            f.write_all(b"+").unwrap();
        }
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "three+");
        check_entry(&p).unwrap();
        check_parent(&p).unwrap();
        remove_file(&p).unwrap();
        assert!(!p.exists());
    }
}
