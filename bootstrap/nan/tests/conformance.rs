//! The conformance harness: runs each `nan` example of a specification chapter against its marker
//! comments, one test per chapter.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use datatest_stable::Utf8Path;
use nan::Code;

datatest_stable::harness! {
    { test = chapter, root = "../../docs/spec", pattern = r"^\d\d-[a-z-]+\.md$" },
}

/// Codes this implementation reports for only some of the constructs their rules cover.
const PARTIAL: &[&str] = &["name.dot-for-path"];

const SUMMARY: &str = "//! Specification example.";

const MANIFEST: &str = "//! Specification example.

package example {
    language 1
    targets any
} example
";

struct Marker {
    class: &'static str,
    code: String,
    path: String,
    line: usize,
}

/// A `nan` block: the line of the specification it starts on, and its files.
struct Example {
    line: usize,
    files: Vec<(String, Vec<String>)>,
    prepended: bool,
}

impl Example {
    fn marker(&self) -> Option<Marker> {
        let markers = [
            (" // error: ", "error"),
            (" // incomplete: ", "incomplete"),
            (" // traps: ", "traps"),
        ];
        self.files.iter().find_map(|(path, lines)| {
            lines.iter().enumerate().find_map(|(index, line)| {
                markers.iter().find_map(|(text, class)| {
                    let at = line.rfind(text)?;
                    let code = &line[at + text.len()..];
                    let is_code = code.contains('.')
                        && code.chars().all(|c| {
                            c.is_ascii_lowercase() || c.is_ascii_digit() || c == '.' || c == '-'
                        });
                    is_code.then(|| Marker {
                        class,
                        code: code.to_owned(),
                        path: path.clone(),
                        line: index + 1,
                    })
                })
            })
        })
    }

    /// Writes the files into `dir`, without their marker comments when `unmarked`.
    fn write(&self, dir: &Path, unmarked: bool) -> std::io::Result<()> {
        if dir.exists() {
            fs::remove_dir_all(dir)?;
        }
        if self.prepended {
            fs::create_dir_all(dir.join("example"))?;
            fs::write(dir.join("example/package.nan"), MANIFEST)?;
        }
        for (path, lines) in &self.files {
            let target = dir.join(path);
            fs::create_dir_all(target.parent().expect("a file lies in a package"))?;
            let mut text = String::new();
            for line in lines {
                let kept = if unmarked { unmark(line) } else { line };
                text.push_str(kept);
                text.push('\n');
            }
            fs::write(target, text)?;
        }
        Ok(())
    }
}

/// A line without the marker comment ending it, and the space before it.
fn unmark(line: &str) -> &str {
    [" // error: ", " // incomplete: ", " // traps: "]
        .iter()
        .find_map(|marker| line.rfind(marker).map(|at| &line[..at]))
        .unwrap_or(line)
}

/// The `nan` blocks of a chapter, each placed in its workspace as the charter states.
fn examples(text: &str) -> Vec<Example> {
    let lines: Vec<&str> = text.lines().collect();
    let mut examples = Vec::new();
    let mut index = 0;
    while index < lines.len() {
        if lines[index] != "```nan" {
            index += 1;
            continue;
        }
        let start = index + 1;
        let end = start
            + lines[start..]
                .iter()
                .position(|line| *line == "```")
                .expect("a block ends");
        let block = &lines[start..end];
        let mut files: Vec<(String, Vec<String>)> = Vec::new();
        let prepended = !block.iter().any(|line| line.starts_with("// file: "));
        if prepended {
            let mut file = vec![SUMMARY.to_owned()];
            file.extend(block.iter().map(|line| (*line).to_owned()));
            files.push(("example/example.nan".to_owned(), file));
        } else {
            for line in block {
                match line.strip_prefix("// file: ") {
                    Some(path) => files.push((path.to_owned(), Vec::new())),
                    None => files
                        .last_mut()
                        .expect("a file line comes first")
                        .1
                        .push((*line).to_owned()),
                }
            }
            for (_, lines) in &mut files {
                while lines.last().is_some_and(|line| line.is_empty()) {
                    lines.pop();
                }
            }
        }
        examples.push(Example {
            line: start + 1,
            files,
            prepended,
        });
        index = end + 1;
    }
    examples
}

/// Runs `nan` with `args` at `dir`, returning its output and exit status.
fn nan(dir: &Path, args: &[&str]) -> Result<(String, Option<i32>), String> {
    let output = Command::new(env!("CARGO_BIN_EXE_nan"))
        .args(args)
        .current_dir(dir)
        .output()
        .map_err(|error| error.to_string())?;
    let stdout = String::from_utf8(output.stdout).map_err(|error| error.to_string())?;
    Ok((stdout, output.status.code()))
}

/// The class word, code and location of each diagnostic record of an output.
fn diagnostics(output: &str) -> Vec<(String, String, String)> {
    let lines: Vec<&str> = output.lines().collect();
    let records = lines
        .get(1..lines.len().saturating_sub(1))
        .unwrap_or_default();
    records
        .iter()
        .filter(|line| !line.starts_with(' '))
        .filter_map(|line| {
            let fields: Vec<&str> = line.split(' ').collect();
            matches!(fields[0], "error" | "incomplete" | "internal").then(|| {
                (
                    fields[0].to_owned(),
                    fields[1].to_owned(),
                    fields[2].to_owned(),
                )
            })
        })
        .collect()
}

fn run(example: &Example, dir: &Path) -> Result<(), String> {
    let marker = example.marker();
    let reported = marker.as_ref().filter(|marker| {
        marker.class != "traps"
            && Code::from_name(&marker.code).is_some()
            && !PARTIAL.contains(&marker.code.as_str())
    });
    let expected: Vec<(String, String, String)> = reported
        .map(|marker| {
            (
                marker.class.to_owned(),
                marker.code.clone(),
                format!("{}:{}", marker.path, marker.line),
            )
        })
        .into_iter()
        .collect();
    let status = Some(i32::from(
        expected.iter().any(|(class, _, _)| class == "error"),
    ));
    example
        .write(dir, false)
        .map_err(|error| error.to_string())?;
    let (output, code) = nan(dir, &["check"])?;
    if diagnostics(&output) != expected || code != status {
        return Err(format!(
            "nan check exits with {code:?} and reports\n{output}expected {expected:?}"
        ));
    }

    example
        .write(dir, true)
        .map_err(|error| error.to_string())?;
    let (output, code) = nan(dir, &["fmt"])?;
    if code != status {
        return Err(format!("nan fmt exits with {code:?}:\n{output}"));
    }
    let reorders_clauses = marker
        .as_ref()
        .is_some_and(|marker| marker.code == "item.clause-order");
    for (path, lines) in &example.files {
        let mut expected: Vec<String> = lines.iter().map(|line| unmark(line).to_owned()).collect();
        if example.prepended {
            expected.insert(1, String::new());
        }
        let written = fs::read_to_string(dir.join(path)).map_err(|error| error.to_string())?;
        let mut written: Vec<String> = written.lines().map(str::to_owned).collect();
        if reorders_clauses {
            expected.sort();
            written.sort();
        }
        if written != expected {
            return Err(format!(
                "nan fmt rewrites {path}:\n{output}{}",
                written.join("\n")
            ));
        }
    }
    Ok(())
}

fn chapter(path: &Utf8Path, text: String) -> datatest_stable::Result<()> {
    let chapter = path.file_stem().expect("a chapter file");
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join("conformance")
        .join(chapter);
    let mut failures = Vec::new();
    for example in examples(&text) {
        let dir = root.join(example.line.to_string());
        match run(&example, &dir) {
            Ok(()) => fs::remove_dir_all(&dir)?,
            Err(failure) => failures.push(format!("{path}:{}: {failure}", example.line)),
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("\n\n").into())
    }
}
