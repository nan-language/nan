//! One model's run of one task: the conversation, the workspace the model edits through its
//! replies, and what the metrics read from the run.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use serde_json::{Value, json};

use crate::corpus::{self, Kind, Task};
use crate::model::{Model, Turn};

/// The most replies a run takes.
const MAX_TURNS: usize = 12;

/// The most bytes of a command's output a reply gets back.
const OUTPUT_BUDGET: usize = 12_000;

/// What the protocol tells a model, after the primer.
const PROTOCOL: &str = "You act on a workspace only through blocks in your replies, each opening and closing on lines of its own:

<<<write <path>
<the file's whole new content>
>>>

<<<run
nan <command and arguments>
>>>

<<<answer <question>
<your answer, alone>
>>>

<<<submit>>>

A write block replaces or creates a file, its path relative to the workspace root. A run block runs one `nan` command in the workspace root; the outputs come back in the next message. `nan primer <topic>` prints a topic of the primer and `nan primer tools` its tool card. A write or repair task ends at `<<<submit>>>`; a read task ends with the reply holding your answers.";

/// What a run gives the metrics.
pub struct Outcome {
    /// For a write task: whether `nan check` accepted, without error, the first workspace the
    /// model compiled, by running a command that checks it or by submitting.
    pub first_compile: Option<bool>,
    /// For each diagnostic code: how many diagnostics with it the model was shown, and how many
    /// its next edit removed without introducing an error.
    pub fixes: BTreeMap<String, (usize, usize)>,
    /// For a read task: each question and whether its answer was right.
    pub answers: Vec<(String, bool)>,
    /// For a write or repair task: whether its acceptance tests pass on what was submitted.
    pub accepted: Option<bool>,
    pub turns: usize,
}

impl Outcome {
    pub fn record(&self, task: &Task, model: &Model) -> Value {
        let fixes: BTreeMap<&String, [usize; 2]> = self
            .fixes
            .iter()
            .map(|(code, (shown, fixed))| (code, [*shown, *fixed]))
            .collect();
        let answers: BTreeMap<&String, bool> = self
            .answers
            .iter()
            .map(|(question, right)| (question, *right))
            .collect();
        json!({
            "model": model.name(),
            "task": task.id,
            "kind": task.kind.word(),
            "area": task.area,
            "first_compile": self.first_compile,
            "fixes": fixes,
            "answers": answers,
            "accepted": self.accepted,
            "turns": self.turns,
        })
    }
}

/// An action of a reply.
enum Action {
    Write(String, String),
    Run(String),
    Answer(String, String),
    Submit,
}

/// The actions of a reply, in order; text outside blocks is ignored.
fn actions(reply: &str) -> Vec<Action> {
    let mut found = Vec::new();
    let mut lines = reply.lines();
    while let Some(line) = lines.next() {
        let line = line.trim_end();
        if line == "<<<submit>>>" {
            found.push(Action::Submit);
            continue;
        }
        let Some(head) = line.strip_prefix("<<<") else {
            continue;
        };
        let mut body = String::new();
        for inner in lines.by_ref() {
            if inner.trim_end() == ">>>" {
                break;
            }
            body.push_str(inner);
            body.push('\n');
        }
        let (word, argument) = head.split_once(' ').unwrap_or((head, ""));
        match word {
            "write" => found.push(Action::Write(argument.trim().to_owned(), body)),
            "run" => found.push(Action::Run(body.trim().to_owned())),
            "answer" => found.push(Action::Answer(argument.trim().to_owned(), body)),
            _ => {}
        }
    }
    found
}

/// A diagnostic as runs are compared: its code, file and item.
type Key = (String, String, String);

/// The errors of a `nan check` output.
fn errors(output: &str) -> Vec<Key> {
    output
        .lines()
        .filter_map(|line| {
            let fields: Vec<&str> = line.split(' ').collect();
            (fields.first() == Some(&"error") && fields.len() >= 4).then(|| {
                let file = fields[2].split(':').next().unwrap_or_default();
                (fields[1].to_owned(), file.to_owned(), fields[3].to_owned())
            })
        })
        .collect()
}

/// Runs `nan` in the workspace and gives its output.
fn nan(workspace: &Path, args: &[String]) -> String {
    nan::run(args, workspace).output
}

/// The system prompt: the primer's core, printed in `dir`, which no workspace holds, then the
/// protocol.
pub fn system(dir: &Path) -> String {
    format!(
        "You are an agent writing NaN, a programming language you have not seen before. Its primer follows; it is the only documentation you get.\n\n{}\n{PROTOCOL}",
        nan::run(&["primer".to_owned()], dir).output
    )
}

/// Runs a task with a model in a fresh copy of its workspace under `dir`.
pub fn run(task: &Task, model: &Model, system: &str, dir: &Path) -> Result<Outcome, String> {
    if dir.exists() {
        fs::remove_dir_all(dir).map_err(|error| error.to_string())?;
    }
    fs::create_dir_all(dir).map_err(|error| error.to_string())?;
    let start = corpus::files(&task.workspace())?;
    corpus::write(dir, &start)?;
    let mut opening = format!("{}\n\nThe workspace holds these files:\n", task.prompt);
    for (path, text) in &start {
        opening.push_str(&format!("\n{path}:\n```\n{text}```\n"));
    }
    let mut outcome = Outcome {
        first_compile: None,
        fixes: BTreeMap::new(),
        answers: Vec::new(),
        accepted: None,
        turns: 0,
    };
    let checked = nan(dir, &["check".to_owned()]);
    let mut shown: Vec<Key> = Vec::new();
    let mut before = errors(&checked);
    if task.kind == Kind::Repair {
        opening.push_str(&format!("\n`nan check` reports:\n```\n{checked}```\n"));
        shown = before.clone();
    }
    let mut turns = vec![Turn {
        user: true,
        text: opening,
    }];
    let mut finished = false;
    while !finished && outcome.turns < MAX_TURNS {
        let reply = model.reply(system, &turns, task)?;
        outcome.turns += 1;
        turns.push(Turn {
            user: false,
            text: reply.clone(),
        });
        let mut outputs = String::new();
        let mut edited = false;
        let mut compiled = false;
        let mut seen: Option<Vec<Key>> = None;
        for action in actions(&reply) {
            match action {
                Action::Write(path, text) => {
                    if path.is_empty()
                        || path.starts_with('/')
                        || path.split('/').any(|part| part == "..")
                    {
                        outputs.push_str(&format!("refused: {path} lies outside the workspace\n"));
                        continue;
                    }
                    corpus::write(dir, &[(path, text)])?;
                    edited = true;
                }
                Action::Run(command) => {
                    let args: Vec<String> = command.split_whitespace().map(str::to_owned).collect();
                    let output = match args.split_first() {
                        Some((first, rest))
                            if first == "nan" && rest.first().is_some_and(|word| word != "run") =>
                        {
                            let output = nan(dir, rest);
                            if ["check", "test", "build"].contains(&rest[0].as_str()) {
                                compiled = true;
                                seen = Some(errors(&output));
                            }
                            output
                        }
                        _ => "refused: only `nan` commands other than `nan run` run here\n"
                            .to_owned(),
                    };
                    let mut output = output;
                    if output.len() > OUTPUT_BUDGET {
                        let cut = (0..=OUTPUT_BUDGET)
                            .rev()
                            .find(|at| output.is_char_boundary(*at))
                            .unwrap_or(0);
                        output.truncate(cut);
                        output.push_str("\n[output cut]\n");
                    }
                    outputs.push_str(&format!("$ {command}\n{output}"));
                }
                Action::Answer(question, answer) => {
                    let wanted = task.answers.iter().find(|(asked, _)| *asked == question);
                    let right = wanted.is_some_and(|(_, expected)| same_answer(&answer, expected));
                    outcome.answers.retain(|(asked, _)| *asked != question);
                    outcome.answers.push((question, right));
                    finished |= task.kind == Kind::Read;
                }
                Action::Submit => {
                    compiled = true;
                    finished |= task.kind != Kind::Read;
                }
            }
        }
        if edited || compiled {
            let after = errors(&nan(dir, &["check".to_owned()]));
            if edited && !shown.is_empty() {
                fixed(&shown, &before, &after, &mut outcome.fixes);
                shown.clear();
            }
            if compiled && task.kind == Kind::Write && outcome.first_compile.is_none() {
                outcome.first_compile = Some(after.is_empty());
            }
            before = after;
        }
        if let Some(seen) = seen {
            shown = seen;
        }
        if !finished {
            if outputs.is_empty() {
                outputs.push_str("No block ran. Act through blocks, and end with `<<<submit>>>` or your answers.\n");
            }
            turns.push(Turn {
                user: true,
                text: outputs,
            });
        }
    }
    if task.kind != Kind::Read {
        corpus::write(dir, &corpus::files(&task.accept())?)?;
        let tested = nan(dir, &["test".to_owned()]);
        outcome.accepted = Some(tested.starts_with("ok "));
    }
    for (question, _) in &task.answers {
        if !outcome.answers.iter().any(|(asked, _)| asked == question) {
            outcome.answers.push((question.clone(), false));
        }
    }
    Ok(outcome)
}

/// Counts, per code, the shown diagnostics and those an edit removed without introducing an
/// error: none when the check after it holds an error the one before did not.
fn fixed(
    shown: &[Key],
    before: &[Key],
    after: &[Key],
    fixes: &mut BTreeMap<String, (usize, usize)>,
) {
    let mut remaining = before.to_vec();
    let mut introduced = false;
    for key in after {
        match remaining.iter().position(|held| held == key) {
            Some(at) => drop(remaining.remove(at)),
            None => introduced = true,
        }
    }
    let mut left = after.to_vec();
    for key in shown {
        let entry = fixes.entry(key.0.clone()).or_default();
        entry.0 += 1;
        match left.iter().position(|held| held == key) {
            Some(at) => drop(left.remove(at)),
            None if !introduced => entry.1 += 1,
            None => {}
        }
    }
}

/// Whether an answer matches the expected one, ignoring case, surrounding space, backticks and
/// quotes, and a final period.
fn same_answer(given: &str, expected: &str) -> bool {
    let normal = |text: &str| {
        text.trim()
            .trim_end_matches('.')
            .trim_matches(|c| c == '`' || c == '"' || c == '\'')
            .trim()
            .to_lowercase()
    };
    normal(given) == normal(expected)
}
