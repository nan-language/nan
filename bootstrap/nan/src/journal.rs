//! The advisory lock on `.nan/write` and the journal `.nan/journal` through which commands write
//! workspace files.

use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

/// The advisory lock a command holds while it reads or writes workspace files.
pub struct Lock {
    _file: File,
}

impl Lock {
    /// Locks the workspace at `root`, exclusively when the command writes, after performing the
    /// replacements of a complete journal and deleting an incomplete one.
    pub fn acquire(root: &Path, exclusive: bool) -> io::Result<Lock> {
        let dir = root.join(".nan");
        fs::create_dir_all(&dir)?;
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(dir.join("write"))?;
        file.lock()?;
        recover(root)?;
        if !exclusive {
            file.unlock()?;
            file.lock_shared()?;
        }
        Ok(Lock { _file: file })
    }
}

fn journal_path(root: &Path) -> PathBuf {
    root.join(".nan").join("journal")
}

const COMPLETE: &[u8] = b"complete\n";

/// Writes files, given by path relative to `root`, through the journal.
pub fn write(root: &Path, files: &[(String, Vec<u8>)]) -> io::Result<()> {
    if files.is_empty() {
        return Ok(());
    }
    let mut journal = Vec::new();
    for (path, content) in files {
        journal.extend(format!("write {} {path}\n", content.len()).bytes());
        journal.extend(content);
    }
    journal.extend(COMPLETE);
    let path = journal_path(root);
    write_synced(&path, &journal)?;
    sync_directory(&root.join(".nan"))?;
    replace(root, &journal)?;
    fs::remove_file(path)
}

/// Replaces one file outside the source and machine files, which a command holding the shared
/// lock writes, by renaming a new file of this process into place.
pub fn replace_alone(root: &Path, path: &str, content: &[u8]) -> io::Result<()> {
    let target = root.join(path);
    let directory = target.parent().expect("a file lies in a directory");
    fs::create_dir_all(directory)?;
    let name = target
        .file_name()
        .expect("a file has a name")
        .to_string_lossy()
        .into_owned();
    let new = directory.join(format!(".{name}.{}.new", std::process::id()));
    write_synced(&new, content)?;
    fs::rename(&new, &target)?;
    sync_directory(directory)
}

fn write_synced(path: &Path, content: &[u8]) -> io::Result<()> {
    let mut file = File::create(path)?;
    file.write_all(content)?;
    file.sync_all()
}

fn sync_directory(path: &Path) -> io::Result<()> {
    File::open(path)?.sync_all()
}

/// Performs the replacements of a complete journal left by an interrupted command.
fn recover(root: &Path) -> io::Result<()> {
    let path = journal_path(root);
    let journal = match fs::read(&path) {
        Ok(journal) => journal,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    if journal.ends_with(COMPLETE) {
        replace(root, &journal)?;
    }
    fs::remove_file(path)
}

/// Replaces each file a complete journal records by renaming a new file into place, and returns
/// once the replacements are on disk.
fn replace(root: &Path, journal: &[u8]) -> io::Result<()> {
    let invalid = || io::Error::new(io::ErrorKind::InvalidData, "the journal is malformed");
    let mut rest = &journal[..journal.len() - COMPLETE.len()];
    let mut renames = Vec::new();
    while !rest.is_empty() {
        let header_end = rest
            .iter()
            .position(|byte| *byte == b'\n')
            .ok_or_else(invalid)?;
        let header = std::str::from_utf8(&rest[..header_end]).map_err(|_| invalid())?;
        let (length, path) = header
            .strip_prefix("write ")
            .and_then(|header| header.split_once(' '))
            .ok_or_else(invalid)?;
        let length: usize = length.parse().map_err(|_| invalid())?;
        let content = rest
            .get(header_end + 1..header_end + 1 + length)
            .ok_or_else(invalid)?;
        let target = root.join(path);
        let name = target
            .file_name()
            .ok_or_else(invalid)?
            .to_string_lossy()
            .into_owned();
        let new = target.with_file_name(format!(".{name}.new"));
        write_synced(&new, content)?;
        renames.push((new, target));
        rest = &rest[header_end + 1 + length..];
    }
    let mut directories = BTreeSet::new();
    for (new, target) in renames {
        fs::rename(&new, &target)?;
        directories.insert(target.parent().ok_or_else(invalid)?.to_owned());
    }
    directories
        .iter()
        .try_for_each(|directory| sync_directory(directory))
}

#[cfg(test)]
mod tests;
