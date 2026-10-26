//! The times the freeze measures, taken by the benchmark programs of `bootstrap/nanbench/bench/` on the
//! reference machine's 8 hardware threads: the host runtime's allocation and generation check,
//! the kernel runtime's generation check, seam and thread switch under the emulator, and the
//! extended-state save and restore its seams and switches perform.

use std::collections::BTreeMap;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use crate::measure::copy_tree;

/// The processors the timings run on.
const PROCESSORS: &str = "0-7";

/// How long the kernel benchmark may run before it has written every time.
const KERNEL_WAIT: Duration = Duration::from_secs(300);

/// Each time in picoseconds, by the benchmark and the label it writes.
pub struct Times {
    pub host: BTreeMap<String, u64>,
    pub kernel: BTreeMap<String, u64>,
    pub native: BTreeMap<String, u64>,
}

/// The `<label> <picoseconds>` lines of a benchmark's output.
fn times(output: &str) -> BTreeMap<String, u64> {
    output
        .lines()
        .filter_map(|line| {
            let (label, value) = line.split_once(' ')?;
            Some((label.to_owned(), value.trim().parse().ok()?))
        })
        .collect()
}

/// Runs a command on the timing processors and gives its standard output.
fn pinned(program: &Path, dir: &Path) -> Result<String, String> {
    let output = Command::new("taskset")
        .args(["-c", PROCESSORS])
        .arg(program)
        .current_dir(dir)
        .output()
        .map_err(|error| format!("taskset: {error}"))?;
    match output.status.success() {
        true => Ok(String::from_utf8_lossy(&output.stdout).into_owned()),
        false => Err(format!(
            "{} exits with {:?}: {}",
            program.display(),
            output.status.code(),
            String::from_utf8_lossy(&output.stderr)
        )),
    }
}

/// Takes every time, building the benchmarks in copies under `dir`; the calling process's user
/// directory is set.
pub fn take(repository: &Path, dir: &Path) -> Result<Times, String> {
    let fault = |error: std::io::Error| error.to_string();
    let bench = repository.join("bootstrap/nanbench/bench");
    let work = dir.join("timing");
    let host = work.join("host");
    copy_tree(&bench.join("host"), &host).map_err(fault)?;
    let built = nan::run(&["build".to_owned(), "hostbench".to_owned()], &host).output;
    if !built.starts_with("ok ") {
        return Err(format!("nan build hostbench reports\n{built}"));
    }
    let host = times(&pinned(
        &host.join(".nan/images/x86_64-host/hostbench"),
        &host,
    )?);
    let kernel = kernel(&bench.join("kernel"), &work.join("kernel"))?;
    let native = work.join("xsave");
    let compiled = Command::new("cc")
        .args(["-O2", "-o"])
        .arg(&native)
        .arg(bench.join("xsave.c"))
        .status()
        .map_err(|error| format!("cc: {error}"))?;
    if !compiled.success() {
        return Err("cc could not build xsave.c".to_owned());
    }
    let native = times(&pinned(&native, &work)?);
    Ok(Times {
        host,
        kernel,
        native,
    })
}

/// Runs the kernel benchmark under the emulator, on the timing processors, until its serial
/// output holds the time of its last timing, then ends the emulator.
fn kernel(source: &Path, root: &Path) -> Result<BTreeMap<String, u64>, String> {
    copy_tree(source, root).map_err(|error| error.to_string())?;
    let run = nan::run(&["run".to_owned(), "kernelbench".to_owned()], root);
    let Some(launch) = run.launch else {
        return Err(format!("nan run kernelbench reports\n{}", run.output));
    };
    let mut emulator = Command::new("taskset")
        .args(["-c", PROCESSORS])
        .arg(&launch.program)
        .args(&launch.args)
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| format!("the emulator: {error}"))?;
    let serial = emulator.stdout.take().ok_or("the serial port's output")?;
    let (lines, received) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(serial).lines() {
            let Ok(line) = line else {
                break;
            };
            if lines.send(line).is_err() {
                break;
            }
        }
    });
    let mut written = String::new();
    while let Ok(line) = received.recv_timeout(KERNEL_WAIT) {
        written.push_str(&line);
        written.push('\n');
        if line.starts_with("switch ") || line.starts_with("halt ") || line.starts_with("trap.") {
            break;
        }
    }
    drop(emulator.kill());
    drop(emulator.wait());
    let found = times(&written);
    match found.contains_key("switch") {
        true => Ok(found),
        false => Err(format!("the kernel benchmark writes\n{written}")),
    }
}

/// The machine the times are taken on: its processor's model and highest clock, its hardware
/// threads and memory.
pub fn machine() -> String {
    let cpuinfo = fs::read_to_string("/proc/cpuinfo").unwrap_or_default();
    let model = cpuinfo
        .lines()
        .find_map(|line| line.strip_prefix("model name"))
        .and_then(|rest| rest.split_once(':'))
        .map(|(_, model)| model.trim().to_owned())
        .unwrap_or_default();
    let threads = cpuinfo
        .lines()
        .filter(|line| line.starts_with("processor"))
        .count();
    let meminfo = fs::read_to_string("/proc/meminfo").unwrap_or_default();
    let memory = meminfo
        .lines()
        .find_map(|line| line.strip_prefix("MemTotal:"))
        .and_then(|rest| rest.split_whitespace().next()?.parse::<u64>().ok())
        .unwrap_or(0)
        / (1024 * 1024);
    let clock = fs::read_to_string("/sys/devices/system/cpu/cpu0/cpufreq/cpuinfo_max_freq")
        .ok()
        .and_then(|text| text.trim().parse::<u64>().ok())
        .map_or_else(String::new, |khz| {
            format!(" at up to {:.1} GHz", khz as f64 / 1_000_000.0)
        });
    format!("{model}{clock}, {threads} hardware threads, {memory} GiB of memory")
}
