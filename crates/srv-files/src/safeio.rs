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
//! which leaves a window of one syscall (recorded in D-549). Missing folders are made
//! one level at a time below a verified handle ([`create_dirs`], D-552), so a junction
//! swapped in mid-call cannot make a folder outside.

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
        let f = imp::open_existing(p, append)?;
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

/// [`check_entry`] for the folder that holds `p` (moves and new folders). A parent that
/// does not exist yet is not skipped: its nearest existing ancestor is checked, and with
/// none the call is refused (fail closed, D-552).
pub fn check_parent(p: &Path) -> io::Result<()> {
    let Some(d) = p.parent() else {
        return Err(no_folder(p));
    };
    let (have, _) = existing_ancestor(d)?;
    check_entry(&have)
}

/// False when the open file `f` has more than one hard link and the approved class did
/// not account for it (a link made after the check; B7, D-549). A link count that cannot
/// be read counts as linked (fail closed, D-553).
pub fn links_ok(f: &File, approved_links: bool) -> bool {
    approved_links || crate::paths::handle_link_count(f).is_some_and(|n| n <= 1)
}

fn no_folder(p: &Path) -> io::Error {
    io::Error::other(format!(
        "not done: no existing folder above {}",
        crate::paths::plain(p)
    ))
}

/// The nearest existing path at or above `dir`, and the names below it that are missing
/// (outermost first).
fn existing_ancestor(dir: &Path) -> io::Result<(PathBuf, Vec<std::ffi::OsString>)> {
    let mut have = dir.to_path_buf();
    let mut missing = Vec::new();
    while !crate::paths::exists(&have) {
        let (Some(parent), Some(name)) = (have.parent(), have.file_name()) else {
            return Err(no_folder(dir));
        };
        missing.push(name.to_os_string());
        have = parent.to_path_buf();
    }
    missing.reverse();
    Ok((have, missing))
}

/// Makes the folder `dir` (already resolved by [`crate::paths::resolve`]) and every
/// missing folder above it, like `create_dir_all`, but one level at a time (D-552): the
/// nearest existing ancestor is opened and checked to be the planned folder, then each
/// missing name is made below that open handle (NtCreateFile relative to it on Windows),
/// and the new folder's own handle is checked before the next level is made below it.
/// A junction swapped in for a folder on the way after the check cannot redirect a
/// level: the next one is made in the folder the handle holds, and when that folder was
/// moved away meanwhile the new one is removed again and the call is refused.
pub fn create_dirs(dir: &Path) -> io::Result<()> {
    let (mut have, missing) = existing_ancestor(dir)?;
    let mut f = imp::open_dir(&have)?;
    verify(&f, &have)?;
    if !f.metadata()?.is_dir() {
        return Err(io::Error::other(format!(
            "not done: {} is not a folder",
            crate::paths::plain(&have)
        )));
    }
    for name in &missing {
        let next = have.join(name);
        f = imp::create_dir_at(&f, name, &next)?;
        have = next;
    }
    Ok(())
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
    /// FILE_GENERIC_WRITE: what std asks for with `.write(true)`, spelled out.
    const FILE_GENERIC_WRITE: u32 = 0x0012_0116;
    const FILE_WRITE_DATA: u32 = 0x0002;
    const DELETE: u32 = 0x0001_0000;
    const SYNCHRONIZE: u32 = 0x0010_0000;
    const FILE_LIST_DIRECTORY: u32 = 0x0001;
    const FILE_ADD_SUBDIRECTORY: u32 = 0x0004;
    const FILE_TRAVERSE: u32 = 0x0020;
    const FILE_READ_ATTRIBUTES: u32 = 0x0080;
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    /// FILE_INFO_BY_HANDLE_CLASS::FileDispositionInfo.
    const FILE_DISPOSITION_INFO: i32 = 4;
    const FILE_ATTRIBUTE_NORMAL: u32 = 0x80;
    const FILE_SHARE_ALL: u32 = 0x1 | 0x2 | 0x4;
    /// NtCreateFile: CreateDisposition FILE_OPEN_IF, CreateOptions, OBJ_CASE_INSENSITIVE,
    /// and IO_STATUS_BLOCK.Information FILE_CREATED.
    const FILE_OPEN_IF: u32 = 3;
    const FILE_DIRECTORY_FILE: u32 = 0x0001;
    const FILE_SYNCHRONOUS_IO_NONALERT: u32 = 0x0020;
    const OBJ_CASE_INSENSITIVE: u32 = 0x40;
    const FILE_CREATED: usize = 2;
    const DIR_ACCESS: u32 = FILE_LIST_DIRECTORY
        | FILE_ADD_SUBDIRECTORY
        | FILE_TRAVERSE
        | FILE_READ_ATTRIBUTES
        | SYNCHRONIZE;

    #[repr(C)]
    struct UnicodeString {
        length: u16,
        maximum_length: u16,
        buffer: *mut u16,
    }

    #[repr(C)]
    struct ObjectAttributes {
        length: u32,
        root_directory: *mut std::ffi::c_void,
        object_name: *mut UnicodeString,
        attributes: u32,
        security_descriptor: *mut std::ffi::c_void,
        security_quality_of_service: *mut std::ffi::c_void,
    }

    #[repr(C)]
    struct IoStatusBlock {
        status: usize,
        information: usize,
    }

    #[link(name = "ntdll")]
    extern "system" {
        fn NtCreateFile(
            handle: *mut *mut std::ffi::c_void,
            access: u32,
            attributes: *mut ObjectAttributes,
            status: *mut IoStatusBlock,
            allocation_size: *mut i64,
            file_attributes: u32,
            share: u32,
            disposition: u32,
            options: u32,
            ea: *mut std::ffi::c_void,
            ea_len: u32,
        ) -> i32;
        fn RtlNtStatusToDosError(status: i32) -> u32;
    }

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

    /// An existing file opened to write (or append) with FILE_READ_ATTRIBUTES too, so its
    /// link count can be read on the handle (D-553). Append leaves out FILE_WRITE_DATA,
    /// as std does, so every write lands at the end.
    pub fn open_existing(p: &Path, append: bool) -> io::Result<File> {
        let write = if append {
            FILE_GENERIC_WRITE & !FILE_WRITE_DATA
        } else {
            FILE_GENERIC_WRITE
        };
        std::fs::OpenOptions::new()
            .write(!append)
            .append(append)
            .access_mode(write | FILE_READ_ATTRIBUTES)
            .open(p)
    }

    /// The existing folder `p` (links followed), with the rights to make folders in it.
    pub fn open_dir(p: &Path) -> io::Result<File> {
        std::fs::OpenOptions::new()
            .access_mode(DIR_ACCESS)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
            .open(p)
    }

    /// Makes (or opens) the folder `name` inside the folder `parent` holds, relative to
    /// that handle, so no path lookup happens above it; then checks the new handle is
    /// `planned`, removing a folder this call made when it is not.
    pub fn create_dir_at(
        parent: &File,
        name: &std::ffi::OsStr,
        planned: &Path,
    ) -> io::Result<File> {
        use std::os::windows::ffi::OsStrExt;
        use std::os::windows::io::FromRawHandle;
        let mut wide: Vec<u16> = name.encode_wide().collect();
        let bad_char = |c: &u16| *c == 0 || *c == u16::from(b'\\') || *c == u16::from(b'/');
        if wide.is_empty() || wide.len() > 255 || wide.iter().any(bad_char) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "not done: a folder name the classifier did not resolve",
            ));
        }
        let bytes = (wide.len() * 2) as u16;
        let mut uname = UnicodeString {
            length: bytes,
            maximum_length: bytes,
            buffer: wide.as_mut_ptr(),
        };
        let mut attrs = ObjectAttributes {
            length: std::mem::size_of::<ObjectAttributes>() as u32,
            root_directory: parent.as_raw_handle(),
            object_name: &mut uname,
            attributes: OBJ_CASE_INSENSITIVE,
            security_descriptor: std::ptr::null_mut(),
            security_quality_of_service: std::ptr::null_mut(),
        };
        let mut status = IoStatusBlock {
            status: 0,
            information: 0,
        };
        let mut h: *mut std::ffi::c_void = std::ptr::null_mut();
        // SAFETY: every pointer is to a live local of the documented layout; `wide` and
        // `uname` outlive the call; `parent` is a live folder handle owned by the caller.
        let st = unsafe {
            NtCreateFile(
                &mut h,
                DIR_ACCESS | DELETE,
                &mut attrs,
                &mut status,
                std::ptr::null_mut(),
                FILE_ATTRIBUTE_NORMAL,
                FILE_SHARE_ALL,
                FILE_OPEN_IF,
                FILE_DIRECTORY_FILE | FILE_SYNCHRONOUS_IO_NONALERT,
                std::ptr::null_mut(),
                0,
            )
        };
        if st < 0 {
            // SAFETY: a plain status-code conversion.
            let code = unsafe { RtlNtStatusToDosError(st) };
            return Err(io::Error::from_raw_os_error(code as i32));
        }
        // SAFETY: NtCreateFile succeeded, so `h` is a new handle this File now owns.
        let f = unsafe { File::from_raw_handle(h) };
        if let Err(e) = verify(&f, planned) {
            if status.information == FILE_CREATED {
                // Empty and made by this call: removed through its own handle.
                let _ = delete_by_handle(&f);
            }
            return Err(e);
        }
        Ok(f)
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

    pub fn open_existing(p: &Path, append: bool) -> io::Result<File> {
        std::fs::OpenOptions::new()
            .write(!append)
            .append(append)
            .open(p)
    }

    pub fn open_dir(p: &Path) -> io::Result<File> {
        open_entry(p)
    }

    /// Without mkdirat (no libc dependency here) the folder is made by path right after
    /// its parent was checked on a handle, then opened and checked itself; a window of
    /// one syscall per level remains off Windows (D-552).
    pub fn create_dir_at(
        _parent: &File,
        _name: &std::ffi::OsStr,
        planned: &Path,
    ) -> io::Result<File> {
        match std::fs::create_dir(planned) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e),
        }
        let f = open_entry(planned)?;
        verify(&f, planned)?;
        Ok(f)
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

    /// D-552 (review-T-14b should-fix 6): folders that do not exist yet below a folder
    /// swapped for a junction are not made in its target, at any depth; a missing parent
    /// is not skipped by check_parent.
    #[test]
    fn new_folders_through_a_swapped_folder_are_refused() {
        let Some((_t, planned, outside)) = swapped() else {
            return;
        };
        let d = planned.parent().unwrap().to_path_buf();
        for deep in [d.join("new"), d.join("new").join("a").join("b")] {
            let e = create_dirs(&deep).unwrap_err().to_string();
            assert!(e.contains("not done"), "{e}");
            assert!(check_parent(&deep.join("f.txt")).is_err());
        }
        assert!(create_dirs(&d).is_err());
        let names: Vec<_> = std::fs::read_dir(&outside)
            .unwrap()
            .flatten()
            .map(|e| e.file_name())
            .collect();
        assert_eq!(names, ["f.txt"]);
        // The swap two levels up is caught the same way.
        let x = d.parent().unwrap().to_path_buf();
        std::fs::remove_dir(&d).unwrap();
        std::fs::create_dir(&d).unwrap();
        let runs = x.parent().unwrap().to_path_buf();
        std::fs::rename(&x, runs.join("x-real")).unwrap();
        if !link(&outside, &x) {
            return;
        }
        assert!(create_dirs(&d.join("n2").join("a")).is_err());
        assert!(!outside.join("d").exists() && !outside.join("n2").exists());
        std::fs::remove_dir(&x).unwrap();
    }

    #[test]
    fn create_dirs_makes_every_missing_level_and_links_are_counted() {
        let t = tempfile::tempdir().unwrap();
        let base = std::fs::canonicalize(t.path()).unwrap();
        let p = crate::paths::resolve(&base, "a/b/c/d").unwrap();
        create_dirs(&p).unwrap();
        assert!(p.is_dir());
        create_dirs(&p).unwrap();
        check_parent(&p.join("x").join("y.txt")).unwrap();
        // A file in the way is not a folder.
        let file = base.join("f.txt");
        std::fs::write(&file, "x").unwrap();
        assert!(create_dirs(&file.join("sub")).is_err());
        assert!(create_dirs(&file).is_err());
        // The link count is read on the handle open_write returns (D-553), so failing
        // closed on an unreadable count does not refuse ordinary writes.
        for append in [false, true] {
            let f = open_write(&file, append).unwrap();
            assert_eq!(crate::paths::handle_link_count(&f), Some(1), "{append}");
            assert!(links_ok(&f, false));
        }
        if std::fs::hard_link(&file, base.join("g.txt")).is_ok() {
            let f = open_write(&file, false).unwrap();
            assert!(!links_ok(&f, false));
            assert!(links_ok(&f, true));
        }
    }
}
