//! The output contract: verdict lines around records, output budgets and JSON Lines.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    Ok,
    Failed,
    Invalid,
    InternalError,
}

impl Verdict {
    pub fn word(self) -> &'static str {
        match self {
            Verdict::Ok => "ok",
            Verdict::Failed => "failed",
            Verdict::Invalid => "invalid",
            Verdict::InternalError => "internal-error",
        }
    }

    pub fn status(self) -> i32 {
        match self {
            Verdict::Ok => 0,
            Verdict::Failed => 1,
            Verdict::Invalid => 2,
            Verdict::InternalError => 3,
        }
    }
}

/// A header line of a record word and fields, and the continuation lines the record holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Record {
    pub word: String,
    pub fields: Vec<String>,
    pub lines: Vec<String>,
}

impl Record {
    fn text(&self) -> String {
        let mut text = self.word.clone();
        for field in &self.fields {
            text.push(' ');
            text.push_str(field);
        }
        text.push('\n');
        for line in &self.lines {
            text.push_str("    ");
            text.push_str(line);
            text.push('\n');
        }
        text
    }

    fn json(&self) -> String {
        format!(
            "{{\"fields\":{},\"lines\":{},\"record\":{}}}\n",
            json_list(&self.fields),
            json_list(&self.lines),
            json_string(&self.word)
        )
    }
}

/// How a command's output is printed: as JSON Lines or text, within a budget in units, after
/// skipping records.
pub struct Format {
    pub json: bool,
    pub budget: Option<usize>,
    pub skip: usize,
}

pub struct Output {
    /// The command line without `nan`, as the verdict line writes it.
    pub command: String,
    /// The command line an elision line names, without its `--skip`.
    pub elision: String,
    pub verdict: Verdict,
    pub summary: Vec<(&'static str, usize)>,
    pub records: Vec<Record>,
}

const DEFAULT_BUDGET: usize = 4_000;

fn units(bytes: usize) -> usize {
    bytes.div_ceil(4)
}

/// The size of records in units, as their text prints them.
pub fn units_of(records: &[Record]) -> usize {
    units(records.iter().map(|record| record.text().len()).sum())
}

impl Output {
    fn verdict_text(&self) -> String {
        let counts: Vec<String> = self
            .summary
            .iter()
            .map(|(name, count)| format!("{name} {count}"))
            .collect();
        format!(
            "{} {}: {}\n",
            self.verdict.word(),
            self.command,
            counts.join(", ")
        )
    }

    fn verdict_json(&self) -> String {
        let mut counts = self.summary.clone();
        counts.sort();
        let members: Vec<String> = counts
            .iter()
            .map(|(name, count)| format!("{}:{count}", json_string(name)))
            .collect();
        format!(
            "{{\"command\":{},\"record\":\"verdict\",\"summary\":{{{}}},\"verdict\":{}}}\n",
            json_string(&self.command),
            members.join(","),
            json_string(self.verdict.word())
        )
    }

    fn elision_command(&self, skip: usize) -> String {
        format!("nan {} --skip {skip}", self.elision)
    }

    /// The output text: the verdict line, the most records after the skipped ones that fit the
    /// budget with the verdict lines and any elision line, at least one, and the verdict line.
    pub fn render(&self, format: &Format) -> String {
        let budget = format.budget.unwrap_or(DEFAULT_BUDGET);
        let skip = format.skip.min(self.records.len());
        let remaining = &self.records[skip..];
        let texts: Vec<String> = remaining.iter().map(Record::text).collect();
        let verdict = self.verdict_text();
        let elision = |printed: usize| {
            let left = remaining.len() - printed;
            (left > 0).then(|| {
                format!(
                    "elided {left} records: {}\n",
                    self.elision_command(skip + printed)
                )
            })
        };
        let mut printed = 0;
        let mut size = 2 * verdict.len();
        for count in 1..=texts.len() {
            size += texts[count - 1].len();
            let fits = units(size + elision(count).map_or(0, |line| line.len())) <= budget;
            if fits || count == 1 {
                printed = count;
            }
        }
        if !format.json {
            let mut out = verdict.clone();
            out.extend(texts[..printed].iter().map(String::as_str));
            out.extend(elision(printed));
            out.push_str(&verdict);
            return out;
        }
        let verdict = self.verdict_json();
        let mut out = verdict.clone();
        out.extend(remaining[..printed].iter().map(Record::json));
        if printed < remaining.len() {
            let command = self.elision_command(skip + printed);
            let count = remaining.len() - printed;
            out.push_str(&format!(
                "{{\"command\":{},\"count\":{count},\"record\":\"elided\"}}\n",
                json_string(&command)
            ));
        }
        out.push_str(&verdict);
        out
    }
}

/// A JSON string escaping only `"` and `\`.
fn json_string(text: &str) -> String {
    let mut out = String::from("\"");
    for c in text.chars() {
        if c == '"' || c == '\\' {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('"');
    out
}

fn json_list(items: &[String]) -> String {
    let items: Vec<String> = items.iter().map(|item| json_string(item)).collect();
    format!("[{}]", items.join(","))
}

/// A command-line argument as a verdict line writes it: as is, or as a text literal when it is
/// empty or holds a space, `"` or `\`.
pub fn argument(arg: &str) -> String {
    if !arg.is_empty() && !arg.contains([' ', '"', '\\']) {
        return arg.to_owned();
    }
    let mut out = String::from("\"");
    for c in arg.chars() {
        match c {
            '"' | '\\' => {
                out.push('\\');
                out.push(c);
            }
            c if c.is_control() => out.push_str(&format!("\\u{{{:X}}}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Output text holds no control character but LF: other controls are written as `\u{H}` and bytes
/// that are not well-formed UTF-8 as U+FFFD.
pub fn printable(bytes: &[u8]) -> String {
    let mut out = String::new();
    for c in String::from_utf8_lossy(bytes).chars() {
        if c.is_control() {
            out.push_str(&format!("\\u{{{:X}}}", c as u32));
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests;
