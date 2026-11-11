//! The size's keys counted in a hash map, then as many others looked up.

use std::collections::HashMap;

/// The number the first argument states in decimal.
fn size() -> u64 {
    let arg = std::env::args().nth(1).expect("a size argument");
    arg.parse().expect("a size in decimal")
}

/// The state after `state` of a 64-bit linear congruential generator.
fn next(state: u64) -> u64 {
    state
        .wrapping_mul(6_364_136_223_846_793_005)
        .wrapping_add(1_442_695_040_888_963_407)
}

fn main() {
    let n = size();
    let mut counts: HashMap<u64, u64> = HashMap::new();
    let mut state = 1;
    for _ in 0..n {
        state = next(state);
        *counts.entry(state >> 40).or_insert(0) += 1;
    }
    let mut found = 0;
    for _ in 0..n {
        state = next(state);
        if let Some(count) = counts.get(&(state >> 40)) {
            found += count;
        }
    }
    println!("{} {found}", counts.len());
}
