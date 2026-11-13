//! NaN's run times against Rust's and C++'s: each program of `bootstrap/nanbench/bench/compare/`,
//! written alike in the three languages, built by the NaN toolchain, by `rustc` and by the system
//! C++ compiler for the base x86-64 instruction set, then run on its size.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use crate::measure::copy_tree;

/// A program: its directory under `bootstrap/nanbench/bench/compare/`, which names its sources,
/// what it measures and the size it runs on, given as its argument.
pub type Program = (&'static str, &'static str, u64);

pub const PROGRAMS: [Program; 10] = [
    ("fib", "function calls", 42),
    ("collatz", "integer arithmetic", 3_000_000),
    ("sieve", "byte stores", 50_000_000),
    ("fannkuch", "small inline arrays", 10),
    ("matmul", "nested loops over float arrays", 800),
    ("nbody", "float arithmetic over structs", 5_000_000),
    ("interpreter", "dispatch over enums", 20_000_000),
    ("binary_trees", "allocation", 16),
    ("hash_map", "hash maps", 4_000_000),
    ("strings", "text formatting", 10_000_000),
];

/// The languages, in the order of each program's images.
const LANGUAGES: [&str; 3] = ["NaN", "Rust", "C++"];

/// The runs of an image whose median time is its time, after the run that checks its output.
const RUNS: usize = 5;

const RUST_FLAGS: [&str; 4] = ["--edition", "2024", "-C", "opt-level=3"];

const CXX_FLAGS: [&str; 2] = ["-std=c++20", "-O3"];

/// Where a workspace's `x86_64-linux` images are written.
const IMAGES: &str = ".nan/images/x86_64-linux";

/// What a program's images give.
pub enum Outcome {
    /// Each image's time, in the order of the languages.
    Timed(Vec<Duration>),
    /// Each image's output, in the order of the languages, not all alike.
    Differs(Vec<String>),
}

/// The programs a command names, every one when it names none.
pub fn chosen(names: &[String]) -> Result<Vec<Program>, String> {
    if let Some(unknown) = names
        .iter()
        .find(|name| PROGRAMS.iter().all(|(program, ..)| program != name))
    {
        return Err(format!(
            "no program {unknown} in bootstrap/nanbench/bench/compare"
        ));
    }
    Ok(PROGRAMS
        .into_iter()
        .filter(|(program, ..)| names.is_empty() || names.iter().any(|name| name == program))
        .collect())
}

/// Runs a command and gives its standard output, or fails with what it writes on both streams.
fn output(command: &mut Command) -> Result<String, String> {
    let program = command.get_program().to_string_lossy().into_owned();
    let output = command
        .output()
        .map_err(|error| format!("{program}: {error}"))?;
    match output.status.success() {
        true => Ok(String::from_utf8_lossy(&output.stdout).into_owned()),
        false => Err(format!(
            "{program} exits with {:?}: {}{}",
            output.status.code(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )),
    }
}

/// A command running a compiler from `work`, so that `rustc` is the toolchain the repository pins.
fn compiler(name: &str, work: &Path) -> Command {
    let mut command = Command::new(name);
    command.current_dir(work);
    command
}

/// Builds the programs in each language under `work`, from a copy of their sources, NaN's by the
/// NaN toolchain the bootstrap builds from a copy of `toolchain/`, and gives each one's images in
/// the order of the languages; the calling process's user directory is set.
pub fn build(
    repository: &Path,
    work: &Path,
    programs: &[&str],
) -> Result<Vec<Vec<PathBuf>>, String> {
    let fault = |error: std::io::Error| error.to_string();
    let toolchain = work.join("toolchain");
    copy_tree(&repository.join("toolchain"), &toolchain).map_err(fault)?;
    let built = nan::run(&["build".to_owned(), "nan".to_owned()], &toolchain).output;
    if !built.starts_with("ok ") {
        return Err(format!("nan build nan reports\n{built}"));
    }
    let workspace = work.join("nan");
    for program in programs {
        copy_tree(
            &repository
                .join("bootstrap/nanbench/bench/compare")
                .join(program),
            &workspace.join(program),
        )
        .map_err(fault)?;
    }
    output(
        Command::new(toolchain.join(IMAGES).join("nan"))
            .arg("build")
            .current_dir(&workspace),
    )?;
    for dir in ["rust", "cpp"] {
        fs::create_dir_all(work.join(dir)).map_err(fault)?;
    }
    let mut images = Vec::new();
    for program in programs {
        let source = workspace.join(program).join(program);
        let rust = work.join("rust").join(program);
        let cpp = work.join("cpp").join(program);
        output(
            compiler("rustc", work)
                .args(RUST_FLAGS)
                .arg("-o")
                .arg(&rust)
                .arg(source.with_extension("rs")),
        )?;
        output(
            compiler("c++", work)
                .args(CXX_FLAGS)
                .arg("-o")
                .arg(&cpp)
                .arg(source.with_extension("cpp")),
        )?;
        images.push(vec![workspace.join(IMAGES).join(program), rust, cpp]);
    }
    Ok(images)
}

/// The first line `--version` writes for `rustc` and for the C++ compiler.
pub fn compilers(work: &Path) -> Result<[String; 2], String> {
    let version = |name: &str| -> Result<String, String> {
        let written = output(compiler(name, work).arg("--version"))?;
        Ok(written.lines().next().unwrap_or_default().to_owned())
    };
    Ok([version("rustc")?, version("c++")?])
}

/// Runs a program's images on its size, once each to compare their outputs, then in turn for
/// each of the timed runs.
pub fn time(images: &[PathBuf], size: u64) -> Result<Outcome, String> {
    let run = |image: &PathBuf| output(Command::new(image).arg(size.to_string()));
    let outputs = images.iter().map(run).collect::<Result<Vec<_>, _>>()?;
    if outputs.iter().any(|written| *written != outputs[0]) {
        return Ok(Outcome::Differs(outputs));
    }
    let mut times = vec![Vec::new(); images.len()];
    for _ in 0..RUNS {
        for (image, taken) in images.iter().zip(&mut times) {
            let start = Instant::now();
            run(image)?;
            taken.push(start.elapsed());
        }
    }
    Ok(Outcome::Timed(
        times
            .into_iter()
            .map(|mut taken| {
                taken.sort();
                taken[RUNS / 2]
            })
            .collect(),
    ))
}

/// A time in seconds to the millisecond.
fn seconds(time: Duration) -> String {
    format!("{:.3} s", time.as_secs_f64())
}

/// A program's line: each language's time, or each one's output when they differ.
pub fn line(program: &str, outcome: &Outcome) -> String {
    let parts: Vec<String> = match outcome {
        Outcome::Timed(times) => LANGUAGES
            .iter()
            .zip(times)
            .map(|(language, time)| format!("{language} {}", seconds(*time)))
            .collect(),
        Outcome::Differs(outputs) => LANGUAGES
            .iter()
            .zip(outputs)
            .map(|(language, written)| format!("{language} writes {written:?}"))
            .collect(),
    };
    format!("{program} {}", parts.join(" "))
}

/// The comparison document of the programs timed, by the compilers named, on the machine
/// described.
pub fn document(timed: &[(Program, Outcome)], compilers: &[String; 2], machine: &str) -> String {
    let mut text = format!(
        "# NaN against Rust and C++\n\nTaken by `nanbench compare`: each program of `bootstrap/nanbench/bench/compare/`, written alike in the three languages, runs on its size, built for the base x86-64 instruction set by the NaN toolchain, which the bootstrap builds from `toolchain/`, for `x86_64-linux`, by `{}` with `{}` and by `{}` with `{}`. Each time is the median wall time of {RUNS} runs, on a machine with {machine}.\n\n| Program | Measures | Size | NaN | Rust | C++ | NaN / Rust | NaN / C++ |\n| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |\n",
        compilers[0],
        RUST_FLAGS.join(" "),
        compilers[1],
        CXX_FLAGS.join(" ")
    );
    let mut logs = [Vec::new(), Vec::new()];
    for ((name, measures, size), outcome) in timed {
        let Outcome::Timed(times) = outcome else {
            text.push_str(&format!(
                "| {name} | {measures} | {size} | outputs differ | | | | |\n"
            ));
            continue;
        };
        let ratios = [1, 2].map(|other| times[0].as_secs_f64() / times[other].as_secs_f64());
        for (log, ratio) in logs.iter_mut().zip(ratios) {
            log.push(ratio.ln());
        }
        text.push_str(&format!(
            "| {name} | {measures} | {size} | {} | {} | {} | {:.2} | {:.2} |\n",
            seconds(times[0]),
            seconds(times[1]),
            seconds(times[2]),
            ratios[0],
            ratios[1]
        ));
    }
    let [rust, cpp] = logs.map(|log| (log.iter().sum::<f64>() / log.len().max(1) as f64).exp());
    text.push_str(&format!(
        "| Geometric mean | | | | | | {rust:.2} | {cpp:.2} |\n"
    ));
    text
}
