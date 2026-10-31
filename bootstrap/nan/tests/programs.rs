//! The program harness: builds each program of `tests/programs` and compares what its image
//! writes with the program's `expected` file, whose first line states the exit status. A host
//! program runs as a process, writing standard output then standard error; one for another
//! architecture than this machine's is only built. A machine program boots under its emulator,
//! writing the serial port until it ends the emulator through the exit port; on a UEFI target the
//! firmware writes first, so its output only ends with the expected text. A machine program with an
//! `emulator` file boots once per line of it, the line's words added to the emulator's arguments.
//! A host program with a `codegen` file is also built and run at each codegen level it lists, one
//! per line, writing what it expects where this machine has the level's extensions and refusing
//! to run otherwise. Its builds use the harnesses' home directory.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use datatest_stable::Utf8Path;

mod machine;

datatest_stable::harness! {
    { test = program, root = "tests/programs", pattern = r"^[a-z0-9_]+/expected$" },
}

/// Copies a directory's files and directories into another, made anew.
fn copy_dir(from: &Path, to: &Path) -> std::io::Result<()> {
    if to.exists() {
        fs::remove_dir_all(to)?;
    }
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&entry.path(), &target)?;
        } else if !["expected", "emulator", "codegen"]
            .iter()
            .any(|kept| entry.file_name() == *kept)
        {
            fs::copy(entry.path(), target)?;
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

/// Builds a program, failing with what the build reports when it does not build.
fn build(root: &Path, name: &str) -> datatest_stable::Result<()> {
    let build = nan::run(&["build".to_owned(), name.to_owned()], root);
    if build.status != 0 {
        return Err(format!(
            "nan build {name} exits with {} and reports\n{}",
            build.status, build.output
        )
        .into());
    }
    Ok(())
}

/// Runs a host program's image, giving its exit status and what it writes.
fn run_host(root: &Path, target: &str, name: &str) -> std::io::Result<(i32, String)> {
    let run = Command::new(root.join(".nan/images").join(target).join(name))
        .current_dir(root)
        .output()?;
    let mut written = String::from_utf8_lossy(&run.stdout).into_owned();
    written.push_str(&String::from_utf8_lossy(&run.stderr));
    Ok((run.status.code().unwrap_or(-1), written))
}

/// Whether this machine has the extensions of a codegen level.
fn runs_level(level: &str) -> bool {
    #[cfg(target_arch = "x86_64")]
    {
        let v2 = is_x86_feature_detected!("cmpxchg16b")
            && is_x86_feature_detected!("popcnt")
            && is_x86_feature_detected!("sse3")
            && is_x86_feature_detected!("ssse3")
            && is_x86_feature_detected!("sse4.1")
            && is_x86_feature_detected!("sse4.2");
        let v3 = v2
            && is_x86_feature_detected!("avx")
            && is_x86_feature_detected!("avx2")
            && is_x86_feature_detected!("bmi1")
            && is_x86_feature_detected!("bmi2")
            && is_x86_feature_detected!("fma")
            && is_x86_feature_detected!("lzcnt");
        let v4 = v3
            && is_x86_feature_detected!("avx512f")
            && is_x86_feature_detected!("avx512dq")
            && is_x86_feature_detected!("avx512vl");
        match level {
            "x86_64-v2" => v2,
            "x86_64-v3" => v3,
            "x86_64-v4" => v4,
            _ => false,
        }
    }
    #[cfg(target_arch = "aarch64")]
    {
        level == "aarch64-dotprod"
            && std::arch::is_aarch64_feature_detected!("lse")
            && std::arch::is_aarch64_feature_detected!("dotprod")
    }
}

fn program(path: &Utf8Path, expected: String) -> datatest_stable::Result<()> {
    let source = path.parent().expect("a program directory");
    let name = source.file_name().expect("a program name");
    let tmp = PathBuf::from(env!("CARGO_TARGET_TMPDIR"));
    let root = tmp.join("programs").join(name);
    copy_dir(source.as_std_path(), &root.join(name))?;
    fs::create_dir_all(tmp.join("home"))?;
    nan::use_home(tmp.join("home"), None);
    build(&root, name)?;
    let (wanted_status, wanted) = expected.split_once('\n').expect("an expected status line");
    let target = target(&root.join(name))?;
    let mut runs = Vec::new();
    match target.strip_suffix("-host") {
        Some(arch) if arch != std::env::consts::ARCH => {}
        Some(_) => {
            let (status, written) = run_host(&root, &target, name)?;
            runs.push((
                String::new(),
                status,
                written,
                wanted_status,
                wanted.to_owned(),
            ));
            let manifest = root.join(name).join("package.nan");
            let base = fs::read_to_string(&manifest)?;
            let levels = fs::read_to_string(source.join("codegen")).unwrap_or_default();
            for level in levels.lines() {
                let directive = format!("    entry main\n    codegen {level}\n");
                fs::write(&manifest, base.replace("    entry main\n", &directive))?;
                build(&root, name)?;
                let (status, written) = run_host(&root, &target, name)?;
                let (status_line, text) = match runs_level(level) {
                    true => (wanted_status, wanted.to_owned()),
                    false => ("status 71", format!("nan: codegen {level} unsupported\n")),
                };
                runs.push((format!(" at {level}"), status, written, status_line, text));
            }
        }
        None => {
            let boots = match fs::read_to_string(source.join("emulator")) {
                Ok(lines) => lines.lines().map(str::to_owned).collect(),
                Err(_) => vec![String::new()],
            };
            for extra in boots {
                let run = nan::run(&["run".to_owned(), name.to_owned()], &root);
                let Some(mut launch) = run.launch else {
                    return Err(format!("nan run {name} reports\n{}", run.output).into());
                };
                launch
                    .args
                    .extend(extra.split_whitespace().map(str::to_owned));
                let booted = machine::boot(&launch)?;
                let label = format!(" booted with `{extra}`");
                runs.push((
                    label,
                    booted.status,
                    booted.written,
                    wanted_status,
                    wanted.to_owned(),
                ));
            }
        }
    }
    for (label, status, written, wanted_status, wanted) in runs {
        let shown = match target.ends_with("-uefi") {
            true => written.ends_with(&wanted),
            false => written == wanted,
        };
        if format!("status {status}") != wanted_status || !shown {
            return Err(format!(
                "{name}{label} exits with {status} and writes\n{written}expected\n{wanted_status}\n{wanted}"
            )
            .into());
        }
    }
    fs::remove_dir_all(&root)?;
    Ok(())
}
