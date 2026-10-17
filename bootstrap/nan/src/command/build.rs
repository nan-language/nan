//! The commands that perform final builds: `nan build` and `nan run`.

use std::io;
use std::path::Path;

use super::{Invocation, Name, Outcome, Trace, record};
use crate::diagnostic::{Class, Code};
use crate::final_build;
use crate::output::{Output, Record, Verdict};
use crate::scope::Scope;
use crate::workspace::{Target, Workspace};

/// An image `nan run` starts once it has built it: its path and its arguments.
pub struct Launch {
    pub image: std::path::PathBuf,
    pub args: Vec<String>,
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
        return Ok(Outcome::Launch(Launch {
            image: workspace.root.join(image),
            args: invocation.passed.to_vec(),
        }));
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
