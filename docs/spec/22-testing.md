# Testing and evidence

This chapter defines test items and runs, generation and `derives Generate`, property tests with shrinking and regression tests, simulated capabilities, `dbg`, the seeded scheduler and replay, affected tests, emulator runs and reliance tests.

## Test items

A **test** is a `test` item: a body that exercises one item, its **test target**, named after `for`.

```ebnf
test_item = ( "todo" TEXT )? "test" IDENT "for" test_target test_params? "{" fn_body ;
test_target = path ( "." IDENT )? ;
test_params = "(" param_list ")" ;
```

- The `IDENT` after `test` is the test's name and [closing label](02-items.md#item-shape).
- The target is a [path](03-packages-modules.md#paths) with an optional member, naming a function item or a method a [derive](08-generics-interfaces.md#derives) writes; naming any other item is an error (`test.target-kind`). A generic target stands for all its instantiations.
- A [disputed](18-contracts.md#todo) test never runs and counts for nothing.
- A `mut` parameter is an error (`test.parameter-form`), and so is one whose type does not implement `Generate`, as no [view](05-properties.md#views) type does (`test.parameter-type`).
- The body is checked as a function body without result that `uses wait`, whose failure type takes every failure unconverted from `try` and `fail` ([failures](11-errors-domains.md#failures)).
- No [image](27-program-model.md#images) holds a test item.

The **reach** of a test is the smallest item set holding the test and, for each of its items, every item it names, the impl lines and derived methods of each type it names and the [contributions](25-registries.md#contributions) to each registry it names, the test's `for` clause naming nothing. A test target outside its test's reach is an error (`test.target-unreachable`).

```nan
pub closed struct Gauge {
    pub level: u32
} Gauge

pub fn Gauge.raise(mut self, step: u32) {
    self.level = self.level.saturating_add(step)
} Gauge.raise

test raise_saturates for Gauge.raise {
    var gauge = Gauge(level: 4_294_967_290)
    mut gauge.raise(10)
    expect gauge.level == 4_294_967_295
} raise_saturates
```

```nan
pub fn double(level: u32) -> u32 {
    return level * 2
} double

pub fn triple(level: u32) -> u32 {
    return level * 3
} triple

test triple_of_two for double { // error: test.target-unreachable
    expect triple(2) == 6
} triple_of_two
```

### Test runs

A **run** is one execution of a test body on one [run target](#emulator-runs). On a host target the body is the first task of a new test [domain](11-errors-domains.md#domains), on a new [simulated world](#simulated-capabilities)'s executor, and the run ends once every task of the domain has ended. The target runs when a call of it begins, through any chain of calls, function values, registry fields or `Dyn`, whatever lowering inlines or folds. A run passes when the body returns and the target ran; otherwise it fails with the first of:

- a trap abandons the domain, reported by its [trap report](11-errors-domains.md#trap-reports);
- a failure escapes the body (`test.escaped-failure`), reported with its variant's path and, when its type implements `Render`, its rendering within a render budget of 4,096 bytes; a trap while rendering it fails the run with that trap's report;
- the target did not run (`test.target-not-run`);
- on a host target, no task of the domain is ready and no deadline is pending (`test.stalled`);
- the run uses more than 10 seconds of host processor time, its process's or its emulator's (`test.timeout`), its domain then being [abandoned](11-errors-domains.md#abandoning-a-domain) without a trap report.

These codes and `test.emulator-unavailable` are the **test-run codes**. `nan test` reports a failing test as [checking commands](33-toolchain.md#checking-commands) state, its cause being its test-run code or trap kind; a test-run code is never a diagnostic. A test passes when every run passes, and reports only its failing run of lowest index. Runs share no state, and their outcomes never depend on how many execute at once.

```nan
pub fn scale(level: u32) -> u32 {
    return level / 2
} scale

pub fn adjust(level: u32) -> u32 {
    if level < 10 {
        return level
    }
    return scale(level)
} adjust

test small_levels_unchanged for scale { // error: test.target-not-run
    expect adjust(3) == 3
} small_levels_unchanged
```

## Generation

`Generate` is the core interface building input values. A **generator** is the method `T.generate`, written by `derives Generate` or by std.

```nan-api
// module: core

pub interface Generate {
    fn generate(mut choices: Choices, alloc: mem::Alloc) -> Self limits terminates
} Generate

pub open struct Choices {
} Choices

pub fn Choices.draw(mut self, low: u64, high: u64) -> u64 limits notrap, terminates
    requires low <= high
    ensures low <= result and result <= high

pub fn Choices.draw_signed(mut self, low: i64, high: i64) -> i64 limits notrap, terminates
    requires low <= high
    ensures low <= result and result <= high
```

- Only the runtime creates `core::Choices` values, one per [case](#property-tests). A **draw** is one call of `draw` or `draw_signed`, whose value the runtime chooses within its bounds ([constraints](#constraints)); no generator rejects it.
- An impl line naming `Generate` outside std is an error (`test.generate-manual-impl`).
- `derives Generate` on a view type, on a type that is not [Inert](05-properties.md#inert) with its type parameters counted Inert, or on one whose generator the depth rule leaves unending, is an error (`test.generate-type`) and that entry's only code; on a [capability type](05-properties.md#capability-types) it is `prop.capability-derive`. Otherwise it is an impl line in the type's module, checked and applied as [derives](08-generics-interfaces.md#derives) states.
- A derived generator of a struct draws its fields in declaration order, then constructs it; of an enum, it draws by `draw` a position among the eligible variants in declaration order, then the payload values as fields. Generated containers store `alloc`.
- Depth rule: under 8 or more enclosing generator calls, collection lengths draw as 0 and an enum's eligible variants are those whose payload types lack the enum as a [constituent type](05-properties.md#structural-properties) at any depth; elsewhere every variant is eligible.

```nan
pub closed struct Span derives Generate {
    pub start: u32
    pub end: u32 where start <= end
} Span

pub fn Span.len(self) -> u32 = self.end - self.start

test len_fits_end for Span.len (span: Span) {
    expect span.len() <= span.end
} len_fits_end
```

```nan
pub closed struct Glossary derives Generate { // error: test.generate-type
    pub words: text::Interner
} Glossary
```

### Std generators

- Integer scalars draw between their minimum and maximum, signed ones by `draw_signed`. `bool` draws 0 (`false`) or 1. `char` draws from 0 to 1_112_063, adding 0x800 to values from 0xD800. `Unit` draws nothing. `f32` and `f64` draw their bit pattern and convert it by `from_bits` ([conversions](09-numbers.md#conversions)).
- `Array[T, N]` with `T` Copy draws its elements in index order.
- `mem::List[T]`, `mem::Bytes`, `mem::Map[K, V]`, `mem::HashMap[K, V]`, `mem::Set[T]` and `text::String` draw a length from 0 to 64, and `mem::Bounded[T, N]` and `text::Fixed[N]` from 0 to N, then that many elements, entries or chars. `text::Fixed` chars lie between U+0020 and U+007E. Entries are inserted in order, a repeated key replacing the earlier entry; a hash map draws its seed first.
- `mem::Box[T]` boxes a drawn `T` with `mem::Heap.of(alloc)`; `Untrusted[T]` and `mem::Id[T]` generate as their chapters state.

### Constraints

The **constraints** of a generation are the conjuncts (operands of top-level `and`) of the predicates of each struct value it builds and of the target `requires` a [property test](#property-tests) adds, visible transparent functions unfolded. A **drawn integer** is a generated integer scalar, named through parameters and fields, or a length term: `len()` of a `mem::List`, `mem::Bytes` or `mem::Bounded`, or `as_str().char_count()` of a `text::String`. A conjunct naming no generated value is ignored; one is constructive when it reads `a op b`, `a op b + c`, `a op b - c`, `a op c` or `a != c`, with `op` among `<`, `<=`, `==`, `>=` and `>`, `a` and `b` drawn integers, and `c` joining integer literals, consts and const generic parameters by `+` and `-`.

- Constructive conjuncts other than `!=` are difference bounds over mathematical integers, each `b + c` or `b - c` adding the bounds keeping it within its type. Before each draw the runtime narrows its bounds, by difference-bound propagation ([procedure](20-prover.md#procedure)), to the values extending those already drawn to a solution of them all.
- `a != c` is constructive only when no other conjunct names `a` with another drawn integer. With k excluded values in its bounds [l, h], the draw of `a` takes the bounds [l, h − k], then adds 1 per excluded value, in ascending order, at or below the result so far.
- Any other conjunct naming a generated value is a **generator gap**: incompleteness (`test.generator-gap`), reported on the first line of the type whose predicate holds it, or on the head of the property test whose target `requires` does. Constructive conjuncts no values satisfy are a gap reported on the property test's head. A property test with a gap never runs and counts for nothing.

```nan
pub closed struct Block derives Generate { // incomplete: test.generator-gap
    pub size: u32 where size % 512 == 0
} Block
```

## Property tests

A **property test** is a test with parameters. Each of its runs is a **case**, which first generates each parameter's value in order through one `core::Choices` and the world's allocator.

- A property test runs cases 0 to 255. In case 0 every draw returns its low bound and in case 1 its high bound; in later cases, when the next value of the draw stream is even, a boundary value chosen uniformly, and otherwise a value chosen uniformly within its bounds ([seeded scheduler](#seeded-scheduler)).
- A draw's boundary values are its bounds, their neighbours within them, and 0 when within them, ascending and without repetition.
- When an instantiation of the target makes its parameter types, `self` included, equal the test's in order, each conjunct of the target's [requires](18-contracts.md#requires) clauses that are not [disputed](18-contracts.md#disputed-clauses) joins the constraints, each target parameter standing for the test parameter at its position.

```nan
pub fn take_prefix(len: u32, count: u32) -> u32
    requires count <= len
{
    return len - count
} take_prefix

test prefix_leaves_rest for take_prefix (len: u32, count: u32) {
    expect take_prefix(len, count) <= len
} prefix_leaves_rest
```

```nan
pub fn half(len: u32) -> u32
    requires len % 2 == 0
{
    return len / 2
} half

test half_is_smaller for half (len: u32) { // incomplete: test.generator-gap
    expect half(len) <= len
} half_is_smaller
```

### Shrinking

**Shrinking** moves the recorded draws of the reported failing case toward their **shrink points**, the values within their bounds nearest 0.

1. A candidate reruns the case with changed draws under the same schedule and random streams; a recorded value outside its draw's bounds moves to the nearer bound, and a draw past the recorded ones returns its shrink point. A candidate is kept, its draws becoming the recorded ones, when it fails the same way: same trap kind on the same line, escaped failure variant or outcome code.
2. For each recorded draw in order whose value v is not its shrink point p, the runtime tries p; unless that is kept, it sets l to p and h to v and, while they differ by more than 1, tries m = l + (h − l) / 2 truncated toward 0, setting h to m when kept and l to m otherwise; the draw keeps h.
3. Step 2 repeats until a pass changes no draw or 4,096 candidates have run.

The report gives each parameter's shrunk value in setup form with the regression-test edit.

### Regression tests

The regression-test edit is an [exact fix](34-diagnostics.md#exact-fixes) inserting after the failing test a parameterless test of the same target, named `<name>_regression_<k>` with the smallest positive `k` unused in the module, whose body is the setup form of each shrunk parameter, then the failing test's statements.

The **setup form** of a value for a name n of type T is `let n: T = e`, or `var n: T = e` for a `take` parameter or when statements follow, where e is:

- a literal for integers, `bool` and `char`, `T.from_bits(...)` with a hexadecimal literal for floats, and `Unit`;
- a [construction](04-types.md#construction), a variant, `Untrusted.of(...)` or `mem::Id[T].of(...)`;
- `text::String.of("...", alloc: testing::world().alloc)`, `text::Fixed[N].of("...")` or `mem::Heap.of(testing::world().alloc).boxed(...)`;
- for an array, `Array[T, N].filled(d)`, d being its first most frequent element, then `n[i] = e` per other element;
- for another collection, its `empty` call with `testing::world().alloc` and any seed, then a `push` or `insert` per element in order, `discard` preceding a call with a result.

Text and char literals escape `\` and their own quote with `\`, and chars outside U+0020 to U+007E as `\u{H}`, H being uppercase hexadecimal without leading zeros. A nested part needing statements is first bound by its setup form to `n_1`, `n_2` and so on, in writing order, and passed by `take` unless Copy. A part whose construction the test's module cannot write, such as another package's open type, is `todo "T"`, T being its type's path.

With `var sum: u8 = 0` and `sum += level` in `total`, case 1 of `total_is_bounded` traps (`trap.overflow`) and shrinks to a length of 2 with elements 1 and 255, and the edit inserts `total_is_bounded_regression_1`, which this block shows once `total` sums into a `u32`:

```nan
pub fn total(levels: mem::List[u8]) -> u32 {
    var sum: u32 = 0
    for level in levels {
        sum += u32.widen(level)
    }
    return sum
} total

test total_is_bounded for total (levels: mem::List[u8]) {
    expect total(levels) <= 16_320
} total_is_bounded

test total_is_bounded_regression_1 for total {
    var levels: mem::List[u8] = mem::List[u8].empty(testing::world().alloc)
    mut levels.push(1)
    mut levels.push(255)
    expect total(levels) <= 16_320
} total_is_bounded_regression_1
```

## Simulated capabilities

A **sim** is a capability of a std capability type acting on state that a test or [example](21-interpreter.md#example-runs) run simulates, meeting the heads and contracts of its type's operations. An awaited sim operation leaves its task ready once the sim's state decides its result.

```nan-api
// module: testing

pub fn world() -> host::World

pub fn same[T: Any](actual: T, expected: T) -> bool limits pure, notrap, terminates
```

On every target, `testing::world()` returns its run's one `host::World`: `fs` is a `sys::sim::fs` without size limit storing through `alloc`, `net` a `sys::sim::net()`, `clock` the virtual clock and `random` the random stream of the [seeded scheduler](#seeded-scheduler), which is `exec`; `log` and `stdio` write to the [test trace](#debug-output), with empty input; `alloc` names a root cell as a root grant's does ([allocators](12-memory.md#allocators)); `args` and `environment` are empty. In [example runs](21-interpreter.md#example-runs), the world is as example runs state.

`testing::same(actual, expected)` is true exactly when the values are structurally equal: scalars and layout values by bits, structs by fields, enum values by variant and payload, Untrusted values, boxes, shared values and text by contents, arrays, views and collections by elements in order, function values by function, and other std open types, capabilities included, by being the same table entry, cell or memory. Stored allocators are ignored.

```nan
pub fn save_greeting(fs: sys::Fs) fails sys::fs::FsError uses wait {
    try await fs.write_all("greeting", data: b"hello")
} save_greeting

test greeting_is_saved for save_greeting {
    let world = testing::world()
    try await save_greeting(world.fs)
    let saved = try await world.fs.read_all("greeting", alloc: world.alloc)
    let expected = Untrusted.of(b"hello")
    expect testing::same(saved.view(), expected)
} greeting_is_saved
```

### Sim constructors

```nan-api
// module: sys::sim

pub fn fs(limit: usize, alloc: mem::Alloc) -> sys::Fs

pub fn net() -> sys::Net

pub fn random(seed: u64) -> sys::Random

pub fn stdio(input: Slice[u8], alloc: mem::Alloc) -> sys::Stdio
```

- `fs` returns an empty in-memory file system storing through `alloc`, which traps on exhaustion (`trap.exhausted`). A write taking its files' total size past `limit` bytes fails with `sys::fs::FsError.NoSpace`, writing nothing.
- `net` returns a new loopback network: `connect` to `localhost`, `127.0.0.1` or `::1` reaches its listener on that port or fails with `sys::net::NetError.Refused`, and to another host fails with `sys::net::NetError.Unreachable`.
- `random` yields stream 2 of the seed `seed`.
- `stdio` returns a `sys::Stdio` whose input yields a copy of `input`, then ends, and whose outputs write to the test trace.

```nan-api
// module: hw::sim

pub open struct Registers[R] derives Copy, Send {
} Registers

pub fn Registers.of(alloc: mem::Alloc) -> Registers[R]

pub fn Registers.mmio(self) -> hw::Mmio[R]

pub fn Registers.ports(self) -> hw::Ports[R]

pub fn Registers.set[T: AnyBits](self, register: hw::Register[R, T], value: T)

pub fn Registers.queue[T: AnyBits](self, register: hw::Register[R, T], value: T)

pub fn Registers.last_written[T: AnyBits](self, register: hw::Register[R, T]) -> Option[T]

pub fn Registers.set_at[E: AnyBits, N: usize](self, register: hw::Register[R, Array[E, N]], index: usize, value: E)

pub fn Registers.queue_at[E: AnyBits, N: usize](self, register: hw::Register[R, Array[E, N]], index: usize, value: E)

pub fn Registers.last_written_at[E: AnyBits, N: usize](self, register: hw::Register[R, Array[E, N]], index: usize) -> Option[E]
```

- R is a [registers block](23-low-level.md#registers). `Registers.of` stores in `alloc` a simulated block whose registers and array elements each hold 0, with an empty queue and no last written value. `mmio` or `ports` returns the one window reaching it; a second call of either traps (`trap.duplicate-access`).
- A volatile read removes and returns the register's oldest queued value, else its held value. A write sets the held and last written values, except that a `write_one_clear` write clears the held bits written as 1.
- `set` replaces the held value of the register or element, `queue` appends a value to its queue and `last_written` returns its last written value, or `None`, whatever its access kinds. Their register argument is written as for [volatile calls](23-low-level.md#register-windows) (`register.operand`), and `set`, `queue` and `last_written` of an array register, and `set_at`, `queue_at` and `last_written_at` of any other register, are errors (`register.array-access`). An index not below the array's length traps (`trap.bounds`). `queue` and `queue_at` draw through the allocator `Registers.of` received and trap when it is exhausted (`trap.exhausted`); the others allocate nothing. Each terminates and has no device fact.

### Simulation boundary

- Items of `testing`, `sys::sim` and `hw::sim` are named only in test items, example clauses and the items of those modules; anywhere else a path to one is an error (`test.test-only-item`).
- No item, intrinsic or capability operation reveals whether code runs in a test. Code under test is checked and compiled as in a build, keeping every [run-time check](18-contracts.md#run-time-checks), and differs only through the capabilities it receives and the scheduler's choices.

```nan
pub fn fresh_log() -> sys::Log {
    return testing::world().log // error: test.test-only-item
} fresh_log
```

## Debug output

```ebnf
dbg_expr = "dbg" "(" expr ")" ;
```

`dbg(e)` passes `e` as an argument of a `take` parameter, [marked](07-ownership.md#marking) as one, and yields it. In a test run it first writes to the test trace one line: the `dbg`'s path and line, the source text of `e`, ` = ` and its value as a [trap report](11-errors-domains.md#trap-reports) writes operands; elsewhere it writes nothing.

- `dbg` adds no fact to the [digest](10-effects-digests.md#digests).
- Each `dbg` is incompleteness (`test.dbg`) and a [ledger](31-trust.md#ledger) entry.
- The **test trace** of a run records in order its `dbg` lines and its sims' log records and output writes; `nan test` shows it for failing runs ([commands](33-toolchain.md#commands)).

```nan
pub fn average(total: u32, count: u32) -> u32 {
    return dbg(total) / count // incomplete: test.dbg
} average
```

## Replay

### Seeded scheduler

The **seeded scheduler** is the executor of every run ([executors](16-concurrency.md#executors)). It runs one task at a time on one processor.

- Its scheduling points are [suspension points](17-suspension-io.md#stackful-tasks), atomic operations, spawns and task ends. At each, it chooses uniformly among the ready tasks, in the order they became ready.
- A **pending deadline** is the end of a `sys::Clock.sleep`, of a `task::within` deadline or of a `sys::Cx.after` delay that has not yet been reached.
- The **virtual clock** starts at 0 and advances only when no task is ready, to the earliest pending deadline.
- A run's **seed** is the first 8 bytes, little-endian, of the BLAKE3-256 hash of the 33 bytes of the test's [item hash](29-identity-versioning.md#hashes), the run target's UTF-8 name and the run's index as 8 bytes little-endian.
- Value n of stream s is the first 8 bytes, little-endian, of the BLAKE3-256 hash of the seed, s and n as 8 bytes little-endian each; stream 0 serves draws, 1 scheduling and 2 random bytes, least significant first. A uniform choice among k ordered options picks position w mod k, w being its stream's next value.
- A test without parameters runs schedules 0 to 63, each a run of that index, stopping after a run in which no scheduling point found two ready tasks. Case i of a property test is the run of index i.

```nan
pub closed struct Counter {
    pub hits: u64
} Counter

pub fn count_twice(counter: task::Cown[Counter], exec: task::Executor) fails task::Cancelled uses wait {
    try await task::scope(exec, body: fn(s: task::Scope) {
        for _ in 0..<2u32 {
            discard s.spawn(fn() fails task::Cancelled uses wait {
                await when mut counter {
                    counter.hits += 1
                }
            })
        }
    })
} count_twice

test both_tasks_count for count_twice {
    let world = testing::world()
    let counter = task::Cown.of(Counter(hits: 0), alloc: world.alloc)
    try await count_twice(counter, exec: world.exec)
    let hits = await when counter => counter.hits
    expect hits == 2
} both_tasks_count
```

### Replay tokens

A **replay token** names one run as `class/target/test/index/seed`: its coverage class, its run target, the test's [canonical path](29-identity-versioning.md#canonical-paths), the run's index in decimal, and the [checksummed base32](29-identity-versioning.md#hashes) of its seed's 8 bytes.

- The report of a failing run ends with its replay token.
- `nan test --replay <token>` reruns that run, whose index and seed fix its draws, schedule, random bytes and clock: while the test's reach is unchanged it fails the same way, and a failing case is shrunk again.
- A token that does not parse, names no test or run target of the workspace, or names a class other than `sc` is an error (`test.replay-token`).

### Coverage classes

A coverage class names what exploration and replay reproduce. Language 1 has exactly one, `sc`: task interleavings at scheduling points under the sequentially consistent [memory model](23-low-level.md#memory-model) of safe code, without the orders atomics in trusted code may observe beyond it or the timing of devices and interrupts. Every other class name is reserved.

## Affected tests

A test is affected on a run target when it has no recorded pass there under the current [toolchain hash](36-builds.md#toolchain-hash), or when its reach or an item hash in it differs from that record. The [build cache](36-builds.md#cache) records, per test and run target, the reach with its item hashes once all runs pass.

- `nan test --affected` runs exactly the affected tests within its scope.
- A candidate release affects each consumer test whose reach holds an item whose item hash differs from the locked release's; `nan dep sync` runs those tests ([Identity and versioning](29-identity-versioning.md)).

## Emulator runs

A package's tests run on each declared [target](27-program-model.md#targets), `any` standing for `x86_64-host`, which is the **run target** of their runs. On a host target each run is a host process.

- On the machine targets, each run boots, under the emulator of the target's [descriptor](27-program-model.md#descriptors), a [test image](30-artefacts.md#lowering) of the test's reach and the support routines. It starts the seeded scheduler on one processor, runs the body as its first task, writes the outcome, test trace and any trap report to the descriptor's serial port, then writes the byte 0 to its [exit port](27-program-model.md#descriptors), which ends the run. The image holds the registry tables that the tested package admits ([contributors](25-registries.md#contributors)).
- The run's domain is that boot's kernel domain: a trap halts it, and the report on serial is the run's failure. When no task is ready and no deadline is pending, the processor waits for an interrupt.
- When the emulator cannot be started, or for a host target when the machine running the toolchain has another architecture, every run on that target fails (`test.emulator-unavailable`).

```nan
// file: checksum/package.nan
//! Boot-time checksums.

package checksum {
    language 1
    targets x86_64-bare
} checksum

// file: checksum/checksum.nan
//! Additive checksum of a boot record.

pub fn fold(record: Array[u8, 4]) -> u8 {
    var total: u8 = 0
    for byte in record {
        total = total.wrapping_add(byte)
    }
    return total
} fold

test fold_wraps for fold {
    expect fold(Array[u8, 4].filled(128)) == 0
} fold_wraps
```

## Reliance tests

A **reliance test** is a test whose target is an item of a dependency, named through its `dep` [alias](03-packages-modules.md#aliases).

- It is checked and run like any test, against the release the [lock](29-identity-versioning.md#lock) names.
- It belongs to the consumer's [code root](30-artefacts.md#code-roots), and `nan dep sync` accepts no candidate release of the dependency under which it fails.

```nan
// file: gauge/package.nan
//! Saturating gauge levels.

package gauge {
    language 1
    targets any
} gauge

// file: gauge/gauge.nan
//! Level arithmetic.

pub fn bump(level: u8) -> u8 = level.saturating_add(1)

// file: panel/package.nan
//! Shows gauge levels.

package panel {
    language 1
    targets any
    dep gauge "../gauge"
} panel

// file: panel/panel.nan
//! Panel level rules.

pub fn next_level(level: u8) -> u8 = gauge::bump(level)

test bump_stops_at_top for gauge::bump {
    expect gauge::bump(255) == 255
} bump_stops_at_top
```

## Diagnostics

| Code | Class | Rule |
| --- | --- | --- |
| `test.dbg` | incompleteness | [Debug output](#debug-output) |
| `test.emulator-unavailable` | error | [Emulator runs](#emulator-runs) |
| `test.escaped-failure` | error | [Test runs](#test-runs) |
| `test.generate-manual-impl` | error | [Generation](#generation) |
| `test.generate-type` | error | [Generation](#generation) |
| `test.generator-gap` | incompleteness | [Constraints](#constraints) |
| `test.parameter-form` | error | [Test items](#test-items) |
| `test.parameter-type` | error | [Test items](#test-items) |
| `test.replay-token` | error | [Replay tokens](#replay-tokens) |
| `test.stalled` | error | [Test runs](#test-runs) |
| `test.target-kind` | error | [Test items](#test-items) |
| `test.target-not-run` | error | [Test runs](#test-runs) |
| `test.target-unreachable` | error | [Test items](#test-items) |
| `test.test-only-item` | error | [Simulation boundary](#simulation-boundary) |
| `test.timeout` | error | [Test runs](#test-runs) |
