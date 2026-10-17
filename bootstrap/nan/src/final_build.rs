//! Final builds: checking a program's final closure, lowering its reach and linking its image.

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::io;
use std::path::Path;

use crate::artefact;
use crate::check;
use crate::check::db::PackageId;
use crate::diagnostic::{Class, Code, Diagnostic, Reason};
use crate::encoding::{bytes, list};
use crate::hash::checksummed_base32;
use crate::lower;
use crate::workspace::{Member, Package, Target, Workspace};

/// The entry landing fn each platform package holds.
const LANDING: &str = "enter";

/// What a final build reports and the image it writes, relative to the workspace root.
pub struct Build {
    pub diagnostics: Vec<Diagnostic>,
    pub image: Option<String>,
}

/// Performs the final build of the program at index `program` of the workspace, comparing its
/// code roots with those of the image at the path `compared` gives, written as it gives, when
/// given.
pub fn build(
    workspace: &Workspace,
    program: usize,
    compared: Option<(&Path, &str)>,
) -> io::Result<Build> {
    let source = &workspace.packages[program];
    let mut diagnostics: Vec<Diagnostic> =
        check::reported(workspace, &crate::scope::Scope::Workspace);
    let std = crate::stdlib::packages();
    let Some(checked) = workspace.closure_of(source) else {
        return Ok(Build {
            diagnostics,
            image: None,
        });
    };
    let target = source.program_target();
    let checker = check::checked(workspace, checked, std, target);
    let root = package_id(&checker, source);
    let closure = closure(&checker, root);
    diagnostics.retain(|diagnostic| {
        diagnostic.path.as_deref().is_none_or(|path| {
            workspace.file(path).is_some_and(|(package, _)| {
                closure
                    .iter()
                    .any(|id| std::ptr::eq(checker.db.package(*id).source, package))
            })
        })
    });
    if errors(&diagnostics) {
        return Ok(Build {
            diagnostics,
            image: None,
        });
    }
    let name = checker.db.package(root).name.clone();
    let entry = checker
        .db
        .package(root)
        .root
        .and_then(|module| checker.db.lookup(module, "main"))
        .expect("a checked program's entry function");
    let landing = checker
        .db
        .packages
        .iter()
        .find(|package| Some(package.name.as_str()) == target.platform_package())
        .and_then(|package| checker.db.lookup(package.root?, LANDING))
        .expect("the platform package's entry landing fn");
    let artefacts = artefacts(workspace, &checker, root);
    if let Some((image, written)) = compared {
        diagnostics.extend(crate::stage::compare(&checker, &artefacts, image, written));
        if errors(&diagnostics) {
            return Ok(Build {
                diagnostics,
                image: None,
            });
        }
    }
    let program = lower::Program {
        entry,
        landing,
        roots: vec![root],
        prefixes: prefixes(&artefacts),
        section: section(&artefacts),
    };
    let lowered = match target.is_machine() {
        true => lower::lower_machine(&checker, &program),
        false => lower::lower(&checker, &program),
    };
    diagnostics.extend(collisions(source, &lowered));
    diagnostics.extend(lower::stack::judge(
        &checker,
        &lowered.frames,
        &lowered.demands,
    ));
    if errors(&diagnostics) {
        return Ok(Build {
            diagnostics,
            image: None,
        });
    }
    let image = format!(
        ".nan/images/{}/{name}{}",
        target.name(),
        target.image_suffix()
    );
    let written = workspace.root.join(&image);
    if let Err(output) = write_image(target, &lowered.object, &written)? {
        let manifest = source.manifest.as_ref().expect("a program's manifest");
        let line = manifest
            .entry
            .as_ref()
            .map_or(manifest.line, |(_, line)| *line);
        let mut diagnostic = Diagnostic::at(Code::TargetLinkFailed, &source.files[0].path, line);
        diagnostic.envelope = Some(("cc".to_owned(), output.lines().map(str::to_owned).collect()));
        diagnostics.push(diagnostic);
        return Ok(Build {
            diagnostics,
            image: None,
        });
    }
    Ok(Build {
        diagnostics,
        image: Some(image),
    })
}

/// Writes the image of a lowered object at `image` through a staged file beside it, replacing any
/// earlier image at once: on `x86_64-host` an executable the `cc` of the `PATH` links, giving
/// `cc`'s output when it fails; on a machine target the image writer's, entered by the program
/// entry seam.
pub fn write_image(target: Target, object: &[u8], image: &Path) -> io::Result<Result<(), String>> {
    let directory = image.parent().expect("an image lies in a directory");
    fs::create_dir_all(directory)?;
    let name = image
        .file_name()
        .expect("an image has a name")
        .to_string_lossy();
    let staged =
        |extension: &str| directory.join(format!(".{name}.{}.{extension}", std::process::id()));
    let new = staged("new");
    match target.is_machine() {
        true => {
            let bytes = crate::image::write(target, object, lower::machine::ENTRY);
            fs::write(&new, bytes)?;
        }
        false => {
            let staged_object = staged("o");
            fs::write(&staged_object, object)?;
            let linked = lower::host::link(&staged_object, &new);
            fs::remove_file(&staged_object)?;
            if let Err(output) = linked {
                return Ok(Err(output));
            }
        }
    }
    fs::rename(&new, image)?;
    Ok(Ok(()))
}

/// The symbol collisions of a lowering for the image of a final-link root or tested package,
/// each reported on its `package` line, quoting both entities.
pub fn collisions(package: &Package, lowered: &lower::Lowered) -> Vec<Diagnostic> {
    let line = package
        .manifest
        .as_ref()
        .map_or(1, |manifest| manifest.line);
    lowered
        .collisions
        .iter()
        .map(|(first, second)| Diagnostic {
            because: vec![Reason::note(first), Reason::note(second)],
            ..Diagnostic::at(Code::ArtefactSymbolCollision, &package.files[0].path, line)
        })
        .collect()
}

fn errors(diagnostics: &[Diagnostic]) -> bool {
    diagnostics
        .iter()
        .any(|diagnostic| diagnostic.code.class() == Class::Error)
}

/// The checker's id of a workspace package.
fn package_id(checker: &check::Checker, package: &crate::workspace::Package) -> PackageId {
    let index = checker
        .db
        .packages
        .iter()
        .position(|data| std::ptr::eq(data.source, package))
        .expect("a workspace package in the checker's database");
    PackageId(index as u32)
}

/// The workspace packages of a program's dependency closure.
fn closure(checker: &check::Checker, root: PackageId) -> Vec<PackageId> {
    let mut found = vec![root];
    let mut index = 0;
    while index < found.len() {
        let package = checker.db.package(found[index]);
        for dependency in package.deps.values().flatten() {
            if !found.contains(dependency) {
                found.push(*dependency);
            }
        }
        index += 1;
    }
    found
}

/// The artefact of each package lowered with the workspace package `root` of a checker's
/// closure, by package name: of the packages `root` reaches, a workspace package's of its current
/// state and a release's from the local store, and of each std package the toolchain's.
pub fn artefacts(
    workspace: &Workspace,
    checker: &check::Checker,
    root: PackageId,
) -> BTreeMap<String, Vec<u8>> {
    let mut packages = closure(checker, root);
    packages.extend(
        (0..checker.db.packages.len())
            .map(|index| PackageId(index as u32))
            .filter(|id| checker.db.package(*id).std),
    );
    let mut found = BTreeMap::new();
    for id in packages {
        let data = checker.db.package(id);
        let (workspace, index) = match data.member {
            Some(Member::Release(index)) => {
                let artefact = workspace.releases[index].entry.artefact.clone();
                found.insert(data.name.clone(), artefact);
                continue;
            }
            Some(Member::Source(index)) => (workspace, index),
            None => {
                let std = crate::stdlib::workspace();
                let index = std
                    .packages
                    .iter()
                    .position(|package| package.name() == Some(data.name.as_str()))
                    .expect("a std package");
                (std, index)
            }
        };
        let Some(closure) = workspace.closure_of(&workspace.packages[index]) else {
            continue;
        };
        let built = check::root::build(workspace, closure, Member::Source(index));
        found.insert(data.name.clone(), built.artefact);
    }
    found
}

/// The prefix the symbols of each package carry, by package name: the first 12 characters of the
/// checksummed base32 of its code root hash's digest bytes.
pub fn prefixes(artefacts: &BTreeMap<String, Vec<u8>>) -> HashMap<String, String> {
    artefacts
        .iter()
        .filter_map(|(name, artefact)| {
            let root = artefact::decode(artefact).ok()?.root_hash();
            Some((
                name.clone(),
                checksummed_base32(&root.0[1..])[..12].to_owned(),
            ))
        })
        .collect()
}

/// The content of an image's `.nan` section: each package's artefact, in byte order of package
/// name.
fn section(artefacts: &BTreeMap<String, Vec<u8>>) -> Vec<u8> {
    let mut out = Vec::new();
    list(&mut out, artefacts.values(), |out, artefact| {
        bytes(out, artefact)
    });
    out
}
