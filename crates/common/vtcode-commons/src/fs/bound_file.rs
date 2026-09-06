//! Read-only files opened beneath a trusted root without following links.
#![allow(
    unsafe_code,
    reason = "The no-follow openat primitive binds reads to a validated directory descriptor."
)]

use std::fs::File;
use std::io;
use std::path::Path;

#[cfg(unix)]
use std::io::Write;
#[cfg(unix)]
use std::os::fd::{AsRawFd, FromRawFd};

/// Open an absolute directory as a live handle for operations that must keep
/// using the same directory even if an attacker renames a path component.
#[cfg(unix)]
pub fn open_directory_handle(path: &Path) -> io::Result<File> {
    open_directory_beneath(path)
}

#[cfg(not(unix))]
pub fn open_directory_handle(_path: &Path) -> io::Result<File> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "handle-bound directory operations are unavailable on this platform",
    ))
}

/// Make a child process start with its working directory bound to `directory`.
/// The descriptor remains open through `exec`, so the `fchdir` runs before the
/// close-on-exec flag can take effect and avoids a path-based cwd race.
#[cfg(unix)]
pub fn set_command_working_directory(command: &mut std::process::Command, directory: &File) -> io::Result<()> {
    use std::os::unix::process::CommandExt;

    let descriptor = directory.as_raw_fd();
    let change_directory = move || {
        // SAFETY: `descriptor` is an open directory descriptor inherited by
        // the child, and `fchdir` does not access Rust-managed memory.
        let result = unsafe { libc::fchdir(descriptor) };
        if result == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    };
    // SAFETY: `pre_exec` is used only to change the child cwd to a descriptor
    // owned by the parent. The closure performs no allocation or locking.
    unsafe {
        command.pre_exec(change_directory);
    }
    Ok(())
}

#[cfg(not(unix))]
pub fn set_command_working_directory(_command: &mut std::process::Command, _directory: &File) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "handle-bound process directories are unavailable on this platform",
    ))
}

/// Open a regular, single-link file through bound directory handles.
/// `root` must be an absolute, previously resolved trusted root.
#[cfg(unix)]
pub fn open_file_beneath(root: &Path, relative: &Path) -> io::Result<File> {
    use std::ffi::CString;
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    use std::path::Component;

    let invalid =
        || io::Error::new(io::ErrorKind::InvalidInput, "expected an absolute root and a normal relative file path");
    if !root.is_absolute() || relative.as_os_str().is_empty() || relative.is_absolute() {
        return Err(invalid());
    }
    let mut directory = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open("/")?;
    let mut names = Vec::new();
    for component in root.components() {
        match component {
            Component::RootDir => {}
            Component::Normal(name) => names.push(name),
            _ => return Err(invalid()),
        }
    }
    for component in relative.components() {
        match component {
            Component::Normal(name) => names.push(name),
            _ => return Err(invalid()),
        }
    }
    let count = names.len();
    for (index, name) in names.into_iter().enumerate() {
        let name = CString::new(name.as_bytes()).map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
        let last = index + 1 == count;
        let flags = libc::O_RDONLY
            | libc::O_NOFOLLOW
            | libc::O_CLOEXEC
            | if last { libc::O_NONBLOCK } else { libc::O_DIRECTORY };
        // SAFETY: directory owns a live descriptor; name is NUL-terminated and
        // flags never create a file, so openat requires no mode argument.
        let descriptor = unsafe { libc::openat(directory.as_raw_fd(), name.as_ptr(), flags) };
        if descriptor < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: openat returned a new, uniquely owned descriptor above.
        directory = unsafe { File::from_raw_fd(descriptor) };
    }
    let metadata = directory.metadata()?;
    if !metadata.is_file() || metadata.nlink() != 1 {
        return Err(io::Error::new(io::ErrorKind::PermissionDenied, "expected a regular single-link file"));
    }
    Ok(directory)
}

/// Ensure a directory exists beneath a trusted root without following
/// symlinks in any component.
///
/// Missing components are created with private Unix permissions. The root
/// must already exist and be an absolute path; `relative` must contain only
/// normal, relative components.
#[cfg(unix)]
pub fn ensure_directory_beneath(root: &Path, relative: &Path) -> io::Result<()> {
    let mut directory = open_directory_beneath(root)?;
    for component in normal_components(relative, false)? {
        directory = open_or_create_directory_at(&directory, &component)?;
    }
    Ok(())
}

/// Validate an existing directory beneath a trusted root without following
/// symlinks in any component.
#[cfg(unix)]
pub fn validate_directory_beneath(root: &Path, relative: &Path) -> io::Result<()> {
    let mut directory = open_directory_beneath(root)?;
    for component in normal_components(relative, false)? {
        directory = open_directory_at(&directory, &component)?;
    }
    Ok(())
}

/// Write a new file beneath a trusted root while directory and file handles
/// remain bound to that root. Existing files (including symlinks) are never
/// replaced.
#[cfg(unix)]
pub fn write_file_beneath(root: &Path, relative: &Path, contents: &[u8]) -> io::Result<()> {
    let mut file = open_new_file_beneath(root, relative)?;
    file.write_all(contents)?;
    file.sync_all()
}

/// Copy a regular file between two trusted roots without resolving a path
/// after its parent has been validated. The destination must not already exist.
#[cfg(unix)]
pub fn copy_file_beneath(
    source_root: &Path,
    source_relative: &Path,
    destination_root: &Path,
    destination_relative: &Path,
) -> io::Result<()> {
    let mut source = open_file_beneath(source_root, source_relative)?;
    let mut destination = open_new_file_beneath(destination_root, destination_relative)?;
    io::copy(&mut source, &mut destination)?;
    destination.sync_all()
}

/// Create a symlink below a trusted root without following or replacing any
/// parent component. The target is stored verbatim and is never resolved.
#[cfg(unix)]
pub fn create_symlink_beneath(root: &Path, relative: &Path, target: &Path) -> io::Result<()> {
    use std::os::unix::ffi::OsStrExt;

    let components = normal_components(relative, false)?;
    let (file_name, parent_components) = components
        .split_last()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "expected a relative symlink path"))?;
    let mut directory = open_directory_beneath(root)?;
    for component in parent_components {
        directory = open_or_create_directory_at(&directory, component)?;
    }

    let name = c_string(file_name)?;
    let target = std::ffi::CString::new(target.as_os_str().as_bytes())
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
    // SAFETY: `directory` owns a live directory descriptor, both strings are
    // NUL-terminated, and symlinkat creates only the named child entry.
    let result = unsafe { libc::symlinkat(target.as_ptr(), directory.as_raw_fd(), name.as_ptr()) };
    if result < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Open an advisory lock file beneath a trusted root without following
/// symlinks. The caller owns the returned file and can hold an exclusive lock
/// with `fs2::FileExt` for the duration of a compound filesystem operation.
#[cfg(unix)]
pub fn open_lock_file_beneath(root: &Path, relative: &Path) -> io::Result<File> {
    use std::os::unix::fs::MetadataExt;

    let components = normal_components(relative, false)?;
    let (file_name, parent_components) = components
        .split_last()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "expected a relative lock file path"))?;
    let mut directory = open_directory_beneath(root)?;
    for component in parent_components {
        directory = open_directory_at(&directory, component)?;
    }

    let name = c_string(file_name)?;
    // SAFETY: `directory` owns a live directory descriptor, `name` is
    // NUL-terminated, and O_NOFOLLOW prevents replacing the lock with a
    // symlink while it is opened.
    let descriptor = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDWR | libc::O_CREAT | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            0o600,
        )
    };
    if descriptor < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: openat returned a new, uniquely owned descriptor above.
    let file = unsafe { File::from_raw_fd(descriptor) };
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.nlink() != 1 {
        return Err(io::Error::new(io::ErrorKind::PermissionDenied, "expected a regular single-link lock file"));
    }
    Ok(file)
}

#[cfg(not(unix))]
pub fn ensure_directory_beneath(root: &Path, relative: &Path) -> io::Result<()> {
    let components = normal_components(relative, false)?;
    let mut current = root.to_path_buf();
    let root_metadata = std::fs::symlink_metadata(&current)?;
    if root_metadata.file_type().is_symlink() || !root_metadata.is_dir() {
        return Err(io::Error::other("trusted root must be a regular directory"));
    }
    for component in components {
        current.push(component);
        match std::fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(io::Error::other(format!("refusing symlink directory {}", current.display())));
            }
            Ok(metadata) if !metadata.is_dir() => {
                return Err(io::Error::other(format!("{} is not a directory", current.display())));
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                std::fs::create_dir(&current)?;
            }
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

#[cfg(not(unix))]
pub fn validate_directory_beneath(root: &Path, relative: &Path) -> io::Result<()> {
    let components = normal_components(relative, false)?;
    let mut current = root.to_path_buf();
    let root_metadata = std::fs::symlink_metadata(&current)?;
    if root_metadata.file_type().is_symlink() || !root_metadata.is_dir() {
        return Err(io::Error::other("trusted root must be a regular directory"));
    }
    for component in components {
        current.push(component);
        let metadata = std::fs::symlink_metadata(&current)?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(io::Error::other(format!("{} is not a regular directory", current.display())));
        }
    }
    Ok(())
}

#[cfg(not(unix))]
pub fn write_file_beneath(root: &Path, relative: &Path, contents: &[u8]) -> io::Result<()> {
    let _ = (root, relative, contents);
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "handle-bound file writes are unavailable on this platform",
    ))
}

#[cfg(not(unix))]
pub fn copy_file_beneath(
    _source_root: &Path,
    _source_relative: &Path,
    _destination_root: &Path,
    _destination_relative: &Path,
) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "handle-bound file copies are unavailable on this platform",
    ))
}

#[cfg(not(unix))]
pub fn create_symlink_beneath(_root: &Path, _relative: &Path, _target: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "handle-bound symlink creation is unavailable on this platform",
    ))
}

/// Advisory lock files are only used for local coordination. The path checks
/// below still reject symlinked parents before opening the handle.
#[cfg(not(unix))]
pub fn open_lock_file_beneath(root: &Path, relative: &Path) -> io::Result<File> {
    use std::fs::OpenOptions;

    let components = normal_components(relative, false)?;
    let (file_name, parent_components) = components
        .split_last()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "expected a relative lock file path"))?;
    let parent = parent_components.iter().fold(root.to_path_buf(), |mut path, component| {
        path.push(component);
        path
    });
    let relative_parent = parent
        .strip_prefix(root)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "bound path escaped trusted root"))?;
    ensure_directory_beneath(root, relative_parent)?;
    let path = parent.join(file_name);
    if let Ok(metadata) = std::fs::symlink_metadata(&path)
        && (metadata.file_type().is_symlink() || !metadata.is_file())
    {
        return Err(io::Error::new(io::ErrorKind::PermissionDenied, "expected a regular lock file"));
    }
    let file = OpenOptions::new().read(true).write(true).create(true).open(&path)?;
    let metadata = std::fs::symlink_metadata(&path)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(io::Error::new(io::ErrorKind::PermissionDenied, "expected a regular lock file"));
    }
    Ok(file)
}

#[cfg(unix)]
fn open_directory_beneath(root: &Path) -> io::Result<File> {
    let mut directory = open_directory_at_path(Path::new("/"))?;
    for component in normal_components(root, true)? {
        directory = open_directory_at(&directory, &component)?;
    }
    Ok(directory)
}

#[cfg(unix)]
fn open_new_file_beneath(root: &Path, relative: &Path) -> io::Result<File> {
    let components = normal_components(relative, false)?;
    let (file_name, parent_components) = components
        .split_last()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "expected a relative file path"))?;
    let mut directory = open_directory_beneath(root)?;
    for component in parent_components {
        directory = open_or_create_directory_at(&directory, component)?;
    }

    let name = c_string(file_name)?;
    let flags = libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC;
    // SAFETY: `directory` owns a live directory descriptor, `name` is
    // NUL-terminated, and the mode is supplied because O_CREAT is set.
    let descriptor = unsafe { libc::openat(directory.as_raw_fd(), name.as_ptr(), flags, 0o600) };
    if descriptor < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: openat returned a new, uniquely owned descriptor above.
    Ok(unsafe { File::from_raw_fd(descriptor) })
}

#[cfg(unix)]
fn open_directory_at_path(path: &Path) -> io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;

    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
}

#[cfg(unix)]
fn open_directory_at(parent: &File, component: &std::ffi::OsString) -> io::Result<File> {
    let name = c_string(component)?;
    // SAFETY: `parent` owns a live directory descriptor and `name` is
    // NUL-terminated. No symlink is followed while resolving the component.
    let descriptor = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if descriptor < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: openat returned a new, uniquely owned descriptor above.
    Ok(unsafe { File::from_raw_fd(descriptor) })
}

#[cfg(unix)]
fn open_or_create_directory_at(parent: &File, component: &std::ffi::OsString) -> io::Result<File> {
    match open_directory_at(parent, component) {
        Ok(directory) => Ok(directory),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let name = c_string(component)?;
            // SAFETY: `parent` owns a live directory descriptor and `name` is
            // NUL-terminated. mkdirat creates only this child entry.
            let result = unsafe { libc::mkdirat(parent.as_raw_fd(), name.as_ptr(), 0o700) };
            if result < 0 {
                let mkdir_error = io::Error::last_os_error();
                if mkdir_error.kind() != io::ErrorKind::AlreadyExists {
                    return Err(mkdir_error);
                }
            }
            open_directory_at(parent, component)
        }
        Err(error) => Err(error),
    }
}

fn normal_components(path: &Path, absolute: bool) -> io::Result<Vec<std::ffi::OsString>> {
    use std::path::Component;

    if absolute != path.is_absolute() || (!absolute && path.as_os_str().is_empty()) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "invalid bound path"));
    }
    path.components()
        .filter_map(|component| match component {
            Component::Prefix(_) if absolute => None,
            Component::RootDir if absolute => None,
            Component::Normal(name) => Some(Ok(name.to_os_string())),
            _ => Some(Err(io::Error::new(io::ErrorKind::InvalidInput, "bound path contains traversal"))),
        })
        .collect()
}

#[cfg(unix)]
fn c_string(name: &std::ffi::OsString) -> io::Result<std::ffi::CString> {
    use std::os::unix::ffi::OsStrExt;

    std::ffi::CString::new(name.as_os_str().as_bytes())
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))
}

#[cfg(not(unix))]
pub fn open_file_beneath(_root: &Path, _relative: &Path) -> io::Result<File> {
    Err(io::Error::new(io::ErrorKind::Unsupported, "bound no-follow reads are unavailable on this platform"))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::io::Read;
    use std::os::unix::fs::symlink;

    #[test]
    fn bound_read_rejects_parent_final_links_and_traversal() {
        let temp = tempfile::tempdir().unwrap();
        let root = crate::canonicalize(temp.path()).unwrap();
        std::fs::create_dir(root.join("real")).unwrap();
        std::fs::write(root.join("real/output"), "retained output").unwrap();
        symlink(root.join("real"), root.join("alias")).unwrap();
        symlink(root.join("real/output"), root.join("link")).unwrap();
        assert!(open_file_beneath(&root, Path::new("alias/output")).is_err());
        assert!(open_file_beneath(&root, Path::new("link")).is_err());
        assert!(open_file_beneath(&root, Path::new("../outside")).is_err());
        assert!(open_file_beneath(&root, Path::new("real")).is_err());
        let mut opened = open_file_beneath(&root, Path::new("real/output")).unwrap();
        std::fs::rename(root.join("real"), root.join("retained")).unwrap();
        symlink("/", root.join("real")).unwrap();
        let mut text = String::new();
        opened.read_to_string(&mut text).unwrap();
        assert_eq!(text, "retained output");
    }
}
