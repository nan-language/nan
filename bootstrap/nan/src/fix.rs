//! Exact fixes: applying them to sources, the in-memory re-check deciding which ones a diagnostic
//! offers, and the edits showing them.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use crate::check;
use crate::diagnostic::{Class, Code, Diagnostic, Fix, Offer, Replacement};
use crate::scope::Scope;
use crate::syntax::file::SourceFile;
use crate::workspace::Workspace;

/// The sources of the files fixes change, once their replacements, which do not overlap, are
/// made.
pub fn apply(workspace: &Workspace, fixes: &[&Fix]) -> HashMap<String, Vec<u8>> {
    let mut by_path: BTreeMap<&str, Vec<&Replacement>> = BTreeMap::new();
    for fix in fixes {
        for (path, replacement) in &fix.0 {
            by_path.entry(path).or_default().push(replacement);
        }
    }
    by_path
        .into_iter()
        .map(|(path, mut replacements)| {
            replacements.sort_by_key(|replacement| (replacement.start, replacement.end));
            let (_, file) = workspace.file(path).expect("fixes change workspace files");
            let source = &file.syntax.source;
            let mut changed = Vec::with_capacity(source.len());
            let mut at = 0;
            for replacement in replacements {
                changed.extend_from_slice(&source[at..replacement.start as usize]);
                changed.extend_from_slice(replacement.text.as_bytes());
                at = replacement.end as usize;
            }
            changed.extend_from_slice(&source[at..]);
            (path.to_owned(), changed)
        })
        .collect()
}

/// Where the bytes of one file stand once a fix's replacements in it are made.
struct Shift<'f> {
    replacements: Vec<&'f Replacement>,
}

impl<'f> Shift<'f> {
    fn of(fix: &'f Fix) -> HashMap<&'f str, Shift<'f>> {
        let mut shifts: HashMap<&str, Shift> = HashMap::new();
        for (path, replacement) in &fix.0 {
            shifts
                .entry(path)
                .or_insert_with(|| Shift {
                    replacements: Vec::new(),
                })
                .replacements
                .push(replacement);
        }
        for shift in shifts.values_mut() {
            shift
                .replacements
                .sort_by_key(|replacement| (replacement.start, replacement.end));
        }
        shifts
    }

    fn delta(replacement: &Replacement) -> i64 {
        replacement.text.len() as i64 - i64::from(replacement.end - replacement.start)
    }

    /// The position of the character at `position` once the fix is made, none inside a changed
    /// text.
    fn position(&self, position: u32) -> Option<u32> {
        let mut shifted = i64::from(position);
        for replacement in &self.replacements {
            if position >= replacement.end {
                shifted += Shift::delta(replacement);
            } else if position >= replacement.start {
                return None;
            }
        }
        Some(shifted as u32)
    }

    /// The bytes each replacement's text holds once the fix is made.
    fn replaced(&self) -> Vec<(u32, u32)> {
        let mut delta = 0;
        self.replacements
            .iter()
            .map(|replacement| {
                let start = (i64::from(replacement.start) + delta) as u32;
                delta += Shift::delta(replacement);
                (start, start + replacement.text.len() as u32)
            })
            .collect()
    }

    /// The bytes a run holds once the fix is made: from the first replacement starting before
    /// `from` on, and through each starting before `to`.
    fn run(&self, from: u32, to: u32, end_of_file: bool) -> (u32, u32) {
        let shifted = |position: u32, inclusive: bool| {
            let delta: i64 = self
                .replacements
                .iter()
                .filter(|replacement| {
                    replacement.start < position || inclusive && replacement.start == position
                })
                .map(|replacement| Shift::delta(replacement))
                .sum();
            (i64::from(position) + delta) as u32
        };
        (shifted(from, false), shifted(to, end_of_file))
    }

    /// The bytes a construct holds once the fix is made, with the texts inserted at its ends.
    fn construct(&self, (start, end): (u32, u32)) -> (u32, u32) {
        let mut delta = 0;
        let mut after = None;
        let mut before = None;
        for replacement in &self.replacements {
            let new_start = (i64::from(replacement.start) + delta) as u32;
            let new_end = new_start + replacement.text.len() as u32;
            let inserted = replacement.start == replacement.end;
            if after.is_none()
                && (replacement.end > start || inserted && replacement.start == start)
            {
                after = Some(if replacement.start < start {
                    new_start
                } else {
                    (i64::from(start) + delta) as u32
                });
            }
            if replacement.start < end || inserted && replacement.start == end {
                before = Some(if replacement.end > end {
                    new_end
                } else {
                    (i64::from(end) + delta + Shift::delta(replacement)) as u32
                });
            }
            delta += Shift::delta(replacement);
        }
        let start = after.unwrap_or_else(|| (i64::from(start) + delta) as u32);
        (start, before.unwrap_or(end).max(start))
    }
}

/// Whether a construct overlaps a replacement's text: it starts before the text's end and ends
/// after its start.
fn overlaps((start, end): (u32, u32), (from, to): (u32, u32)) -> bool {
    start < to.max(from + 1) && end > from
}

/// The workspace once a fix is made.
fn fixed(workspace: &Workspace, fix: &Fix) -> Workspace {
    workspace.with_sources(&apply(workspace, &[fix]))
}

/// Keeps in each diagnostic the fixes it offers among those its rule names, each re-checked over
/// the scope written `scope` from `current`; where its rule states neither expected nor found,
/// they show the reported construct before and after the first offered fix.
pub fn offer(
    workspace: &Workspace,
    current: &Path,
    scope: Option<&str>,
    diagnostics: &mut [Diagnostic],
) {
    let before = diagnostics.to_vec();
    for diagnostic in diagnostics.iter_mut() {
        let candidates = std::mem::take(&mut diagnostic.fixes);
        let mut offered = Vec::new();
        let mut fitting = Vec::new();
        let mut first = None;
        for (index, fix) in candidates.into_iter().enumerate() {
            let after = fixed(workspace, &fix);
            let Some((scope, _)) = Scope::resolve(&after, current, scope) else {
                continue;
            };
            let rechecked = check::reported(&after, &scope);
            if !passes(workspace, diagnostic, &fix, &before, &after, &rechecked) {
                continue;
            }
            if first.is_none() {
                first = Some(after);
            }
            fitting.push(index);
            offered.push(fix);
            if diagnostic.offer == Offer::First {
                break;
            }
        }
        if diagnostic.offer == Offer::Unique && offered.len() != 1 {
            offered.clear();
        }
        if diagnostic.code == Code::NameUnknown {
            let (listed, unfit): (Vec<_>, Vec<_>) = std::mem::take(&mut diagnostic.because)
                .into_iter()
                .enumerate()
                .partition(|(index, _)| fitting.contains(index));
            diagnostic.because = listed
                .into_iter()
                .chain(unfit)
                .map(|(_, note)| note)
                .collect();
        }
        if let (Some(fix), Some(after)) = (offered.first(), &first) {
            describe(workspace, after, fix, diagnostic);
        }
        diagnostic.fixes = offered;
    }
}

/// Whether a fix of `fixed` passes the re-check: no diagnostic of its code left on its construct
/// or a replacement, and each error either one the workspace reported before or one the fixed
/// diagnostic kept unchecked, beside the replacements: anywhere for a declaration, whose uses
/// depend on it, and otherwise later in its item or on its line than its failing token, or than
/// its construct's start when the parse did not fail.
fn passes(
    workspace: &Workspace,
    fixed: &Diagnostic,
    fix: &Fix,
    before: &[Diagnostic],
    after: &Workspace,
    rechecked: &[Diagnostic],
) -> bool {
    let shifts = Shift::of(fix);
    let start = |diagnostic: &Diagnostic| diagnostic.span.map(|(start, _)| start);
    let moved = |diagnostic: &Diagnostic| -> Option<u32> {
        let start = start(diagnostic)?;
        match diagnostic.path.as_deref().and_then(|path| shifts.get(path)) {
            Some(shift) => shift.position(start),
            None => Some(start),
        }
    };
    let replaced = |diagnostic: &Diagnostic| {
        let (Some(path), Some(span)) = (&diagnostic.path, diagnostic.span) else {
            return false;
        };
        shifts.get(path.as_str()).is_some_and(|shift| {
            shift
                .replaced()
                .into_iter()
                .any(|text| overlaps(span, text))
        })
    };
    let fixed_at = moved(fixed);
    let left = rechecked.iter().any(|diagnostic| {
        diagnostic.code == fixed.code
            && (replaced(diagnostic)
                || diagnostic.path == fixed.path
                    && fixed_at.is_some()
                    && start(diagnostic) == fixed_at)
    });
    if left {
        return false;
    }
    let failing = match fixed.failing {
        Some(failing) => fixed
            .path
            .as_deref()
            .and_then(|path| shifts.get(path))
            .map_or(Some(failing), |shift| shift.position(failing)),
        None => fixed_at,
    };
    let anchor = failing.or_else(|| {
        let shift = shifts.get(fixed.path.as_deref()?)?;
        shift.replaced().first().map(|(start, _)| *start)
    });
    let declaration = fixed.failing.is_none()
        && fixed
            .path
            .as_deref()
            .zip(fixed.span)
            .is_some_and(|(path, (start, _))| declares(workspace, path, start));
    let masked = |diagnostic: &Diagnostic| {
        if declaration {
            return true;
        }
        let (Some(path), Some(anchor), Some(at)) = (&fixed.path, anchor, start(diagnostic)) else {
            return false;
        };
        if diagnostic.path.as_ref() != Some(path) || at < anchor {
            return false;
        }
        let Some((_, file)) = after.file(path) else {
            return false;
        };
        let line = line_of(&file.syntax, anchor);
        diagnostic.line == Some(line)
            || file.syntax.item_at(line).is_some_and(|item| {
                diagnostic
                    .line
                    .is_some_and(|line| file.syntax.item_lines(item).contains(&line))
            })
    };
    rechecked
        .iter()
        .filter(|diagnostic| diagnostic.code.class() == Class::Error)
        .all(|diagnostic| {
            let reported = before.iter().any(|earlier| {
                earlier.code == diagnostic.code
                    && earlier.path == diagnostic.path
                    && moved(earlier) == start(diagnostic)
            });
            reported || !replaced(diagnostic) && masked(diagnostic)
        })
}

/// The errors a workspace changed by `fix` reports that the workspace did not report before, in
/// `before`: each of another code, or not at the place an earlier one moves to.
pub fn introduced(before: &[Diagnostic], fix: &Fix, rechecked: &[Diagnostic]) -> Vec<Diagnostic> {
    let shifts = Shift::of(fix);
    let start = |diagnostic: &Diagnostic| diagnostic.span.map(|(start, _)| start);
    let moved = |diagnostic: &Diagnostic| -> Option<u32> {
        let start = start(diagnostic)?;
        match diagnostic.path.as_deref().and_then(|path| shifts.get(path)) {
            Some(shift) => shift.position(start),
            None => Some(start),
        }
    };
    rechecked
        .iter()
        .filter(|diagnostic| diagnostic.code.class() == Class::Error)
        .filter(|diagnostic| {
            !before.iter().any(|earlier| {
                earlier.code == diagnostic.code
                    && earlier.path == diagnostic.path
                    && moved(earlier) == start(diagnostic)
            })
        })
        .cloned()
        .collect()
}

/// Whether a byte of a file lies in an item's declaration, whose uses anywhere depend on it,
/// rather than in a body.
fn declares(workspace: &Workspace, path: &str, position: u32) -> bool {
    let Some((_, file)) = workspace.file(path) else {
        return false;
    };
    let syntax = &file.syntax;
    syntax.items.iter().any(|item| {
        let (first, last) = (&syntax.tokens[item.first], &syntax.tokens[item.end - 1]);
        first.start <= position
            && position <= last.end
            && item.tree.as_ref().is_some_and(|tree| {
                crate::syntax::items::Item { file: syntax, tree }.declares(position)
            })
    })
}

/// The line holding a byte of a file.
fn line_of(file: &SourceFile, position: u32) -> u32 {
    file.lines
        .partition_point(|line| line.start <= position)
        .max(1) as u32
}

/// Writes expected and found from the first offered fix, when the rule states neither and the
/// fix changes the reported construct's text: found is that text as written, expected that text
/// once the fix is made, absent when empty. A rule offering the first of alternatives states as
/// expected the alternative that is the fix.
fn describe(workspace: &Workspace, after: &Workspace, fix: &Fix, diagnostic: &mut Diagnostic) {
    let stated = diagnostic.expected.is_some() || diagnostic.found.is_some();
    if stated && diagnostic.offer != Offer::First {
        return;
    }
    let (Some(path), Some(span)) = (&diagnostic.path, diagnostic.span) else {
        return;
    };
    let shifts = Shift::of(fix);
    let Some(shift) = shifts.get(path.as_str()) else {
        return;
    };
    let touches = shift.replacements.iter().any(|replacement| {
        if replacement.start == replacement.end {
            span.0 <= replacement.start && replacement.start <= span.1
        } else {
            replacement.start < span.1 && replacement.end > span.0
        }
    });
    let (Some((_, file)), Some((_, fixed))) = (workspace.file(path), after.file(path)) else {
        return;
    };
    if !touches {
        return;
    }
    let expected = construct_text(&fixed.syntax, shift.construct(span));
    diagnostic.expected = (!expected.is_empty()).then_some(expected);
    if !stated {
        diagnostic.found = Some(construct_text(&file.syntax, span));
    }
}

/// The text of a construct as diagnostics write it: as written on one line, and on several as
/// its lines without comments and indentation, joined by single spaces.
fn construct_text(file: &SourceFile, (start, end): (u32, u32)) -> String {
    let first = line_of(file, start);
    let last = line_of(file, end.max(start + 1) - 1);
    if first == last {
        return String::from_utf8_lossy(&file.source[start as usize..end as usize]).into_owned();
    }
    let parts: Vec<String> = file.lines[first as usize - 1..last as usize]
        .iter()
        .map(|line| {
            let from = line.start.max(start) as usize;
            let to = line.comment.unwrap_or(line.end).min(end) as usize;
            String::from_utf8_lossy(&file.source[from..to.max(from)])
                .trim()
                .to_owned()
        })
        .filter(|part| !part.is_empty())
        .collect();
    parts.join(" ")
}

/// The lines holding a fix's changed texts, by path, an inserted text held by the line holding
/// the character after it, or by the last line at the end of its file.
pub fn lines(workspace: &Workspace, fix: &Fix) -> Vec<(String, u32)> {
    let mut lines = Vec::new();
    for (path, replacement) in &fix.0 {
        let (_, file) = workspace.file(path).expect("fixes change workspace files");
        let (first, last) = held(&file.syntax, replacement);
        lines.extend((first..=last).map(|line| (path.clone(), line)));
    }
    lines
}

/// The first and last lines holding a replacement's changed text.
fn held(file: &SourceFile, replacement: &Replacement) -> (u32, u32) {
    let first = line_of(file, replacement.start);
    let last = if replacement.end > replacement.start {
        line_of(file, replacement.end - 1)
    } else {
        first
    };
    (first, last)
}

/// An edit showing a fix in one file: a run of whole lines and those lines once it is made.
pub struct Edit {
    pub path: String,
    pub old: Vec<String>,
    pub new: Vec<String>,
}

/// The edits showing a fix, by path: each a run of the lines holding its changed texts, extended
/// by earlier lines, or later ones at the file's start, until its text occurs once in its file.
pub fn edits(workspace: &Workspace, fix: &Fix) -> Vec<Edit> {
    let changed = apply(workspace, &[fix]);
    let shifts = Shift::of(fix);
    let mut paths: Vec<&str> = shifts.keys().copied().collect();
    paths.sort_unstable();
    let mut edits = Vec::new();
    for path in paths {
        let (_, file) = workspace.file(path).expect("fixes change workspace files");
        let file = &file.syntax;
        let shift = &shifts[path];
        let runs: Vec<(u32, u32)> = shift
            .replacements
            .iter()
            .map(|replacement| unique(file, held(file, replacement)))
            .collect();
        for (first, last) in merged(runs) {
            let from = file.lines[first as usize - 1].start;
            let to = line_end(file, last);
            let (new_from, new_to) = shift.run(from, to, to as usize == file.source.len());
            edits.push(Edit {
                path: path.to_owned(),
                old: lines_of(&file.source[from as usize..to as usize]),
                new: lines_of(&changed[path][new_from as usize..new_to as usize]),
            });
        }
    }
    edits
}

/// The byte after a line's line end, or the file's end.
fn line_end(file: &SourceFile, line: u32) -> u32 {
    file.lines
        .get(line as usize)
        .map_or(file.source.len() as u32, |next| next.start)
}

/// A run of lines extended by the line before it, or after it at the file's first line, until
/// its text occurs exactly once in the file, overlapping occurrences counted.
fn unique(file: &SourceFile, (mut first, mut last): (u32, u32)) -> (u32, u32) {
    let count = file.lines.len() as u32;
    loop {
        let text = &file.source
            [file.lines[first as usize - 1].start as usize..line_end(file, last) as usize];
        let occurrences = (0..=file.source.len().saturating_sub(text.len()))
            .filter(|&at| file.source[at..].starts_with(text))
            .count();
        if occurrences <= 1 || first == 1 && last == count {
            return (first, last);
        }
        if first > 1 {
            first -= 1;
        } else {
            last += 1;
        }
    }
}

/// Runs of lines, those sharing a line merged, in line order.
fn merged(mut runs: Vec<(u32, u32)>) -> Vec<(u32, u32)> {
    runs.sort_unstable();
    let mut merged: Vec<(u32, u32)> = Vec::new();
    for (first, last) in runs {
        match merged.last_mut() {
            Some((_, end)) if first <= *end => *end = (*end).max(last),
            _ => merged.push((first, last)),
        }
    }
    merged
}

/// The lines of a text of whole lines, without their line ends.
fn lines_of(text: &[u8]) -> Vec<String> {
    if text.is_empty() {
        return Vec::new();
    }
    let text = String::from_utf8_lossy(text);
    text.strip_suffix('\n')
        .unwrap_or(&text)
        .split('\n')
        .map(str::to_owned)
        .collect()
}

#[cfg(test)]
mod tests;
