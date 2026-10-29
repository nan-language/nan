//! A test image as its runs start it, and one run of a test, given the test's number, the run's
//! index and seed, its replay token and the draws a rerun forces: a process of the image on a
//! host target, a boot of the emulator on it on a machine target, whose standard output or
//! serial port carries frames, and how it ended.

use std::fs;
use std::io;
use std::os::unix::process::ExitStatusExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::command::Launch;
use crate::emulator::{self, Writes};
use crate::lower::test_image::{RUN_FILE, RUN_START, TIMED_OUT, frame, outcome};
use crate::workspace::Target;

/// A test image as its runs start it: a host target's executable, or a machine image and the
/// emulator invocation booting it.
pub enum Image {
    Host(PathBuf),
    Machine(PathBuf, Launch),
}

impl Image {
    /// The test image at a path, as its runs on a run target start it: a machine image's boot
    /// volume is made once, and every run finds it as it was made.
    pub fn new(target: Target, path: PathBuf) -> io::Result<Image> {
        Ok(match target.is_machine() {
            true => {
                let launch = emulator::launch(target, &path, &[], Writes::Dropped)?;
                Image::Machine(path, launch)
            }
            false => Image::Host(path),
        })
    }

    fn path(&self) -> &Path {
        match self {
            Image::Host(path) | Image::Machine(path, _) => path,
        }
    }

    /// Removes the image and its boot volume.
    pub fn discard(self) -> io::Result<()> {
        match self {
            Image::Host(path) => fs::remove_file(path),
            Image::Machine(path, _) => emulator::discard(&path),
        }
    }
}

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
    image: &Image,
    test: usize,
    (index, seed): (u64, u64),
    token: &str,
    forced: Option<&[i128]>,
) -> io::Result<Run> {
    let mut texts = vec![
        test.to_string(),
        index.to_string(),
        seed.to_string(),
        token.to_owned(),
    ];
    texts.extend(forced.into_iter().flatten().map(i128::to_string));
    let (output, start) = match image {
        Image::Host(path) => {
            let output = Command::new(path).args(&texts).output()?;
            if output.status.code() == Some(TIMED_OUT) || output.status.signal() == Some(24) {
                return Ok(timed_out());
            }
            (output, Some(0))
        }
        Image::Machine(path, launch) => {
            let file = path.with_extension("run");
            fs::write(&file, texts.join("\0") + "\0")?;
            let booted = emulator::boot(launch, (RUN_FILE, &file));
            fs::remove_file(&file)?;
            let Some(output) = booted? else {
                return Ok(timed_out());
            };
            let start = output
                .stdout
                .windows(RUN_START.len())
                .position(|window| window == RUN_START)
                .map(|at| at + RUN_START.len());
            (output, start)
        }
    };
    let frames = start.map(|start| &output.stdout[start..]);
    let mut trace = Vec::new();
    let mut draws = Vec::new();
    let mut ended = None;
    let mut rest = frames.unwrap_or_default();
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
            "test run {test}/{index} of {} ended with {} without an outcome:\n{}{}",
            image.path().display(),
            output.status,
            String::from_utf8_lossy(&output.stderr),
            match frames {
                Some(_) => String::new(),
                None => String::from_utf8_lossy(&output.stdout).into_owned(),
            },
        ))),
    }
}

/// The run of a test that outlived its bound.
fn timed_out() -> Run {
    Run {
        ended: Ended::TimedOut,
        contended: false,
        trace: Vec::new(),
        draws: Vec::new(),
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
