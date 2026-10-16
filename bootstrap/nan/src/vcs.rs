//! The git history of a VCS workspace as the author rule reads it: its changes, the uncommitted
//! state of the working tree being one more, each with its author and parents, and the
//! workspace's files in each commit's tree.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// A change: the uncommitted state of the working tree, or a commit by its object name.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Change {
    Uncommitted,
    Commit(String),
}

/// The git working tree holding a workspace.
pub struct Vcs {
    top: PathBuf,
    /// The workspace root relative to the working tree's top level, `/`-separated, empty at it.
    prefix: String,
    /// The author of the uncommitted change: the email the git configuration gives, lowercased.
    email: String,
}

impl Vcs {
    /// The working tree holding the workspace at `root`; none outside every git working tree.
    pub fn open(root: &Path) -> Option<Vcs> {
        let top = git(root, &["rev-parse", "--show-toplevel"])?;
        let top = PathBuf::from(top.trim_end());
        let prefix = root
            .canonicalize()
            .ok()?
            .strip_prefix(top.canonicalize().ok()?)
            .ok()?
            .to_string_lossy()
            .replace('\\', "/");
        let email = git(&top, &["config", "user.email"]).unwrap_or_default();
        Some(Vcs {
            email: email.trim().to_ascii_lowercase(),
            top,
            prefix,
        })
    }

    /// A change's author: its author email, lowercased.
    pub fn author(&self, change: &Change) -> String {
        match change {
            Change::Uncommitted => self.email.clone(),
            Change::Commit(name) => git(&self.top, &["log", "-1", "--format=%ae", name])
                .unwrap_or_default()
                .trim()
                .to_ascii_lowercase(),
        }
    }

    /// The commits whose trees change the workspace, most recent first, each with its nearest
    /// ancestors that change it, as git simplifies the history of the workspace's directory.
    pub fn history(&self) -> Vec<(String, Vec<String>)> {
        let mut args = vec!["rev-list", "--parents", "HEAD"];
        if !self.prefix.is_empty() {
            args.extend(["--", &self.prefix]);
        }
        git(&self.top, &args)
            .unwrap_or_default()
            .lines()
            .filter_map(|line| {
                let mut names = line.split_whitespace().map(str::to_owned);
                Some((names.next()?, names.collect()))
            })
            .collect()
    }

    /// The commits changing a file of the workspace whose path within it matches a glob.
    pub fn changing(&self, glob: &str) -> Vec<String> {
        let pattern = match self.prefix.is_empty() {
            true => format!(":(glob){glob}"),
            false => format!(":(glob){}/{glob}", self.prefix),
        };
        git(&self.top, &["rev-list", "HEAD", "--", &pattern])
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    /// The files of the workspace in a commit's tree that `wanted` keeps, by path relative to
    /// the workspace root.
    pub fn files(&self, commit: &str, wanted: &dyn Fn(&str) -> bool) -> Vec<(String, Vec<u8>)> {
        let mut args = vec!["ls-tree", "-r", "-z", "--name-only", commit];
        if !self.prefix.is_empty() {
            args.extend(["--", &self.prefix]);
        }
        let listed = git(&self.top, &args).unwrap_or_default();
        let within = |path: &str| -> Option<String> {
            match self.prefix.is_empty() {
                true => Some(path.to_owned()),
                false => path
                    .strip_prefix(&format!("{}/", self.prefix))
                    .map(str::to_owned),
            }
        };
        let paths: Vec<&str> = listed
            .split('\0')
            .filter(|path| within(path).is_some_and(|path| wanted(&path)))
            .collect();
        let Ok(mut batch) = Command::new("git")
            .arg("-C")
            .arg(&self.top)
            .args(["cat-file", "--batch"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
        else {
            return Vec::new();
        };
        let mut input = batch.stdin.take().expect("a piped stdin");
        let requests: String = paths
            .iter()
            .map(|path| format!("{commit}:{path}\n"))
            .collect();
        let writer = std::thread::spawn(move || input.write_all(requests.as_bytes()));
        let mut output = BufReader::new(batch.stdout.take().expect("a piped stdout"));
        let mut files = Vec::new();
        for path in &paths {
            let mut header = String::new();
            if output.read_line(&mut header).unwrap_or(0) == 0 {
                break;
            }
            let Some(size) = header
                .split_whitespace()
                .nth(2)
                .and_then(|size| size.parse::<usize>().ok())
            else {
                continue;
            };
            let mut content = vec![0; size + 1];
            if output.read_exact(&mut content).is_err() {
                break;
            }
            content.pop();
            if let Some(within) = within(path) {
                files.push((within, content));
            }
        }
        drop(writer.join());
        drop(batch.wait());
        files
    }
}

/// The standard output of a git command run in `dir`, none when it fails.
fn git(dir: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .stderr(Stdio::null())
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}
