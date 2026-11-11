// The sum of the entries of the product of two square matrices whose order is the size.

#include <cstdint>
#include <iostream>
#include <stdexcept>
#include <string>
#include <vector>

// The number the first argument states in decimal.
static std::uint64_t size(int argc, char** argv) {
    if (argc < 2) {
        throw std::invalid_argument("a size argument");
    }
    return std::stoull(argv[1]);
}

// A row-major `n` by `n` matrix whose entry at row `i` and column `j` is `(i * step + j) % 17`.
static std::vector<double> matrix(std::size_t n, std::size_t step) {
    std::vector<double> entries;
    entries.reserve(n * n);
    for (std::size_t i = 0; i < n; ++i) {
        for (std::size_t j = 0; j < n; ++j) {
            entries.push_back(static_cast<double>((i * step + j) % 17));
        }
    }
    return entries;
}

// The product of two row-major `n` by `n` matrices.
static std::vector<double> product(const std::vector<double>& a, const std::vector<double>& b, std::size_t n) {
    std::vector<double> c(n * n, 0.0);
    for (std::size_t i = 0; i < n; ++i) {
        for (std::size_t k = 0; k < n; ++k) {
            double factor = a[i * n + k];
            for (std::size_t j = 0; j < n; ++j) {
                c[i * n + j] += factor * b[k * n + j];
            }
        }
    }
    return c;
}

int main(int argc, char** argv) {
    std::size_t order = size(argc, argv);
    std::vector<double> a = matrix(order, 7);
    std::vector<double> b = matrix(order, 3);
    std::vector<double> c = product(a, b, order);
    double total = 0.0;
    for (double entry : c) {
        total += entry;
    }
    std::cout << static_cast<std::uint64_t>(total) << '\n';
}
