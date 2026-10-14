//! The bootstrap implementation of nan language 1.

mod command;
mod diagnostic;
mod encoding;
mod format;
mod hash;
mod journal;
mod output;
mod scope;
mod syntax;
mod toolchain;
mod workspace;

pub use command::{Run, run};
pub use diagnostic::Code;
