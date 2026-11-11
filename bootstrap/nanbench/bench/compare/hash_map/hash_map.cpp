// The size's keys counted in a hash map, then as many others looked up.

#include <cstdint>
#include <iostream>
#include <stdexcept>
#include <string>
#include <unordered_map>

// The number the first argument states in decimal.
static std::uint64_t size(int argc, char** argv) {
    if (argc < 2) {
        throw std::invalid_argument("a size argument");
    }
    return std::stoull(argv[1]);
}

// The state after `state` of a 64-bit linear congruential generator.
static std::uint64_t next(std::uint64_t state) {
    return state * 6364136223846793005u + 1442695040888963407u;
}

int main(int argc, char** argv) {
    std::uint64_t n = size(argc, argv);
    std::unordered_map<std::uint64_t, std::uint64_t> counts;
    std::uint64_t state = 1;
    for (std::uint64_t round = 0; round < n; ++round) {
        state = next(state);
        counts[state >> 40] += 1;
    }
    std::uint64_t found = 0;
    for (std::uint64_t round = 0; round < n; ++round) {
        state = next(state);
        auto entry = counts.find(state >> 40);
        if (entry != counts.end()) {
            found += entry->second;
        }
    }
    std::cout << counts.size() << ' ' << found << '\n';
}
