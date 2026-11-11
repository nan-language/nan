// A bytecode loop of the size's rounds, run by an interpreter matching each instruction.

#include <array>
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

enum class Code : std::uint8_t { Load, Add, Mul, Xor, Shr, JumpIfOdd, JumpIfLess, Jump, Halt };

// An instruction over eight registers, which it names by index; each code reads some of the fields.
struct Op {
    Code code;
    std::size_t out = 0;
    std::size_t left = 0;
    std::size_t right = 0;
    std::size_t goal = 0;
    std::uint64_t value = 0;
    std::uint32_t amount = 0;
};

// A program hashing the numbers below `rounds` with FNV-1a and a shift, counting the even hashes into register 6 and summing the odd ones into register 7.
static std::vector<Op> bytecode(std::uint64_t rounds) {
    std::vector<Op> ops;
    ops.push_back(Op{.code = Code::Load, .out = 0, .value = 0});
    ops.push_back(Op{.code = Code::Load, .out = 1, .value = rounds});
    ops.push_back(Op{.code = Code::Load, .out = 2, .value = 0xCBF29CE484222325});
    ops.push_back(Op{.code = Code::Load, .out = 3, .value = 0x100000001B3});
    ops.push_back(Op{.code = Code::Load, .out = 4, .value = 1});
    ops.push_back(Op{.code = Code::Load, .out = 6, .value = 0});
    ops.push_back(Op{.code = Code::Load, .out = 7, .value = 0});
    ops.push_back(Op{.code = Code::Xor, .out = 2, .left = 2, .right = 0});
    ops.push_back(Op{.code = Code::Mul, .out = 2, .left = 2, .right = 3});
    ops.push_back(Op{.code = Code::Shr, .out = 5, .left = 2, .amount = 29});
    ops.push_back(Op{.code = Code::Xor, .out = 2, .left = 2, .right = 5});
    ops.push_back(Op{.code = Code::JumpIfOdd, .left = 2, .goal = 14});
    ops.push_back(Op{.code = Code::Add, .out = 6, .left = 6, .right = 4});
    ops.push_back(Op{.code = Code::Jump, .goal = 15});
    ops.push_back(Op{.code = Code::Add, .out = 7, .left = 7, .right = 2});
    ops.push_back(Op{.code = Code::Add, .out = 0, .left = 0, .right = 4});
    ops.push_back(Op{.code = Code::JumpIfLess, .left = 0, .right = 1, .goal = 7});
    ops.push_back(Op{.code = Code::Halt});
    return ops;
}

// The register bank once `program` halts.
static std::array<std::uint64_t, 8> run(const std::vector<Op>& program) {
    std::array<std::uint64_t, 8> bank{};
    std::size_t at = 0;
    while (true) {
        const Op& op = program[at];
        switch (op.code) {
        case Code::Load:
            bank[op.out] = op.value;
            at += 1;
            break;
        case Code::Add:
            bank[op.out] = bank[op.left] + bank[op.right];
            at += 1;
            break;
        case Code::Mul:
            bank[op.out] = bank[op.left] * bank[op.right];
            at += 1;
            break;
        case Code::Xor:
            bank[op.out] = bank[op.left] ^ bank[op.right];
            at += 1;
            break;
        case Code::Shr:
            bank[op.out] = bank[op.left] >> op.amount;
            at += 1;
            break;
        case Code::JumpIfOdd:
            at = bank[op.left] % 2 == 1 ? op.goal : at + 1;
            break;
        case Code::JumpIfLess:
            at = bank[op.left] < bank[op.right] ? op.goal : at + 1;
            break;
        case Code::Jump:
            at = op.goal;
            break;
        case Code::Halt:
            return bank;
        }
    }
}

int main(int argc, char** argv) {
    std::vector<Op> ops = bytecode(size(argc, argv));
    std::array<std::uint64_t, 8> bank = run(ops);
    std::cout << bank[2] << ' ' << bank[6] << ' ' << bank[7] << '\n';
}
