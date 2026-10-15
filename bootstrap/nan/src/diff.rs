//! Line diffs: an old and a new text merged along a longest common subsequence of their lines.

/// The lines of `old` and `new` merged along their longest common subsequence of lines whose old,
/// then new, line numbers are least in lexicographic order: kept lines prefixed with 2 spaces,
/// removed ones `- ` and added ones `+ `, removals first.
pub fn merged(old: &[String], new: &[String]) -> Vec<String> {
    let mut longest = vec![vec![0usize; new.len() + 1]; old.len() + 1];
    for at in (0..old.len()).rev() {
        for other in (0..new.len()).rev() {
            longest[at][other] = if old[at] == new[other] {
                longest[at + 1][other + 1] + 1
            } else {
                longest[at + 1][other].max(longest[at][other + 1])
            };
        }
    }
    let mut lines = Vec::new();
    let (mut at, mut other) = (0, 0);
    loop {
        let remaining = longest[at][other];
        let next = (remaining > 0)
            .then(|| {
                (at..old.len()).find_map(|from| {
                    (other..new.len())
                        .find(|to| {
                            old[from] == new[*to] && longest[from + 1][to + 1] + 1 == remaining
                        })
                        .map(|to| (from, to))
                })
            })
            .flatten();
        let (until, until_new) = next.unwrap_or((old.len(), new.len()));
        lines.extend(old[at..until].iter().map(|line| format!("- {line}")));
        lines.extend(new[other..until_new].iter().map(|line| format!("+ {line}")));
        let Some((kept, kept_new)) = next else {
            return lines;
        };
        lines.push(format!("  {}", old[kept]));
        (at, other) = (kept + 1, kept_new + 1);
    }
}

#[cfg(test)]
mod tests;
