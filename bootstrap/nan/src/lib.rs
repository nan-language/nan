//! The bootstrap implementation of nan language 1.

mod artefact;
mod cache;
mod check;
mod command;
mod diagnostic;
mod diff;
mod emulator;
mod encoding;
mod final_build;
mod fix;
mod format;
mod hash;
mod image;
mod interp;
mod journal;
mod lower;
mod machine;
mod merge;
mod nir;
mod output;
mod prover;
mod release;
mod resolution;
mod scope;
mod sign;
mod stage;
mod stdlib;
mod store;
mod syntax;
mod test_runs;
mod toolchain;
mod vcs;
mod workspace;

pub use cache::use_home;
pub use check::{Discharge, Measured};
pub use command::{Launch, Run, run};
pub use diagnostic::Code;
pub use final_build::TaskStack;

/// The facts checking each item of the workspace rooted at `root` reads that its context pack
/// lacks, for the completeness test.
pub fn context_gaps(root: &std::path::Path) -> std::io::Result<Vec<String>> {
    let workspace = workspace::Workspace::load(root.to_owned(), &mut Vec::new())?;
    Ok(check::context_gaps(&workspace))
}

/// The canonical NIR stream of each package of the workspace at `root`, by name, for the
/// comparison of the NaN front end with the bootstrap.
pub fn nir_streams(root: &std::path::Path) -> std::io::Result<Vec<(String, Vec<u8>)>> {
    let workspace = workspace::Workspace::load(root.to_owned(), &mut Vec::new())?;
    Ok(check::nir_streams(&workspace))
}

/// The symbol name and Cranelift text of each function the final build of the program `package`
/// of the workspace at `root` lowers, in order of definition, for the comparison of the NaN
/// toolchain's lowering with the bootstrap's.
pub fn lowered_functions(
    root: &std::path::Path,
    package: &str,
) -> std::io::Result<Vec<(String, String)>> {
    let workspace = workspace::Workspace::load(root.to_owned(), &mut Vec::new())?;
    let program = workspace
        .packages
        .iter()
        .position(|source| source.name() == Some(package))
        .ok_or_else(|| std::io::Error::other(format!("no package {package}")))?;
    Ok(final_build::lowered_functions(&workspace, program))
}

/// The obligation hashes of the workspace at `root` whose result the proof cache holds and the
/// prover's procedure does not give, for the harnesses.
pub fn proof_cache_mismatches(root: &std::path::Path) -> std::io::Result<Vec<String>> {
    let workspace = workspace::Workspace::load(root.to_owned(), &mut Vec::new())?;
    Ok(check::proof_cache_mismatches(&workspace))
}

/// The measurements of the freeze that checking gives over the workspace at `root`.
pub fn measure(root: &std::path::Path) -> std::io::Result<Measured> {
    let workspace = workspace::Workspace::load(root.to_owned(), &mut Vec::new())?;
    Ok(check::measure(&workspace))
}

/// The tasks of the programs and test images of the workspace at `root`, each entry once, with
/// their stack uses, for the measurements of the freeze.
pub fn task_stacks(root: &std::path::Path) -> std::io::Result<Vec<TaskStack>> {
    let workspace = workspace::Workspace::load(root.to_owned(), &mut Vec::new())?;
    let mut found = final_build::program_tasks(&workspace);
    for task in test_runs::task_stacks(&workspace) {
        if !found.iter().any(|known| known.site == task.site) {
            found.push(task);
        }
    }
    Ok(found)
}
