//! Nanbench, the benchmark that gates NaN language versions: the token counts of the primer
//! tiers, the measurements of the freeze, and the runs of named models over the task corpus with
//! the metrics they give. Each command writes its results under `nanbench/results/`.

mod agent;
mod corpus;
mod measure;
mod model;
mod report;
mod timing;
mod tokens;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use model::Model;

/// The repository's root.
fn repository() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..")
}

/// Where nanbench publishes its results.
fn results() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("results")
}

/// The task corpus.
fn tasks() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tasks")
}

/// Where nanbench keeps what it fetches and builds.
fn cache() -> PathBuf {
    repository().join("target/nanbench")
}

/// Writes a results file and says so.
fn publish(name: &str, text: &str) -> Result<(), String> {
    let path = results().join(name);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    fs::write(&path, text).map_err(|error| error.to_string())?;
    println!("wrote nanbench/results/{name}");
    Ok(())
}

/// `nanbench tokens`: counts each primer tier, writes `primer.md`, and fails when a tier is
/// above its cap.
fn tokens() -> Result<bool, String> {
    let tokenizers = tokens::Tokenizers::load(&cache())?;
    let mut table = format!(
        "# Primer tiers\n\nThe token count of each tier's `nan primer` text under each tokenizer; a tier's size is the largest, and its cap bounds it.\n\n| Tier | {} | Size | Cap |\n| --- | ---: | ---: | ---: | ---: | ---: |\n",
        tokens::NAMES.join(" | ")
    );
    let mut within = true;
    for (tier, cap) in tokens::TIERS {
        let counts = tokenizers.counts(&tokens::primer(tier, &cache()));
        let size = counts.iter().copied().max().unwrap_or_default();
        within &= size <= cap;
        let counts: Vec<String> = counts.iter().map(usize::to_string).collect();
        table.push_str(&format!(
            "| {tier} | {} | {size} | {cap} |\n",
            counts.join(" | ")
        ));
        println!("tier {tier} {} size {size} cap {cap}", counts.join(" "));
    }
    publish("primer.md", &table)?;
    Ok(within)
}

/// `nanbench measure`: takes the times, saying so once they are taken, then the other
/// measurements of the freeze, writes `measurements.md` and `radii.tsv`, and fails when a
/// bounded measurement misses its bound.
fn measure() -> Result<bool, String> {
    let fault = |error: std::io::Error| error.to_string();
    let work = cache().join("measure");
    if work.exists() {
        fs::remove_dir_all(&work).map_err(fault)?;
    }
    fs::create_dir_all(work.join("home")).map_err(fault)?;
    nan::use_home(work.join("home"), Some(work.join("store")));
    let times = timing::take(&repository(), &work)?;
    println!("took the times");
    let scenarios = measure::scenarios(&repository(), &work)?;
    let mut radii = String::new();
    let (sections, within) = measure::sections(&scenarios, &times, &mut radii);
    let text = format!(
        "# Measurements of the freeze\n\nTaken by `nanbench measure` over the workspace packages of each milestone's scenario, M5's being the NaN front end's workspace. Times are taken on 8 hardware threads of a machine with {}.\n\n{sections}",
        timing::machine()
    );
    publish("measurements.md", &text)?;
    publish("radii.tsv", &radii)?;
    Ok(within)
}

/// Runs tasks with a model, each in a workspace of its own under the cache, and gives each
/// task's record.
fn runs(model: &Model, tasks: &[corpus::Task]) -> Result<Vec<serde_json::Value>, String> {
    let work = cache().join("runs");
    fs::create_dir_all(work.join("home")).map_err(|error| error.to_string())?;
    nan::use_home(work.join("home"), Some(work.join("store")));
    let system = agent::system(&work);
    let mut records = Vec::new();
    for task in tasks {
        let dir = work.join(model.name()).join(&task.id);
        let outcome = agent::run(task, model, &system, &dir)?;
        let record = outcome.record(task, model);
        println!("{record}");
        records.push(record);
    }
    Ok(records)
}

/// `nanbench corpus`: runs every task with the reference solver, which must compile at once,
/// pass every acceptance test and answer every question right, each repair task starting from
/// a workspace with errors.
fn check_corpus() -> Result<bool, String> {
    let tasks = corpus::load(&tasks())?;
    let mut sound = true;
    for (task, record) in tasks.iter().zip(runs(&Model::Reference, &tasks)?) {
        let broken = task.kind != corpus::Kind::Repair || {
            let fixes = record["fixes"].as_object();
            fixes.is_some_and(|fixes| !fixes.is_empty())
        };
        let answered = record["answers"]
            .as_object()
            .is_none_or(|answers| answers.values().all(|right| right.as_bool() == Some(true)));
        let passes = broken
            && answered
            && record["first_compile"].as_bool() != Some(false)
            && record["accepted"].as_bool() != Some(false);
        if !passes {
            println!("unsound task {}", task.id);
        }
        sound &= passes;
    }
    Ok(sound)
}

/// `nanbench run <model> [<task>...]`: runs the tasks named, every one when none is, with a
/// model, and records each run in `runs/<model>.jsonl` in place of its earlier record.
fn run(args: &[String]) -> Result<bool, String> {
    let Some((name, wanted)) = args.split_first() else {
        return Err("usage: nanbench run <model> [<task>...]".to_owned());
    };
    let model = Model::of(name);
    let tasks: Vec<corpus::Task> = corpus::load(&tasks())?
        .into_iter()
        .filter(|task| wanted.is_empty() || wanted.contains(&task.id))
        .collect();
    let file = format!("runs/{name}.jsonl");
    let mut kept: Vec<serde_json::Value> = fs::read_to_string(results().join(&file))
        .unwrap_or_default()
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect();
    let records = runs(&model, &tasks)?;
    kept.retain(|record: &serde_json::Value| {
        !records.iter().any(|new| new["task"] == record["task"])
    });
    kept.extend(records);
    kept.sort_by(|left, right| left["task"].as_str().cmp(&right["task"].as_str()));
    let lines: Vec<String> = kept.iter().map(serde_json::Value::to_string).collect();
    publish(&file, &(lines.join("\n") + "\n"))?;
    Ok(true)
}

/// `nanbench report`: writes `nanbench.md` from every model's recorded runs.
fn report() -> Result<bool, String> {
    let mut records = Vec::new();
    let runs = results().join("runs");
    let files = match runs.exists() {
        true => fs::read_dir(&runs)
            .map_err(|error| error.to_string())?
            .collect(),
        false => Vec::new(),
    };
    for entry in files {
        let text = fs::read_to_string(entry.map_err(|error| error.to_string())?.path())
            .map_err(|error| error.to_string())?;
        records.extend(
            text.lines()
                .filter_map(|line| serde_json::from_str(line).ok()),
        );
    }
    publish("nanbench.md", &report::document(&records))?;
    Ok(true)
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let outcome = match args.first().map(String::as_str) {
        Some("tokens") => tokens(),
        Some("measure") => measure(),
        Some("corpus") => check_corpus(),
        Some("run") => run(&args[1..]),
        Some("report") => report(),
        _ => Err(
            "usage: nanbench tokens | measure | corpus | run <model> [<task>...] | report"
                .to_owned(),
        ),
    };
    match outcome {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(error) => {
            eprintln!("nanbench: {error}");
            ExitCode::from(2)
        }
    }
}
