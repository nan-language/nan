//! The commands that perform final builds, and build bundles: `nan build` and `nan run`.

use std::io;
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use super::{Invocation, Name, Outcome, Trace, record};
use crate::diagnostic::{Class, Code};
use crate::emulator::{self, Boots, Machine};
use crate::final_build;
use crate::output::{Output, Record, Verdict};
use crate::scope::Scope;
use crate::workspace::{Kind, Target, Workspace};

/// What `nan run` starts once it has built an image: the image on a host target, the emulator
/// booting it on a machine target, and its arguments.
pub struct Launch {
    pub program: PathBuf,
    pub args: Vec<String>,
    /// The machine directory of the emulator, whose end gives the run's status; none for an
    /// image.
    pub machine: Option<Machine>,
}

impl Launch {
    /// Starts the program: the image replaces this process, so that termination requests reach
    /// it and its status is the run's; the emulator runs as a child, and the status the run ends
    /// with is given once it ends.
    pub fn start(&self) -> i32 {
        if let Some(machine) = &self.machine {
            return emulator::run(self, machine);
        }
        let error = Command::new(&self.program).args(&self.args).exec();
        panic!("starting {}: {error}", self.program.display());
    }
}

pub fn run(
    invocation: &Invocation,
    workspace: &Workspace,
    current: &Path,
    trace: &mut Trace,
) -> io::Result<Outcome> {
    let Some((scope, written)) = Scope::resolve(workspace, current, invocation.scope()) else {
        let scope = invocation.scope().unwrap_or(".");
        return Ok(Outcome::Output(
            invocation.tool_error(Code::ToolUnknownScope, scope),
        ));
    };
    trace.scope = Some(written.clone());
    if let Some(against) = invocation.value("--against") {
        return bundle(invocation, workspace, current, &scope, &written, against);
    }
    let programs = programs(workspace, &scope);
    let single = matches!(scope, Scope::Package(_) | Scope::Workspace) && programs.len() == 1;
    let compared = invocation.value("--compare-with");
    let host = programs.iter().all(|program| {
        let manifest = workspace.packages[*program].manifest.as_ref();
        manifest.is_some_and(|manifest| manifest.targets.iter().any(|target| target.is_host()))
    });
    let resets = invocation.flag("--keep-resets");
    let unusable = programs.is_empty()
        || invocation.value("--as").is_some()
        || (invocation.name == Name::Run && !single)
        || (compared.is_some() && (!single || !host))
        || (resets && host);
    if unusable {
        return Ok(Outcome::Output(
            invocation.tool_error(Code::ToolUsage, &written),
        ));
    }
    let compared = compared.map(|image| (current.join(image), image));
    let mut diagnostics = Vec::new();
    let mut images = Vec::new();
    let mut interfaces = Vec::new();
    for program in &programs {
        let compared = compared
            .as_ref()
            .map(|(path, image)| (path.as_path(), *image));
        let build = final_build::build(workspace, *program, compared)?;
        diagnostics.extend(build.diagnostics);
        images.extend(build.image);
        interfaces.extend(build.interface);
    }
    if invocation.name == Name::Run && !errors(&diagnostics) {
        let image = images.pop().expect("a built image");
        let path = workspace.root.join(&image);
        let target = workspace.packages[programs[0]].program_target();
        if !emulator::available(target) {
            return Ok(Outcome::Ended(emulator::OTHER_END));
        }
        let boots = || Boots::Kept {
            dir: workspace.root.join(".nan/machine").join(target.name()),
            resets,
        };
        let launch = match target.kind() {
            Some(Kind::Uefi) => {
                let files = volume_files(workspace, target, &path)?;
                emulator::launch(target, &path, &files, boots())?
            }
            Some(Kind::Bare) => emulator::launch(target, &path, &[], boots())?,
            _ => Launch {
                program: path,
                args: invocation.passed.to_vec(),
                machine: None,
            },
        };
        return Ok(Outcome::Launch(launch));
    }
    images.extend(interfaces);
    Ok(Outcome::Output(report(
        invocation,
        workspace,
        &scope,
        &written,
        diagnostics,
        images,
    )))
}

/// `nan build --against`: builds the bundle of the package the scope names, which declares no
/// entry, against a kernel's interface file.
fn bundle(
    invocation: &Invocation,
    workspace: &Workspace,
    current: &Path,
    scope: &Scope,
    written: &str,
    against: &str,
) -> io::Result<Outcome> {
    let package = scope.package(workspace).and_then(|package| {
        workspace
            .packages
            .iter()
            .position(|candidate| std::ptr::eq(&**candidate, package))
    });
    let library = package.filter(|index| {
        workspace.packages[*index]
            .manifest
            .as_ref()
            .is_some_and(|manifest| manifest.entry.is_none())
    });
    let usable = invocation.name == Name::Build
        && matches!(scope, Scope::Package(_))
        && invocation.value("--compare-with").is_none();
    let Some(package) = library.filter(|_| usable) else {
        return Ok(Outcome::Output(
            invocation.tool_error(Code::ToolUsage, written),
        ));
    };
    let source = &workspace.packages[package];
    let seed = match invocation.value("--as") {
        Some(principal) => match super::publishing_key(source, principal, &[])? {
            Ok(seed) => Some(seed),
            Err(diagnostic) => {
                let diagnostics = vec![final_build::at_manifest(source, diagnostic)];
                return Ok(Outcome::Output(report(
                    invocation,
                    workspace,
                    scope,
                    written,
                    diagnostics,
                    Vec::new(),
                )));
            }
        },
        None => None,
    };
    let build = final_build::bundle(workspace, package, &current.join(against), seed)?;
    Ok(Outcome::Output(report(
        invocation,
        workspace,
        scope,
        written,
        build.diagnostics,
        build.image.into_iter().collect(),
    )))
}

/// Whether diagnostics hold an error.
fn errors(diagnostics: &[crate::diagnostic::Diagnostic]) -> bool {
    diagnostics
        .iter()
        .any(|diagnostic| diagnostic.code.class() == Class::Error)
}

/// The output of a build: its diagnostics, then a record for each file it wrote.
fn report(
    invocation: &Invocation,
    workspace: &Workspace,
    scope: &Scope,
    written: &str,
    mut diagnostics: Vec<crate::diagnostic::Diagnostic>,
    mut images: Vec<String>,
) -> Output {
    diagnostics.sort_by(|left, right| {
        (&left.path, left.line, left.code.name()).cmp(&(&right.path, right.line, right.code.name()))
    });
    diagnostics.dedup();
    let count = |class: Class| {
        diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.code.class() == class)
            .count()
    };
    let (errors, incomplete) = (count(Class::Error), count(Class::Incompleteness));
    images.sort();
    let mut records: Vec<Record> = diagnostics
        .iter()
        .map(|diagnostic| record(workspace, scope, diagnostic))
        .collect();
    records.extend(images.iter().map(|image| Record {
        word: "wrote".to_owned(),
        fields: vec![image.clone()],
        lines: Vec::new(),
    }));
    Output {
        command: invocation.command(written, true),
        elision: invocation.command(written, false),
        verdict: if errors > 0 {
            Verdict::Failed
        } else {
            Verdict::Ok
        },
        summary: vec![
            ("files", images.len()),
            ("errors", errors),
            ("incomplete", incomplete),
        ],
        records,
    }
}

/// The image files of the workspace's programs for the machine targets of a UEFI target's
/// architecture, other than `image`, which the boot volume of that UEFI target's image holds
/// under `nan run`.
fn volume_files(workspace: &Workspace, uefi: Target, image: &Path) -> io::Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    let arch = uefi.arch().expect("a UEFI target's architecture");
    for kind in [Kind::Uefi, Kind::Bare] {
        let directory = workspace
            .root
            .join(".nan/images")
            .join(Target::of(arch, kind).name());
        let Ok(entries) = std::fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries {
            let path = entry?.path();
            if path.is_file() && path != image {
                files.push(path);
            }
        }
    }
    files.sort();
    Ok(files)
}

/// The indexes of the programs a scope holds: every program of the workspace, or the program
/// holding a narrower scope.
fn programs(workspace: &Workspace, scope: &Scope) -> Vec<usize> {
    let program = |index: &usize| {
        workspace.packages[*index]
            .manifest
            .as_ref()
            .is_some_and(|manifest| manifest.entry.is_some())
    };
    let held: Vec<usize> = match scope {
        Scope::Workspace => (0..workspace.packages.len()).collect(),
        _ => scope
            .package(workspace)
            .and_then(|package| {
                workspace
                    .packages
                    .iter()
                    .position(|candidate| std::ptr::eq(&**candidate, package))
            })
            .into_iter()
            .collect(),
    };
    held.into_iter().filter(program).collect()
}

#[cfg(test)]
mod tests;
