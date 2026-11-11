//! The size's numbers written into a string, then read back.

use std::fmt::Write;

/// The number the first argument states in decimal.
fn size() -> u64 {
    let arg = std::env::args().nth(1).expect("a size argument");
    arg.parse().expect("a size in decimal")
}

/// The numbers below `n`, each followed by a comma.
fn numbers(n: u64) -> String {
    let mut written = String::new();
    for number in 0..n {
        write!(written, "{number},").expect("a string takes any text");
    }
    written
}

/// The sum of the numbers a text of numbers each followed by a comma holds.
fn sum(written: &[u8]) -> u64 {
    let mut total = 0;
    let mut number = 0;
    for &byte in written {
        if byte == b',' {
            total += number;
            number = 0;
        } else {
            number = number * 10 + u64::from(byte - b'0');
        }
    }
    total
}

fn main() {
    let written = numbers(size());
    println!("{} {}", written.len(), sum(written.as_bytes()));
}
