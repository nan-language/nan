//! The machine files of a package as loaded: its lock, its history, the API snapshots of its
//! modules and its principals file, each with its bytes and what it reads as.

use std::io;

use crate::diagnostic::Diagnostic;
use crate::machine::Invalid;
use crate::machine::api::Snapshot;
use crate::machine::history::History;
use crate::machine::lock::Lock;
use crate::machine::principals::Principals;

/// A machine file: its path relative to the workspace root, its bytes, and what it reads as.
pub struct Loaded<T> {
    pub path: String,
    pub bytes: Vec<u8>,
    pub read: Result<T, Invalid>,
}

impl<T> Loaded<T> {
    fn load(
        path: &str,
        source: &mut dyn FnMut(&str) -> io::Result<Vec<u8>>,
        read: fn(&[u8]) -> Result<T, Invalid>,
    ) -> io::Result<Loaded<T>> {
        let bytes = source(path)?;
        Ok(Loaded {
            path: path.to_owned(),
            read: read(&bytes),
            bytes,
        })
    }

    /// What the file holds, when it is valid.
    pub fn valid(&self) -> Option<&T> {
        self.read.as_ref().ok()
    }
}

static NO_PRINCIPALS: Principals = Principals { keys: Vec::new() };

#[derive(Default)]
pub struct Machine {
    pub lock: Option<Loaded<Lock>>,
    pub history: Option<Loaded<History>>,
    /// The API snapshots, by the module name their file is named after, in byte order.
    pub snapshots: Vec<(String, Loaded<Snapshot>)>,
    pub principals: Option<Loaded<Principals>>,
}

impl Machine {
    /// Loads the machine files at `paths`, those of the package directory `dir`.
    pub fn load(
        dir: &str,
        paths: &[String],
        source: &mut dyn FnMut(&str) -> io::Result<Vec<u8>>,
    ) -> io::Result<Machine> {
        let mut machine = Machine::default();
        for path in paths {
            let Some(within) = path.strip_prefix(&format!("{dir}/.nan/")) else {
                continue;
            };
            match within {
                "lock.nan" => machine.lock = Some(Loaded::load(path, source, Lock::read)?),
                "history.nan" => {
                    machine.history = Some(Loaded::load(path, source, History::read)?);
                }
                "principals.nan" => {
                    machine.principals = Some(Loaded::load(path, source, Principals::read)?);
                }
                _ => {
                    let Some(module) = within
                        .strip_prefix("api/")
                        .and_then(|name| name.strip_suffix(".nan"))
                    else {
                        continue;
                    };
                    let loaded = Loaded::load(path, source, Snapshot::read)?;
                    machine.snapshots.push((module.to_owned(), loaded));
                }
            }
        }
        machine
            .snapshots
            .sort_by(|(left, _), (right, _)| left.cmp(right));
        Ok(machine)
    }

    /// A diagnostic per machine file that is not valid.
    pub fn diagnostics(&self) -> Vec<Diagnostic> {
        let mut found = Vec::new();
        let mut add = |path: &str, invalid: Option<&Invalid>| {
            if let Some(invalid) = invalid {
                found.push(Diagnostic::at(invalid.code, path, invalid.line));
            }
        };
        if let Some(lock) = &self.lock {
            add(&lock.path, lock.read.as_ref().err());
        }
        if let Some(history) = &self.history {
            add(&history.path, history.read.as_ref().err());
        }
        for (_, snapshot) in &self.snapshots {
            add(&snapshot.path, snapshot.read.as_ref().err());
        }
        if let Some(principals) = &self.principals {
            add(&principals.path, principals.read.as_ref().err());
        }
        found
    }

    pub fn lock(&self) -> Option<&Lock> {
        self.lock.as_ref().and_then(Loaded::valid)
    }

    pub fn history(&self) -> Option<&History> {
        self.history.as_ref().and_then(Loaded::valid)
    }

    /// The principals file, empty when the package has none or it is not valid.
    pub fn principals(&self) -> &Principals {
        self.principals
            .as_ref()
            .and_then(Loaded::valid)
            .unwrap_or(&NO_PRINCIPALS)
    }

    /// The valid API snapshot of a module, by its name.
    pub fn snapshot(&self, module: &str) -> Option<&Snapshot> {
        self.snapshots
            .iter()
            .find(|(name, _)| name == module)
            .and_then(|(_, loaded)| loaded.valid())
    }

    /// Each machine file of the package, by path, with its bytes.
    pub fn files(&self) -> impl Iterator<Item = (&String, &Vec<u8>)> {
        let lock = self.lock.iter().map(|loaded| (&loaded.path, &loaded.bytes));
        let history = self
            .history
            .iter()
            .map(|loaded| (&loaded.path, &loaded.bytes));
        let snapshots = self
            .snapshots
            .iter()
            .map(|(_, loaded)| (&loaded.path, &loaded.bytes));
        let principals = self
            .principals
            .iter()
            .map(|loaded| (&loaded.path, &loaded.bytes));
        lock.chain(history).chain(snapshots).chain(principals)
    }

    /// The bytes of a machine file of the package, by path.
    pub fn bytes(&self, path: &str) -> Option<&[u8]> {
        self.files()
            .find(|(known, _)| *known == path)
            .map(|(_, bytes)| bytes.as_slice())
    }
}
