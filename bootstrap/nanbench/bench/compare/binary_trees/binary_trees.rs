//! Binary trees of boxes up to the size's depth, built, counted and dropped.

/// The number the first argument states in decimal.
fn size() -> u64 {
    let arg = std::env::args().nth(1).expect("a size argument");
    arg.parse().expect("a size in decimal")
}

/// A node of a binary tree, with both children or neither.
struct Node {
    left: Option<Box<Node>>,
    right: Option<Box<Node>>,
}

/// A complete binary tree `depth` levels deep below its root.
fn tree(depth: u64) -> Node {
    if depth == 0 {
        return Node {
            left: None,
            right: None,
        };
    }
    Node {
        left: Some(Box::new(tree(depth - 1))),
        right: Some(Box::new(tree(depth - 1))),
    }
}

impl Node {
    /// The nodes of the tree under this one, itself included.
    fn count(&self) -> u64 {
        let (Some(left), Some(right)) = (&self.left, &self.right) else {
            return 1;
        };
        1 + left.count() + right.count()
    }
}

fn main() {
    let n = size();
    let deepest = if n < 6 { 6 } else { n };
    println!("{}", tree(deepest + 1).count());
    let long_lived = tree(deepest);
    let mut depth = 4;
    while depth <= deepest {
        let trees = 1u64 << (deepest - depth + 4);
        let mut total = 0;
        for _ in 0..trees {
            total += tree(depth).count();
        }
        println!("{trees} {depth} {total}");
        depth += 2;
    }
    println!("{}", long_lived.count());
}
