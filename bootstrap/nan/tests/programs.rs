//! The program harness: builds each program of `tests/programs` and compares what its image
//! writes with the program's `expected` file, whose first line states the exit status. A host
//! program runs as a process, writing standard output then standard error; one for another
//! architecture than this machine's is only built. A machine program boots under its emulator,
//! writing the serial port until it ends the emulator through the exit port; on a UEFI target the
//! firmware writes first, so its output only ends with the expected text. A machine program with an
//! `emulator` file boots once per line of it, the line's words added to the emulator's arguments.
//! Its builds use the harnesses' home directory.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use datatest_stable::Utf8Path;

mod machine;

datatest_stable::harness! {
    { test = program, root = "tests/programs", pattern = r"^[a-z0-9_]+/expected$" },
}

/// Copies a directory's files into another, made anew.
fn copy_dir(from: &Path, to: &Path) -> std::io::Result<()> {
    if to.exists() {
        fs::remove_dir_all(to)?;
    }
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        if entry.file_name() != "expected" && entry.file_name() != "emulator" {
            fs::copy(entry.path(), to.join(entry.file_name()))?;
        }
    }
    Ok(())
}

/// The one target a program's manifest declares.
fn target(package: &Path) -> std::io::Result<String> {
    let manifest = fs::read_to_string(package.join("package.nan"))?;
    let target = manifest
        .lines()
        .find_map(|line| line.trim().strip_prefix("targets "))
        .expect("a program's targets directive");
    Ok(target.to_owned())
}

fn program(path: &Utf8Path, expected: String) -> datatest_stable::Result<()> {
    let source = path.parent().expect("a program directory");
    let name = source.file_name().expect("a program name");
    let tmp = PathBuf::from(env!("CARGO_TARGET_TMPDIR"));
    let root = tmp.join("programs").join(name);
    copy_dir(source.as_std_path(), &root.join(name))?;
    fs::create_dir_all(tmp.join("home"))?;
    nan::use_home(tmp.join("home"), None);
    let build = nan::run(&["build".to_owned(), name.to_owned()], &root);
    if build.status != 0 {
        return Err(format!(
            "nan build {name} exits with {} and reports\n{}",
            build.status, build.output
        )
        .into());
    }
    let target = target(&root.join(name))?;
    let runs = match target.strip_suffix("-host") {
        Some(arch) if arch != std::env::consts::ARCH => Vec::new(),
        Some(_) => {
            let run = Command::new(root.join(".nan/images").join(&target).join(name))
                .current_dir(&root)
                .output()?;
            let mut written = String::from_utf8_lossy(&run.stdout).into_owned();
            written.push_str(&String::from_utf8_lossy(&run.stderr));
            vec![(String::new(), run.status.code().unwrap_or(-1), written)]
        }
        None => {
            let boots = match fs::read_to_string(source.join("emulator")) {
                Ok(lines) => lines.lines().map(str::to_owned).collect(),
                Err(_) => vec![String::new()],
            };
            let mut runs = Vec::new();
            for extra in boots {
                let run = nan::run(&["run".to_owned(), name.to_owned()], &root);
                let Some(mut launch) = run.launch else {
                    return Err(format!("nan run {name} reports\n{}", run.output).into());
                };
                launch
                    .args
                    .extend(extra.split_whitespace().map(str::to_owned));
                let booted = machine::boot(&launch, &root)?;
                runs.push((extra, booted.status, booted.written));
            }
            runs
        }
    };
    let (wanted_status, wanted) = expected.split_once('\n').expect("an expected status line");
    for (extra, status, written) in runs {
        let shown = match target.ends_with("-uefi") {
            true => written.ends_with(wanted),
            false => written == wanted,
        };
        if format!("status {status}") != wanted_status || !shown {
            return Err(format!(
                "{name} booted with `{extra}` exits with {status} and writes\n{written}expected\n{expected}"
            )
            .into());
        }
    }
    fs::remove_dir_all(&root)?;
    Ok(())
}
