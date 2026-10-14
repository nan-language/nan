# The reference interpreter

This chapter defines the interpreter and its runs with their ends, limits and reports, const evaluation, example runs, refutation confirmation, the Ptr checks and the `const` diagnostic family.

## Interpreter

The **interpreter** is the toolchain component that executes [NIR](19-nir.md#nir) at check time. An **interpreter run** executes one NIR function, its **run function**, with inputs. The interpreter makes exactly three kinds of interpreter runs: [const evaluation](#const-evaluation), [example runs](#example-runs) and [confirmation runs](#refutation-confirmation). Programs and tests never execute in it.

- Each instruction and terminator has the behaviour that [NIR](19-nir.md#instructions) and its owning chapter state. The interpreter adds only the rules of this chapter.
- An interpreter run is made for one target and executes the units of the [target variants](03-packages-modules.md#target-variants) that target matches.
- An interpreter run checks every [obligation](18-contracts.md#obligations) site that has a trap kind and is not open, proven and assumed sites included ([statuses](18-contracts.md#statuses)), and traps with that kind when its condition is false.
- An interpreter run executes on one task, outside every [domain](11-errors-domains.md#domains). A [trap](11-errors-domains.md#traps) ends it at once, and no code of the run executes afterwards.
- The end of an interpreter run depends only on its run function, its inputs and the NIR units it executes or reads. No clock, machine, thread count or order of checking affects it.

### Run ends

An interpreter run ends in exactly one of these ways:

| End | The run |
| --- | --- |
| value | returns a value from its run function |
| failure | fails from its run function |
| trap | executes an operation that traps |
| exhaustion | would take a step past its [step limit](#run-limits) |
| violation | breaks a rule of the [Ptr checks](#checked-rules) |
| uninterpretable | reaches an uninterpretable operation, which it does not execute |
| missing | reaches a const or contribution without a value ([const evaluation](#const-evaluation)), or a function of an item the checker rejects, which has no unit |

An operation is **uninterpretable** when it is:

- an awaited call or an `await when`;
- a call of a [foreign fn](23-low-level.md#foreign-functions), or through a function value whose type has a convention clause;
- an operation that brings the device or priv [digest fact](10-effects-digests.md#digests), unless it acts on a [sim](22-testing.md#simulated-capabilities);
- the read of a [registry](25-registries.md#tables);
- an access through a `hw::Ptr` without [provenance](#interpreter-memory).

A missing end is never reported. In const evaluation, a trap is an error (`const.trapped`). In const evaluation and example runs, exhaustion is an error (`const.steps-exhausted`), a violation is an error (`const.trusted-violation`), and an uninterpretable operation is an error (`const.uninterpretable`). These diagnostics and `const.assert-false`, `example.failed` and `example.trapped` are reported on the line holding the run function's source, or for a `where` condition on the line supplying its arguments, with the [run report](#run-reports) in their because-chain.

```nan
// file: geometry/package.nan
//! Grid offsets computed with the C library.

package geometry {
    language 1
    targets x86_64-host
    allow trusted in geometry
} geometry

// file: geometry/geometry.nan
//! Fixed offsets between grid points.

foreign fn abs(value: i32) -> i32 limits pure, terminates convention "sysv"

pub const OFFSET: i32 = abs(-7) // error: const.uninterpretable
```

### Run limits

- Each instruction and terminator that an interpreter run executes is one **step**. A call of a built-in operation that copies, fills or compares n elements is n + 1 steps.
- The **step limit** of every interpreter run is 1,000,000 steps.
- A call that would make the run's call depth exceed 4,096 frames traps (`trap.stack-overflow`).

```nan
fn triangle(limit: u64) -> u64 {
    var total: u64 = 0
    for term in 0..<limit {
        total = total.wrapping_add(term)
    }
    return total
} triangle

pub const SMALL_TRIANGLE: u64 = triangle(1_000)

const assert SMALL_TRIANGLE == 499_500
```

```nan
fn triangle(limit: u64) -> u64 {
    var total: u64 = 0
    for term in 0..<limit {
        total = total.wrapping_add(term)
    }
    return total
} triangle

pub const LARGE_TRIANGLE: u64 = triangle(5_000_000) // error: const.steps-exhausted
```

### Run reports

A **run report** describes how an interpreter run ended, in the lines of a [trap report](11-errors-domains.md#trap-reports):

- For a trap, it is the trap's report.
- For exhaustion, a violation and an uninterpretable operation, its first line starts with `steps exhausted`, with the broken rule's name followed by `violation` (`bounds violation`), or with `uninterpretable`, in place of the trap kind. The operation it names is the one the run was about to execute, or for a violation the one breaking the rule. Only a violation has an operand line, `address = A, extent = E, len = L, access = N`: the pointer's address, its extent's address and length, and the access's length in bytes.
- For a run function that yields `false`, it is one operand line giving both sides when its expression is a comparison, and empty otherwise.

## Const evaluation

**Const evaluation** is the interpreter run that computes a value at check time from a **const context**. The const contexts are exactly the initializer of a [const item](02-items.md#constants), the condition of a const assert, the value of a [contribution](25-registries.md#contributions), and the `where` condition of a [const parameter](08-generics-interfaces.md#generic-parameters) whose arguments are all values. Its run function is the `body` [function](19-nir.md#functions) of the const, const assert or contribution, without inputs, or the `where` function with the arguments as inputs.

- A `constant` instruction yields the value of the named const's or contribution's own const evaluation. A const or contribution whose const evaluation ends without a value has no value.
- A const's value, with the data it refers to, becomes read-only data of every image holding the const. A `hw::Ptr` in it keeps the address the run gave it, without provenance.
- A const or contribution whose value refers to an [extent](#interpreter-memory) other than literal data and the data of consts and contributions is an error (`const.view-not-static`).
- A const assert whose condition yields `false` is an error (`const.assert-false`).

```nan
const FRAME_BYTES: u32 = 1_500

const HEADER_BYTES: u32 = 10

pub const PAYLOAD_BYTES: u32 = FRAME_BYTES - HEADER_BYTES

pub const FRAMES_PER_CHUNK: u32 = frames_in(65_536)

pub const GREETING: Str = "link ready"

const assert FRAMES_PER_CHUNK == 43

fn frames_in(bytes: u32) -> u32 {
    return bytes / FRAME_BYTES
} frames_in
```

```nan
const FRAME_BYTES: u32 = 1_500

const HEADER_BYTES: u32 = 10

pub const SPARE_BYTES: u32 = HEADER_BYTES - FRAME_BYTES // error: const.trapped
```

```nan
const FRAME_BYTES: u32 = 1_500

const assert FRAME_BYTES <= 1_024 // error: const.assert-false
```

## Example runs

An **example run** executes the `example` function of one [example](18-contracts.md#examples) that is not disputed, without inputs.

In an interpreter run, `testing::world()` returns a world whose [sims](22-testing.md#simulated-capabilities) behave as in a [test run](22-testing.md#test-runs) whose seed is 0 ([seeded scheduler](22-testing.md#seeded-scheduler)), except that its allocator names a root cell ([budgets](12-memory.md#budgets)) with a limit of 16 MiB whose supply is [interpreter memory](#interpreter-memory), and that writes to the test trace are discarded.

```nan
pub fn checksum(bytes: Slice[u8]) -> u8
    example checksum(b"") == 0
    example checksum(b"AB") == 131
{
    var total: u8 = 0
    for byte in bytes {
        total = total.wrapping_add(byte)
    }
    return total
} checksum

pub fn filled(value: u8, count: usize, alloc: mem::Alloc) -> mem::List[u8]
    example filled(7, count: 3, alloc: testing::world().alloc).len() == 3
{
    var list = mem::List[u8].empty(alloc)
    for _ in 0..<count {
        mut list.push(value)
    }
    return list
} filled
```

```nan
pub fn per_item(total: u32, items: u32) -> u32
    example per_item(10, items: 0) == 0 // error: example.trapped
{
    return total / items
} per_item
```

## Refutation confirmation

**Refutation confirmation** decides whether the candidate the [prover](20-prover.md#refutation) builds for an obligation is a counterexample. A **confirmation run** executes the NIR function holding the obligation's site, instantiated with the candidate's values of the const parameters, with each parameter bound to the candidate's value for it.

- An obligation in a function of a role other than `body`, `requires`, `old` and `decreases` ([NIR functions](19-nir.md#functions)), or with a generic type parameter in scope, has no confirmation run, and its candidate is unconfirmed.
- Each `Slice[T]` or `SliceMut[T]` within a candidate value views all of a fresh extent holding its elements, writable for `SliceMut[T]`. A parameter whose NIR type is a reference receives a reference to a fresh extent holding its value, writable for `ref mut`.
- Before the function, the run evaluates its **entry facts** in order: the `where` conditions of the const parameters; the predicates that are facts at the item's entry ([predicate obligations](04-types.md#obligations)); and the requires clauses of the item or interface method that are not disputed, all of them for a `body`, `decreases` or `old` function and those written before it for a `requires` function.
- The candidate is **confirmed** exactly when every entry fact is true and the run then reaches the obligation's site with the obligation's condition false; the run stops there instead of trapping. Every other end leaves the candidate unconfirmed, and no confirmation run reports anything.

```nan
pub fn per_slot(total: u32, slots: u32) -> u32 limits notrap {
    if slots == 0 {
        return 0
    }
    return total / slots
} per_slot
```

```nan
pub fn per_slot(total: u32, slots: u32) -> u32 limits notrap {
    return total / slots // error: proof.refuted
} per_slot
```

In the next example the prover cannot show that `slots | 1` is nonzero, and no candidate is confirmed.

```nan
fn odd_slots(slots: u32) -> u32 = slots | 1

pub fn per_slot(total: u32, slots: u32) -> u32 limits notrap {
    return total / odd_slots(slots) // error: proof.unknown
} per_slot
```

## Ptr checks

In every interpreter run, the interpreter checks each access through a `hw::Ptr`, each `offset` and each `assume` against the [Ptr rules](23-low-level.md#ptr-rules), made exact by this section.

### Interpreter memory

**Interpreter memory** is the set of **extents** of an interpreter run. An extent has an address, a length, a writability, a liveness and, for each byte, a value or the uninitialised state. The extents are exactly:

- each execution of a `slot` instruction, writable and uninitialised, live until the same instruction executes again in its frame or the frame ends;
- for each argument of `hw::Ptr.of` that is not a place within an extent, an extent holding its value, read-only and live until the calling frame ends;
- the data of each text or byte-string literal and the value of each const and contribution with the data it refers to, read-only, created when the run first reaches it and live for the rest of the run;
- each [allocation](12-memory.md#budgets) drawn through an allocator, writable and uninitialised, live until it is freed;
- each extent a confirmation run creates for a candidate value.

The interpreter gives extents addresses in creation order: the first at `0x1_0000_0000`, each later one at the lowest multiple of 65,536 at least 65,536 bytes past the end of the previous one. For the charges of [budgets](12-memory.md#budgets), an allocation of at most 64 KiB lies in the chunk last drawn through its allocator's cell, after that chunk's earlier allocations at the next multiple of its alignment, or in a newly drawn chunk when it does not fit.

A type with a **fixed layout** (scalars, `hw::Ptr`, [layouts](23-low-level.md#layouts) and arrays of these) has the size, alignment and bytes that [type layout](30-artefacts.md#type-layout) gives it. Every other type has the **interpreter layout**:

- `Unit` and `Never` take 0 bytes with alignment 1. `Slice`, `SliceMut`, `Str`, `Dyn` and function types take 16 bytes and reference types 8, each with alignment 8.
- A struct places its fields in declaration order, each at the lowest offset past the previous field that is a multiple of its alignment. An enum places a `u32` tag, the index of its variant in declaration order, at offset 0, and the payload fields of every variant from the end of the tag, as a struct places its fields. `Array[T, N]` has the alignment of T and places element i at i × the size of T, in N × that size.
- A struct's or enum's alignment is the largest of 1 and its parts' alignments, and its size is the largest end of its parts, rounded up to that alignment.

An extent holds the values written into it; the data of a literal, const or contribution and a candidate's extent hold their values from creation. A held struct, enum or array value also holds its fields, the payload fields of its variant and its elements. A held value whose type has no fixed layout is an **opaque unit**. A write replaces the values it covers and makes its bytes initialised. An access is **layout-correct** exactly when:

- its type has a fixed layout and, within each opaque unit it overlaps, it lies inside one held value of fixed layout; or
- its type has no fixed layout and it covers exactly a held value of its type; or
- it is a write of a type without fixed layout and every opaque unit it overlaps lies inside the written bytes.

A pointer that `hw::Ptr.of`, `of_mut`, `of_view` or `of_view_mut` makes has **provenance**: the extent holding the place or the view's elements, whether it was made through a read access (`of`, `of_view`), and its chain of `mut` accesses. The chain of a pointer from `of_mut` or `of_view_mut` is that call's access then, while the argument is a place of a `mut` parameter, the access of the call that received it, and so on outward. `offset` and `cast` keep provenance, and so does storing a pointer and reading it back as a `hw::Ptr`. A pointer from `of_addr` or from bytes written as another type has none.

In an interpreter run, `is_aligned()` uses the alignment this section gives T, `copy(to, n)` is n reads of consecutive elements followed by n writes, each `atomic_` method of `hw::Ptr` is a read, a write or a read followed by a write, and `hw::fence` does nothing. An interpreter run has one task and never breaks the races rule.

### Checked rules

The interpreter run ends with a violation of the first of these rules, in table order, that an operation breaks:

| Rule | Broken when |
| --- | --- |
| `provenance` | an access's extent is no longer live |
| `bounds` | an `offset` of a pointer with provenance yields an address outside its extent other than the one past its end, or an access covers a byte outside its extent |
| `alignment` | an access's address is not a multiple of its type's alignment |
| `read-only` | a write reaches a read-only extent, or reaches, through a pointer made through a read access, a byte outside every value of a type with [interior mutability](12-memory.md#interior-mutability) |
| `initialisation` | a read covers an uninitialised byte |
| `validity` | the value read is not a value of its type: a bit pattern its type excludes, or a [predicate](04-types.md#predicates) that fails |
| `layout` | an access is not layout-correct |
| `exclusivity` | an access reaches a byte of a place, or of a mutable view's elements, passed by `mut` to a call still running, through a pointer whose chain lacks that call's access |
| `assume` | the condition of an `assume` is false |

```nan
// file: ring/package.nan
//! Fixed-capacity rings.

package ring {
    language 1
    targets any
    allow trusted in ring
} ring

// file: ring/ring.nan
//! Index arithmetic and byte access for rings.

pub fn wrap_index(index: usize, capacity: usize) -> usize
    example wrap_index(9, capacity: 8) == 1
{
    trusted "every ring capacity is a nonzero const" {
        assume capacity > 0 because "callers pass a ring's const capacity, which its predicate keeps nonzero"
    }
    return index % capacity
} wrap_index

pub fn first_byte(source: Slice[u8]) -> u8
    example first_byte(b"A") == 65
{
    let start = hw::Ptr.of_view(source)
    return trusted "every ring hands over a nonempty view" { start.read() }
} first_byte
```

```nan
// file: ring/package.nan
//! Fixed-capacity rings.

package ring {
    language 1
    targets any
    allow trusted in ring
} ring

// file: ring/ring.nan
//! Index arithmetic for rings.

pub fn wrap_index(index: usize, capacity: usize) -> usize
    example wrap_index(9, capacity: 0) == 0 // error: const.trusted-violation
{
    trusted "every ring capacity is a nonzero const" {
        assume capacity > 0 because "callers pass a ring's const capacity, which its predicate keeps nonzero"
    }
    return index % capacity
} wrap_index
```

```nan
// file: ring/package.nan
//! Fixed-capacity rings.

package ring {
    language 1
    targets any
    allow trusted in ring
} ring

// file: ring/ring.nan
//! Byte access for rings.

pub fn first_byte(source: Slice[u8]) -> u8
    example first_byte(b"") == 0 // error: const.trusted-violation
{
    let start = hw::Ptr.of_view(source)
    return trusted "every ring hands over a nonempty view" { start.read() }
} first_byte
```

## Diagnostics

| Code | Class | Rule |
| --- | --- | --- |
| `const.assert-false` | error | [Const evaluation](#const-evaluation), [Constants](02-items.md#constants) |
| `const.steps-exhausted` | error | [Run ends](#run-ends) |
| `const.trapped` | error | [Run ends](#run-ends), [Constants](02-items.md#constants) |
| `const.trusted-violation` | error | [Run ends](#run-ends) |
| `const.uninterpretable` | error | [Run ends](#run-ends) |
| `const.view-not-static` | error | [Const evaluation](#const-evaluation) |
