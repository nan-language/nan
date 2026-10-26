# 0062. Startup and cold-load costs of the self-built toolchain

Status: accepted

## Context

The self-built toolchain ran `nan primer` on a one-package workspace in 13 ms against the bootstrap's 7, and a first `nan check` of such a workspace in an empty user directory in 178 ms against 111. Its profiles showed costs paid by every process or by every first run:

- Every process built the grammar's tables from the ebnf blocks, about three million instructions, though a command whose files the build cache holds parses nothing and lexes only machine files.
- Loading the image relocated 24,402 addresses, 20,888 of them the function addresses of the line tables, writing about 140 pages that the process then owns: half a millisecond before `main`.
- The first executor thread set up the ring of every other thread and started each, about 60 µs per processor of the affinity set, before the command ran.
- The build cache checked every file it wrote or read with BLAKE3, about 55 instructions per byte in the self-built code: a tenth of a first check.
- The std files the toolchain carries were parsed by the command's task alone, while the crew of workers slept.

## Decision

- **The grammar builds its parser's tables at the first parse needing them.** Loading it builds only the lexer's tables, reading the ebnf blocks up to the lexical rules the rejected code points follow from.
- **Manifests take their syntax from the build cache** like every other workspace file ([0054](0054-source-syntax-in-the-build-cache.md)).
- **A file of the build cache holds a 128-bit checksum** of its contents: two lanes of 64 bits folding each word of eight bytes by a multiplication and a rotation, then MurmurHash3's avalanche. The checksum tells a file cut short or altered from the one written; keys stay BLAKE3 hashes.
- **On `x86_64-host`, a function record of the line tables holds its function's address as the signed 32-bit distance from the record**, which the static link resolves; the code reading the tables adds the record's address. Machine targets keep absolute addresses.
- **Each executor thread but the first sets up its own ring and starts the next thread**, so that starting costs the first thread one ring and one thread whatever the processors. A thread the system refuses its ring or the next thread ends the process as the runtime unavailable, as `Init` does for the first.
- **A batch of the crew parses the std files the toolchain carries** that the build cache lacks, when the std workspace is first read ([0059](0059-sized-spawns-and-parallel-check-batches.md)).
- **`nan primer` runs without a crew**, since it checks nothing.
- **The self-check compares host lowerings without `Init`, `ThreadMain`, the trap routines reading the line tables and the functions of std's `hw` package**, whose synchronisation reads them inline; programs and test runs compare what those do, as for the routines of [0050](0050-host-support-routines-of-the-nan-lowering.md).

Rejected alternatives:

- **Position-dependent executables:** they would leave out every dynamic relocation, but the specification makes host images position-independent.
- **The grammar's tables in the build cache:** decoding them costs a command more than lexing needs, and a first run still builds them.
- **A faster BLAKE3 for cache files:** unrolling its rounds spills more than it saves, and the checksum needs no collision resistance, the cache being trusted as the toolchain is.
- **Starting executor threads when a second task is ready:** every waiting primitive would have to start them.

## Consequences

- A process of the self-built toolchain starts in about the bootstrap's time, whatever its processors; `nan primer` runs in about 4 ms.
- Cache files are 16 bytes shorter, and a cache record the bootstrap wrote reads as absent to the NaN toolchain.
- A first check parses std on every processor of the crew; on one processor it still takes about a third longer than the bootstrap's.
- A trap report or a stopping task reads the line tables through one more addition.
