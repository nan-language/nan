//! The milestone harness: in a copy of each scenario workspace of `tests/milestones`, runs the
//! commands its milestone states, then `nan verify`, which must accept the workspace after
//! running its tests; for M2, the obligation outcomes are also compared with the proof cache's,
//! and for each M4 scenario, one per architecture, each program runs under its emulator until its
//! serial output shows the behaviour.
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

/// The NaN toolchain's files generated from the repository: the grammar text its parser's tables
/// are built from, the std sources it carries and the primer chapter `nan primer` prints from.
const EBNF_FILE: &str = "toolchain/nan/syntax/ebnf.nan";
const STD_FILE: &str = "toolchain/nan/stdlib/sources.nan";
const PRIMER_FILE: &str = "toolchain/nan/command/primer.nan";

/// A line of a text run indented once: `\\` and the line, each backslash doubled.
fn run_line(line: &str) -> String {
    format!("    \\\\{}\n", line.replace('\\', "\\\\"))
}

/// The lines of a text run holding `text`, which ends with a line feed.
fn text_run(text: &str) -> String {
    let mut run: String = text.lines().map(run_line).collect();
    run.push_str("    \\\\\n");
    run
}

/// The source of the front end's grammar text: the lines of the ebnf blocks of the grammar
/// chapter, each ending with a line feed.
fn ebnf_source(repository: &Path) -> String {
    let chapter =
        fs::read_to_string(repository.join("docs/spec/41-grammar.md")).expect("the grammar");
    let mut grammar = String::new();
    let mut inside = false;
    for line in chapter.lines() {
        if line == "```ebnf" {
            inside = true;
        } else if line.starts_with("```") {
            inside = false;
        } else if inside {
            grammar.push_str(line);
            grammar.push('\n');
        }
    }
    format!(
        "//! The ebnf blocks of the published grammar, which the parser's tables are built from.\n\n\
         /// The lines of the ebnf blocks of the grammar chapter of the specification, in order, each\n\
         /// ending with a line feed.\n\
         pkg const EBNF: Str =\n{}",
        text_run(&grammar)
    )
}

/// The source of the toolchain's primer text: the primer chapter, each line ending with a line
/// feed.
fn primer_source(repository: &Path) -> String {
    let chapter =
        fs::read_to_string(repository.join("docs/spec/39-primer.md")).expect("the primer");
    format!(
        "//! The primer chapter of the specification, whose sections `nan primer` prints.\n\n\
         /// The lines of the primer chapter of the specification, each ending with a line feed.\n\
         const PRIMER: Str =\n{}",
        text_run(&chapter)
    )
}

/// The std source files, by path relative to the std directory in byte order, skipping names
/// starting with `.`.
fn std_files(dir: &Path, prefix: &str, files: &mut Vec<(String, String)>) {
    for entry in fs::read_dir(dir).expect("a std directory") {
        let entry = entry.expect("a std directory entry");
        let name = entry.file_name().into_string().expect("a UTF-8 file name");
        if name.starts_with('.') {
            continue;
        }
        let path = format!("{prefix}{name}");
        match entry.file_type().expect("a file type").is_dir() {
            true => std_files(&entry.path(), &format!("{path}/"), files),
            false => files.push((
                path,
                fs::read_to_string(entry.path()).expect("a UTF-8 std file"),
            )),
        }
    }
}

/// The words of a std file's path, without its extension.
fn path_words(path: &str) -> Vec<String> {
    path.trim_end_matches(".nan")
        .split(['/', '_'])
        .map(str::to_owned)
        .collect()
}

/// The end of a function head whose body holds `lines` lines: a waiver and the opener on its own
/// line beyond the length threshold, else the opener on the head line.
fn opener(lines: usize) -> &'static str {
    if lines > 80 {
        "\n    waive length \"one line per std file\"\n{\n"
    } else {
        " {\n"
    }
}

/// The source of the front end's std sources: an enum of the files with their paths and texts.
fn std_source(repository: &Path) -> String {
    let mut files = Vec::new();
    std_files(&repository.join("std"), "", &mut files);
    files.sort();
    let variant = |path: &str| -> String {
        path_words(path)
            .iter()
            .map(|word| {
                let mut chars = word.chars();
                chars.next().map_or_else(String::new, |first| {
                    first.to_ascii_uppercase().to_string() + chars.as_str()
                })
            })
            .collect()
    };
    let constant = |path: &str| format!("TEXT_{}", path_words(path).join("_").to_uppercase());
    let mut out = String::from(
        "//! The std source files the toolchain carries, generated from the std directory.\n\n\
         /// A std source file the toolchain carries.\n\
         pkg enum StdFile derives Copy, Eq {\n",
    );
    for (path, _) in &files {
        out.push_str(&format!("    {}\n", variant(path)));
    }
    out.push_str("} StdFile\n\n/// Every std source file, in byte order of path.\n");
    out.push_str("pkg fn StdFile.all(alloc: mem::Alloc) -> mem::List[StdFile]");
    out.push_str(opener(files.len() + 2));
    out.push_str("    var files = mem::List[StdFile].empty(alloc)\n");
    for (path, _) in &files {
        out.push_str(&format!("    mut files.push(StdFile.{})\n", variant(path)));
    }
    out.push_str("    return files\n} StdFile.all\n\n");
    for (name, doc, value) in [
        (
            "path",
            "The file's path relative to the std directory.",
            true,
        ),
        ("text", "The file's contents.", false),
    ] {
        out.push_str(&format!(
            "/// {doc}\npkg proj fn StdFile.{name}(self) -> Str"
        ));
        out.push_str(opener(files.len() + 2));
        out.push_str("    return match self {\n");
        for (path, _) in &files {
            let held = match value {
                true => format!("\"{path}\""),
                false => constant(path),
            };
            out.push_str(&format!("        StdFile.{} => {held}\n", variant(path)));
        }
        out.push_str(&format!("    }}\n}} StdFile.{name}\n\n"));
    }
    let texts: Vec<String> = files
        .iter()
        .map(|(path, text)| format!("const {}: Str =\n{}", constant(path), text_run(text)))
        .collect();
    out.push_str(&texts.join("\n"));
    out
}

/// The toolchain carries the current grammar text, std sources and primer chapter. With
/// `NAN_BLESS` set, the test writes them instead.
#[test]
fn m5_embedded_sources() {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    for (file, source) in [
        (EBNF_FILE, ebnf_source(&repository)),
        (STD_FILE, std_source(&repository)),
        (PRIMER_FILE, primer_source(&repository)),
    ] {
        let path = repository.join(file);
        if std::env::var_os("NAN_BLESS").is_some() {
            fs::write(&path, source).expect("the generated file written");
            continue;
        }
        let held = fs::read_to_string(&path).unwrap_or_default();
        assert!(
            held == source,
            "{file} is not current; run this test with NAN_BLESS=1 to regenerate it"
        );
    }
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
    let mut command = Command::new(&launch.program);
    command
        .args(&launch.args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let machine = launch.machine.as_ref().expect("the emulator's machine");
    let mut boot = machine.boot(command).expect("the emulator started");
    let emulator = &mut boot.emulator;
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
        // The PL011 the AArch64 emulator models also writes out a byte it loops back, which
        // can begin a line.
        missing.retain(|wanted| !line.ends_with(wanted));
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

/// What the M4 kernel and loader of either architecture write once they show their behaviour.
const KERNEL_SHOWS: [&str; 5] = [
    "kernel: running on 4 cpus",
    "smp: tasks ran on every one of 4 cpus",
    "preempt: 5 tasks that never await all ran",
    "uart: reader woke on byte 78",
    "virtio-rng: received 16 bytes",
];
const LOADER_SHOWS: [&str; 1] =
    ["loader: copied 3 loadable segments of kernel.elf to their physical addresses"];

/// Builds a copy of an M4 scenario outside every git working tree, runs each program under its
/// emulator until it shows its behaviour, then verifies the scenario.
fn m4_scenario(milestone: &str) {
    let outside =
        std::env::temp_dir().join(format!("nan-milestones-{milestone}-{}", std::process::id()));
    let root = copied_into(milestone, &outside);
    ok(&root, &["build"]);
    shows(&root, "kernel", &KERNEL_SHOWS);
    shows(&root, "loader", &LOADER_SHOWS);
    verified(&root);
    fs::remove_dir(outside).expect("the scenario's directory removed");
}

#[test]
fn m4() {
    m4_scenario("m4");
}

#[test]
fn m4_aarch64() {
    m4_scenario("m4-aarch64");
}
