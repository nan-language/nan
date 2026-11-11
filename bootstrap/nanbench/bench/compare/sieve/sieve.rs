//! The primes up to the size, counted by a sieve of Eratosthenes.

/// The number the first argument states in decimal.
fn size() -> u64 {
    let arg = std::env::args().nth(1).expect("a size argument");
    arg.parse().expect("a size in decimal")
}

/// The primes up to `limit`.
fn primes(limit: usize) -> u64 {
    let mut composite = vec![0u8; limit + 1];
    let mut count = 0;
    for candidate in 2..=limit {
        if composite[candidate] == 0 {
            count += 1;
            let mut multiple = candidate * candidate;
            while multiple <= limit {
                composite[multiple] = 1;
                multiple += candidate;
            }
        }
    }
    count
}

fn main() {
    println!("{}", primes(size() as usize));
}
