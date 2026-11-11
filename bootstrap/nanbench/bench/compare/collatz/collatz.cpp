// The longest Collatz sequence starting below the size.

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

// The steps the Collatz sequence from `start` takes to reach 1.
static std::uint64_t steps(std::uint64_t start) {
    std::uint64_t value = start;
    std::uint64_t count = 0;
    while (value != 1) {
        if (value % 2 == 0) {
            value /= 2;
        } else {
            value = 3 * value + 1;
        }
        count += 1;
    }
    return count;
}

int main(int argc, char** argv) {
    std::uint64_t n = size(argc, argv);
    std::uint64_t best = 1;
    std::uint64_t longest = 0;
    for (std::uint64_t start = 1; start < n; ++start) {
        std::uint64_t count = steps(start);
        if (count > longest) {
            best = start;
            longest = count;
        }
    }
    std::cout << best << ' ' << longest << '\n';
}
