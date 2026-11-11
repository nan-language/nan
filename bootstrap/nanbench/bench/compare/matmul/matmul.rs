//! The sum of the entries of the product of two square matrices whose order is the size.

/// The number the first argument states in decimal.
fn size() -> u64 {
    let arg = std::env::args().nth(1).expect("a size argument");
    arg.parse().expect("a size in decimal")
}

/// A row-major `n` by `n` matrix whose entry at row `i` and column `j` is `(i * step + j) % 17`.
fn matrix(n: usize, step: usize) -> Vec<f64> {
    let mut entries = Vec::with_capacity(n * n);
    for i in 0..n {
        for j in 0..n {
            entries.push(((i * step + j) % 17) as f64);
        }
    }
    entries
}

/// The product of two row-major `n` by `n` matrices.
fn product(a: &[f64], b: &[f64], n: usize) -> Vec<f64> {
    let mut c = vec![0.0; n * n];
    for i in 0..n {
        for k in 0..n {
            let factor = a[i * n + k];
            for j in 0..n {
                c[i * n + j] += factor * b[k * n + j];
            }
        }
    }
    c
}

fn main() {
    let order = size() as usize;
    let a = matrix(order, 7);
    let b = matrix(order, 3);
    let c = product(&a, &b, order);
    let mut total = 0.0;
    for entry in c {
        total += entry;
    }
    println!("{}", total as u64);
}
