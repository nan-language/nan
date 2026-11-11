// Binary trees of boxes up to the size's depth, built, counted and dropped.

#include <cstdint>
#include <iostream>
#include <memory>
#include <stdexcept>
#include <string>

// The number the first argument states in decimal.
static std::uint64_t size(int argc, char** argv) {
    if (argc < 2) {
        throw std::invalid_argument("a size argument");
    }
    return std::stoull(argv[1]);
}

// A node of a binary tree, with both children or neither.
struct Node {
    std::unique_ptr<Node> left;
    std::unique_ptr<Node> right;
};

// A complete binary tree `depth` levels deep below its root.
static Node tree(std::uint64_t depth) {
    if (depth == 0) {
        return Node{};
    }
    return Node{std::make_unique<Node>(tree(depth - 1)), std::make_unique<Node>(tree(depth - 1))};
}

// The nodes of the tree under `node`, itself included.
static std::uint64_t count(const Node& node) {
    if (!node.left || !node.right) {
        return 1;
    }
    return 1 + count(*node.left) + count(*node.right);
}

int main(int argc, char** argv) {
    std::uint64_t n = size(argc, argv);
    std::uint64_t deepest = n < 6 ? 6 : n;
    std::cout << count(tree(deepest + 1)) << '\n';
    Node long_lived = tree(deepest);
    for (std::uint64_t depth = 4; depth <= deepest; depth += 2) {
        std::uint64_t trees = std::uint64_t{1} << (deepest - depth + 4);
        std::uint64_t total = 0;
        for (std::uint64_t round = 0; round < trees; ++round) {
            total += count(tree(depth));
        }
        std::cout << trees << ' ' << depth << ' ' << total << '\n';
    }
    std::cout << count(long_lived) << '\n';
}
