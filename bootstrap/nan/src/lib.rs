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
mod stage;
mod stdlib;
mod store;
mod syntax;
mod test_runs;
mod toolchain;
mod vcs;
mod workspace;

pub use cache::use_home;
pub use command::{Launch, Run, run};
pub use diagnostic::Code;

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

/// The obligation hashes of the workspace at `root` whose result the proof cache holds and the
/// prover's procedure does not give, for the harnesses.
pub fn proof_cache_mismatches(root: &std::path::Path) -> std::io::Result<Vec<String>> {
    let workspace = workspace::Workspace::load(root.to_owned(), &mut Vec::new())?;
    Ok(check::proof_cache_mismatches(&workspace))
}
