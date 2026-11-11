//! The Fibonacci number of the size, by doubly recursive calls.

/// The number the first argument states in decimal.
fn size() -> u64 {
    let arg = std::env::args().nth(1).expect("a size argument");
    arg.parse().expect("a size in decimal")
}

fn fib(n: u64) -> u64 {
    if n < 2 {
        return n;
    }
    fib(n - 1) + fib(n - 2)
}

fn main() {
    println!("{}", fib(size()));
}
