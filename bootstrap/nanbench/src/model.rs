//! The models nanbench runs: each answers a conversation of user and assistant turns under a
//! system prompt.

use std::io::Write;
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

use serde_json::{Value, json};

use crate::corpus::{self, Kind, Task};

pub enum Model {
    /// A model of the Anthropic Messages API, by id, reached with `curl` and the key that
    /// `ANTHROPIC_API_KEY` holds.
    Anthropic(String),
    /// The reference solver: it writes each task's reference solution, or gives its expected
    /// answers, then submits, which checks the corpus and the harness rather than a language.
    Reference,
}

/// A turn of a conversation: the user's, or the model's reply.
pub struct Turn {
    pub user: bool,
    pub text: String,
}

/// How many times a request is tried before it is given up.
const ATTEMPTS: u32 = 5;

/// The most tokens a reply may hold.
const MAX_TOKENS: u32 = 16_000;

impl Model {
    pub fn of(name: &str) -> Model {
        match name {
            "reference" => Model::Reference,
            id => Model::Anthropic(id.to_owned()),
        }
    }

    pub fn name(&self) -> &str {
        match self {
            Model::Anthropic(id) => id,
            Model::Reference => "reference",
        }
    }

    /// The model's reply to a conversation about a task.
    pub fn reply(&self, system: &str, turns: &[Turn], task: &Task) -> Result<String, String> {
        match self {
            Model::Anthropic(id) => anthropic(id, system, turns),
            Model::Reference => reference(task),
        }
    }
}

/// Asks the Messages API, retrying refused or failed requests with growing pauses.
fn anthropic(model: &str, system: &str, turns: &[Turn]) -> Result<String, String> {
    let key = std::env::var("ANTHROPIC_API_KEY").map_err(|_| "ANTHROPIC_API_KEY is not set")?;
    let base = std::env::var("ANTHROPIC_BASE_URL")
        .unwrap_or_else(|_| "https://api.anthropic.com".to_owned());
    let messages: Vec<Value> = turns
        .iter()
        .map(|turn| {
            let role = if turn.user { "user" } else { "assistant" };
            json!({ "role": role, "content": turn.text })
        })
        .collect();
    let body = json!({
        "model": model,
        "max_tokens": MAX_TOKENS,
        "system": system,
        "messages": messages,
    })
    .to_string();
    let mut failure = String::new();
    for attempt in 0..ATTEMPTS {
        if attempt > 0 {
            thread::sleep(Duration::from_secs(10 << attempt));
        }
        match request(&base, &key, &body) {
            Ok(response) => return text_of(&response),
            Err(error) => failure = error,
        }
    }
    Err(failure)
}

/// One request, its headers on `curl`'s standard input so that the key stays out of its
/// arguments, and its body in a file removed once it is answered.
fn request(base: &str, key: &str, body: &str) -> Result<Value, String> {
    let headers = format!(
        "x-api-key: {key}\nanthropic-version: 2023-06-01\ncontent-type: application/json\n"
    );
    let file = std::env::temp_dir().join(format!("nanbench-request-{}.json", std::process::id()));
    std::fs::write(&file, body).map_err(|error| error.to_string())?;
    let mut child = Command::new("curl")
        .args([
            "-sS",
            "--fail-with-body",
            "--max-time",
            "600",
            "-H",
            "@-",
            "--data-binary",
        ])
        .arg(format!("@{}", file.display()))
        .arg(format!("{base}/v1/messages"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("curl: {error}"))?;
    child
        .stdin
        .take()
        .ok_or("curl's input")?
        .write_all(headers.as_bytes())
        .map_err(|error| error.to_string())?;
    let output = child.wait_with_output().map_err(|error| error.to_string());
    drop(std::fs::remove_file(&file));
    let output = output?;
    let text = String::from_utf8_lossy(&output.stdout).into_owned();
    match output.status.success() {
        true => serde_json::from_str(&text).map_err(|error| error.to_string()),
        false => Err(format!(
            "the request failed: {text}{}",
            String::from_utf8_lossy(&output.stderr)
        )),
    }
}

/// The text of a Messages API response.
fn text_of(response: &Value) -> Result<String, String> {
    let blocks = response["content"]
        .as_array()
        .ok_or_else(|| format!("a response without content: {response}"))?;
    Ok(blocks
        .iter()
        .filter_map(|block| block["text"].as_str())
        .collect::<Vec<_>>()
        .join(""))
}

/// The reference solver's reply: the solution's files and a submission, or the answers.
fn reference(task: &Task) -> Result<String, String> {
    let mut reply = String::new();
    match task.kind {
        Kind::Read => {
            for (question, answer) in &task.answers {
                reply.push_str(&format!("<<<answer {question}\n{answer}\n>>>\n"));
            }
        }
        Kind::Write | Kind::Repair => {
            for (path, text) in corpus::files(&task.solution())? {
                reply.push_str(&format!("<<<write {path}\n{text}>>>\n"));
            }
            reply.push_str("<<<submit>>>\n");
        }
    }
    Ok(reply)
}
