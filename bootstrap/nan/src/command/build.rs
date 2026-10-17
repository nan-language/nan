//! The commands that perform final builds: `nan build` and `nan run`.

use std::io;
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use super::{Invocation, Name, Outcome, Trace, record};
use crate::diagnostic::{Class, Code};
use crate::emulator::{self, Writes};
use crate::final_build;
use crate::output::{Output, Record, Verdict};
use crate::scope::Scope;
use crate::workspace::{Target, Workspace};

/// What `nan run` starts once it has built an image: the image on `x86_64-host`, the emulator
/// booting it on a machine target, and its arguments.
pub struct Launch {
    pub program: PathBuf,
    pub args: Vec<String>,
    /// Whether the program is the emulator, whose end gives the run's status.
    pub emulated: bool,
}

impl Launch {
    /// Starts the program: the image replaces this process, so that termination requests reach
    /// it and its status is the run's; the emulator runs as a child, and the status the run ends
    /// with is given once it ends.
    pub fn start(&self) -> i32 {
        if self.emulated {
            return emulator::run(self);
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
    let programs = programs(workspace, &scope);
    let single = matches!(scope, Scope::Package(_) | Scope::Workspace) && programs.len() == 1;
    let compared = invocation.value("--compare-with");
    let host = programs.iter().all(|program| {
        let manifest = workspace.packages[*program].manifest.as_ref();
        manifest.is_some_and(|manifest| manifest.targets.contains(&Target::Host))
    });
    let unusable = programs.is_empty()
        || (invocation.name == Name::Run && !single)
        || (compared.is_some() && (!single || !host));
    if unusable {
        return Ok(Outcome::Output(
            invocation.tool_error(Code::ToolUsage, &written),
        ));
    }
    let compared = compared.map(|image| (current.join(image), image));
    let mut diagnostics = Vec::new();
    let mut images = Vec::new();
    for program in &programs {
        let compared = compared
            .as_ref()
            .map(|(path, image)| (path.as_path(), *image));
        let build = final_build::build(workspace, *program, compared)?;
        diagnostics.extend(build.diagnostics);
        images.extend(build.image);
    }
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
    if invocation.name == Name::Run && errors == 0 {
        let image = images.pop().expect("a built image");
        let path = workspace.root.join(&image);
        let target = workspace.packages[programs[0]].program_target();
        if target.is_machine() && !emulator::available(target) {
            return Ok(Outcome::Ended(emulator::OTHER_END));
        }
        let launch = match target {
            Target::Uefi => {
                let files = volume_files(workspace, &path)?;
                emulator::launch(target, &path, &files, Writes::Kept)?
            }
            Target::Bare => emulator::launch(target, &path, &[], Writes::Kept)?,
            _ => Launch {
                program: path,
                args: invocation.passed.to_vec(),
                emulated: false,
            },
        };
        return Ok(Outcome::Launch(launch));
    }
    images.sort();
    let mut records: Vec<Record> = diagnostics
        .iter()
        .map(|diagnostic| record(workspace, &scope, diagnostic))
        .collect();
    records.extend(images.iter().map(|image| Record {
        word: "wrote".to_owned(),
        fields: vec![image.clone()],
        lines: Vec::new(),
    }));
    let output = Output {
        command: invocation.command(&written, true),
        elision: invocation.command(&written, false),
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
    };
    Ok(Outcome::Output(output))
}

/// The image files of the workspace's machine programs other than `image`, which the boot volume
/// of an `x86_64-uefi` image holds under `nan run`.
fn volume_files(workspace: &Workspace, image: &Path) -> io::Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    for target in [Target::Uefi, Target::Bare] {
        let directory = workspace.root.join(".nan/images").join(target.name());
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
                    .position(|candidate| std::ptr::eq(candidate, package))
            })
            .into_iter()
            .collect(),
    };
    held.into_iter().filter(program).collect()
}

#[cfg(test)]
mod tests;
