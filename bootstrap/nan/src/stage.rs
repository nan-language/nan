//! Stage comparison: the code roots of the artefacts an image's `.nan` section holds against
//! those a final build places in its own, by package name.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use crate::artefact::{self, read_bytes, read_uvar};
use crate::check::Checker;
use crate::diagnostic::{Code, Diagnostic, Reason};
use crate::hash::Hash;
use crate::release;
use crate::workspace::Member;

/// The diagnostics of comparing the image at `image`, written `written`, with the artefacts a
/// checker's final build places in its image, by package name: the image unreadable, or each
/// package whose code roots differ or that one side lacks.
pub fn compare(
    checker: &Checker,
    artefacts: &BTreeMap<String, Vec<u8>>,
    image: &Path,
    written: &str,
) -> Vec<Diagnostic> {
    let read = fs::read(image).ok();
    let Some(theirs) = read.as_deref().and_then(section).and_then(roots) else {
        return vec![Diagnostic {
            path: Some(written.to_owned()),
            ..Diagnostic::new(Code::BuildImageUnreadable)
        }];
    };
    let ours: BTreeMap<String, Hash> = artefacts
        .iter()
        .filter_map(|(name, artefact)| {
            Some((name.clone(), artefact::decode(artefact).ok()?.root_hash()))
        })
        .collect();
    let mut names: Vec<&String> = ours.keys().chain(theirs.keys()).collect();
    names.sort();
    names.dedup();
    let shown = |root: Option<&Hash>| root.map_or_else(|| "absent".to_owned(), Hash::short);
    names
        .into_iter()
        .filter(|name| ours.get(*name) != theirs.get(*name))
        .map(|name| {
            let source = checker.db.packages.iter().find(|package| {
                package.name == *name && matches!(package.member, Some(Member::Source(_)))
            });
            let location = source.and_then(|package| {
                let manifest = package.source.manifest.as_ref()?;
                Some((format!("{}/package.nan", package.source.dir), manifest.line))
            });
            Diagnostic {
                path: location.as_ref().map(|(path, _)| path.clone()),
                line: location.map(|(_, line)| line),
                because: vec![
                    Reason::note(name),
                    Reason::note(&shown(ours.get(name))),
                    Reason::note(&shown(theirs.get(name))),
                ],
                ..Diagnostic::new(Code::BuildStageMismatch)
            }
        })
        .collect()
}

/// The code root hash of each artefact, by the package name its manifest declares; none when
/// one does not decode.
fn roots(artefacts: Vec<Vec<u8>>) -> Option<BTreeMap<String, Hash>> {
    artefacts
        .iter()
        .map(|artefact| {
            let decoded = artefact::decode(artefact).ok()?;
            let name = release::package_name(&decoded.code_root)?;
            Some((name, decoded.root_hash()))
        })
        .collect()
}

/// The artefacts the one section named `.nan` of an ELF64 or PE32+ file holds; none when the file
/// is neither or holds no such section, or several.
fn section(image: &[u8]) -> Option<Vec<Vec<u8>>> {
    let found: Vec<&[u8]> = sections(image)?
        .into_iter()
        .filter(|(name, _)| *name == b".nan")
        .map(|(_, contents)| contents)
        .collect::<Option<_>>()?;
    let [content] = found[..] else {
        return None;
    };
    let (count, mut rest) = read_uvar(content)?;
    let mut artefacts = Vec::new();
    for _ in 0..count {
        let (artefact, after) = read_bytes(rest)?;
        artefacts.push(artefact.to_vec());
        rest = after;
    }
    rest.is_empty().then_some(artefacts)
}

/// The little-endian unsigned number of `width` bytes at an offset of bytes.
fn number(bytes: &[u8], at: usize, width: usize) -> Option<usize> {
    let held = bytes.get(at..at.checked_add(width)?)?;
    let mut value = 0u64;
    for (index, byte) in held.iter().enumerate() {
        value |= u64::from(*byte) << (8 * index);
    }
    usize::try_from(value).ok()
}

/// A section of a file: its name and its contents, none when the file does not hold them.
type Section<'a> = (&'a [u8], Option<&'a [u8]>);

/// The sections of a little-endian ELF64 or PE32+ file; none for any other file.
fn sections(image: &[u8]) -> Option<Vec<Section<'_>>> {
    if image.get(..2)? == b"MZ" {
        return pe_sections(image);
    }
    if image.get(..6)? != [0x7F, b'E', b'L', b'F', 2, 1] {
        return None;
    }
    let (headers, size, count, names) = (
        number(image, 0x28, 8)?,
        number(image, 0x3A, 2)?,
        number(image, 0x3C, 2)?,
        number(image, 0x3E, 2)?,
    );
    let header = |index: usize| headers.checked_add(index.checked_mul(size)?);
    let contents = |index: usize| -> Option<&[u8]> {
        let at = header(index)?;
        let offset = number(image, at.checked_add(24)?, 8)?;
        image.get(offset..offset.checked_add(number(image, at.checked_add(32)?, 8)?)?)
    };
    let table = contents(names)?;
    (0..count)
        .map(|index| {
            let name = number(image, header(index)?, 4)?;
            let named = table.get(name..)?.split(|byte| *byte == 0).next()?;
            Some((named, contents(index)))
        })
        .collect()
}

/// The sections of a PE32+ file, each name without its padding.
fn pe_sections(image: &[u8]) -> Option<Vec<Section<'_>>> {
    let signature = number(image, 0x3C, 4)?;
    if image.get(signature..signature.checked_add(4)?)? != b"PE\0\0" {
        return None;
    }
    let count = number(image, signature + 6, 2)?;
    let table = signature
        .checked_add(24)?
        .checked_add(number(image, signature + 20, 2)?)?;
    (0..count)
        .map(|index| {
            let at = table.checked_add(index * 40)?;
            let field = image.get(at..at.checked_add(8)?)?;
            let name = field.split(|byte| *byte == 0).next()?;
            let size = number(image, at + 8, 4)?;
            let offset = number(image, at + 20, 4)?;
            Some((name, image.get(offset..offset.checked_add(size)?)))
        })
        .collect()
}
