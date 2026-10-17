//! The program harness: builds each program of `tests/programs` for `x86_64-host`, runs its
//! image and compares what it writes, standard output then standard error, with the program's
//! `expected` file, whose first line states the exit status. Its builds use the harnesses' home
//! directory.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use datatest_stable::Utf8Path;

datatest_stable::harness! {
    { test = program, root = "tests/programs", pattern = r"^[a-z-]+/expected$" },
}

/// Copies a directory's files into another, made anew.
fn copy_dir(from: &Path, to: &Path) -> std::io::Result<()> {
    if to.exists() {
        fs::remove_dir_all(to)?;
    }
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        if entry.file_name() != "expected" {
            fs::copy(entry.path(), to.join(entry.file_name()))?;
        }
    }
    Ok(())
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
    let run = Command::new(root.join(".nan/images/x86_64-host").join(name))
        .current_dir(&root)
        .output()?;
    let mut written = format!("status {}\n", run.status.code().unwrap_or(-1));
    written.push_str(&String::from_utf8_lossy(&run.stdout));
    written.push_str(&String::from_utf8_lossy(&run.stderr));
    if written != expected {
        return Err(format!("{name} writes\n{written}expected\n{expected}").into());
    }
    fs::remove_dir_all(&root)?;
    Ok(())
}
