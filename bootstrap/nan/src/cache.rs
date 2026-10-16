//! The user directory and its build cache: cache entries, each a file never rewritten, and cache
//! records, each a file later runs replace whole; both hold their contents followed by their
//! BLAKE3-256 digest.

use std::fs::{self, DirBuilder, Metadata, OpenOptions};
use std::io::{self, ErrorKind, Write as _};
use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _, OpenOptionsExt as _};
use std::path::{Path, PathBuf};

use crate::hash::{Hash, blake3};

/// The build cache of the running user; without a directory when the user database gives the
/// user no home directory, so that no record is found and none is kept.
pub struct Cache {
    dir: Option<PathBuf>,
    home: PathBuf,
    uid: u32,
}

/// The local store's directory in the user directory, once checked, and how to check its files.
pub struct StoreDir {
    pub dir: Option<PathBuf>,
    pub home: PathBuf,
    pub uid: u32,
}

impl StoreDir {
    /// Whether a file or directory of the store is exposed: its path as `~/.nan/...` when it is.
    pub fn exposed(&self, path: &Path) -> io::Result<Option<Exposed>> {
        match fs::symlink_metadata(path) {
            Ok(metadata) if exposed(&metadata, self.uid) => {
                Ok(Some(Exposed(shown(&self.home, path))))
            }
            Ok(_) => Ok(None),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }
}

/// A part of the user directory that a symbolic link could redirect or another user could
/// write: its path as `~/.nan/...`.
pub struct Exposed(pub String);

impl Cache {
    /// The running user's build cache, after checking the user directory, `cache` and `store`
    /// and creating each that is missing.
    pub fn open() -> io::Result<Result<Cache, Exposed>> {
        let uid = running_user()?;
        let Some(home) = home(uid)? else {
            return Ok(Ok(Cache {
                dir: None,
                home: PathBuf::new(),
                uid,
            }));
        };
        let user = home.join(".nan");
        let mut missing = Vec::new();
        for dir in [user.clone(), user.join("cache"), user.join("store")] {
            match fs::symlink_metadata(&dir) {
                Ok(metadata) if exposed(&metadata, uid) => {
                    return Ok(Err(Exposed(shown(&home, &dir))));
                }
                Ok(_) => {}
                Err(error) if error.kind() == ErrorKind::NotFound => missing.push(dir),
                Err(error) => return Err(error),
            }
        }
        for dir in missing {
            DirBuilder::new().mode(0o700).create(dir)?;
        }
        Ok(Ok(Cache {
            dir: Some(user.join("cache")),
            home,
            uid,
        }))
    }

    /// The local store of the user directory.
    pub fn store(&self) -> StoreDir {
        StoreDir {
            dir: self
                .dir
                .as_ref()
                .and_then(|cache| cache.parent())
                .map(|user| user.join("store")),
            home: self.home.clone(),
            uid: self.uid,
        }
    }

    /// The contents of a cache entry, none when it is missing or its digest does not match.
    pub fn entry(&self, key: &Hash) -> io::Result<Result<Option<Vec<u8>>, Exposed>> {
        self.record(&format!("entry-{}", key.full()))
    }

    /// Writes a cache entry under its key unless one is there.
    pub fn put(&self, key: &Hash, contents: &[u8]) -> io::Result<()> {
        let name = format!("entry-{}", key.full());
        match &self.dir {
            Some(dir) if !dir.join(&name).exists() => self.replace(&name, contents),
            _ => Ok(()),
        }
    }

    /// The contents of a cache record, none when it is missing or its digest does not match.
    pub fn record(&self, name: &str) -> io::Result<Result<Option<Vec<u8>>, Exposed>> {
        let Some(dir) = &self.dir else {
            return Ok(Ok(None));
        };
        let path = dir.join(name);
        match fs::symlink_metadata(&path) {
            Ok(metadata) if exposed(&metadata, self.uid) => {
                return Ok(Err(Exposed(shown(&self.home, &path))));
            }
            Ok(_) => {}
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(Ok(None)),
            Err(error) => return Err(error),
        }
        let file = fs::read(&path)?;
        let contents = file
            .split_last_chunk::<32>()
            .filter(|(contents, digest)| blake3(contents) == **digest)
            .map(|(contents, _)| contents.to_vec());
        Ok(Ok(contents))
    }

    /// Replaces a cache record, writing it whole under a new name and renaming it into place.
    pub fn replace(&self, name: &str, contents: &[u8]) -> io::Result<()> {
        let Some(dir) = &self.dir else {
            return Ok(());
        };
        let fresh = dir.join(format!(".{name}.{}", std::process::id()));
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&fresh)?;
        file.write_all(contents)?;
        file.write_all(&blake3(contents))?;
        fs::rename(fresh, dir.join(name))
    }
}

/// Whether a part of the user directory is a symbolic link, belongs to another user, or is
/// writable by its group or by others.
fn exposed(metadata: &Metadata, uid: u32) -> bool {
    metadata.file_type().is_symlink() || metadata.uid() != uid || metadata.mode() & 0o022 != 0
}

/// A path within the home directory, written as `~/` and its path there.
fn shown(home: &Path, path: &Path) -> String {
    let within = path.strip_prefix(home).unwrap_or(path);
    format!("~/{}", within.display())
}

/// The real user id of this process, as the kernel gives its status.
fn running_user() -> io::Result<u32> {
    fs::read_to_string("/proc/self/status")?
        .lines()
        .find_map(|line| line.strip_prefix("Uid:"))
        .and_then(|ids| ids.split_whitespace().next()?.parse().ok())
        .ok_or_else(|| io::Error::other("/proc/self/status gives no user id"))
}

/// The home directory the system's user database gives a user.
fn home(uid: u32) -> io::Result<Option<PathBuf>> {
    let users = match fs::read_to_string("/etc/passwd") {
        Ok(users) => users,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    Ok(users.lines().find_map(|line| {
        let fields: Vec<&str> = line.split(':').collect();
        (fields.len() == 7 && fields[2].parse() == Ok(uid) && !fields[5].is_empty())
            .then(|| PathBuf::from(fields[5]))
    }))
}

#[cfg(test)]
mod tests;
