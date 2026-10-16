//! Final builds: checking a program's final closure, lowering its reach and linking its image.

use std::collections::HashMap;
use std::io;
use std::path::PathBuf;

use crate::check;
use crate::check::db::PackageId;
use crate::diagnostic::{Class, Code, Diagnostic};
use crate::hash::{Hash, checksummed_base32};
use crate::lower;
use crate::workspace::{Target, Workspace};

/// The platform package of `x86_64-host` and its entry landing fn.
const PLATFORM: &str = "host_platform";
const LANDING: &str = "enter";

/// What a final build reports and the image it writes, relative to the workspace root.
pub struct Build {
    pub diagnostics: Vec<Diagnostic>,
    pub image: Option<String>,
}

/// Performs the final build of the program at index `program` of the workspace.
pub fn build(workspace: &Workspace, program: usize) -> io::Result<Build> {
    let source = &workspace.packages[program];
    let mut diagnostics: Vec<Diagnostic> =
        check::reported(workspace, &crate::scope::Scope::Workspace);
    let std = crate::stdlib::packages();
    let checker = check::checked(workspace, std, Target::Host);
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
        .find(|package| package.name == PLATFORM)
        .and_then(|package| checker.db.lookup(package.root?, LANDING))
        .expect("the host platform's entry landing fn");
    let prefixes = checker
        .db
        .packages
        .iter()
        .map(|package| (package.name.clone(), prefix(&package.name)))
        .collect::<HashMap<_, _>>();
    let lowered = lower::lower(
        &checker,
        &lower::Program {
            entry,
            landing,
            roots: vec![root],
            prefixes,
        },
    );
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
    let image = format!(".nan/images/{}/{name}", Target::Host.name());
    let target = workspace.root.join(&image);
    if let Err(output) = link(&lowered.object, &target) {
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

/// The prefix a package's symbols carry: until packages have code roots, the first 12
/// characters of the checksummed base32 of its name's hash.
fn prefix(name: &str) -> String {
    let hash = Hash::of(name.as_bytes());
    checksummed_base32(&hash.0[1..])[..12].to_owned()
}

/// Links an object into a position-independent executable at `image` with the `cc` of the
/// `PATH`, replacing any earlier image at once; `cc`'s output when it fails.
fn link(object: &[u8], image: &std::path::Path) -> Result<(), String> {
    let directory = image.parent().expect("an image lies in a directory");
    std::fs::create_dir_all(directory).map_err(|error| error.to_string())?;
    let name = image
        .file_name()
        .expect("an image has a name")
        .to_string_lossy()
        .into_owned();
    let staged = |extension: &str| -> PathBuf {
        directory.join(format!(".{name}.{}.{extension}", std::process::id()))
    };
    let (object_path, new) = (staged("o"), staged("new"));
    std::fs::write(&object_path, object).map_err(|error| error.to_string())?;
    let linked = lower::host::link(&object_path, &new);
    let _ = std::fs::remove_file(&object_path);
    linked?;
    std::fs::rename(&new, image).map_err(|error| error.to_string())
}
