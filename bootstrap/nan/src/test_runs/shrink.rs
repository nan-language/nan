//! Shrinking: moving the recorded draws of a failing case toward their shrink points by
//! candidates, reruns of the case with changed draws kept when they fail the same way.

use std::io;

use super::outcome::{Draw, Run};

/// The most candidates a shrinking runs.
const CANDIDATES: usize = 4_096;

/// The value within a draw's bounds nearest 0.
pub fn shrink_point(draw: &Draw) -> i128 {
    0.clamp(draw.low, draw.high)
}

/// The shrinking of a case: the run of the last candidate kept, or the case's own, and the
/// count of candidates run.
struct Shrinking<F> {
    kept: Run,
    candidates: usize,
    rerun: F,
}

impl<F: FnMut(&[i128]) -> io::Result<Option<Run>>> Shrinking<F> {
    fn exhausted(&self) -> bool {
        self.candidates >= CANDIDATES
    }

    /// Runs the candidate changing one recorded draw's value, keeping it when it fails the way
    /// the case does.
    fn attempt(&mut self, index: usize, value: i128) -> io::Result<bool> {
        self.candidates += 1;
        let mut values: Vec<i128> = self.kept.draws.iter().map(|draw| draw.value).collect();
        values[index] = value;
        match (self.rerun)(&values)? {
            Some(run) => {
                self.kept = run;
                Ok(true)
            }
            None => Ok(false),
        }
    }
}

/// Shrinks a failing case from its run: each pass tries each recorded draw's shrink point, else
/// bisects between it and the draw's value, the draw keeping the upper end, until a pass changes
/// no draw or the candidates run out. `rerun` runs a candidate's draws and gives its run when it
/// fails the way the case does.
pub fn shrink(
    failing: Run,
    rerun: impl FnMut(&[i128]) -> io::Result<Option<Run>>,
) -> io::Result<Run> {
    let mut shrinking = Shrinking {
        kept: failing,
        candidates: 0,
        rerun,
    };
    let mut changed = true;
    while changed && !shrinking.exhausted() {
        changed = false;
        let mut index = 0;
        while index < shrinking.kept.draws.len() && !shrinking.exhausted() {
            let draw = shrinking.kept.draws[index];
            let point = shrink_point(&draw);
            if draw.value != point {
                if shrinking.attempt(index, point)? {
                    changed = true;
                } else {
                    let (mut low, mut high) = (point, draw.value);
                    while (high - low).abs() > 1 && !shrinking.exhausted() {
                        let middle = low + (high - low) / 2;
                        if shrinking.attempt(index, middle)? {
                            high = middle;
                            changed = true;
                        } else {
                            low = middle;
                        }
                    }
                }
            }
            index += 1;
        }
    }
    Ok(shrinking.kept)
}
