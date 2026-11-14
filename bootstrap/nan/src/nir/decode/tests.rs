use super::*;
use crate::encoding::{Record, bytes, list, uvar};
use crate::workspace::Workspace;
use crate::workspace::tests::Scratch;

const SOURCE: &str = r#"//! Matches.

/// An identifier.
pub closed struct Id derives Copy {
    pub text: text::Fixed[8]
} Id

/// A match.
pub closed enum Match derives Copy {
    Named(Id)
} Match

/// A name.
pub const NAME: text::Fixed[8] = text::Fixed[8].of("PNP0B00")

/// The matches.
pub const MATCHES: Array[Match, 1] = Array[Match, 1].filled(Match.Named(Id(text: NAME)))

/// A percentage.
pub fn clamp_percent(value: u32) -> u8
    ensures result <= 100
{
    if value > 100 {
        return 100
    }
    return u8.fit(value)
} clamp_percent

/// A sum.
pub fn total(values: Slice[u32]) -> u64 {
    var sum: u64 = 0
    for value in values {
        sum += u64.widen(value)
    }
    return sum
} total
"#;

/// The NIR stream of a package of consts, types and functions.
fn written() -> Vec<u8> {
    let scratch = Scratch::new(&[
        (
            "example/package.nan",
            "//! Matches.\n\npackage example {\n    language 1\n    targets any\n} example\n",
        ),
        ("example/example.nan", SOURCE),
    ]);
    let workspace = Workspace::load(scratch.0.clone(), &mut Vec::new()).expect("the workspace");
    let mut streams = crate::check::nir_streams(&workspace);
    streams.pop().expect("the package's stream").1
}

/// A stream of units, each a record of its parts by key.
fn stream(version: u8, units: &[&[(u64, Vec<u8>)]]) -> Vec<u8> {
    let mut out = MAGIC.to_vec();
    out.push(version);
    list(&mut out, units.iter(), |out, parts| {
        let mut record = Record::default();
        for (key, content) in parts.iter() {
            record.field(*key, content.clone());
        }
        out.extend(record.finish());
    });
    out
}

fn number(value: u64) -> Vec<u8> {
    let mut out = Vec::new();
    uvar(&mut out, u128::from(value));
    out
}

fn texts(values: &[&str]) -> Vec<u8> {
    let mut out = Vec::new();
    list(&mut out, values.iter(), |out, value| {
        bytes(out, value.as_bytes())
    });
    out
}

/// The parts of a private struct's unit without a declaration: its path, kind, targets and
/// visibility.
fn parts(path: &str, targets: &[&str]) -> Vec<(u64, Vec<u8>)> {
    let mut text = Vec::new();
    bytes(&mut text, path.as_bytes());
    vec![
        (1, text),
        (2, number(1)),
        (3, texts(targets)),
        (4, number(0)),
    ]
}

#[test]
fn a_written_stream_decodes() {
    assert_eq!(defect(&written()), None);
}

#[test]
fn another_version_is_unsupported_and_a_redundant_group_malformed() {
    let written = written();
    let mut other = written.clone();
    other[4] = 2;
    assert_eq!(defect(&other), Some(Code::NirVersionUnsupported));
    let redundant = [&written[..4], &[0x81, 0x00], &written[5..]].concat();
    assert_eq!(defect(&redundant), Some(Code::NirMalformed));
    assert_eq!(
        defect(&written[..written.len() - 1]),
        Some(Code::NirMalformed)
    );
}

#[test]
fn units_decode_only_in_order_with_the_keys_and_values_this_version_defines() {
    let first = parts("example::A", &["any"]);
    let second = parts("example::B", &["any"]);
    assert_eq!(defect(&stream(1, &[&first, &second])), None);
    assert_eq!(
        defect(&stream(1, &[&second, &first])),
        Some(Code::NirMalformed)
    );
    let unsorted = parts("example::A", &["x86_64-linux", "any"]);
    assert_eq!(defect(&stream(1, &[&unsorted])), Some(Code::NirMalformed));
    let mut undefined = first.clone();
    undefined.push((13, number(0)));
    assert_eq!(defect(&stream(1, &[&undefined])), Some(Code::NirMalformed));
    let mut public = first.clone();
    public[3].1 = number(2);
    assert_eq!(defect(&stream(1, &[&public])), Some(Code::NirMalformed));
    let mut premises = first.clone();
    premises.push((10, number(0)));
    assert_eq!(defect(&stream(1, &[&premises])), Some(Code::NirMalformed));
    premises[4].1 = texts(&["checked"]);
    assert_eq!(defect(&stream(1, &[&premises])), None);
}
