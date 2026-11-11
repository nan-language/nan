//! The most prefix reversals that sort a permutation of the size's elements, and a checksum of them all.

/// The number the first argument states in decimal.
fn size() -> u64 {
    let arg = std::env::args().nth(1).expect("a size argument");
    arg.parse().expect("a size in decimal")
}

/// The most elements a permutation holds.
const MOST: usize = 16;

/// The permutations' checksum and the most flips one takes.
struct Outcome {
    checksum: i64,
    most_flips: u64,
}

/// The flips that sort `permutation`: each reverses the prefix up to the position its first element names.
fn flips(permutation: [usize; MOST]) -> u64 {
    let mut order = permutation;
    let mut count = 0;
    loop {
        let leading = order[0];
        if leading == 0 {
            return count;
        }
        let mut low = 0;
        let mut high = leading;
        while low < high {
            let swapped = order[low];
            order[low] = order[high];
            order[high] = swapped;
            low += 1;
            high -= 1;
        }
        count += 1;
    }
}

/// The flips of every permutation of `n` elements, visited in the order of the counting rotations.
fn fannkuch(n: usize) -> Outcome {
    let mut permutation = [0; MOST];
    let mut counts = [0; MOST];
    for i in 0..n {
        permutation[i] = i;
    }
    let mut checksum = 0;
    let mut most_flips = 0;
    let mut even = true;
    let mut rotated = n;
    loop {
        while rotated != 1 {
            counts[rotated - 1] = rotated;
            rotated -= 1;
        }
        let count = flips(permutation);
        if count > most_flips {
            most_flips = count;
        }
        if even {
            checksum += count as i64;
        } else {
            checksum -= count as i64;
        }
        even = !even;
        loop {
            if rotated == n {
                return Outcome {
                    checksum,
                    most_flips,
                };
            }
            let head = permutation[0];
            for i in 0..rotated {
                permutation[i] = permutation[i + 1];
            }
            permutation[rotated] = head;
            counts[rotated] -= 1;
            if counts[rotated] > 0 {
                break;
            }
            rotated += 1;
        }
    }
}

fn main() {
    let elements = size() as usize;
    assert!(elements != 0 && elements <= MOST, "a size from 1 to {MOST}");
    let outcome = fannkuch(elements);
    println!("{} {}", outcome.checksum, outcome.most_flips);
}
