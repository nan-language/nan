//! The toolchain hash of each language version this implementation implements.

use crate::encoding::{bytes, uvar};
use crate::hash::Hash;

include!(concat!(env!("OUT_DIR"), "/toolchain.rs"));

/// The language versions this implementation implements.
pub const LANGUAGES: [u64; 1] = [1];

/// The hash of the canonical encoding of the record of a language version and the toolchain
/// source tree hash.
pub fn hash(language: u64) -> Hash {
    let mut version = Vec::new();
    uvar(&mut version, language);
    let mut tree = Vec::new();
    bytes(&mut tree, &SOURCE_TREE);
    let mut record = Vec::new();
    uvar(&mut record, 2);
    for (key, content) in [(1, version), (2, tree)] {
        uvar(&mut record, key);
        bytes(&mut record, &content);
    }
    Hash::of(&record)
}
