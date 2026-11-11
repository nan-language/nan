// The Fibonacci number of the size, by doubly recursive calls.

#include <cstdint>
#include <iostream>
#include <stdexcept>
#include <string>

// The number the first argument states in decimal.
static std::uint64_t size(int argc, char** argv) {
    if (argc < 2) {
        throw std::invalid_argument("a size argument");
    }
    return std::stoull(argv[1]);
}

static std::uint64_t fib(std::uint64_t n) {
    if (n < 2) {
        return n;
    }
    return fib(n - 1) + fib(n - 2);
}

int main(int argc, char** argv) {
    std::cout << fib(size(argc, argv)) << '\n';
}
