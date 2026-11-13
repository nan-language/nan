use std::fs;
use std::path::{Path, PathBuf};

use super::Code;

/// The codes of rules that no workspace state reaches: no NIR stream is decoded, and language 1
/// is the only language.
const UNREACHED: [&str; 3] = [
    "language.newer-dependency",
    "nir.malformed",
    "nir.version-unsupported",
];

/// Whether `text` holds `name` as a whole code name.
fn names(text: &str, name: &str) -> bool {
    let part = |c: char| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_');
    text.match_indices(name)
        .any(|(at, _)| !text[..at].ends_with(part) && !text[at + name.len()..].starts_with(part))
}

/// The text of the fenced `nan` and `text` blocks of a specification chapter.
fn blocks(chapter: &str) -> String {
    let mut found = String::new();
    let mut inside = false;
    for line in chapter.lines() {
        if line.starts_with("```") {
            inside = !inside && matches!(line, "```nan" | "```text");
            continue;
        }
        if inside {
            found.push_str(line);
            found.push('\n');
        }
    }
    found
}

/// Appends the text of each file under `dir` that `test` accepts, given its path and text.
fn files(dir: &Path, test: &dyn Fn(&Path, &str) -> bool, found: &mut String) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            files(&path, test, found);
        } else if let Ok(text) = fs::read_to_string(&path)
            && test(&path, &text)
        {
            found.push_str(&text);
        }
    }
}

/// Whether a Rust or NaN source file is a test: a module's tests, a harness, or NaN source
/// holding `test` items.
fn is_test(path: &Path, text: &str) -> bool {
    match path.extension().and_then(|extension| extension.to_str()) {
        Some("rs") => {
            path.ends_with("tests.rs") || path.components().any(|part| part.as_os_str() == "tests")
        }
        Some("nan") => text.lines().any(|line| line.starts_with("test ")),
        _ => false,
    }
}

#[test]
fn every_code_is_marked_by_an_example_or_named_by_a_test() {
    let repository = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut spec = String::new();
    for entry in fs::read_dir(repository.join("docs/spec")).expect("the specification") {
        let chapter = fs::read_to_string(entry.expect("a chapter").path()).expect("a chapter");
        spec.push_str(&blocks(&chapter));
    }
    let mut tests = String::new();
    let others = |path: &Path, text: &str| is_test(path, text) && !path.ends_with(file!());
    for dir in ["bootstrap/nan/src", "bootstrap/nan/tests", "toolchain"] {
        files(&repository.join(dir), &others, &mut tests);
    }
    let missing: Vec<&str> = Code::ALL
        .iter()
        .filter(|code| {
            !names(&spec, code.name())
                && !names(&tests, code.name())
                && !tests.contains(&format!("Code::{code:?}"))
        })
        .map(|code| code.name())
        .collect();
    assert_eq!(missing, UNREACHED);
}
