// The size's numbers written into a string, then read back.

#include <cstdint>
#include <format>
#include <iostream>
#include <iterator>
#include <stdexcept>
#include <string>

// The number the first argument states in decimal.
static std::uint64_t size(int argc, char** argv) {
    if (argc < 2) {
        throw std::invalid_argument("a size argument");
    }
    return std::stoull(argv[1]);
}

// The numbers below `n`, each followed by a comma.
static std::string numbers(std::uint64_t n) {
    std::string written;
    for (std::uint64_t number = 0; number < n; ++number) {
        std::format_to(std::back_inserter(written), "{},", number);
    }
    return written;
}

// The sum of the numbers a text of numbers each followed by a comma holds.
static std::uint64_t sum(const std::string& written) {
    std::uint64_t total = 0;
    std::uint64_t number = 0;
    for (char byte : written) {
        if (byte == ',') {
            total += number;
            number = 0;
        } else {
            number = number * 10 + static_cast<std::uint64_t>(byte - '0');
        }
    }
    return total;
}

int main(int argc, char** argv) {
    std::string written = numbers(size(argc, argv));
    std::cout << written.size() << ' ' << sum(written) << '\n';
}
