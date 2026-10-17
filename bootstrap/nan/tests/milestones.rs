//! The milestone harness: in a copy of each scenario workspace of `tests/milestones`, runs the
//! commands its milestone states, then `nan verify`, which must accept the workspace after
//! running its tests; for M2, the obligation outcomes are also compared with the proof cache's,
//! and for M4 each program runs under its emulator until its serial output shows the behaviour.
//! Its runs of `nan` use the harnesses' home directory and a local store of their own, which each
//! scenario starts empty.

use std::fs;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// Copies a directory tree into another, made anew.
fn copy_tree(from: &Path, to: &Path) -> std::io::Result<()> {
    if to.exists() {
        fs::remove_dir_all(to)?;
    }
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        match entry.file_type()?.is_dir() {
            true => copy_tree(&entry.path(), &target)?,
            false => drop(fs::copy(entry.path(), target)?),
        }
    }
    Ok(())
}

/// Copies a scenario into the harnesses' temporary directory, made anew, and gives its root.
fn copied(milestone: &str) -> PathBuf {
    let tmp = PathBuf::from(env!("CARGO_TARGET_TMPDIR"));
    copied_into(milestone, &tmp.join("milestones"))
}

/// Copies a scenario into `directory`, made anew, and gives its root, starting the scenario's
/// local store empty.
fn copied_into(milestone: &str, directory: &Path) -> PathBuf {
    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/milestones")
        .join(milestone);
    let tmp = PathBuf::from(env!("CARGO_TARGET_TMPDIR"));
    let root = directory.join(milestone);
    copy_tree(&source, &root).expect("a copy of the scenario");
    let store = tmp.join("stores").join(milestone);
    match fs::remove_dir_all(&store) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
            panic!("the scenario's local store not emptied: {error}")
        }
        _ => {}
    }
    fs::create_dir_all(tmp.join("home")).expect("the harnesses' home directory");
    fs::create_dir_all(tmp.join("stores")).expect("the directory of local stores");
    nan::use_home(tmp.join("home"), Some(store));
    root
}

/// Runs `nan` in a directory and gives its output, asserting that its verdict is `ok`.
fn ok(dir: &Path, args: &[&str]) -> String {
    let args: Vec<String> = args.iter().map(|arg| (*arg).to_owned()).collect();
    let output = nan::run(&args, dir).output;
    assert!(
        output.starts_with("ok "),
        "nan {} reports\n{output}",
        args.join(" ")
    );
    output
}

fn verified(root: &Path) {
    let output = ok(root, &["verify"]);
    assert!(
        !output
            .lines()
            .next()
            .unwrap_or_default()
            .contains("tests 0,"),
        "nan verify runs no test\n{output}"
    );
    fs::remove_dir_all(root).expect("the scenario's copy removed");
}

#[test]
fn m2() {
    let root = copied("m2");
    ok(&root, &["check"]);
    let mismatches = nan::proof_cache_mismatches(&root).expect("the scenario checked");
    assert!(
        mismatches.is_empty(),
        "the proof cache holds results the prover does not give for {mismatches:?}"
    );
    verified(&root);
}

/// The item the second release of the M3 scenario's dependency adds.
const ADDED: &str = "
/// Whether a raw reading is at full scale.
pub fn is_full(raw: u32) -> bool = raw >= 1_024
";

/// The code root a `nan publish` output reports.
fn published(output: &str) -> String {
    let line = output.lines().find(|line| line.starts_with("published "));
    let root = line.and_then(|line| line.split(' ').nth(2));
    root.expect("a published release").to_owned()
}

#[test]
fn m3() {
    let root = copied("m3");
    let first = published(&ok(
        &root,
        &[
            "publish",
            "--local",
            "gauge",
            "--name",
            "nan-milestones/m3/gauge",
        ],
    ));
    let source = root.join("gauge/gauge.nan");
    let text = fs::read_to_string(&source).expect("the gauge source");
    fs::write(&source, text + ADDED).expect("the gauge source written");
    ok(&root, &["api", "update", "gauge"]);
    let second = published(&ok(&root, &["publish", "--local", "gauge"]));
    assert_ne!(first, second);
    let sync = ok(&root, &["dep", "sync", "panel"]);
    assert!(
        sync.lines()
            .any(|line| line.starts_with(&format!("release gauge - {second} "))),
        "nan dep sync locks another release than the second\n{sync}"
    );

    let versions = root.with_extension("merge");
    fs::create_dir_all(&versions).expect("the merge versions' directory");
    let file = root.join("panel/panel.nan");
    let base = fs::read_to_string(&file).expect("the panel source");
    let adding =
        |variant: &str| base.replacen("    Stop\n", &format!("    Stop\n    {variant}\n"), 1);
    let (base_path, theirs_path) = (versions.join("base.nan"), versions.join("theirs.nan"));
    fs::write(&base_path, &base).expect("the base version");
    fs::write(&theirs_path, adding("Resume")).expect("their version");
    fs::write(&file, adding("Pause")).expect("our version");
    let merged = ok(
        &root,
        &[
            "merge",
            base_path.to_str().expect("a UTF-8 path"),
            "panel/panel.nan",
            theirs_path.to_str().expect("a UTF-8 path"),
            "--path",
            "panel/panel.nan",
        ],
    );
    assert!(
        merged.contains(", conflicts 0,"),
        "nan merge reports\n{merged}"
    );
    let text = fs::read_to_string(&file).expect("the merged source");
    assert!(
        text.contains("    Stop\n    Pause\n    Resume\n"),
        "nan merge writes\n{text}"
    );
    fs::remove_dir_all(versions).expect("the merge versions removed");
    verified(&root);
}

#[test]
fn m6() {
    verified(&copied("m6"));
}

/// How long a machine program may run before its serial output shows its behaviour.
const BEHAVIOUR_WAIT: Duration = Duration::from_secs(60);

/// Runs a machine program of a scenario with `nan run` under its emulator until its serial
/// output holds each line of `behaviour`, then ends the emulator by a termination request. The
/// output must show no kernel halt or trap report.
fn shows(root: &Path, program: &str, behaviour: &[&str]) {
    let run = nan::run(&["run".to_owned(), program.to_owned()], root);
    let Some(launch) = run.launch else {
        panic!("nan run {program} reports\n{}", run.output);
    };
    let mut emulator = Command::new(&launch.program)
        .args(&launch.args)
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the emulator started");
    let serial = emulator.stdout.take().expect("the serial port's output");
    let mut diagnostics = emulator.stderr.take().expect("the emulator's diagnostics");
    let diagnosed = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        drop(diagnostics.read_to_end(&mut bytes));
        String::from_utf8_lossy(&bytes).into_owned()
    });
    let (lines, received) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(serial).split(b'\n') {
            let Ok(line) = line else {
                break;
            };
            let line = String::from_utf8_lossy(&line)
                .trim_end_matches('\r')
                .to_owned();
            if lines.send(line).is_err() {
                break;
            }
        }
    });
    let deadline = Instant::now() + BEHAVIOUR_WAIT;
    let mut written = Vec::new();
    let mut missing = behaviour.to_vec();
    while !missing.is_empty() {
        let left = deadline.saturating_duration_since(Instant::now());
        let Ok(line) = received.recv_timeout(left) else {
            break;
        };
        missing.retain(|wanted| *wanted != line);
        written.push(line);
    }
    let terminated = Command::new("kill")
        .args(["-TERM", &emulator.id().to_string()])
        .status()
        .is_ok_and(|status| status.success());
    if !terminated {
        drop(emulator.kill());
    }
    drop(emulator.wait());
    let diagnostics = diagnosed.join().expect("the emulator's diagnostics");
    let written = written.join("\n");
    assert!(
        missing.is_empty(),
        "{program} writes no {missing:?} in\n{written}\nand the emulator reports\n{diagnostics}"
    );
    assert!(
        !written
            .lines()
            .any(|line| line.starts_with("halt ") || line.starts_with("trap.")),
        "{program} halts in\n{written}"
    );
}

/// Builds a copy of the M4 scenario outside every git working tree, runs each program under its
/// emulator until it shows its behaviour, then verifies the scenario.
#[test]
fn m4() {
    let outside = std::env::temp_dir().join(format!("nan-milestones-{}", std::process::id()));
    let root = copied_into("m4", &outside);
    ok(&root, &["build"]);
    shows(
        &root,
        "kernel",
        &[
            "kernel: running on 4 cpus",
            "smp: tasks ran on every one of 4 cpus",
            "preempt: 5 tasks that never await all ran",
            "uart: reader woke on byte 78",
            "virtio-rng: received 16 bytes",
        ],
    );
    shows(
        &root,
        "loader",
        &["loader: copied 3 loadable segments of kernel.elf to their physical addresses"],
    );
    verified(&root);
    fs::remove_dir(outside).expect("the scenario's directory removed");
}
