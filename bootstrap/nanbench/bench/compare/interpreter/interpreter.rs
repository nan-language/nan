//! A bytecode loop of the size's rounds, run by an interpreter matching each instruction.

/// The number the first argument states in decimal.
fn size() -> u64 {
    let arg = std::env::args().nth(1).expect("a size argument");
    arg.parse().expect("a size in decimal")
}

/// An instruction over eight registers, which it names by index.
#[derive(Clone, Copy)]
enum Op {
    Load {
        out: usize,
        value: u64,
    },
    Add {
        out: usize,
        left: usize,
        right: usize,
    },
    Mul {
        out: usize,
        left: usize,
        right: usize,
    },
    Xor {
        out: usize,
        left: usize,
        right: usize,
    },
    Shr {
        out: usize,
        source: usize,
        amount: u32,
    },
    JumpIfOdd {
        source: usize,
        goal: usize,
    },
    JumpIfLess {
        left: usize,
        right: usize,
        goal: usize,
    },
    Jump {
        goal: usize,
    },
    Halt,
}

/// A program hashing the numbers below `rounds` with FNV-1a and a shift, counting the even hashes into register 6 and summing the odd ones into register 7.
fn bytecode(rounds: u64) -> Vec<Op> {
    let mut ops = Vec::new();
    ops.push(Op::Load { out: 0, value: 0 });
    ops.push(Op::Load {
        out: 1,
        value: rounds,
    });
    ops.push(Op::Load {
        out: 2,
        value: 0xCBF2_9CE4_8422_2325,
    });
    ops.push(Op::Load {
        out: 3,
        value: 0x100_0000_01B3,
    });
    ops.push(Op::Load { out: 4, value: 1 });
    ops.push(Op::Load { out: 6, value: 0 });
    ops.push(Op::Load { out: 7, value: 0 });
    ops.push(Op::Xor {
        out: 2,
        left: 2,
        right: 0,
    });
    ops.push(Op::Mul {
        out: 2,
        left: 2,
        right: 3,
    });
    ops.push(Op::Shr {
        out: 5,
        source: 2,
        amount: 29,
    });
    ops.push(Op::Xor {
        out: 2,
        left: 2,
        right: 5,
    });
    ops.push(Op::JumpIfOdd {
        source: 2,
        goal: 14,
    });
    ops.push(Op::Add {
        out: 6,
        left: 6,
        right: 4,
    });
    ops.push(Op::Jump { goal: 15 });
    ops.push(Op::Add {
        out: 7,
        left: 7,
        right: 2,
    });
    ops.push(Op::Add {
        out: 0,
        left: 0,
        right: 4,
    });
    ops.push(Op::JumpIfLess {
        left: 0,
        right: 1,
        goal: 7,
    });
    ops.push(Op::Halt);
    ops
}

/// The register bank once `program` halts.
fn run(program: &[Op]) -> [u64; 8] {
    let mut bank = [0u64; 8];
    let mut at = 0;
    loop {
        match program[at] {
            Op::Load { out, value } => {
                bank[out] = value;
                at += 1;
            }
            Op::Add { out, left, right } => {
                bank[out] = bank[left].wrapping_add(bank[right]);
                at += 1;
            }
            Op::Mul { out, left, right } => {
                bank[out] = bank[left].wrapping_mul(bank[right]);
                at += 1;
            }
            Op::Xor { out, left, right } => {
                bank[out] = bank[left] ^ bank[right];
                at += 1;
            }
            Op::Shr {
                out,
                source,
                amount,
            } => {
                bank[out] = bank[source] >> amount;
                at += 1;
            }
            Op::JumpIfOdd { source, goal } => {
                at = if bank[source] % 2 == 1 { goal } else { at + 1 };
            }
            Op::JumpIfLess { left, right, goal } => {
                at = if bank[left] < bank[right] {
                    goal
                } else {
                    at + 1
                };
            }
            Op::Jump { goal } => {
                at = goal;
            }
            Op::Halt => return bank,
        }
    }
}

fn main() {
    let ops = bytecode(size());
    let bank = run(&ops);
    println!("{} {} {}", bank[2], bank[6], bank[7]);
}
