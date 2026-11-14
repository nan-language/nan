//! What the bootstrap does differently on the systems it runs on, Linux and Windows: the running
//! user and home directory, the owners and modes of the user directory's files, the sync of a
//! directory's entries, handing this process over to a program, and the signal ending a process.

use std::fs::{DirBuilder, Metadata, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};

/// The id of the user running this process: on Linux the real user id the kernel's status gives,
/// and 0 on Windows, where the toolchain compares no owners.
#[cfg(unix)]
pub fn running_user() -> io::Result<u32> {
    std::fs::read_to_string("/proc/self/status")?
        .lines()
        .find_map(|line| line.strip_prefix("Uid:"))
        .and_then(|ids| ids.split_whitespace().next()?.parse().ok())
        .ok_or_else(|| io::Error::other("/proc/self/status gives no user id"))
}

#[cfg(windows)]
pub fn running_user() -> io::Result<u32> {
    Ok(0)
}

/// The home directory of the user `uid`: on Linux the one the system's user database gives, and
/// on Windows the running user's profile directory, which `USERPROFILE` names.
#[cfg(unix)]
pub fn home(uid: u32) -> io::Result<Option<PathBuf>> {
    let users = match std::fs::read_to_string("/etc/passwd") {
        Ok(users) => users,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    Ok(users.lines().find_map(|line| {
        let fields: Vec<&str> = line.split(':').collect();
        (fields.len() == 7 && fields[2].parse() == Ok(uid) && !fields[5].is_empty())
            .then(|| PathBuf::from(fields[5]))
    }))
}

#[cfg(windows)]
pub fn home(_uid: u32) -> io::Result<Option<PathBuf>> {
    Ok(std::env::var_os("USERPROFILE").map(PathBuf::from))
}

/// A builder of directories that the running user alone may access, on Linux by their mode.
#[cfg(unix)]
pub fn private_dir() -> DirBuilder {
    let mut builder = DirBuilder::new();
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder
}

#[cfg(windows)]
pub fn private_dir() -> DirBuilder {
    DirBuilder::new()
}

/// Options for a file that the running user alone may read and write, on Linux by its mode.
#[cfg(unix)]
pub fn private_file() -> OpenOptions {
    let mut options = OpenOptions::new();
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    options
}

#[cfg(windows)]
pub fn private_file() -> OpenOptions {
    OpenOptions::new()
}

/// Whether a user other than `uid` owns a file or may write it, as its owner and mode say on
/// Linux; never on Windows, whose access control lists the toolchain does not read.
#[cfg(unix)]
pub fn writable_by_others(metadata: &Metadata, uid: u32) -> bool {
    use std::os::unix::fs::MetadataExt as _;
    metadata.uid() != uid || metadata.mode() & 0o022 != 0
}

#[cfg(windows)]
pub fn writable_by_others(_metadata: &Metadata, _uid: u32) -> bool {
    false
}

/// Whether a user other than `uid` owns a file or may access it, as its owner and mode say on
/// Linux; never on Windows, whose access control lists the toolchain does not read.
#[cfg(unix)]
pub fn accessible_by_others(metadata: &Metadata, uid: u32) -> bool {
    use std::os::unix::fs::MetadataExt as _;
    metadata.uid() != uid || metadata.mode() & 0o077 != 0
}

#[cfg(windows)]
pub fn accessible_by_others(_metadata: &Metadata, _uid: u32) -> bool {
    false
}

/// Returns once the entries of the directory `path` are on disk, as renames into it leave them:
/// on Linux by syncing the directory, and on Windows, whose file systems journal their entries in
/// order, once it finds that `path` names a directory.
#[cfg(unix)]
pub fn sync_dir(path: &Path) -> io::Result<()> {
    std::fs::File::open(path)?.sync_all()
}

#[cfg(windows)]
pub fn sync_dir(path: &Path) -> io::Result<()> {
    if std::fs::metadata(path)?.is_dir() {
        return Ok(());
    }
    Err(io::Error::new(
        io::ErrorKind::NotADirectory,
        format!("{} is no directory", path.display()),
    ))
}

/// Hands this process over to the program `command` starts: on Linux the program replaces it, and
/// on Windows this process runs it to its end and exits with its exit code. Returns only when the
/// system refuses to start the program.
#[cfg(unix)]
pub fn hand_over(mut command: Command) -> io::Error {
    std::os::unix::process::CommandExt::exec(&mut command)
}

#[cfg(windows)]
pub fn hand_over(mut command: Command) -> io::Error {
    match command.status() {
        Ok(status) => std::process::exit(status.code().unwrap_or(1)),
        Err(error) => error,
    }
}

/// The number of the signal that ended a process, none on Windows, which ends none by a signal.
#[cfg(unix)]
pub fn signal(status: ExitStatus) -> Option<i32> {
    std::os::unix::process::ExitStatusExt::signal(&status)
}

#[cfg(windows)]
pub fn signal(_status: ExitStatus) -> Option<i32> {
    None
}
