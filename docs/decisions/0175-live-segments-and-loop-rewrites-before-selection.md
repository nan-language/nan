# 0175. Live segments and loop rewrites before selection

Status: accepted

## Context

Profiles of the comparison's programs ([0170](0170-run-times-compared-with-rust-and-cpp.md)) as the NaN toolchain builds them showed loops doing far more than their source asks. Each value had one live interval from its first position to its last, so a cold trap block laid out after the hot code, reading a loop counter for its report, kept the counter live over the whole loop: the next counter could not take its register, and every iteration moved values between registers or spilled them. Building SSA form from variables left block parameters that only carried a value around a loop or received one value, and each moved through registers on every edge. Invariant arithmetic, such as `i * n` in an inner loop indexing `i * n + j`, ran on every iteration, and field accesses recomputed `index * size` for every field. The `inclusive` byte of a `Range`, held in a word with padding once its slot became values, was tested at every loop exit though no path set it. A load of a stack slot's element added the slot's address to the index with its own instruction, and stores folded no index at all.

## Decision

- **Liveness is a set of segments per virtual register**: for each block it is live in, from its first to its last position there, joined with the next block's where they touch. A physical register holds every virtual register whose segments never meet another's it holds, and fixed ranges and saves across calls count only where a virtual register is live.
- **The function is rewritten again before selection**, each rewrite copying it through the copier only when it changes something, after threading:
    - **Trimming**: a block parameter that only branches passing it to parameters read is dropped with the arguments passed to it, and one whose branches pass a single value other than itself stands for that value.
    - **Known bits**: the bits of each integer value every path gives alike, through bitwise operations, shifts, extensions, comparisons, selects and block parameters, fold a value they fix into a constant and a branch they decide into a jump.
    - **Speculation**: a conditional branch whose arms hold only constants and arithmetic other than multiplication and division, comparisons, conversions and selects, joining one block, computes both arms and passes the join the values its condition selects; a branch of an arm to a cold block that traps is taken only when its arm is.
    - **Hoisting**: an instruction of those kinds, division by a constant other than 0 and -1 included, whose operands are defined outside a natural loop with one entry, moves to the end of the one block outside the loop branching to its header, out of every enclosing loop it can.
- **Every copy takes the value of an alike computation before it** in the block, or in the blocks continuing it, and leaves out unread loads that cannot fault. A copy threading edges continues no block, as edges threaded into a block reach it past the block before it.
- **Selection folds more into operands**: a stack slot's address as the base of an indexed operand, an addition into every memory operand reading it, an index into stores, emission computing a store's address first when its value, base and index are all spilled, and a mask's bits into `test` for a comparison with zero.

Alternatives rejected:

- **Splitting intervals or colouring a graph**: more code than segments, which already free the registers trap blocks held.
- **Speculating multiplications and divisions**: their latency lengthens every iteration that takes the other arm; Collatz steps ran twice as long.
- **Value numbering over the dominator tree**: the repeated computations measured lay within one block.

## Consequences

- The comparison's programs run fewer instructions: 27% fewer for the matrix product, 14% for Collatz steps, 11% for n-body steps, 9% for the hash map and 7% for the interpreter.
- Code generation copies a function up to four more times; each copy is skipped when its rewrite finds nothing.
- AArch64 code takes the same rewrites and allocation.
