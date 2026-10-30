//! Resolution: the greatest acceptable assignment of releases to the dependency sources of a
//! package, and the lock `nan dep sync` and `nan upgrade` write from it, appending the path
//! releases it locks to the local store. `nan upgrade` also forwards the package's uses of
//! retired forms.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io;

use crate::artefact;
use crate::cache::{Cache, Exposed};
use crate::check;
use crate::diagnostic::{Class as DiagnosticClass, Code, Diagnostic, Fix};
use crate::fix;
use crate::hash::Hash;
use crate::machine::lock::{Lock, Locked};
use crate::release;
use crate::scope::Scope;
use crate::store::{Append, Class, Entry, Store};
use crate::syntax::file::parse;
use crate::test_runs::{self, Runs};
use crate::workspace::manifest_deps;
use crate::workspace::{Member, ReleaseStore, Workspace};

/// A package whose locked release changes: its name, its old and new code roots and the class of
/// the new release with the old as its parent.
pub struct Change {
    pub package: String,
    pub old: Option<Hash>,
    pub new: Option<Hash>,
    pub class: Class,
}

/// What resolution decides.
pub enum Outcome {
    /// The lock to write, the packages whose locked release changes, the package's source files
    /// that forwarding rewrites, and its uses that stay moved.
    Resolved {
        lock: Lock,
        changes: Vec<Change>,
        sources: HashMap<String, Vec<u8>>,
        moved: Vec<Diagnostic>,
    },
    /// No assignment is acceptable: the diagnostics the newest one fails with.
    Unresolved(Vec<Diagnostic>),
}

/// A source a `dep` directive of the closure names: as the locking package's lock writes it, with
/// the package it reaches and, for a path, the package's directory.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Source {
    written: String,
    package: String,
    dir: Option<String>,
}

/// A candidate release of a source: the current state of a path source's workspace package, or
/// a release of the store.
#[derive(Clone)]
enum Candidate {
    Current(usize),
    Stored(Box<Entry>),
}

impl Candidate {
    /// The publication index that orders assignments: 0 for the current state of a path.
    fn index(&self) -> u64 {
        match self {
            Candidate::Current(_) => 0,
            Candidate::Stored(entry) => entry.index,
        }
    }
}

/// The root a lock line names for the current state of a path source while candidates are
/// checked, before that state's code root is computed.
fn pending_root(dir: &str) -> Hash {
    Hash::of(format!("current\n{dir}").as_bytes())
}

/// The store a candidate lock reads: the entries of the local store, and for each path source's
/// current state an entry holding its files.
struct Candidates<'a> {
    entries: &'a [Entry],
    current: HashMap<Hash, Entry>,
}

impl ReleaseStore for Candidates<'_> {
    fn entry(&mut self, root: &Hash) -> io::Result<Option<Entry>> {
        if let Some(entry) = self.current.get(root) {
            return Ok(Some(entry.clone()));
        }
        Ok(self
            .entries
            .iter()
            .find(|entry| entry.root() == *root)
            .cloned())
    }
}

struct Resolver<'a> {
    workspace: &'a Workspace,
    package: usize,
    entries: Vec<Entry>,
    cache: &'a Cache,
    /// The failures of the first assignment judged, the newest.
    newest: Option<Vec<Diagnostic>>,
    /// Whether the package's own errors make an assignment unacceptable: until no assignment is
    /// acceptable with them.
    own_errors: bool,
    /// Whether the package is checked with each use matching a `preserves` or `changes`
    /// tombstone replaced by its forwarding fix, `name.moved` accepted.
    forwarding: bool,
}

/// Resolves the dependency closure of the workspace package at `package`, forwarding its uses of
/// retired forms when `forwarding`.
pub fn resolve(
    workspace: &Workspace,
    package: usize,
    store: &Store,
    cache: &Cache,
    forwarding: bool,
) -> io::Result<Result<Outcome, Exposed>> {
    let entries = match store.entries()? {
        Ok(entries) => entries,
        Err(exposed) => return Ok(Err(exposed)),
    };
    let mut resolver = Resolver {
        workspace,
        package,
        entries,
        cache,
        newest: None,
        own_errors: true,
        forwarding,
    };
    let root = &workspace.packages[package];
    let direct = resolver.sources_of(&root.dir, manifest_deps(root));
    let mut found = resolver.search(&mut Vec::new(), direct.clone())?;
    if found.is_none() {
        resolver.own_errors = false;
        found = resolver.search(&mut Vec::new(), direct)?;
    }
    let Some(assignment) = found else {
        let manifest = root.manifest.as_ref().expect("a package with a manifest");
        let mut diagnostics = vec![Diagnostic::at(
            Code::DepUnresolved,
            &format!("{}/package.nan", root.dir),
            manifest.line,
        )];
        diagnostics.extend(resolver.newest.take().unwrap_or_default());
        return Ok(Ok(Outcome::Unresolved(diagnostics)));
    };
    resolver.lock(store, &assignment)
}

impl Resolver<'_> {
    /// The sources of `dep` directives written in the package directory `dir`, as the locking
    /// package writes them.
    fn sources_of(&self, dir: &str, deps: Vec<(String, String, bool)>) -> Vec<Source> {
        let root = &self.workspace.packages[self.package];
        deps.into_iter()
            .filter_map(|(_, written, path)| {
                if path {
                    let target = crate::workspace::resolve_source(dir, &written);
                    let package = self
                        .workspace
                        .packages
                        .iter()
                        .find(|package| package.dir == target)?
                        .name()?
                        .to_owned();
                    Some(Source {
                        written: crate::workspace::relative_source(&root.dir, &target),
                        package,
                        dir: Some(target),
                    })
                } else {
                    let package = self
                        .entries
                        .iter()
                        .find(|entry| entry.name.as_deref() == Some(written.as_str()))
                        .map(|entry| entry.package.clone())?;
                    Some(Source {
                        written,
                        package,
                        dir: None,
                    })
                }
            })
            .collect()
    }

    /// A source's candidates, newest first.
    fn candidates(&self, source: &Source) -> Vec<Candidate> {
        if let Some(dir) = &source.dir {
            return self
                .workspace
                .packages
                .iter()
                .position(|package| package.dir == *dir)
                .map(Candidate::Current)
                .into_iter()
                .collect();
        }
        let locked = self.workspace.packages[self.package]
            .machine
            .lock()
            .and_then(|lock| lock.deps.iter().find(|line| line.source == source.written))
            .map(|line| line.root);
        let named: Vec<&Entry> = self
            .entries
            .iter()
            .filter(|entry| entry.name.as_deref() == Some(source.written.as_str()))
            .collect();
        let descends = |entry: &Entry, ancestor: Hash| {
            let mut current = Some(entry);
            while let Some(held) = current {
                if held.root() == ancestor {
                    return true;
                }
                current = held
                    .parent
                    .and_then(|parent| named.iter().copied().find(|other| other.root() == parent));
            }
            false
        };
        let mut found: Vec<Candidate> = named
            .iter()
            .filter(|entry| locked.is_none_or(|ancestor| descends(entry, ancestor)))
            .map(|entry| Candidate::Stored(Box::new((*entry).clone())))
            .collect();
        found.sort_by_key(|candidate| std::cmp::Reverse(candidate.index()));
        found
    }

    /// The dependency sources a candidate's manifest names.
    fn reached(&self, candidate: &Candidate, source: &Source) -> Vec<Source> {
        match candidate {
            Candidate::Current(index) => {
                let package = &self.workspace.packages[*index];
                self.sources_of(&package.dir, manifest_deps(package))
            }
            Candidate::Stored(entry) => {
                let Some((_, manifest)) =
                    entry.sources.iter().find(|(path, _)| path == "package.nan")
                else {
                    return Vec::new();
                };
                let file = parse(manifest.clone());
                let dir = source.dir.clone().unwrap_or_else(|| source.package.clone());
                let deps = crate::workspace::read_deps(&file, &dir);
                self.sources_of(&dir, deps)
            }
        }
    }

    /// The greatest acceptable assignment extending `assigned` to the sources of `pending`.
    fn search(
        &mut self,
        assigned: &mut Vec<(Source, Candidate)>,
        mut pending: Vec<Source>,
    ) -> io::Result<Option<Vec<(Source, Candidate)>>> {
        pending.retain(|source| !assigned.iter().any(|(known, _)| known == source));
        pending.sort_by(|left, right| {
            (&left.package, &left.written).cmp(&(&right.package, &right.written))
        });
        pending.dedup();
        let Some(next) = pending.first().cloned() else {
            return match self.judge(assigned)? {
                None => Ok(Some(assigned.clone())),
                Some(failures) => {
                    self.newest.get_or_insert(failures);
                    Ok(None)
                }
            };
        };
        let root_name = self.workspace.packages[self.package]
            .name()
            .unwrap_or_default();
        let split = assigned
            .iter()
            .any(|(known, _)| known.package == next.package && known.written != next.written);
        if split || next.package == root_name {
            return Ok(None);
        }
        for candidate in self.candidates(&next) {
            let mut rest: Vec<Source> = pending[1..].to_vec();
            rest.extend(self.reached(&candidate, &next));
            assigned.push((next.clone(), candidate));
            if let Some(found) = self.search(assigned, rest)? {
                return Ok(Some(found));
            }
            assigned.pop();
        }
        Ok(None)
    }

    /// The candidate lock of an assignment: path lines name the pending roots of current
    /// states.
    fn candidate_lock(&self, assigned: &[(Source, Candidate)]) -> Lock {
        let root = &self.workspace.packages[self.package];
        let aliases: HashMap<String, String> = manifest_deps(root)
            .into_iter()
            .map(|(alias, written, _)| (written, alias))
            .collect();
        let mut deps: Vec<Locked> = assigned
            .iter()
            .map(|(source, candidate)| Locked {
                package: source.package.clone(),
                alias: aliases.get(&source.written).cloned(),
                source: source.written.clone(),
                root: match candidate {
                    Candidate::Current(_) => {
                        pending_root(source.dir.as_deref().unwrap_or_default())
                    }
                    Candidate::Stored(entry) => entry.root(),
                },
                signer: match candidate {
                    Candidate::Current(_) => None,
                    Candidate::Stored(entry) => artefact::decode(&entry.artefact)
                        .ok()
                        .and_then(|decoded| decoded.publisher_key()),
                },
                line: 0,
            })
            .collect();
        deps.sort_by(|left, right| left.package.cmp(&right.package));
        Lock {
            snapshot: self.entries.last().map_or(0, |entry| entry.index),
            deps,
            approved: Vec::new(),
            epochs: Vec::new(),
        }
    }

    /// The workspace of an assignment's candidate lock, the package's uses of retired forms
    /// forwarded when resolving so, with the files forwarding rewrites.
    fn assigned(&self, lock: &Lock) -> io::Result<(Workspace, HashMap<String, Vec<u8>>)> {
        let workspace = self.with_lock(lock, HashMap::new())?;
        if !self.forwarding {
            return Ok((workspace, HashMap::new()));
        }
        let sources = forwarded(&workspace, self.package);
        if sources.is_empty() {
            return Ok((workspace, sources));
        }
        Ok((self.with_lock(lock, sources.clone())?, sources))
    }

    /// The workspace with the candidate lock of an assignment and changed source files.
    fn with_lock(
        &self,
        lock: &Lock,
        mut changed: HashMap<String, Vec<u8>>,
    ) -> io::Result<Workspace> {
        let root = &self.workspace.packages[self.package];
        let current: HashMap<Hash, Entry> = self
            .workspace
            .packages
            .iter()
            .map(|package| {
                let entry = Entry {
                    index: 0,
                    package: package.name().unwrap_or_default().to_owned(),
                    name: None,
                    parent: None,
                    summary: None,
                    artefact: Vec::new(),
                    sources: check::package_sources(package),
                };
                (pending_root(&package.dir), entry)
            })
            .collect();
        let mut store = Candidates {
            entries: &self.entries,
            current,
        };
        let path = format!("{}/.nan/lock.nan", root.dir);
        changed.insert(path, lock.bytes().expect("a lock holds its snapshot"));
        self.workspace.rebuilt(&changed, &mut store)
    }

    /// Why an assignment is not acceptable, none when it is: the errors checking the closure
    /// finds on the package, the current states and the releases, and its failing tests.
    fn judge(&self, assigned: &[(Source, Candidate)]) -> io::Result<Option<Vec<Diagnostic>>> {
        let lock = self.candidate_lock(assigned);
        let (workspace, _) = self.assigned(&lock)?;
        let root = &workspace.packages[self.package];
        let Some(closure) = workspace.closure_of(root) else {
            return Ok(Some(Vec::new()));
        };
        let dirs: Vec<String> = assigned
            .iter()
            .map(|(source, candidate)| match candidate {
                Candidate::Current(_) => source.dir.clone().unwrap_or_default(),
                Candidate::Stored(_) => source.package.clone(),
            })
            .collect();
        let within = |diagnostic: &Diagnostic, dir: &str| {
            diagnostic
                .path
                .as_deref()
                .is_some_and(|path| path.starts_with(&format!("{dir}/")))
        };
        let errors: Vec<Diagnostic> = check::closure_errors(&workspace, closure)
            .into_iter()
            .chain(workspace.diagnostics())
            .filter(|diagnostic| diagnostic.code.class() == DiagnosticClass::Error)
            .filter(|diagnostic| !self.forwarding || diagnostic.code != Code::NameMoved)
            .collect();
        let (own, others): (Vec<Diagnostic>, Vec<Diagnostic>) = errors
            .into_iter()
            .filter(|diagnostic| {
                diagnostic.path.is_none()
                    || within(diagnostic, &root.dir)
                    || dirs.iter().any(|dir| within(diagnostic, dir))
            })
            .partition(|diagnostic| within(diagnostic, &root.dir));
        let mut failures = others;
        if own.is_empty() {
            let scope = Scope::Package(self.package);
            let changed = self.changed(&lock);
            match test_runs::run(&workspace, &scope, &Runs::Reaching(changed), self.cache)? {
                Ok(report) => {
                    failures.extend(report.failures.iter().map(|failure| {
                        Diagnostic::at(Code::DepUnresolved, &failure.path, failure.line)
                    }));
                    failures.extend(report.diagnostics);
                }
                Err(exposed) => failures.push(Diagnostic {
                    path: Some(exposed.0),
                    ..Diagnostic::new(Code::BuildCacheExposed)
                }),
            }
        } else if self.own_errors {
            failures.extend(own);
        }
        Ok((!failures.is_empty()).then_some(failures))
    }

    /// The packages whose release a candidate lock assigns differently from the current lock: a
    /// path source's current state counting as changed.
    fn changed(&self, candidate: &Lock) -> Vec<String> {
        let current = self.workspace.packages[self.package].machine.lock();
        let unchanged = |line: &Locked| {
            let Some(known) = current
                .and_then(|lock| lock.deps.iter().find(|known| known.package == line.package))
            else {
                return false;
            };
            if known.root == line.root {
                return true;
            }
            let dir = crate::workspace::resolve_source(
                &self.workspace.packages[self.package].dir,
                &line.source,
            );
            let source = self
                .workspace
                .packages
                .iter()
                .find(|package| package.dir == dir);
            line.root == pending_root(&dir)
                && source.is_some_and(|package| {
                    self.entries.iter().any(|entry| {
                        entry.root() == known.root
                            && crate::workspace::holds_sources(entry, package)
                    })
                })
        };
        candidate
            .deps
            .iter()
            .filter(|line| !unchanged(line))
            .map(|line| line.package.clone())
            .collect()
    }

    /// The lock of an acceptable assignment, appending the path releases it locks.
    fn lock(
        &self,
        store: &Store,
        assigned: &[(Source, Candidate)],
    ) -> io::Result<Result<Outcome, Exposed>> {
        let candidate = self.candidate_lock(assigned);
        let (workspace, sources) = self.assigned(&candidate)?;
        let root = &workspace.packages[self.package];
        let closure = workspace
            .closure_of(root)
            .expect("a resolved package's closure");
        let mut deps = Vec::new();
        for line in candidate.deps {
            let source = assigned
                .iter()
                .find(|(source, _)| source.package == line.package)
                .map(|(source, candidate)| (source.clone(), candidate.clone()));
            let root = match source {
                Some((_, Candidate::Current(index))) => {
                    let built = check::root::build(&workspace, closure, Member::Source(index));
                    let appended = store.append(Append {
                        package: line.package.clone(),
                        name: None,
                        parent: None,
                        summary: None,
                        artefact: built.artefact,
                        sources: built.sources,
                    })?;
                    match appended {
                        Ok(entry) => entry.root(),
                        Err(exposed) => return Ok(Err(exposed)),
                    }
                }
                _ => line.root,
            };
            deps.push(Locked { root, ..line });
        }
        let snapshot = match store.index()? {
            Ok(index) => index.last().map_or(0, |(number, _)| *number),
            Err(exposed) => return Ok(Err(exposed)),
        };
        let lock = Lock {
            snapshot,
            deps,
            ..self.workspace.packages[self.package]
                .machine
                .lock()
                .cloned()
                .unwrap_or_default()
        };
        let first = self.workspace.packages[self.package].machine.lock.is_none();
        let lock = check::approvals(&workspace, &workspace.packages[self.package], lock, first);
        let changes = self.changes(&lock);
        let moved = match self.forwarding {
            true => check::reported(&workspace, &Scope::Package(self.package))
                .into_iter()
                .filter(|diagnostic| diagnostic.code == Code::NameMoved)
                .collect(),
            false => Vec::new(),
        };
        Ok(Ok(Outcome::Resolved {
            lock,
            changes,
            sources,
            moved,
        }))
    }

    /// The packages whose locked release a new lock changes, by name.
    fn changes(&self, lock: &Lock) -> Vec<Change> {
        let old: BTreeMap<&str, Hash> = self.workspace.packages[self.package]
            .machine
            .lock()
            .map(|lock| {
                lock.deps
                    .iter()
                    .map(|line| (line.package.as_str(), line.root))
                    .collect()
            })
            .unwrap_or_default();
        let new: BTreeMap<&str, Hash> = lock
            .deps
            .iter()
            .map(|line| (line.package.as_str(), line.root))
            .collect();
        let mut names: Vec<&str> = old.keys().chain(new.keys()).copied().collect();
        names.sort();
        names.dedup();
        names
            .into_iter()
            .filter(|name| old.get(name) != new.get(name))
            .map(|name| {
                let entry = |root: Option<&Hash>| {
                    root.and_then(|root| self.entries.iter().find(|entry| entry.root() == *root))
                };
                let artefact = |root: Option<&Hash>| {
                    entry(root).map(|entry| (entry.artefact.clone(), entry.summary.clone()))
                };
                let published = entry(new.get(name))
                    .filter(|entry| entry.parent.as_ref() == old.get(name))
                    .and_then(|entry| entry.summary.as_ref())
                    .map(|summary| summary.class);
                let (parent, release) = (artefact(old.get(name)), artefact(new.get(name)));
                let class = match (published, &parent, &release) {
                    (Some(class), _, _) => class,
                    (None, Some((parent, old_summary)), Some((release, new_summary))) => {
                        let epochs = |path: &str| {
                            (
                                release::epoch(old_summary.as_ref(), path),
                                release::epoch(new_summary.as_ref(), path),
                            )
                        };
                        let classes = release::classes(
                            &release::exported(parent),
                            &release::exported(release),
                            &epochs,
                            &release::forwards(release),
                            &BTreeSet::new(),
                        );
                        release::release_class(&classes, true)
                    }
                    _ => Class::Additive,
                };
                Change {
                    package: name.to_owned(),
                    old: old.get(name).copied(),
                    new: new.get(name).copied(),
                    class,
                }
            })
            .collect()
    }
}

/// The source files of a workspace package once each use matching a `preserves` or `changes`
/// tombstone is replaced by its forwarding fix, the first of overlapping fixes alone.
fn forwarded(workspace: &Workspace, package: usize) -> HashMap<String, Vec<u8>> {
    let diagnostics = check::reported(workspace, &Scope::Package(package));
    let mut kept: Vec<&Fix> = Vec::new();
    let fixes = diagnostics
        .iter()
        .filter(|diagnostic| matches!(diagnostic.code, Code::RetiredForwardedUse | Code::NameMoved))
        .filter_map(|diagnostic| diagnostic.fixes.first());
    for fix in fixes {
        let overlaps = kept.iter().any(|other| {
            other.0.iter().any(|(path, held)| {
                fix.0.iter().any(|(at, replacement)| {
                    at == path && replacement.start < held.end && held.start < replacement.end
                })
            })
        });
        if !overlaps {
            kept.push(fix);
        }
    }
    fix::apply(workspace, &kept)
}
