//! The metrics of the recorded runs, each share with its 95% Wilson score interval: first-compile
//! success over write tasks, the fix rate of each diagnostic code, and comprehension over read
//! tasks' questions.

use std::collections::BTreeMap;

use serde_json::Value;

/// The 95% Wilson score interval of `hits` out of `trials`.
fn interval(hits: usize, trials: usize) -> (f64, f64) {
    if trials == 0 {
        return (0.0, 1.0);
    }
    let z = 1.96_f64;
    let n = trials as f64;
    let share = hits as f64 / n;
    let centre = (share + z * z / (2.0 * n)) / (1.0 + z * z / n);
    let half = z * (share * (1.0 - share) / n + z * z / (4.0 * n * n)).sqrt() / (1.0 + z * z / n);
    ((centre - half).max(0.0), (centre + half).min(1.0))
}

/// A share with its interval, as `k/n p% [low%, high%]`.
fn share(hits: usize, trials: usize) -> String {
    if trials == 0 {
        return "none".to_owned();
    }
    let (low, high) = interval(hits, trials);
    format!(
        "{hits}/{trials} {:.0}% [{:.0}%, {:.0}%]",
        100.0 * hits as f64 / trials as f64,
        100.0 * low,
        100.0 * high
    )
}

#[derive(Default)]
struct Tally {
    tasks: usize,
    compiled: (usize, usize),
    answered: (usize, usize),
    accepted: (usize, usize),
    fixes: BTreeMap<String, (usize, usize)>,
}

/// The metrics document of the records of every model's runs.
pub fn document(records: &[Value]) -> String {
    let mut models: BTreeMap<String, Tally> = BTreeMap::new();
    for record in records {
        let tally = models
            .entry(record["model"].as_str().unwrap_or_default().to_owned())
            .or_default();
        tally.tasks += 1;
        if let Some(compiled) = record["first_compile"].as_bool() {
            tally.compiled.0 += usize::from(compiled);
            tally.compiled.1 += 1;
        }
        if let Some(accepted) = record["accepted"].as_bool() {
            tally.accepted.0 += usize::from(accepted);
            tally.accepted.1 += 1;
        }
        for right in record["answers"].as_object().into_iter().flatten() {
            tally.answered.0 += usize::from(right.1.as_bool() == Some(true));
            tally.answered.1 += 1;
        }
        for (code, counts) in record["fixes"].as_object().into_iter().flatten() {
            let entry = tally.fixes.entry(code.clone()).or_default();
            entry.0 += counts[0].as_u64().unwrap_or(0) as usize;
            entry.1 += counts[1].as_u64().unwrap_or(0) as usize;
        }
    }
    if models.is_empty() {
        return "# Nanbench metrics\n\nNo model run is recorded.\n".to_owned();
    }
    let mut text = String::from(
        "# Nanbench metrics\n\nEach share is `hits/trials`, its percentage and its 95% Wilson score interval. First-compile success counts write tasks whose first compiled workspace `nan check` accepts without error; comprehension counts read-task questions answered right; a code's fix rate counts its diagnostics a model was shown that its next edit removed without introducing an error; acceptance counts write and repair tasks whose acceptance tests pass.\n\n| Model | Tasks | First-compile success | Comprehension | Acceptance |\n| --- | ---: | --- | --- | --- |\n",
    );
    for (model, tally) in &models {
        text.push_str(&format!(
            "| {model} | {} | {} | {} | {} |\n",
            tally.tasks,
            share(tally.compiled.0, tally.compiled.1),
            share(tally.answered.0, tally.answered.1),
            share(tally.accepted.0, tally.accepted.1)
        ));
    }
    text.push_str(
        "\n## Fix rate per diagnostic code\n\n| Model | Code | Fix rate |\n| --- | --- | --- |\n",
    );
    for (model, tally) in &models {
        for (code, (shown, fixed)) in &tally.fixes {
            text.push_str(&format!(
                "| {model} | `{code}` | {} |\n",
                share(*fixed, *shown)
            ));
        }
    }
    text
}
