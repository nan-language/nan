//! The longest Collatz sequence starting below the size.

/// The number the first argument states in decimal.
fn size() -> u64 {
    let arg = std::env::args().nth(1).expect("a size argument");
    arg.parse().expect("a size in decimal")
}

/// The steps the Collatz sequence from `start` takes to reach 1.
fn steps(start: u64) -> u64 {
    let mut value = start;
    let mut count = 0;
    while value != 1 {
        if value % 2 == 0 {
            value /= 2;
        } else {
            value = 3 * value + 1;
        }
        count += 1;
    }
    count
}

fn main() {
    let n = size();
    let mut best = 1;
    let mut longest = 0;
    for start in 1..n {
        let count = steps(start);
        if count > longest {
            best = start;
            longest = count;
        }
    }
    println!("{best} {longest}");
}
