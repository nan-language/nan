//! The task corpus: each directory of `nanbench/tasks/` is a task, its `task.md` giving its kind,
//! its area and the prompt an agent reads; `workspace/` holds the workspace it starts from,
//! `accept/` the files its acceptance tests add, `solution/` the files of a reference solution,
//! and `answers.txt` a read task's expected answers, one `<question> <answer>` per line.

use std::fs;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// Write code the prompt describes, from a workspace that lacks it.
    Write,
    /// Fix a workspace that `nan check` rejects.
    Repair,
    /// Answer questions about a workspace.
    Read,
}

impl Kind {
    pub fn word(self) -> &'static str {
        match self {
            Kind::Write => "write",
            Kind::Repair => "repair",
            Kind::Read => "read",
        }
    }
}

pub struct Task {
    pub id: String,
    pub kind: Kind,
    /// Library, host program, driver or kernel code.
    pub area: String,
    pub prompt: String,
    pub dir: PathBuf,
    /// A read task's questions with their expected answers.
    pub answers: Vec<(String, String)>,
}

impl Task {
    pub fn workspace(&self) -> PathBuf {
        self.dir.join("workspace")
    }

    pub fn accept(&self) -> PathBuf {
        self.dir.join("accept")
    }

    pub fn solution(&self) -> PathBuf {
        self.dir.join("solution")
    }
}

/// The tasks of the corpus under `dir`, by id.
pub fn load(dir: &Path) -> Result<Vec<Task>, String> {
    let fault = |error: std::io::Error| error.to_string();
    let mut tasks = Vec::new();
    for entry in fs::read_dir(dir).map_err(fault)? {
        let dir = entry.map_err(fault)?.path();
        if dir.join("task.md").exists() {
            tasks.push(task(&dir)?);
        }
    }
    tasks.sort_by(|left, right| left.id.cmp(&right.id));
    Ok(tasks)
}

/// A task from its directory: `task.md` opens with `kind: <kind>` and `area: <area>` lines, then
/// a blank line and the prompt.
fn task(dir: &Path) -> Result<Task, String> {
    let id = dir
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or("a task directory without a name")?
        .to_owned();
    let text = fs::read_to_string(dir.join("task.md")).map_err(|error| error.to_string())?;
    let (header, prompt) = text
        .split_once("\n\n")
        .ok_or_else(|| format!("{id}: task.md has no prompt"))?;
    let field = |name: &str| {
        header
            .lines()
            .find_map(|line| line.strip_prefix(&format!("{name}: ")))
            .map(str::to_owned)
            .ok_or_else(|| format!("{id}: task.md names no {name}"))
    };
    let kind = match field("kind")?.as_str() {
        "write" => Kind::Write,
        "repair" => Kind::Repair,
        "read" => Kind::Read,
        other => return Err(format!("{id}: unknown kind {other}")),
    };
    let answers = match kind {
        Kind::Read => fs::read_to_string(dir.join("answers.txt"))
            .map_err(|error| format!("{id}: {error}"))?
            .lines()
            .filter_map(|line| line.split_once(' '))
            .map(|(question, answer)| (question.to_owned(), answer.to_owned()))
            .collect(),
        _ => Vec::new(),
    };
    let area = field("area")?;
    Ok(Task {
        id,
        kind,
        area,
        prompt: prompt.trim().to_owned(),
        dir: dir.to_owned(),
        answers,
    })
}

/// The files under `dir`, by path relative to it in byte order.
pub fn files(dir: &Path) -> Result<Vec<(String, String)>, String> {
    let mut found = Vec::new();
    collect(dir, "", &mut found).map_err(|error| error.to_string())?;
    found.sort();
    Ok(found)
}

fn collect(dir: &Path, prefix: &str, found: &mut Vec<(String, String)>) -> std::io::Result<()> {
    if !dir.exists() {
        return Ok(());
    }
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        let path = format!("{prefix}{name}");
        match entry.file_type()?.is_dir() {
            true => collect(&entry.path(), &format!("{path}/"), found)?,
            false => found.push((path, fs::read_to_string(entry.path())?)),
        }
    }
    Ok(())
}

/// Writes files into `dir`, each at its relative path.
pub fn write(dir: &Path, files: &[(String, String)]) -> Result<(), String> {
    for (path, text) in files {
        let target = dir.join(path);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        fs::write(&target, text).map_err(|error| error.to_string())?;
    }
    Ok(())
}
