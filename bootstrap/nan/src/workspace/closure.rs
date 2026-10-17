//! Dependency closures: the release through which a workspace package's lock reaches each package
//! of its dependency closure, std aside — a workspace package checked from source while its
//! locked release holds its current source files, else the release the local store holds — and
//! the closures that workspace packages share.

use std::collections::{BTreeMap, HashMap};
use std::io;

use super::Package;
use super::manifest::{relative, resolve};
use crate::diagnostic::{Code, Diagnostic};
use crate::hash::Hash;
use crate::store::Entry;

/// Where a package of a closure comes from: a workspace package checked from source, or a
/// release of the local store, by its index among the workspace's releases.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Member {
    Source(usize),
    Release(usize),
}

/// A dependency closure: its members by package name, the workspace packages checked and
/// reported in it, and the member each alias of each member names.
pub struct Closure {
    pub members: BTreeMap<String, Member>,
    pub checked: Vec<usize>,
    pub aliases: HashMap<(Member, String), Member>,
}

/// A store entry loaded as a package, by the code root of its release.
pub struct Release {
    pub root: Hash,
    pub entry: Entry,
    pub package: Package,
}

/// What reading closures needs: the store entry holding a release, by its code root.
pub trait Store {
    fn entry(&mut self, root: &Hash) -> io::Result<Option<Entry>>;
}

/// What reading the closures of the workspace packages gives: the closures, the releases they
/// read from the store, the store entries confirming the current states of path sources, and
/// the diagnostics of their locks.
pub struct Read {
    pub closures: Vec<Closure>,
    pub releases: Vec<Release>,
    pub confirmed: Vec<Entry>,
    pub found: Vec<Diagnostic>,
}

pub fn closures(packages: &[Package], store: &mut dyn Store) -> io::Result<Read> {
    let mut reader = Reader {
        packages,
        store,
        releases: Vec::new(),
        confirmed: Vec::new(),
        found: Vec::new(),
    };
    let owns: Vec<Option<Own>> = (0..packages.len())
        .map(|index| reader.own(index))
        .collect::<io::Result<_>>()?;
    let mut closures: Vec<Closure> = Vec::new();
    for (index, own) in owns.into_iter().enumerate() {
        let Some(own) = own else {
            continue;
        };
        let fits =
            |closure: &Closure| {
                own.members.iter().all(|(name, member)| {
                    closure
                        .members
                        .get(name)
                        .is_none_or(|known| known == member)
                }) && own.aliases.iter().all(|(key, member)| {
                    closure.aliases.get(key).is_none_or(|known| known == member)
                }) && closure.checked.iter().all(|checked| {
                    let name = packages[*checked].name().unwrap_or_default();
                    own.members
                        .get(name)
                        .is_none_or(|member| *member == Member::Source(*checked))
                })
            };
        match closures.iter_mut().find(|closure| fits(closure)) {
            Some(closure) => {
                closure.members.extend(own.members);
                closure.aliases.extend(own.aliases);
                closure.checked.push(index);
            }
            None => closures.push(Closure {
                members: own.members,
                checked: vec![index],
                aliases: own.aliases,
            }),
        }
    }
    Ok(Read {
        closures,
        releases: reader.releases,
        confirmed: reader.confirmed,
        found: reader.found,
    })
}

/// A package's own closure.
struct Own {
    members: BTreeMap<String, Member>,
    aliases: HashMap<(Member, String), Member>,
}

struct Reader<'a> {
    packages: &'a [Package],
    store: &'a mut dyn Store,
    releases: Vec<Release>,
    confirmed: Vec<Entry>,
    found: Vec<Diagnostic>,
}

impl Reader<'_> {
    fn report(&mut self, code: Code, package: &Package, line: u32) {
        let path = format!("{}/package.nan", package.dir);
        let diagnostic = Diagnostic::at(code, &path, line);
        if !self.found.contains(&diagnostic) {
            self.found.push(diagnostic);
        }
    }

    /// The workspace package at a directory, by index.
    fn at(&self, dir: &str) -> Option<usize> {
        self.packages
            .iter()
            .position(|package| package.dir == dir && package.manifest.is_some())
    }

    /// The release of a store entry, loaded once into a directory.
    fn release(&mut self, entry: Entry, dir: &str) -> io::Result<usize> {
        let root = entry.root();
        if let Some(index) = self
            .releases
            .iter()
            .position(|release| release.root == root && release.package.dir == dir)
        {
            return Ok(index);
        }
        let package = Package::from_entry(&entry, dir)?;
        self.releases.push(Release {
            root,
            entry,
            package,
        });
        Ok(self.releases.len() - 1)
    }

    /// The directory and manifest dependencies of a member.
    fn member_package(&self, member: Member) -> &Package {
        match member {
            Member::Source(index) => &self.packages[index],
            Member::Release(index) => &self.releases[index].package,
        }
    }

    /// A workspace package's own closure, reporting its lock's diagnostics; none without a
    /// manifest.
    fn own(&mut self, index: usize) -> io::Result<Option<Own>> {
        let package = &self.packages[index];
        let Some(manifest) = package.manifest.as_ref() else {
            return Ok(None);
        };
        let me = Member::Source(index);
        let mut own = Own {
            members: BTreeMap::from([(manifest.name.clone(), me)]),
            aliases: HashMap::new(),
        };
        self.split(index);
        let Some(lock) = package.machine.lock() else {
            if package.machine.lock.is_none() {
                for dep in manifest.deps.iter().filter(|dep| dep.source.is_some()) {
                    self.report(Code::DepLockStale, package, dep.line);
                }
            }
            self.unlocked(index, &mut own);
            return Ok(Some(own));
        };
        let mut lines: HashMap<&str, Member> = HashMap::new();
        for line in &lock.deps {
            let reaching = manifest
                .deps
                .iter()
                .filter(|dep| dep.written == line.source)
                .map(|dep| dep.line)
                .next()
                .unwrap_or(manifest.line);
            let held = self.store.entry(&line.root)?;
            let mismatched = held.as_ref().is_some_and(|entry| entry.root() != line.root);
            if mismatched {
                self.report(Code::ArtefactRootMismatch, package, reaching);
            }
            let missing = |reader: &mut Reader| {
                if !mismatched {
                    reader.report(Code::DepReleaseMissing, package, reaching);
                }
            };
            let entry = held.filter(|_| !mismatched);
            let member = if line.source.starts_with('.') {
                let dir = resolve(&package.dir, &line.source);
                let source = self.at(&dir).filter(|at| {
                    self.packages[*at].name() == Some(line.package.as_str())
                        && entry
                            .as_ref()
                            .is_some_and(|entry| holds_sources(entry, &self.packages[*at]))
                });
                match (source, entry) {
                    (Some(at), entry) => {
                        self.confirmed.extend(entry);
                        Some(Member::Source(at))
                    }
                    (None, Some(entry)) => Some(Member::Release(self.release(entry, &dir)?)),
                    (None, None) => {
                        missing(self);
                        self.at(&dir).map(Member::Source)
                    }
                }
            } else {
                match entry {
                    Some(entry) => Some(Member::Release(self.release(entry, &line.package)?)),
                    None => {
                        missing(self);
                        None
                    }
                }
            };
            if let Some(member) = member {
                lines.insert(line.package.as_str(), member);
                own.members.insert(line.package.clone(), member);
            }
        }
        for dep in manifest.deps.iter().filter(|dep| dep.source.is_some()) {
            let line = lock
                .deps
                .iter()
                .find(|line| line.alias.as_deref() == Some(dep.alias.as_str()));
            match line {
                Some(line) if line.source == dep.written => {
                    if let Some(member) = lines.get(line.package.as_str()) {
                        own.aliases.insert((me, dep.alias.clone()), *member);
                    }
                }
                _ => self.report(Code::DepLockStale, package, dep.line),
            }
        }
        let mut reached: Vec<&str> = Vec::new();
        let members: Vec<Member> = own
            .members
            .values()
            .copied()
            .filter(|member| *member != me)
            .collect();
        for member in members {
            let held = self.member_package(member);
            let held_dir = held.dir.clone();
            let deps: Vec<(String, String, bool)> = held
                .manifest
                .iter()
                .flat_map(|held_manifest| &held_manifest.deps)
                .map(|dep| (dep.alias.clone(), dep.written.clone(), dep.path))
                .collect();
            for (alias, written, path) in deps {
                let written = match path {
                    true => relative(&package.dir, &resolve(&held_dir, &written)),
                    false => written,
                };
                match lock.deps.iter().find(|line| line.source == written) {
                    Some(line) => {
                        reached.push(line.package.as_str());
                        if let Some(target) = lines.get(line.package.as_str()) {
                            own.aliases.insert((member, alias.clone()), *target);
                        }
                        if line.package == manifest.name {
                            let first = manifest.deps.first().map_or(manifest.line, |dep| dep.line);
                            self.report(Code::DepCycle, package, first);
                        }
                    }
                    None => self.report(Code::DepLockStale, package, manifest.line),
                }
            }
        }
        let surplus = lock
            .deps
            .iter()
            .any(|line| line.alias.is_none() && !reached.contains(&line.package.as_str()));
        if surplus {
            self.report(Code::DepLockStale, package, manifest.line);
        }
        Ok(Some(own))
    }

    /// Reports each `dep` directive of a workspace package reaching the package name an earlier
    /// directive in alias order reaches through another source: a path source the package at its
    /// directory, a store name the package its lock line names.
    fn split(&mut self, index: usize) {
        let package = &self.packages[index];
        let Some(manifest) = &package.manifest else {
            return;
        };
        let mut deps: Vec<&super::manifest::Dep> = manifest
            .deps
            .iter()
            .filter(|dep| dep.source.is_some())
            .collect();
        deps.sort_by(|left, right| left.alias.cmp(&right.alias));
        let mut seen: HashMap<String, String> = HashMap::new();
        let mut split = Vec::new();
        for dep in deps {
            let (source, reached) = match dep.path {
                true => {
                    let dir = resolve(&package.dir, &dep.written);
                    let name = self.at(&dir).and_then(|at| self.packages[at].name());
                    (dir, name.map(str::to_owned))
                }
                false => {
                    let lock = package.machine.lock();
                    let line = lock
                        .and_then(|lock| lock.deps.iter().find(|line| line.source == dep.written));
                    (dep.written.clone(), line.map(|line| line.package.clone()))
                }
            };
            let Some(reached) = reached else {
                continue;
            };
            match seen.get(&reached) {
                Some(earlier) if *earlier != source => split.push(dep.line),
                Some(_) => {}
                None => {
                    seen.insert(reached, source);
                }
            }
        }
        for line in split {
            self.report(Code::DepSplitPackage, package, line);
        }
    }

    /// The closure of a package without a valid lock: its path dependencies from source, through
    /// their own path dependencies.
    fn unlocked(&mut self, index: usize, own: &mut Own) {
        let mut pending = vec![index];
        let mut seen = vec![index];
        while let Some(current) = pending.pop() {
            let Some(manifest) = self.packages[current].manifest.as_ref() else {
                continue;
            };
            for dep in manifest.deps.iter().filter(|dep| dep.path) {
                let Some(target) = dep.source.as_deref().and_then(|dir| self.at(dir)) else {
                    continue;
                };
                let name = self.packages[target].name().unwrap_or_default().to_owned();
                own.members.entry(name).or_insert(Member::Source(target));
                own.aliases.insert(
                    (Member::Source(current), dep.alias.clone()),
                    Member::Source(target),
                );
                if !seen.contains(&target) {
                    seen.push(target);
                    pending.push(target);
                }
            }
        }
    }
}

/// Whether a store entry holds a package's current source files, its machine files aside.
pub fn holds_sources(entry: &Entry, package: &Package) -> bool {
    let prefix = format!("{}/", package.dir);
    let mut current: Vec<(&str, &[u8])> = package
        .files
        .iter()
        .filter_map(|file| {
            let within = file.path.strip_prefix(&prefix)?;
            Some((within, file.syntax.source.as_slice()))
        })
        .collect();
    current.sort();
    let mut stored: Vec<(&str, &[u8])> = entry
        .sources
        .iter()
        .filter(|(path, _)| !path.starts_with(".nan/"))
        .map(|(path, content)| (path.as_str(), content.as_slice()))
        .collect();
    stored.sort();
    current == stored
}
