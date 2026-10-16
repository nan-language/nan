//! One run of a test: a process of the test image, given the test's number, the run's index and
//! seed, its replay token and the draws a rerun forces, whose standard output is frames, and how
//! it ended.

use std::io;
use std::os::unix::process::ExitStatusExt as _;
use std::path::Path;
use std::process::Command;

use crate::lower::test_image::{TIMED_OUT, frame, outcome};

/// How a run ended.
#[derive(Debug, PartialEq, Eq)]
pub enum Ended {
    /// The body returned, and whether the target ran.
    Passed {
        ran: bool,
    },
    /// A trap abandoned the test domain, with its report.
    Trapped {
        report: String,
    },
    /// A failure escaped the body: its variant's canonical path and its rendering.
    Escaped {
        path: String,
        rendering: String,
    },
    /// No task was ready and no deadline pending, and whether the target ran.
    Stalled {
        ran: bool,
    },
    TimedOut,
}

/// A draw of a case: its value, the bounds it was drawn within, and the value it gave once the
/// values its exclusions skip were set apart.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Draw {
    pub value: i128,
    pub low: i128,
    pub high: i128,
    pub given: i128,
}

/// What a run gave: how it ended, whether a scheduling point found two ready tasks, its test
/// trace and its draws.
#[derive(Debug)]
pub struct Run {
    pub ended: Ended,
    pub contended: bool,
    pub trace: Vec<u8>,
    pub draws: Vec<Draw>,
}

/// Performs one run of the numbered test of an image, its draws forced for a rerun.
pub fn run(
    image: &Path,
    test: usize,
    (index, seed): (u64, u64),
    token: &str,
    forced: Option<&[i128]>,
) -> io::Result<Run> {
    let mut command = Command::new(image);
    command
        .arg(test.to_string())
        .arg(index.to_string())
        .arg(seed.to_string())
        .arg(token);
    if let Some(forced) = forced {
        command.args(forced.iter().map(i128::to_string));
    }
    let output = command.output()?;
    if output.status.code() == Some(TIMED_OUT) || output.status.signal() == Some(24) {
        return Ok(Run {
            ended: Ended::TimedOut,
            contended: false,
            trace: Vec::new(),
            draws: Vec::new(),
        });
    }
    let mut trace = Vec::new();
    let mut draws = Vec::new();
    let mut ended = None;
    let mut rest = output.stdout.as_slice();
    while rest.len() >= 5 {
        let kind = rest[0];
        let len = u32::from_le_bytes(rest[1..5].try_into().expect("four bytes")) as usize;
        let Some(payload) = rest.get(5..5 + len) else {
            break;
        };
        match kind {
            frame::TRACE => trace.extend_from_slice(payload),
            frame::DRAW if payload.len() == 64 => {
                let wide = |at: usize| {
                    i128::from_le_bytes(payload[at..at + 16].try_into().expect("16 bytes"))
                };
                draws.push(Draw {
                    value: wide(0),
                    low: wide(16),
                    high: wide(32),
                    given: wide(48),
                });
            }
            frame::OUTCOME => ended = Some(decode(payload)),
            _ => {}
        }
        rest = &rest[5 + len..];
    }
    match ended {
        Some((ended, contended)) => Ok(Run {
            ended,
            contended,
            trace,
            draws,
        }),
        None => Err(io::Error::other(format!(
            "test run {test}/{index} of {} ended with {} without an outcome:\n{}",
            image.display(),
            output.status,
            String::from_utf8_lossy(&output.stderr)
        ))),
    }
}

/// An outcome frame's payload: how the run ended and whether it was contended.
fn decode(payload: &[u8]) -> (Ended, bool) {
    let (code, ran, contended) = (payload[0], payload[1] != 0, payload[2] != 0);
    let text = String::from_utf8_lossy(&payload[3..]).into_owned();
    let ended = match code {
        outcome::PASSED => Ended::Passed { ran },
        outcome::TRAPPED => Ended::Trapped { report: text },
        outcome::ESCAPED => {
            let (path, rendering) = text.split_once('\0').unwrap_or((&text, ""));
            Ended::Escaped {
                path: path.to_owned(),
                rendering: rendering.to_owned(),
            }
        }
        _ => Ended::Stalled { ran },
    };
    (ended, contended)
}
