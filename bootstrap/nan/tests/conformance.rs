//! The conformance harness: runs each `nan` example of a specification chapter against its marker
//! comments, builds its `x86_64-host` programs and compares what one writes with the text its
//! section gives, and finds each item of its `nan-api` blocks in the std sources, one test per
//! chapter.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use datatest_stable::Utf8Path;

datatest_stable::harness! {
    { test = chapter, root = "../../docs/spec", pattern = r"^\d\d-[a-z-]+\.md$" },
}

/// The test-run codes, which `nan test` gives as a failing test's cause.
const TEST_RUN: &[&str] = &[
    "test.emulator-unavailable",
    "test.escaped-failure",
    "test.stalled",
    "test.target-not-run",
    "test.timeout",
];

/// The codes only a final build reports.
const FINAL_BUILD: &[&str] = &["limit.stack-exceeded", "limit.stack-unbounded"];

/// The families of the gate codes, which only the release gate reports.
const GATE: &[&str] = &["api.", "seal.", "trust."];

/// Whether check time reports a code.
fn checked(code: &str) -> bool {
    !TEST_RUN.contains(&code)
        && !FINAL_BUILD.contains(&code)
        && !GATE.iter().any(|family| code.starts_with(family))
}

const SUMMARY: &str = "//! Specification example.";

/// The std sources, relative to the crate.
const STD: &str = "../../std";

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

/// A `nan` block: the line of the specification it starts on, its files, and what running its
/// program writes when its section gives it.
struct Example {
    line: usize,
    files: Vec<(String, Vec<String>)>,
    prepended: bool,
    written: Option<Written>,
}

/// What a program writes when run, and the status it exits with.
struct Written {
    text: String,
    status: i32,
}

impl Example {
    /// Whether the block is a kernel conformance test: a package of it declares `x86_64-uefi`
    /// or `x86_64-bare`. Its tests need the emulator, which the harness does not start.
    fn kernel(&self) -> bool {
        self.files.iter().any(|(path, lines)| {
            path.ends_with("/package.nan")
                && lines.iter().any(|line| {
                    let line = line.trim();
                    line.starts_with("targets ")
                        && (line.contains("x86_64-uefi") || line.contains("x86_64-bare"))
                })
        })
    }

    /// The names of the packages of the block that are `x86_64-host` programs.
    fn host_programs(&self) -> Vec<String> {
        self.files
            .iter()
            .filter(|(path, lines)| {
                path.ends_with("/package.nan")
                    && lines.iter().any(|line| line.trim() == "entry main")
                    && lines
                        .iter()
                        .any(|line| line.trim() == "targets x86_64-host")
            })
            .filter_map(|(path, _)| path.split_once('/').map(|(package, _)| package.to_owned()))
            .collect()
    }

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

/// The text a `text` block starting after line `index` gives as what a program writes, when the
/// sentence before it says so: the block's text and the exit status the sentence states.
fn program_output(lines: &[&str], index: usize) -> Option<Written> {
    let sentence = lines[..index].iter().rev().find(|line| !line.is_empty())?;
    if !sentence.starts_with("Running this program writes") {
        return None;
    }
    let status = sentence
        .split_once("exits with status ")
        .and_then(|(_, rest)| rest.trim_end_matches(':').parse().ok())
        .unwrap_or(0);
    let end = index + 1 + lines[index + 1..].iter().position(|line| *line == "```")?;
    let mut text = String::new();
    for line in &lines[index + 1..end] {
        text.push_str(line);
        text.push('\n');
    }
    Some(Written { text, status })
}

/// The `nan` blocks of a chapter, each placed in its workspace as the charter states.
fn examples(text: &str) -> Vec<Example> {
    let lines: Vec<&str> = text.lines().collect();
    let mut examples: Vec<Example> = Vec::new();
    let mut same_section = false;
    let mut index = 0;
    while index < lines.len() {
        if lines[index].starts_with('#') {
            same_section = false;
        }
        if lines[index] == "```text"
            && same_section
            && let Some(written) = program_output(&lines, index)
            && let Some(example) = examples.last_mut()
        {
            example.written = Some(written);
        }
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
            written: None,
        });
        same_section = true;
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
    let reported = marker
        .as_ref()
        .filter(|marker| marker.class != "traps" && checked(&marker.code));
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
    let gaps = nan::context_gaps(dir).map_err(|error| error.to_string())?;
    if !gaps.is_empty() {
        return Err(format!("context packs lack facts:\n{}", gaps.join("\n")));
    }
    if status == Some(0) {
        if !example.kernel() {
            test(marker.as_ref(), &expected, dir)?;
        }
        let test_run = marker
            .as_ref()
            .is_some_and(|marker| TEST_RUN.contains(&marker.code.as_str()));
        if !test_run {
            build(example, marker.as_ref(), &expected, dir)?;
        }
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

/// The cause, location and first held line of each `fail` record of an output.
fn failures(output: &str) -> Vec<(String, String, String)> {
    let lines: Vec<&str> = output.lines().collect();
    let mut found = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        let fields: Vec<&str> = line.split(' ').collect();
        if fields[0] != "fail" || fields.len() != 5 {
            continue;
        }
        let first = lines
            .get(index + 1)
            .and_then(|next| next.strip_prefix("    "))
            .unwrap_or_default();
        found.push((fields[4].to_owned(), fields[2].to_owned(), first.to_owned()));
    }
    found
}

/// Runs the tests of an example: `nan test` reports what check time reports, and every test
/// passes but the one a test-run code or trap marker names, which fails with that cause, its
/// head on the marked line for a test-run code, its trap report naming that line for a trap.
fn test(
    marker: Option<&Marker>,
    checked: &[(String, String, String)],
    dir: &Path,
) -> Result<(), String> {
    let (output, code) = nan(dir, &["test"])?;
    let failing =
        marker.filter(|marker| marker.class == "traps" || TEST_RUN.contains(&marker.code.as_str()));
    let failures = failures(&output);
    let expected = match failing {
        Some(marker) => {
            let location = format!("{}:{}", marker.path, marker.line);
            failures.len() == 1 && {
                let (cause, at, first) = &failures[0];
                *cause == marker.code
                    && match marker.class {
                        "traps" => first.starts_with(&format!("{cause} at {location} ")),
                        _ => *at == location,
                    }
            }
        }
        None => failures.is_empty(),
    };
    let status = Some(i32::from(failing.is_some()));
    if diagnostics(&output) != checked || !expected || code != status {
        return Err(format!(
            "nan test exits with {code:?} and reports\n{output}expected {checked:?} and {:?}",
            failing.map(|marker| &marker.code)
        ));
    }
    Ok(())
}

/// Builds each `x86_64-host` program of an example: every image builds, reporting what check
/// time reports and the one diagnostic a final-build marker states, and a program whose
/// section gives what it writes writes exactly that and exits with the status stated.
fn build(
    example: &Example,
    marker: Option<&Marker>,
    checked: &[(String, String, String)],
    dir: &Path,
) -> Result<(), String> {
    for program in example.host_programs() {
        let mut expected = checked.to_vec();
        let final_marker = marker.filter(|marker| FINAL_BUILD.contains(&marker.code.as_str()));
        if let Some(marker) = final_marker {
            expected.push((
                marker.class.to_owned(),
                marker.code.clone(),
                format!("{}:{}", marker.path, marker.line),
            ));
        }
        let (output, code) = nan(dir, &["build", &program])?;
        let status = i32::from(final_marker.is_some());
        if diagnostics(&output) != expected || code != Some(status) {
            return Err(format!(
                "nan build {program} exits with {code:?} and reports\n{output}expected {expected:?}"
            ));
        }
        let Some(written) = example.written.as_ref().filter(|_| final_marker.is_none()) else {
            continue;
        };
        let image = dir.join(".nan/images/x86_64-host").join(&program);
        let run = Command::new(&image)
            .current_dir(dir)
            .output()
            .map_err(|error| error.to_string())?;
        let mut text = String::from_utf8_lossy(&run.stdout).into_owned();
        text.push_str(&String::from_utf8_lossy(&run.stderr));
        if text != written.text || run.status.code() != Some(written.status) {
            return Err(format!(
                "{program} exits with {:?} and writes\n{text}expected status {} and\n{}",
                run.status.code(),
                written.status,
                written.text
            ));
        }
    }
    Ok(())
}

/// The items of a declaration text, each the lines from a column-0 head to before the next,
/// without trailing blank lines.
fn chunks(lines: &[&str]) -> Vec<Vec<String>> {
    let mut chunks: Vec<Vec<String>> = Vec::new();
    for line in lines {
        let starts = !line.is_empty()
            && !line.starts_with(' ')
            && !line.starts_with('}')
            && !line.starts_with("//");
        if starts {
            chunks.push(Vec::new());
        }
        if let Some(chunk) = chunks.last_mut() {
            chunk.push((*line).to_owned());
        }
    }
    for chunk in &mut chunks {
        while chunk.last().is_some_and(|line| line.is_empty()) {
            chunk.pop();
        }
    }
    chunks
}

/// The items the source files of a std module declare.
fn std_items(module: &str) -> std::io::Result<Vec<Vec<String>>> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(STD)
        .join(module.replace("::", "/"));
    let mut items = Vec::new();
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if path.extension().is_some_and(|extension| extension == "nan") {
            let text = fs::read_to_string(path)?;
            items.extend(chunks(&text.lines().collect::<Vec<_>>()));
        }
    }
    Ok(items)
}

/// Whether a std item declares a `nan-api` block's item with exactly its text: the same head and
/// members, the std item adding only a body, clauses and private fields.
fn declares(declared: &[String], published: &[String]) -> bool {
    let head = &published[0];
    if declared[0] != *head && declared[0] != format!("{head} {{") {
        return false;
    }
    let function = head.contains("fn ");
    let text: &[String] = match declared.iter().position(|line| line == "{") {
        Some(opener) if function => &declared[..opener],
        _ if function && declared[0] != *head => &declared[..1],
        _ => declared,
    };
    let mut lines = published.iter().peekable();
    for (index, line) in text.iter().enumerate() {
        let matches = lines.peek().is_some_and(|next| {
            *next == line || (index == 0 && line.strip_suffix(" {") == Some(next.as_str()))
        });
        if matches {
            lines.next();
            continue;
        }
        let extra = match function {
            true => line.starts_with("    "),
            false => {
                line.starts_with("    ")
                    && !line.starts_with("    pub ")
                    && !line.starts_with("    pkg ")
            }
        };
        if !extra {
            return false;
        }
    }
    lines.next().is_none()
}

/// The items of each `nan-api` block of a chapter that their std module does not declare with
/// exactly their text, by the line of the block.
fn api_failures(text: &str) -> std::io::Result<Vec<String>> {
    let lines: Vec<&str> = text.lines().collect();
    let mut failures = Vec::new();
    let mut index = 0;
    while index < lines.len() {
        if lines[index] != "```nan-api" {
            index += 1;
            continue;
        }
        let module = lines[index + 1]
            .strip_prefix("// module: ")
            .expect("a block names its module");
        let start = index + 2;
        let end = start
            + lines[start..]
                .iter()
                .position(|line| *line == "```")
                .expect("a block ends");
        let declared = std_items(module)?;
        for published in chunks(&lines[start..end]) {
            if !declared.iter().any(|item| declares(item, &published)) {
                failures.push(format!(
                    "{}: {module} lacks\n{}",
                    index + 1,
                    published.join("\n")
                ));
            }
        }
        index = end + 1;
    }
    Ok(failures)
}

fn chapter(path: &Utf8Path, text: String) -> datatest_stable::Result<()> {
    let chapter = path.file_stem().expect("a chapter file");
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join("conformance")
        .join(chapter);
    let mut failures: Vec<String> = api_failures(&text)?
        .into_iter()
        .map(|failure| format!("{path}:{failure}"))
        .collect();
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
