//! The std packages the toolchain carries: the source files of the `std` directory, embedded when
//! the toolchain is built.

include!(concat!(env!("OUT_DIR"), "/std_files.rs"));

use std::path::PathBuf;
use std::sync::OnceLock;

use crate::workspace::{Package, Workspace};

/// The std packages in layer order.
pub const PACKAGES: [&str; 9] = [
    "core", "mem", "text", "task", "sys", "hw", "boot", "host", "testing",
];

/// The std packages, loaded once per run.
pub fn packages() -> &'static [Package] {
    &workspace().packages
}

/// The std packages as a workspace of their own, whose files the locations of std items name.
pub fn workspace() -> &'static Workspace {
    static STD: OnceLock<Workspace> = OnceLock::new();
    STD.get_or_init(|| Workspace::of_files(PathBuf::from("std"), &FILES))
}

#[cfg(test)]
mod tests;
