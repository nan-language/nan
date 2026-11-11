// The primes up to the size, counted by a sieve of Eratosthenes.

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

// The primes up to `limit`.
static std::uint64_t primes(std::size_t limit) {
    std::vector<std::uint8_t> composite(limit + 1);
    std::uint64_t count = 0;
    for (std::size_t candidate = 2; candidate <= limit; ++candidate) {
        if (composite[candidate] == 0) {
            count += 1;
            std::size_t multiple = candidate * candidate;
            while (multiple <= limit) {
                composite[multiple] = 1;
                multiple += candidate;
            }
        }
    }
    return count;
}

int main(int argc, char** argv) {
    std::cout << primes(size(argc, argv)) << '\n';
}
