// The most prefix reversals that sort a permutation of the size's elements, and a checksum of them all.

#include <array>
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

// The most elements a permutation holds.
constexpr std::size_t MOST = 16;

using Permutation = std::array<std::size_t, MOST>;

// The permutations' checksum and the most flips one takes.
struct Outcome {
    std::int64_t checksum;
    std::uint64_t most_flips;
};

// The flips that sort `permutation`: each reverses the prefix up to the position its first element names.
static std::uint64_t flips(Permutation permutation) {
    Permutation order = permutation;
    std::uint64_t count = 0;
    while (true) {
        std::size_t leading = order[0];
        if (leading == 0) {
            return count;
        }
        std::size_t low = 0;
        std::size_t high = leading;
        while (low < high) {
            std::size_t swapped = order[low];
            order[low] = order[high];
            order[high] = swapped;
            low += 1;
            high -= 1;
        }
        count += 1;
    }
}

// The flips of every permutation of `n` elements, visited in the order of the counting rotations.
static Outcome fannkuch(std::size_t n) {
    Permutation permutation{};
    Permutation counts{};
    for (std::size_t i = 0; i < n; ++i) {
        permutation[i] = i;
    }
    std::int64_t checksum = 0;
    std::uint64_t most_flips = 0;
    bool even = true;
    std::size_t rotated = n;
    while (true) {
        while (rotated != 1) {
            counts[rotated - 1] = rotated;
            rotated -= 1;
        }
        std::uint64_t count = flips(permutation);
        if (count > most_flips) {
            most_flips = count;
        }
        if (even) {
            checksum += static_cast<std::int64_t>(count);
        } else {
            checksum -= static_cast<std::int64_t>(count);
        }
        even = !even;
        while (true) {
            if (rotated == n) {
                return Outcome{checksum, most_flips};
            }
            std::size_t head = permutation[0];
            for (std::size_t i = 0; i < rotated; ++i) {
                permutation[i] = permutation[i + 1];
            }
            permutation[rotated] = head;
            counts[rotated] -= 1;
            if (counts[rotated] > 0) {
                break;
            }
            rotated += 1;
        }
    }
}

int main(int argc, char** argv) {
    std::size_t elements = size(argc, argv);
    if (elements == 0 || elements > MOST) {
        throw std::invalid_argument("a size from 1 to 16");
    }
    Outcome outcome = fannkuch(elements);
    std::cout << outcome.checksum << ' ' << outcome.most_flips << '\n';
}
