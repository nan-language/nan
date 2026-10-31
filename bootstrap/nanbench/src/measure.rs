//! The measurements of the freeze taken over the workspace packages of each milestone's
//! scenario: what checking gives, the ledger `nan explain` lists and the closure radii
//! `nan check --radius` reports.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::timing::Times;

/// The scenarios: each one's milestone, the label its rows carry and its workspace, relative to
/// the repository.
pub const SCENARIOS: [(&str, &str, &str); 6] = [
    ("M2", "M2", "bootstrap/nan/tests/milestones/m2"),
    ("M3", "M3", "bootstrap/nan/tests/milestones/m3"),
    ("M4", "M4", "bootstrap/nan/tests/milestones/m4"),
    (
        "M4",
        "M4 AArch64",
        "bootstrap/nan/tests/milestones/m4-aarch64",
    ),
    ("M5", "M5", "toolchain"),
    ("M6", "M6", "bootstrap/nan/tests/milestones/m6"),
];

/// What a scenario measures.
pub struct Scenario {
    pub milestone: &'static str,
    pub label: &'static str,
    pub measured: nan::Measured,
    /// Each ledger entry's kind, by package.
    pub ledger: BTreeMap<String, BTreeMap<String, usize>>,
    /// The waivers by threshold.
    pub waivers: BTreeMap<String, usize>,
    /// Each item's closure radius, largest first, for the milestones whose radii are measured.
    pub radii: Vec<(String, usize)>,
    /// The tasks of the scenario's programs and test images, for the milestones whose stacks
    /// are measured.
    pub tasks: Vec<nan::TaskStack>,
}

/// Runs `nan` in a workspace and gives its output, which must start with `ok`.
fn ok(root: &Path, args: &[&str]) -> Result<String, String> {
    let args: Vec<String> = args.iter().map(|arg| (*arg).to_owned()).collect();
    let output = nan::run(&args, root).output;
    match output.starts_with("ok ") {
        true => Ok(output),
        false => Err(format!("nan {} reports\n{output}", args.join(" "))),
    }
}

/// Copies a directory tree into another, made anew.
pub fn copy_tree(from: &Path, to: &Path) -> std::io::Result<()> {
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

/// Measures every scenario on a copy of its workspace under `work`, M3's dependency published
/// and locked first, as the milestone does; the calling process's user directory and local store
/// are set.
pub fn scenarios(repository: &Path, work: &Path) -> Result<Vec<Scenario>, String> {
    let fault = |error: std::io::Error| error.to_string();
    let mut measured = Vec::new();
    for (milestone, label, source) in SCENARIOS {
        let root = work.join(
            Path::new(source)
                .file_name()
                .expect("a workspace directory"),
        );
        copy_tree(&repository.join(source), &root).map_err(fault)?;
        if milestone == "M3" {
            ok(
                &root,
                &[
                    "publish",
                    "--local",
                    "gauge",
                    "--name",
                    "nan-milestones/m3/gauge",
                ],
            )?;
            ok(&root, &["dep", "sync", "panel"])?;
        }
        measured.push(scenario(milestone, label, &root)?);
    }
    Ok(measured)
}

fn scenario(milestone: &'static str, label: &'static str, root: &Path) -> Result<Scenario, String> {
    let measured = nan::measure(root).map_err(|error| error.to_string())?;
    let mut ledger: BTreeMap<String, BTreeMap<String, usize>> = BTreeMap::new();
    let mut waivers = BTreeMap::new();
    for package in packages(root)? {
        let kinds = ledger.entry(package.clone()).or_default();
        for line in ok(root, &["explain", &package])?.lines() {
            let fields: Vec<&str> = line.split(' ').collect();
            if fields.first() != Some(&"ledger") {
                continue;
            }
            *kinds.entry(fields[1].to_owned()).or_default() += 1;
            if fields[1] == "waiver" {
                *waivers
                    .entry(threshold(root, fields[fields.len() - 1])?)
                    .or_default() += 1;
            }
        }
    }
    let radii = match ["M4", "M5"].contains(&milestone) {
        true => ok(root, &["check", "--radius", "--budget", "1000000000"])?
            .lines()
            .filter_map(|line| {
                let mut fields = line.split(' ');
                (fields.next() == Some("radius")).then_some(())?;
                let item = fields.next()?.to_owned();
                Some((item, fields.next()?.parse().ok()?))
            })
            .collect(),
        false => Vec::new(),
    };
    let tasks = match ["M2", "M4"].contains(&milestone) {
        true => nan::task_stacks(root).map_err(|error| error.to_string())?,
        false => Vec::new(),
    };
    Ok(Scenario {
        milestone,
        label,
        measured,
        ledger,
        waivers,
        radii,
        tasks,
    })
}

/// The names of the packages of a workspace: its directories holding a `package.nan`.
fn packages(root: &Path) -> Result<Vec<String>, String> {
    let mut names = Vec::new();
    for entry in fs::read_dir(root).map_err(|error| error.to_string())? {
        let path: PathBuf = entry.map_err(|error| error.to_string())?.path();
        if path.join("package.nan").exists() {
            names.extend(
                path.file_name()
                    .and_then(|name| name.to_str())
                    .map(str::to_owned),
            );
        }
    }
    names.sort();
    Ok(names)
}

/// The threshold a waiver's clause names, from its line `<path>:<line>`.
fn threshold(root: &Path, at: &str) -> Result<String, String> {
    let (path, line) = at.rsplit_once(':').ok_or("a waiver without its line")?;
    let line: usize = line.parse().map_err(|_| "a waiver without its line")?;
    let text = fs::read_to_string(root.join(path)).map_err(|error| error.to_string())?;
    let clause = text.lines().nth(line - 1).unwrap_or_default().trim();
    let word = clause
        .strip_prefix("waive ")
        .and_then(|rest| rest.split(' ').next());
    word.map(str::to_owned)
        .ok_or_else(|| format!("{at} holds no waiver"))
}

/// The scenarios of the milestones a measurement row names.
fn of<'s>(scenarios: &'s [Scenario], milestones: &[&str]) -> Vec<&'s Scenario> {
    scenarios
        .iter()
        .filter(|scenario| milestones.contains(&scenario.milestone))
        .collect()
}

/// The sections of the measurements document that checking and the toolchain's reports give,
/// and whether each bounded one meets its bound; each item's closure radius goes to `radii`.
pub fn sections(scenarios: &[Scenario], times: &Times, radii: &mut String) -> (String, bool) {
    let mut text = String::new();
    let mut within = true;
    text.push_str("## Share of bounds and overflow obligations that the prover proves\n\n| Milestone | Proved | Obligations | Share | Bound |\n| --- | ---: | ---: | ---: | --- |\n");
    for scenario in of(scenarios, &["M4", "M5"]) {
        let discharge = &scenario.measured.discharge;
        within &= discharge.proved * 10 >= discharge.total * 9;
        text.push_str(&format!(
            "| {} | {} | {} | {:.1}% | at least 90% |\n",
            scenario.label,
            discharge.proved,
            discharge.total,
            100.0 * discharge.proved as f64 / discharge.total.max(1) as f64
        ));
    }
    text.push_str("\n## Predicate run-time checks kept inside their defining modules\n\n| Milestone | Checks | Sites |\n| --- | ---: | --- |\n");
    for scenario in of(scenarios, &["M4", "M5"]) {
        let checks = &scenario.measured.predicate_checks;
        text.push_str(&format!(
            "| {} | {} | {} |\n",
            scenario.label,
            checks.len(),
            checks.join(", ")
        ));
    }
    text.push_str(
        "\n## Items whose code names `mem::Arena`\n\n| Milestone | Items |\n| --- | --- |\n",
    );
    for scenario in scenarios {
        let items = &scenario.measured.arena_items;
        let listed = match items.is_empty() {
            true => "none".to_owned(),
            false => items.join(", "),
        };
        text.push_str(&format!("| {} | {listed} |\n", scenario.label));
    }
    text.push_str("\n## Ledger entries, per kind and package\n\n| Milestone | Package | Entries |\n| --- | --- | --- |\n");
    for scenario in scenarios {
        for (package, kinds) in &scenario.ledger {
            let entries: Vec<String> = kinds
                .iter()
                .map(|(kind, count)| format!("{kind} {count}"))
                .collect();
            let listed = match entries.is_empty() {
                true => "none".to_owned(),
                false => entries.join(", "),
            };
            text.push_str(&format!("| {} | {package} | {listed} |\n", scenario.label));
        }
    }
    text.push_str("\n## Deepest stack use of each task against its task stack size\n\nEach task's stack use is its entry's, judged as a final build judges a `stack N` demand; an unbounded one names the call that leaves it unbounded.\n\n| Milestone | Task entry | Stack use | Task stack |\n| --- | --- | ---: | ---: |\n");
    for scenario in of(scenarios, &["M2", "M4"]) {
        for task in &scenario.tasks {
            let used = match &task.used {
                Ok(bytes) => bytes.to_string(),
                Err(call) if call.is_empty() => "unbounded".to_owned(),
                Err(call) => format!("unbounded: {call}"),
            };
            text.push_str(&format!(
                "| {} | {} | {used} | {} |\n",
                scenario.label, task.site, task.stack
            ));
        }
    }
    text.push_str(&format!(
        "\n## Time of an allocation through `mem::Alloc`, its cell charge included\n\nA box drawn through a heap over an allocator, every box kept so that the heap draws chunks and charges its cell, by an `x86_64-host` image.\n\n| Milestone | Time |\n| --- | ---: |\n| M5 | {} |\n",
        nanoseconds(times.host.get("alloc"))
    ));
    text.push_str("\n## Waivers, per threshold, and the closure radius of each item\n\nEach item's radius is in `radii.tsv`.\n\n| Milestone | Waivers | Items | Median radius | 90th percentile | Largest |\n| --- | --- | ---: | ---: | ---: | --- |\n");
    for scenario in of(scenarios, &["M4", "M5"]) {
        let waivers: Vec<String> = scenario
            .waivers
            .iter()
            .map(|(threshold, count)| format!("{threshold} {count}"))
            .collect();
        let mut sizes: Vec<usize> = scenario.radii.iter().map(|(_, radius)| *radius).collect();
        sizes.sort_unstable();
        let at = |share: usize| sizes.get(sizes.len().saturating_sub(1) * share / 100);
        let largest = scenario
            .radii
            .first()
            .map(|(item, radius)| format!("{radius} ({item})"))
            .unwrap_or_default();
        text.push_str(&format!(
            "| {} | {} | {} | {} | {} | {largest} |\n",
            scenario.label,
            match waivers.is_empty() {
                true => "none".to_owned(),
                false => waivers.join(", "),
            },
            sizes.len(),
            at(50).copied().unwrap_or_default(),
            at(90).copied().unwrap_or_default(),
        ));
        for (item, radius) in &scenario.radii {
            radii.push_str(&format!("{}\t{item}\t{radius}\n", scenario.label));
        }
    }
    text.push_str("\n## Parameter lists with more than 4 parameters, `self` included\n\n| Milestone | Heads | Parameters |\n| --- | ---: | --- |\n");
    for scenario in of(scenarios, &["M5"]) {
        let heads = &scenario.measured.wide_heads;
        let mut counts: BTreeMap<usize, usize> = BTreeMap::new();
        for (_, count) in heads {
            *counts.entry(*count).or_default() += 1;
        }
        let counts: Vec<String> = counts
            .iter()
            .map(|(parameters, heads)| format!("{heads} with {parameters}"))
            .collect();
        text.push_str(&format!(
            "| {} | {} | {} |\n",
            scenario.label,
            heads.len(),
            counts.join(", ")
        ));
    }
    text.push_str("\n## Task closures outside lexical spawns, with their `take` captures\n\n| Milestone | Site | `take` captures |\n| --- | --- | --- |\n");
    for scenario in of(scenarios, &["M2", "M6"]) {
        for (site, taken) in &scenario.measured.task_closures {
            text.push_str(&format!(
                "| {} | {site} | {} |\n",
                scenario.label,
                taken.join(", ")
            ));
        }
    }
    text.push_str(&format!(
        "\n## Time of a machine seam and of `hw::switch_to`\n\nA seam's round trip, a self-interrupt sent through the local APIC and taken, and a thread switch, half of a round trip between two tasks each waking the other with a signal, are timed under the emulator.\n\n| Milestone | Seam round trip | Thread switch |\n| --- | ---: | ---: |\n| M4 | {} | {} |\n",
        nanoseconds(times.kernel.get("seam")),
        nanoseconds(times.kernel.get("switch"))
    ));
    text.push_str(&format!(
        "\n## Time of a generation check in a capability table\n\nAn operation failing at a stale capability's generation check: on `x86_64-host` for M2, on `x86_64-bare` under the emulator for M4.\n\n| Milestone | Time |\n| --- | ---: |\n| M2 | {} |\n| M4 | {} |\n",
        nanoseconds(times.host.get("generation")),
        nanoseconds(times.kernel.get("generation"))
    ));
    text.push_str("\n## Nanbench's metrics\n\nIn `nanbench.md`.\n\n## Token count of each primer tier\n\nIn `primer.md`.\n");
    (text, within)
}

/// Picoseconds as nanoseconds to a tenth.
fn nanoseconds(picoseconds: Option<&u64>) -> String {
    match picoseconds {
        Some(picoseconds) => format!("{}.{} ns", picoseconds / 1_000, picoseconds % 1_000 / 100),
        None => "not taken".to_owned(),
    }
}
