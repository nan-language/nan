//! The bootstrap implementation of nan language 1.

mod check;
mod command;
mod diagnostic;
mod diff;
mod encoding;
mod final_build;
mod fix;
mod format;
mod hash;
mod interp;
mod journal;
mod lower;
mod nir;
mod output;
mod prover;
mod scope;
mod stdlib;
mod syntax;
mod toolchain;
mod workspace;

pub use command::{Launch, Run, run};
pub use diagnostic::Code;

/// The facts checking each item of the workspace rooted at `root` reads that its context pack
/// lacks, for the completeness test.
pub fn context_gaps(root: &std::path::Path) -> std::io::Result<Vec<String>> {
    let workspace = workspace::Workspace::load(root.to_owned(), &mut Vec::new())?;
    Ok(check::context_gaps(&workspace))
}
