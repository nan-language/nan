//! The user directory: its build cache, of cache entries, each a file never rewritten, and cache
//! records, each a file later runs replace whole, both holding their contents followed by their
//! BLAKE3-256 digest; and the secret keys of its principals.

use std::fs::{self, DirBuilder, Metadata, OpenOptions};
use std::io::{self, ErrorKind, Write as _};
use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _, OpenOptionsExt as _};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use crate::hash::{Hash, blake3};

/// The home directory this process uses in place of the one the user database gives, and the
/// directory of its local store in place of the user directory's `store`, once a harness gives
/// them.
static HOME: OnceLock<(PathBuf, Option<PathBuf>)> = OnceLock::new();

/// Makes this process use `home` as the running user's home directory and, when given, `store`
/// as the directory of its local store, unless an earlier call gave them.
pub fn use_home(home: PathBuf, store: Option<PathBuf>) {
    let _ = HOME.set((home, store));
}

/// The build cache of the running user, with the directory of its local store; without a
/// directory when the user database gives the user no home directory, so that no record is found
/// and none is kept.
pub struct Cache {
    dir: Option<PathBuf>,
    store: Option<PathBuf>,
    keys: Option<PathBuf>,
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
        let (home, store) = match HOME.get() {
            Some((home, store)) => (Some(home.clone()), store.clone()),
            None => (home(uid)?, None),
        };
        let Some(home) = home else {
            return Ok(Ok(Cache {
                dir: None,
                store: None,
                keys: None,
                home: PathBuf::new(),
                uid,
            }));
        };
        let user = home.join(".nan");
        let store = store.unwrap_or_else(|| user.join("store"));
        let mut missing = Vec::new();
        for dir in [user.clone(), user.join("cache"), store.clone()] {
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
            match DirBuilder::new().mode(0o700).create(&dir) {
                Ok(()) => {}
                Err(error) if error.kind() == ErrorKind::AlreadyExists => {
                    if exposed(&fs::symlink_metadata(&dir)?, uid) {
                        return Ok(Err(Exposed(shown(&home, &dir))));
                    }
                }
                Err(error) => return Err(error),
            }
        }
        Ok(Ok(Cache {
            dir: Some(user.join("cache")),
            store: Some(store),
            keys: Some(user.join("keys")),
            home,
            uid,
        }))
    }

    /// The local store of the user directory.
    pub fn store(&self) -> StoreDir {
        StoreDir {
            dir: self.store.clone(),
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

    /// The secret seed `keys/<principal>.key` holds: none when the file is missing or does not
    /// hold 32 bytes.
    pub fn secret_key(&self, principal: &str) -> io::Result<Result<Option<[u8; 32]>, Exposed>> {
        let Some(keys) = &self.keys else {
            return Ok(Ok(None));
        };
        let file = keys.join(format!("{principal}.key"));
        for path in [keys, &file] {
            match fs::symlink_metadata(path) {
                Ok(metadata) if secret_exposed(&metadata, self.uid) => {
                    return Ok(Err(Exposed(shown(&self.home, path))));
                }
                Ok(_) => {}
                Err(error) if error.kind() == ErrorKind::NotFound => return Ok(Ok(None)),
                Err(error) => return Err(error),
            }
        }
        Ok(Ok(fs::read(&file)?.try_into().ok()))
    }

    /// Writes the secret seed of a principal that has no key file, creating `keys` when it is
    /// missing: false, writing nothing, when the file exists. Gives the file's path as
    /// `~/.nan/...`.
    pub fn new_key(
        &self,
        principal: &str,
        seed: &[u8; 32],
    ) -> io::Result<Result<(bool, String), Exposed>> {
        let Some(keys) = &self.keys else {
            return Err(io::Error::other(
                "the user database gives no home directory",
            ));
        };
        match DirBuilder::new().mode(0o700).create(keys) {
            Ok(()) => {}
            Err(error) if error.kind() == ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
        if secret_exposed(&fs::symlink_metadata(keys)?, self.uid) {
            return Ok(Err(Exposed(shown(&self.home, keys))));
        }
        let file = keys.join(format!("{principal}.key"));
        let shown = shown(&self.home, &file);
        let opened = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&file);
        match opened {
            Ok(mut written) => {
                written.write_all(seed)?;
                Ok(Ok((true, shown)))
            }
            Err(error) if error.kind() == ErrorKind::AlreadyExists => Ok(Ok((false, shown))),
            Err(error) => Err(error),
        }
    }
}

/// Whether the directory of secret keys or a key file is a symbolic link, belongs to another
/// user, or is accessible to its group or to others.
fn secret_exposed(metadata: &Metadata, uid: u32) -> bool {
    metadata.file_type().is_symlink() || metadata.uid() != uid || metadata.mode() & 0o077 != 0
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
