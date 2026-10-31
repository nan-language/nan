//! Bundle builds: the entries a package adds to the loaded registries of a kernel, lowered against
//! the kernel's interface file, and that file, which a kernel's final build writes.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::Path;
use std::rc::Rc;

use super::{
    Build, artefacts, collisions, errors, final_closure, items, package_id, prefixes, roots,
    write_file,
};
use crate::artefact::{read_bytes, read_uvar};
use crate::check::db::{ItemId, PackageId};
use crate::check::{self, Checker};
use crate::diagnostic::{Code, Diagnostic, Reason};
use crate::encoding::{Record, bytes, list, uvar};
use crate::hash::Hash;
use crate::lower;
use crate::lower::load::Interface;
use crate::workspace::{Level, Package, Target, Workspace};

/// The magic and format version of an interface file.
const MAGIC: &[u8; 4] = b"NLIF";
const VERSION: u128 = 1;

/// A kernel's interface file: its toolchain hash, its target, the code root hash of each package
/// of its final closure by name, the names of its exports in order, the stack its interrupt
/// functions may use, and its codegen level's name, empty for the base set.
pub fn interface_file(
    target: Target,
    roots: &BTreeMap<String, Hash>,
    exports: &[String],
    interrupt_stack: u64,
    level: Option<Level>,
) -> Vec<u8> {
    let field = |write: &dyn Fn(&mut Vec<u8>)| {
        let mut out = Vec::new();
        write(&mut out);
        out
    };
    let mut record = Record::default();
    record.field(1, crate::toolchain::hash(1).0.to_vec());
    record.field(2, target.name().as_bytes().to_vec());
    record.field(
        3,
        field(&|out| {
            list(out, roots.iter(), |out, (name, root)| {
                bytes(out, name.as_bytes());
                bytes(out, &root.0);
            })
        }),
    );
    record.field(
        4,
        field(&|out| list(out, exports.iter(), |out, name| bytes(out, name.as_bytes()))),
    );
    record.field(5, field(&|out| uvar(out, u128::from(interrupt_stack))));
    record.field(6, level.map_or("", Level::name).as_bytes().to_vec());
    let mut out = MAGIC.to_vec();
    uvar(&mut out, VERSION);
    out.extend(record.finish());
    out
}

/// What an interface file records.
struct Read {
    toolchain: Hash,
    target: Target,
    roots: BTreeMap<String, Hash>,
    exports: Vec<String>,
    interrupt_stack: u64,
    level: Option<Level>,
}

/// The interface file a toolchain wrote, none for any other bytes.
fn read_interface(file: &[u8]) -> Option<Read> {
    let rest = file.strip_prefix(&MAGIC[..])?;
    let (version, rest) = read_uvar(rest)?;
    let (count, mut rest) = read_uvar(rest)?;
    if version != VERSION || count != 6 {
        return None;
    }
    let mut fields = Vec::new();
    for key in 1..=6 {
        let (held, after) = read_uvar(rest)?;
        let (content, after) = read_bytes(after)?;
        if held != key {
            return None;
        }
        fields.push(content);
        rest = after;
    }
    let text = |bytes: &[u8]| String::from_utf8(bytes.to_vec()).ok();
    let mut roots = BTreeMap::new();
    let (count, mut list) = read_uvar(fields[2])?;
    for _ in 0..count {
        let (name, after) = read_bytes(list)?;
        let (root, after) = read_bytes(after)?;
        roots.insert(text(name)?, Hash(root.try_into().ok()?));
        list = after;
    }
    let mut exports = Vec::new();
    let (count, mut names) = read_uvar(fields[3])?;
    for _ in 0..count {
        let (name, after) = read_bytes(names)?;
        exports.push(text(name)?);
        names = after;
    }
    let (interrupt_stack, after) = read_uvar(fields[4])?;
    let level = match text(fields[5])?.as_str() {
        "" => None,
        name => Some(Level::from_name(name)?),
    };
    let read = Read {
        toolchain: Hash(fields[0].try_into().ok()?),
        target: Target::from_name(&text(fields[1])?)?,
        roots,
        exports,
        interrupt_stack: u64::try_from(interrupt_stack).ok()?,
        level,
    };
    (rest.is_empty() && list.is_empty() && names.is_empty() && after.is_empty()).then_some(read)
}

/// The largest `limits stack N` among the interrupt fns of packages.
pub fn interrupt_stack(checker: &Checker, packages: &[PackageId]) -> u64 {
    items(checker, packages)
        .filter_map(|item| checker.fn_sig(item))
        .filter(|sig| sig.modifier == Some(crate::syntax::ast::FnModifier::Interrupt))
        .filter_map(|sig| sig.limits.stack)
        .max()
        .unwrap_or(0)
}

/// Builds the bundle of the package at index `package` of the workspace against the interface
/// file at `against`, signed with the secret key of `seed` when given, writing it into
/// `.nan/images/<target>/`.
pub fn bundle(
    workspace: &Workspace,
    package: usize,
    against: &Path,
    seed: Option<[u8; 32]>,
) -> io::Result<Build> {
    let source = &workspace.packages[package];
    let unusable = |reason: &str| Ok(Build::failed(vec![interface_error(source, reason)]));
    let Ok(file) = fs::read(against) else {
        return unusable("the interface file cannot be read");
    };
    let Some(read) = read_interface(&file) else {
        return unusable("the interface file does not decode");
    };
    if read.toolchain != crate::toolchain::hash(1) {
        return unusable("another toolchain built the kernel");
    }
    let declared = source
        .manifest
        .as_ref()
        .is_some_and(|manifest| manifest.targets.contains(&read.target));
    if !declared {
        return unusable(&format!(
            "the package does not declare {}",
            read.target.name()
        ));
    }
    let mut diagnostics = check::reported(workspace, &crate::scope::Scope::Workspace);
    let Some(checked) = workspace.closure_of(source) else {
        return Ok(Build::failed(diagnostics));
    };
    let checker = check::checked(workspace, checked, crate::stdlib::packages(), read.target);
    let root = package_id(&checker, source);
    let closure = checker.reached(root);
    diagnostics.retain(|diagnostic| {
        diagnostic.path.as_deref().is_none_or(|path| {
            workspace.file(path).is_some_and(|(package, _)| {
                closure
                    .iter()
                    .any(|id| std::ptr::eq(checker.db.package(*id).source, package))
            })
        })
    });
    diagnostics.extend(check::release_errors(&checker, root));
    let artefacts = artefacts(workspace, &checker, root);
    for (name, root) in roots(&artefacts) {
        if read.roots.get(&name).is_some_and(|kernel| *kernel != root) {
            let reason = format!("{name} differs from the kernel's release of it");
            diagnostics.push(interface_error(source, &reason));
        }
    }
    let own: Vec<PackageId> = final_closure(&checker, root)
        .into_iter()
        .filter(|id| !read.roots.contains_key(&checker.db.package(*id).name))
        .collect();
    if interrupt_stack(&checker, &own) > read.interrupt_stack {
        let reason = "an interrupt fn demands more stack than the kernel's interrupt stacks hold";
        diagnostics.push(interface_error(source, reason));
    }
    if errors(&diagnostics) {
        return Ok(Build::failed(diagnostics));
    }
    let interface = Rc::new(Interface {
        hash: Hash::of(&file).0.to_vec(),
        exports: read.exports,
        interrupt_stack: read.interrupt_stack,
    });
    let bundle = lower::Bundle {
        contributions: contributions(&checker, root, &read.roots),
        roots: vec![root],
        prefixes: prefixes(&artefacts),
        level: read.level,
    };
    let lowered = lower::lower_bundle(&checker, &bundle);
    diagnostics.extend(collisions(source, &lowered));
    diagnostics.extend(lower::stack::judge(
        &checker,
        &lowered.frames,
        &lowered.demands,
    ));
    if errors(&diagnostics) {
        return Ok(Build::failed(diagnostics));
    }
    let contributed = &lowered.contributed;
    let written = match crate::image::bundle::write(
        &lowered.object,
        contributed,
        &interface,
        seed.as_ref(),
    ) {
        Ok(written) => written,
        Err(missing) => {
            let reason = format!("the kernel exports no {}", missing.join(", "));
            diagnostics.push(interface_error(source, &reason));
            return Ok(Build::failed(diagnostics));
        }
    };
    let name = &checker.db.package(root).name;
    let path = format!(".nan/images/{}/{name}.nbun", read.target.name());
    write_file(&workspace.root.join(&path), &written)?;
    Ok(Build {
        diagnostics,
        image: Some(path),
        interface: None,
    })
}

/// The admitted contributions of a bundle's closure to loaded registries by packages outside the
/// kernel's final closure, whose packages `kernel` names, with their registries, in byte order of
/// canonical path.
fn contributions(
    checker: &Checker,
    root: PackageId,
    kernel: &BTreeMap<String, Hash>,
) -> Vec<(ItemId, ItemId)> {
    let mut found: Vec<(String, ItemId, ItemId)> = checker
        .contributions(root)
        .into_iter()
        .filter(|contribution| {
            contribution.admitted
                && super::is_loaded(checker, contribution.registry)
                && !kernel.contains_key(&checker.db.package(contribution.contributor).name)
        })
        .map(|contribution| {
            let path = checker.db.canonical(contribution.item);
            (path, contribution.item, contribution.registry)
        })
        .collect();
    found.sort();
    found.dedup();
    found
        .into_iter()
        .map(|(_, item, registry)| (item, registry))
        .collect()
}

/// A `target.bundle-interface` error on a package's `package` line, with its reason.
fn interface_error(package: &Package, reason: &str) -> Diagnostic {
    let mut diagnostic = at_manifest(package, Diagnostic::new(Code::TargetBundleInterface));
    diagnostic.because.push(Reason::note(reason));
    diagnostic
}

/// A diagnostic located on a package's `package` line.
pub fn at_manifest(package: &Package, diagnostic: Diagnostic) -> Diagnostic {
    let line = package
        .manifest
        .as_ref()
        .map_or(1, |manifest| manifest.line);
    Diagnostic {
        because: diagnostic.because,
        ..Diagnostic::at(diagnostic.code, &package.files[0].path, line)
    }
}
