//! The `nan` blocks of the specification as workspaces: reading them from a chapter, writing their
//! files and preparing their packages as the conformance harness runs them, shared by the
//! conformance and self-check harnesses.

use std::fs;
use std::path::Path;

pub const SUMMARY: &str = "//! Specification example.";

pub const MANIFEST: &str = "//! Specification example.

package example {
    language 1
    targets any
} example
";

/// A `nan` block: the line of the specification it starts on, its files, what running its
/// program writes when its section gives it, and the commands of its section's `text` blocks with
/// their exact outputs.
pub struct Example {
    pub line: usize,
    pub files: Vec<(String, Vec<String>)>,
    pub prepended: bool,
    pub written: Option<Written>,
    pub commands: Vec<(String, String)>,
}

/// What a program writes when run, and the status it exits with.
#[allow(
    dead_code,
    reason = "the conformance harness reads it, the self-check harness does not"
)]
pub struct Written {
    pub text: String,
    pub status: i32,
}

impl Example {
    /// The package directories of the block, each after the packages its sources name.
    pub fn packages(&self) -> Vec<String> {
        let mut deps: Vec<(String, Vec<String>)> = Vec::new();
        if self.prepended {
            deps.push(("example".to_owned(), Vec::new()));
        }
        for (path, lines) in &self.files {
            let Some(package) = path.strip_suffix("/package.nan") else {
                continue;
            };
            let named = lines
                .iter()
                .filter_map(|line| source(line))
                .map(|source| match source.strip_prefix("../") {
                    Some(dir) => dir.to_owned(),
                    None => named_package(&source).to_owned(),
                })
                .collect();
            deps.push((package.to_owned(), named));
        }
        let mut ordered: Vec<String> = Vec::new();
        while ordered.len() < deps.len() {
            let before = ordered.len();
            for (package, named) in &deps {
                let ready = named.iter().all(|dep| {
                    ordered.contains(dep) || !deps.iter().any(|(known, _)| known == dep)
                });
                if !ordered.contains(package) && ready {
                    ordered.push(package.clone());
                }
            }
            if ordered.len() == before {
                for (package, _) in &deps {
                    if !ordered.contains(package) {
                        ordered.push(package.clone());
                    }
                }
            }
        }
        ordered
    }

    /// The store names that the `dep` directives of the block write for a package directory,
    /// in byte order.
    pub fn store_names(&self, package: &str) -> Vec<String> {
        let mut names: Vec<String> = self
            .files
            .iter()
            .filter(|(path, _)| path.ends_with("/package.nan"))
            .flat_map(|(_, lines)| lines.iter().filter_map(|line| source(line)))
            .filter(|source| !source.starts_with('.') && named_package(source) == package)
            .collect();
        names.sort();
        names.dedup();
        names
    }

    /// Writes the files into `dir`, without their marker comments when `unmarked`; the marked
    /// files into a new directory, the unmarked ones over them, keeping the machine files.
    pub fn write(&self, dir: &Path, unmarked: bool) -> std::io::Result<()> {
        if dir.exists() && !unmarked {
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

    /// Locks and snapshots each package of the block and publishes the packages its `dep`
    /// directives name by store name, after the packages they name.
    pub fn prepare(&self, dir: &Path) -> Result<(), String> {
        for package in self.packages() {
            for command in [&["dep", "sync"][..], &["api", "update"]] {
                nan(&dir.join(&package), command)?;
            }
            for name in self.store_names(&package) {
                nan(dir, &["publish", "--local", &package, "--name", &name])?;
            }
        }
        Ok(())
    }
}

/// A line without the marker comment ending it, and the space before it.
pub fn unmark(line: &str) -> &str {
    [" // error: ", " // incomplete: ", " // traps: "]
        .iter()
        .find_map(|marker| line.rfind(marker).map(|at| &line[..at]))
        .unwrap_or(line)
}

/// The text a `text` block starting after line `index` gives as what a program writes, when the
/// sentence before it says so: the block's text and the exit status the sentence states.
pub fn program_output(lines: &[&str], index: usize) -> Option<Written> {
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

/// The commands of a `text` block starting after line `index`, each the text after `$ ` with the
/// lines up to the next command.
pub fn commands(lines: &[&str], index: usize) -> Vec<(String, String)> {
    let mut commands: Vec<(String, String)> = Vec::new();
    for line in lines[index + 1..].iter().take_while(|line| **line != "```") {
        match line.strip_prefix("$ ") {
            Some(command) => commands.push((command.to_owned(), String::new())),
            None => {
                if let Some((_, output)) = commands.last_mut() {
                    output.push_str(line);
                    output.push('\n');
                }
            }
        }
    }
    commands
}

/// The `nan` blocks of a chapter, each placed in its workspace as the charter states.
pub fn examples(text: &str) -> Vec<Example> {
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
            && let Some(example) = examples.last_mut()
        {
            if let Some(written) = program_output(&lines, index) {
                example.written = Some(written);
            }
            example.commands.extend(commands(&lines, index));
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
            commands: Vec::new(),
        });
        same_section = true;
        index = end + 1;
    }
    examples
}

/// The source of a `dep` directive's line.
pub fn source(line: &str) -> Option<String> {
    let source = line.trim().strip_prefix("dep ")?.split_once(" \"")?.1;
    Some(source.split('"').next()?.to_owned())
}

/// The package directory a store name names: its last segment.
pub fn named_package(store_name: &str) -> &str {
    store_name.rsplit('/').next().unwrap_or(store_name)
}

/// Runs `nan` with `args` at `dir`, returning its output and exit status.
pub fn nan(dir: &Path, args: &[&str]) -> Result<(String, Option<i32>), String> {
    let args: Vec<String> = args.iter().map(|arg| (*arg).to_owned()).collect();
    let run = nan::run(&args, dir);
    match run.launch {
        Some(launch) => Err(format!(
            "nan {} launches {}",
            args.join(" "),
            launch.program.display()
        )),
        None => Ok((run.output, Some(run.status))),
    }
}
