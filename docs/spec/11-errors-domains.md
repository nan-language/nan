# Errors, traps and domains

This chapter defines failures and the `fails` clause, `fail`, `try` and `attempt`; error enums and their from variants; failure conversion and from resolution; unhandled failures; the typing of cancellation with its origin and absorption; traps, their kinds, the abandoning of a domain and trap reports; the four domains with the domain runs, settlement and domain memory; and poisoning.

## Failures

A **failure** is a value a function passes to its caller instead of a result. The `fails` clause of a head names the function's **failure type**; a head without one declares a function that cannot fail.

```ebnf
fails_clause = "fails" type_expr ;
fail_expr = "fail" operator_expr ;
```

- A failure type is an enum type that is neither a [view](05-properties.md#views) nor [linear](05-properties.md#linear-types), a generic type parameter in scope that is not bounded by `Any`, or `task::Cancelled`. Any other type is an error (`error.failure-type`).
- The same rules apply to the `fails` clause of a [closure](07-ownership.md#closures), an interface method head and a [function type](10-effects-digests.md#function-types). Whether a head needs its clause is decided in [heads](02-items.md#heads).
- In the rules of this chapter, F is the failure type of the innermost function, closure or test body holding the expression. In a test body every failure type converts to F unchanged ([test items](22-testing.md#test-items)).
- A call **may fail** when the head or function type of its callee has a `fails` clause whose type, once the call fixes its generic parameters, is not `Never` ([failure polymorphism](10-effects-digests.md#failure-polymorphism)); otherwise the call cannot fail.

### Fail

`fail e` evaluates `e`, converts its value to F by [failure conversion](#failure-conversion), and ends the innermost function, closure or test body with the converted value as its failure; the blocks it leaves exit as at a `return` ([registration and exits](17-suspension-io.md#registration-and-exits)). The type of `fail e` is `Never`.

A `fail e` whose `e` has a type C, or a `try` whose call has a failure type C, where C does not convert to F, is an error (`error.unconvertible`). In a [const context](21-interpreter.md#const-evaluation), which has no failure type, every `fail` and `try` is that error.

### Try

`try` marks a call that may fail, whose failure type is C ([markers](06-statements-expressions.md#markers)). When the call returns, `try` yields its result. When the call fails with c, `try` acts exactly as `fail c`. `try` and `attempt` each mark only the call they precede.

### Attempt

`attempt` marks a call that may fail, whose failure type is C and whose result type is R, `Unit` when the head omits it. It yields `Ok(r)` when the call returns r and `Err(c)` when it fails with c, a value of type `Result[R, C]`. It never fails and converts nothing.

- `try` or `attempt` on a call that cannot fail, and `try` before `await when`, are errors (`error.marker-unneeded`); the exact fix deletes a `try`. `attempt await when` is the one use of `attempt` outside calls ([await when](16-concurrency.md#await-when)).

```nan
pub closed enum ParseError derives Eq, Render {
    Empty
    BadDigit(u8)
} ParseError

pub closed enum ConfigError derives Eq, Render {
    Parse(ParseError) from
    Zero
} ConfigError

pub fn digit(text: Slice[u8]) -> u8 fails ParseError
    example attempt digit(b"7") == Ok(7)
    example attempt digit(b"") == Err(ParseError.Empty)
{
    if text.len() == 0 {
        fail ParseError.Empty
    }
    let byte = text[0]
    if byte < b'0' or byte > b'9' {
        fail ParseError.BadDigit(byte)
    }
    return byte - b'0'
} digit

pub fn retries(text: Slice[u8]) -> u8 fails ConfigError
    example attempt retries(b"x") == Err(ConfigError.Parse(ParseError.BadDigit(b'x')))
{
    let count = try digit(text)
    if count == 0 {
        fail ConfigError.Zero
    }
    return count
} retries

pub fn retries_or_default(text: Slice[u8]) -> u8
    example retries_or_default(b"") == 3
{
    let count: u8 = match attempt retries(text) {
        Ok(value) => value
        Err(ConfigError.Parse(ParseError.Empty)) => 3
        else => 1
    }
    return count
} retries_or_default
```

## Error enums

An **error enum** is an enum named as a failure type; any enum may be one. A **from variant** is a variant declared with `'from'` ([enums](04-types.md#enums)); it lets failure conversion wrap a failure of its payload type.

- A from variant has exactly one unlabelled payload, `Name(T) from`, and T is a failure type. Any other variant marked `'from'` is an error (`error.from-shape`).
- A from variant is **generic** when its payload type mentions a generic parameter of its enum, and **concrete** otherwise.
- Two from variants of one enum whose declared payload types are equal are an error (`error.from-duplicate`), reported at the second in source order.
- The **cancellation variant** of an enum is its from variant whose declared payload type is `task::Cancelled`.
- An enum type is **ambiguous** when its type arguments make the payload types of two generic from variants equal while no concrete from variant has that payload type.
- A type expression denoting an ambiguous type, with the generic parameters in scope abstract, is an error (`error.from-ambiguous`). So is a type expression or call whose generic arguments, given or [inferred](08-generics-interfaces.md#inference), make a type that the instantiated item's head or fields mention ambiguous; the error is reported at that type expression or call.

```nan
pub closed enum ReadError {
    Empty
} ReadError

pub closed enum LoadError[E] {
    Read(ReadError) from
    Source(E) from
} LoadError
```

```nan
pub closed enum PairError[A, B] {
    First(A) from
    Second(B) from
} PairError

pub closed enum ReadError {
    Empty
} ReadError

pub closed struct ReadLog {
    pub last: Option[PairError[ReadError, ReadError]] // error: error.from-ambiguous
} ReadLog
```

## Failure conversion

**Failure conversion** turns a value of type C into a value of the failure type F, at `fail` and `try`. A **cancellable type** is `task::Cancelled`, an enum with a cancellation variant, or a generic parameter carrying the implied cancellable bound of [cancellation](#cancellation).

C converts to F in exactly these cases, checked in order:

1. C equals F. The value is unchanged.
2. F is an enum and from resolution chooses a from variant V for C. The value c becomes `F.V(c)`.
3. F is a cancellable generic parameter and C is `task::Cancelled`. The value k becomes the cancellation of F holding k.

A conversion inside a generic item is decided once, at its definition, with the item's generic parameters abstract; instantiation never changes the case or the variant. Conversion takes one step: a C that converts only through another type does not convert.

At run time a [cancellation](#cancellation) keeps its place at the top of each failure type it reaches: when the value converted is a cancellation holding k and F is cancellable, the result is the cancellation of F, as instantiated, holding k, whichever case applied. Otherwise the result is the one the case gives.

### From resolution

From resolution chooses the from variant of an enum F that converts a failure of type C. The candidates are F's from variants whose payload type, with F's type arguments as known at the conversion substituted, equals C.

1. When exactly one candidate is concrete, it is chosen.
2. Otherwise, when exactly one candidate exists, it is chosen.
3. Otherwise no variant is chosen.

```nan
pub closed enum StepError[E] {
    Step(E) from
    Cancelled(task::Cancelled) from
} StepError

pub closed struct Totals {
    pub bytes: u64
} Totals

pub fn run_step[E](step: fn(input: u32) -> u32 fails E uses wait, input: u32) -> u32 fails StepError[E] {
    return try await step(input)
} run_step

pub fn record(totals: task::Cown[Totals], count: u64) fails task::Cancelled uses wait {
    await when mut totals {
        totals.bytes = totals.bytes.saturating_add(count)
    }
} record

pub fn record_step(totals: task::Cown[Totals], count: u64) fails StepError[task::Cancelled] uses wait {
    try await record(totals, count)
} record_step
```

In `run_step`, `try` chooses `Step`, and a cancellation of `step` arrives as `StepError.Cancelled`. In `record_step`, both variants have the payload type `task::Cancelled`, and the concrete `Cancelled` is chosen.

### Conversion fixes

`error.unconvertible` has an exact fix when F is an enum declared in the current package and C mentions no generic parameter: it adds the variant `Name(C) from` to F, where Name is the last segment of C's path, without generic arguments, with a final `Error` removed (`Net` for `sys::net::NetError`). No fix is offered when that leaves no name or names an existing variant of F.

```nan
pub closed enum ParseError derives Eq, Render {
    Empty
} ParseError

pub closed enum ConfigError derives Eq, Render {
    Zero
} ConfigError

pub fn lead_byte(text: Slice[u8]) -> u8 fails ParseError {
    if text.len() == 0 {
        fail ParseError.Empty
    }
    return text[0]
} lead_byte

pub fn first_setting(text: Slice[u8]) -> u8 fails ConfigError {
    let value = try lead_byte(text) // error: error.unconvertible
    if value == 0 {
        fail ConfigError.Zero
    }
    return value
} first_setting
```

## Unhandled failures

A call that may fail is marked `try` or `attempt`. A call that may fail without either is an error (`error.unhandled`). When the call's failure type converts to F, the exact fix inserts `try`.

```nan
pub closed enum ParseError derives Eq, Render {
    Empty
} ParseError

pub fn lead_byte(text: Slice[u8]) -> u8 fails ParseError {
    if text.len() == 0 {
        fail ParseError.Empty
    }
    return text[0]
} lead_byte

pub fn first_byte(text: Slice[u8]) -> u8 fails ParseError {
    return lead_byte(text) // error: error.unhandled
} first_byte
```

## Cancellation

**Cancellation** is the failure `task::Cancelled`, with which a cancellation point fails once its call is cancelled ([acknowledged cancellation](17-suspension-io.md#acknowledged-cancellation)).

```nan-api
// module: task

pub open struct Cancelled derives Send {
} Cancelled

impl Cancelled: Render
```

- Only the runtime creates values of `task::Cancelled`; std declares no function returning one. Code passes on a value it received, with `try` or `fail`.
- Rendering a `task::Cancelled` writes `cancelled`.
- `task::Cancelled` is a [parsing type](15-untrusted.md#validator-classes).
- A failure value is a cancellation when it is a `task::Cancelled` value k or a value of its enum's cancellation variant holding k; the cancellation holds k. The cancellation of a cancellable type F holding k is k when F is `task::Cancelled`, `F.V(k)` when F is an enum with cancellation variant V, and the cancellation of its argument holding k when F is a generic parameter.
- The failure type of a function head, closure or function type that states `uses wait`, and of a function head with a parameter whose function type states `uses wait`, is cancellable. A failure type that is not, or a missing `fails` clause beside `uses wait` where `item.fails-missing` does not apply, is an error (`error.cancel-unmapped`). The exact fix adds `fails task::Cancelled` to a head without a clause, and adds `Cancelled(task::Cancelled) from` to an enum of the current package that has no variant named `Cancelled`.

### Failure bounds

- A generic type parameter named as a failure type in its item, in a `fails` clause or as the payload of a from variant, carries the **implied failure bound**: its argument is a failure type or `Never`.
- A generic type parameter that is a failure type required to be cancellable in its item, by [cancellation](#cancellation), carries the **implied cancellable bound**: its argument is a cancellable type. Inside the item, `task::Cancelled` converts to it by case 3 of [failure conversion](#failure-conversion).
- A parameter of a struct or enum also carries the bounds that its uses in the heads of the functions named after its type in the type's module imply.
- An argument outside an implied bound fails that bound (`generic.unsatisfied-bound`).
- Where [failure polymorphism](10-effects-digests.md#failure-polymorphism) would fix a parameter carrying the implied cancellable bound as `Never`, it is fixed as `task::Cancelled`.

```nan
pub closed enum StepError[E] {
    Step(E) from
} StepError

pub closed struct Journal {
    pub last: Option[StepError[u32]] // error: generic.unsatisfied-bound
} Journal
```

### Origin and absorption

A **cancelling construct** is a task scope ([task scopes](16-concurrency.md#task-scopes)), a `task::within`, `task::within_ns` or `task::race` call ([deadlines and races](16-concurrency.md#deadlines-and-races)), or a runtime action that cancels tasks: a termination request ([host runs](27-program-model.md#host-runs)) or the cancellation of a [keyed task](26-components.md#keyed-tasks). Its **region**, stated where it is defined, is a set of tasks and bodies, and holds every task of the task scopes, races and domains opened within it.

- Each `task::Cancelled` value records its **origin**, which only the runtime reads: the outermost cancelling construct that has cancelled and whose region holds the call that fails with it, or none when no such construct exists, as for a cancellation through a stale executor ([revocation](14-authority.md#revocation)); a cancellation without an origin passes through every construct.
- A cancellation reaching its origin, as the failure of a task or body in its region, is **absorbed**: the origin does not pass it on and completes as its definition gives for cancellation.
- A cancellation reaching any other construct passes through it like any other failure.

```nan
pub closed struct Totals {
    pub bytes: u64
} Totals

pub closed enum StoreError derives Render {
    Full
    Cancelled(task::Cancelled) from
} StoreError

pub fn store(totals: task::Cown[Totals], count: u64) fails StoreError uses wait {
    if count > 65_536 {
        fail StoreError.Full
    }
    await when mut totals {
        totals.bytes = totals.bytes.saturating_add(count)
    }
} store

pub fn store_or_skip(totals: task::Cown[Totals], count: u64) fails task::Cancelled uses wait {
    let outcome = attempt await store(totals, count)
    let Err(StoreError.Cancelled(cancelled)) = outcome else {
        return
    }
    fail cancelled
} store_or_skip
```

```nan
pub closed struct Totals {
    pub bytes: u64
} Totals

pub closed enum StoreError derives Render {
    Full
} StoreError

pub fn store(totals: task::Cown[Totals], count: u64) fails StoreError uses wait { // error: error.cancel-unmapped
    if count > 65_536 {
        fail StoreError.Full
    }
    await when mut totals {
        totals.bytes = totals.bytes.saturating_add(count)
    }
} store
```

## Traps

A **trap** is the abandonment of the innermost domain holding the task or kernel context that executes an operation whose rule says it traps. No code of the abandoned domain observes the trap; only a [domain run](#domains) turns one into a value, in the caller's domain.

The **trap kinds** are exactly these, forming the `trap` family:

| Kind | Raised when |
| --- | --- |
| `trap.bounds` | an index, offset or id lies outside what it reaches, or an offset is not aligned for its type ([indexing](07-ownership.md#indexing)) |
| `trap.overflow` | an integer result is not a value of its type ([overflow](09-numbers.md#overflow)) |
| `trap.division-by-zero` | an integer division or remainder has the divisor 0 ([division](09-numbers.md#division)) |
| `trap.shift` | a shift amount is at least the width of the shifted type ([shifts](09-numbers.md#shifts), [Secret values](15-untrusted.md#secret-values)) |
| `trap.conversion` | a value is converted into, or stored in, a type or field that cannot represent it ([conversions](09-numbers.md#conversions), [layout values](23-low-level.md#layout-values)) |
| `trap.stale-handle` | a handle is used after what it names has ended ([generational handles](12-memory.md#generational-handles)) |
| `trap.exhausted` | an allocation or a fixed capacity runs out ([growth](12-memory.md#growth)) |
| `trap.predicate` | a predicate check fails ([predicate obligations](04-types.md#obligations)) |
| `trap.requires` | a requires check fails, a trusted primitive's included ([run-time checks](18-contracts.md#run-time-checks)) |
| `trap.ensures` | a return check fails ([run-time checks](18-contracts.md#run-time-checks)) |
| `trap.expect` | an `expect` or loop invariant check fails ([expect](18-contracts.md#expect), [loop invariants](18-contracts.md#loop-invariants)) |
| `trap.todo` | a `todo` expression is reached ([todo](18-contracts.md#todo)) |
| `trap.stack-overflow` | a task stack overflows ([stack overflow](17-suspension-io.md#stack-overflow)) |
| `trap.duplicate-access` | one exclusive access is claimed twice at once, as by an `await when` naming one cown twice ([cowns](16-concurrency.md#cowns)) |
| `trap.poisoned` | an `await when` acquires a poisoned cown ([poisoning](#poisoning)) |

A trap kind appears only in trap reports, in `sys::TrapKind` and in `// traps:` markers; it is never a diagnostic code. [Interpreter](21-interpreter.md#interpreter) runs execute outside every domain; the interpreter defines what a trap there produces.

### Abandoning a domain

The runtime abandons a domain D when a task of D traps, when a [test run](22-testing.md#test-runs) on D times out, and, on the host targets, when a task calls `with` on a spin lock or per-CPU value its CPU is already inside, D then being the innermost process or test domain holding the task ([locks](24-kernel.md#locks)), whose end writes the [halt report](24-kernel.md#kernel-halts) in place of a trap report. The steps are, in this order:

1. The trapping task, if any, stops at the trapping operation.
2. Every other task of D stops. A suspended task is never resumed. A running task stops at some point of its execution, once every operation of these kinds that it has begun has completed: a call of a std function of a type with [interior mutability](12-memory.md#interior-mutability) or of `task::Sender`, `task::Receiver`, `task::Scope`, `sys::Cx` or `sys::Outbox`, a change of the count of a `mem::Shared` or of the handles of a `task::Cown`, and a support routine. An `await when` body is not such an operation. What it executes before it stops happens before the steps that follow.
3. Every cown that a task of D holds by running an `await when` body naming it is poisoned. Every cown granted to a task of D that has not yet started the `await when` body naming it is released unpoisoned, and every request of a task of D for a cown not yet granted is withdrawn.
4. D ends as its kind gives in [domains](#domains).

The tasks of every domain nested in D count as tasks of D in each step, and the nested domains end with D without trap reports of their own. When several tasks of D trap, the first trap the runtime records is D's trap and the others are not reported.

Nothing unwinds. No deferred call, run-time check, release of a value ([end of scope](07-ownership.md#ownership)) or other code of a task of D runs once that task has stopped.

```nan
pub fn mean(total: u32, count: u32) -> u32 {
    return total / count // traps: trap.division-by-zero
} mean

test mean_of_nothing for mean {
    expect mean(0, count: 0) == 0
} mean_of_nothing
```

### Trap reports

A **trap report** describes one trap in at most 15 lines of text, each ending with a line feed. The domain's kind decides where it goes. Paths in it are package-relative and prefixed by the package name, as in `ring/ring.nan`. Its lines are, in order:

1. The trap kind, `at`, the path and line of the trapping operation joined by `:`, `in`, and the canonical path of its item, separated by single spaces.
2. The source line of the trapping operation without its indentation, indented 4 spaces.
3. The operand line, indented 4 spaces, when the trap has operands: each operand's source text, ` = ` and its value, separated by `, `. The operands are those of the trapping operation, its receiver and arguments when it is a call, for `trap.overflow`, `trap.division-by-zero`, `trap.shift` and `trap.conversion`; the index for `trap.bounds` followed by `len = ` and the length, and, for `trap.predicate`, `trap.requires`, `trap.ensures` and `trap.expect`, the two sides of the failing condition when it is a comparison. Other traps have no operand line.
4. One line per caller, innermost first, indented 4 spaces: `called from`, the path and line of the call, `in` and the caller's canonical path, separated by single spaces.
5. On the [seeded scheduler](22-testing.md#seeded-scheduler), `replay`, a space and the [replay token](22-testing.md#replay-tokens), indented 4 spaces.

- A number or `bool` is written as `Render` writes it, a `char` as a char literal, text as a text literal and `mem::Bytes` as a byte-string literal. A struct, enum, `Option`, `Result`, array or slice whose parts can be written is written as the construction, variant or `[a, b]` form that builds it. Any other value, such as a function value, a capability or an `Untrusted` value, is written `_`.
- Frames of std items and support routines are left out. The trapping operation is then the call in the innermost frame outside std; when every frame is std, none is left out. The frame of a closure is named by the canonical path of the item whose body holds it. Caller lines end at the first frame of the trapping task.
- For `trap.stack-overflow` the trapping operation is the call whose frame does not fit.
- When the caller lines would take the report past 15 lines, the report keeps as many as fit with one line left, and that last caller line is `called from`, the number of frames left out and `more frames`, separated by single spaces.
- Line tables and frame pointers make the report on every target ([images](27-program-model.md#images)).

```nan
// file: tally/package.nan
//! Splits a total among items.

package tally {
    language 1
    targets x86_64-host
    entry main
} tally

// file: tally/tally.nan
//! Logs one item's share of a total.

fn share(total: u32, items: u32) -> u32 {
    return total / items
} share

fn main(take world: host::World) fails task::Cancelled uses wait {
    try await world.clock.sleep(10)
    world.log.info("share", value: share(10, items: 0))
} main
```

Running this program writes this trap report to standard error, and the process exits with status 70:

```text
trap.division-by-zero at tally/tally.nan:4 in tally::share
    return total / items
    total = 10, items = 0
    called from tally/tally.nan:9 in tally::main
```

## Domains

A **domain** is a unit that a trap abandons; a kernel domain also ends at every other [kernel halt](24-kernel.md#kernel-halts). Domains nest: every task belongs to the domain in which it was spawned, and the body of a **domain run**, a call of `sys::Domain.run` or `sys::Domain.run_sized`, starts a new domain nested in the caller's. **Settlement** prepares an abandoned domain for freeing by cancelling its operations in flight and waiting for every acknowledgement ([settlement](#settlement)).

| Domain | Holds | Abandoning ends it by |
| --- | --- | --- |
| process | the tasks of a host target's program outside other domains | writing the trap report to standard error and exiting with status 70 ([host runs](27-program-model.md#host-runs)) |
| `sys::Domain` body | the body passed to a domain run and its tasks | settlement, then freeing; the domain run returns `Err` |
| test | one run of a test on a host target ([test runs](22-testing.md#test-runs)) | settlement, then freeing; the run fails with the trap report, or with `test.timeout` and no report when it timed out |
| kernel | every context of a machine target's image, or of the boot of a test image ([emulator runs](22-testing.md#emulator-runs)) | a [kernel halt](24-kernel.md#kernel-halts) |

The process and the kernel end without settlement. The domains are exactly these four kinds.

### Domain run

```nan-api
// module: sys

pub open struct Domain {
} Domain

pub fn Domain.run[T: Send, E: Send](take body: task fn() -> T fails E uses wait, exec: task::Executor, alloc: mem::Alloc) -> Result[T, Trap] fails E uses wait

pub fn Domain.run_sized[T: Send, E: Send](take body: task fn() -> T fails E uses wait, stack: usize, exec: task::Executor, alloc: mem::Alloc) -> Result[T, Trap] fails E uses wait

pub open struct Trap derives Send {
} Trap

impl Trap: Render

pub fn Trap.kind(self) -> TrapKind

pub closed enum TrapKind derives Copy, Eq, Render {
    Bounds
    Overflow
    DivisionByZero
    Shift
    Conversion
    StaleHandle
    Exhausted
    Predicate
    Requires
    Ensures
    Expect
    Todo
    StackOverflow
    DuplicateAccess
    Poisoned
} TrapKind
```

`sys::Domain.run` runs `body` on `exec` as the first task of a new domain D. The calling task waits until D ends: when its first task ends, or once D, abandoned, is settled and freed.

- A [task closure](16-concurrency.md#task-closures) passed as `body` captures by `take` and Copy only ([lending](16-concurrency.md#lending)).
- Before D starts, the calling task draws through `alloc`, which names D's [cell](#domain-memory), the first task's stack, which belongs to D, and the storage of D's trap report, which belongs to the caller's domain; when `alloc` cannot supply them, the call traps (`trap.exhausted`) in the caller's domain. Other tasks of D draw their stacks as [joins](16-concurrency.md#joins) states. When `run` returns `Ok` or fails, it frees that storage before it returns; on `Err` the storage belongs to the returned `sys::Trap` and is freed when that value is released.
- When `body` returns t, `run` returns `Ok(t)`. When it fails with e, `run` fails with e.
- When `exec` is stale, `run` starts no task and fails at once with the cancellation of E ([revocation](14-authority.md#revocation)).
- When D is abandoned, `run` returns `Err(trap)` after settlement and freeing. `trap.kind()` is the variant of `sys::TrapKind` whose name is the trap kind's condition in PascalCase (`DivisionByZero` for `trap.division-by-zero`), and rendering `trap` writes its trap report.
- The tasks of D lie in the region of every cancelling construct whose region holds the call of `run` ([origin and absorption](#origin-and-absorption)).
- `sys::Domain` has no values that code obtains; it names the operation.

`sys::Domain.run_sized(body, stack, exec, alloc)` behaves as `run(body, exec, alloc)`, except that the first task's stack holds at least `stack` bytes ([task stacks](17-suspension-io.md#task-stacks)), so that a body whose depth no [stack limit](10-effects-digests.md#stack-limits) bounds, such as a recursive one, can run in a domain.

A use of `sys::Domain.run` or `sys::Domain.run_sized` in code checked for a machine target, or within the reach of a final build for one ([environments](27-program-model.md#environments)), is an error (`domain.unavailable`).

```nan
pub closed enum JobError derives Render {
    TooLarge
    Cancelled(task::Cancelled) from
} JobError

pub fn per_item(total: u32, items: u32) -> u32 fails JobError {
    if total > 1_000_000 {
        fail JobError.TooLarge
    }
    return total / items
} per_item

pub fn contained_per_item(total: u32, items: u32, exec: task::Executor, alloc: mem::Alloc) -> Option[u32] fails JobError uses wait {
    let outcome = try await sys::Domain.run(fn() -> u32 fails JobError {
        return try per_item(total, items)
    }, exec, alloc)
    let Ok(share) = outcome else {
        return None
    }
    return Some(share)
} contained_per_item

test zero_items_is_contained for contained_per_item {
    let world = testing::world()
    let share = try await contained_per_item(10, items: 0, exec: world.exec, alloc: world.alloc)
    expect share == None
} zero_items_is_contained
```

```nan
// file: probe/package.nan
//! Probe steps for a bare machine.

package probe {
    language 1
    targets x86_64-bare
} probe

// file: probe/probe.nan
//! Rates a probe level inside a domain.

pub closed enum ProbeError derives Render {
    Idle
    Cancelled(task::Cancelled) from
} ProbeError

pub fn rate(level: u32) -> u32 fails ProbeError {
    if level == 0 {
        fail ProbeError.Idle
    }
    return 1_000 / level
} rate

pub fn contained_rate(level: u32, exec: task::Executor, alloc: mem::Alloc) -> Result[u32, sys::Trap] fails ProbeError uses wait {
    return try await sys::Domain.run(fn() -> u32 fails ProbeError limits stack 4_096 { // error: domain.unavailable
        return try rate(level)
    }, exec, alloc)
} contained_rate
```

### Settlement

The runtime settles an abandoned domain D in this order. No user code runs during settlement.

1. It cancels every operation in flight at a cancellation point of a task of D, ordinary reads and writes as well as completion operations ([completion I/O](17-suspension-io.md#completion-io)), and waits until the kernel or device acknowledges each one.
2. It releases without waiting every [resource](14-authority.md#resources) that a task of D owns, directly or through the values holding it, an affine one as its end of scope does and a linear one as its [abandon method](17-suspension-io.md#abandoning-resources) does, and revokes every `sys::Revoker` D owns that way ([revocation](14-authority.md#revocation)), and every entry belonging to a `sys::Minter` D owns that way, never releasing the minter's state ([capability kinds](14-authority.md#capability-kinds)), and closes every `mem::Budget` D owns that way, as `close` does ([budgets](12-memory.md#budgets)).
3. It counts every `task::Sender` and `task::Receiver` that a task of D owns that way as released by its [channel](16-concurrency.md#channels), releasing none of the values in the channel's buffer.
4. It frees D's memory as [domain memory](#domain-memory) states.

### Domain memory

A domain owns the stacks of its tasks, the first task's stack of a `sys::Domain` body included, and every [chunk](12-memory.md#budgets) drawn while one of its tasks allocates, through any allocator; the chunk is charged to that allocator's cell until passing to another domain charges it to that domain's cell ([chunk ownership](12-memory.md#chunk-ownership)). Each domain has a **cell**: the cell of the root grant's allocator for the process and the kernel, the cell `alloc` names for a `sys::Domain` body, and the cell of the simulated world's allocator for a test. A value **occupies** the chunks that hold its own memory and the chunks that every value it holds occupies, transitively: its fields, elements and payloads, the contents of its boxes, the storage and entries of a container or store, the contents of a `mem::Shared`, and the storage behind a handle it holds: a cown's value, a channel's buffer, an arena's and a once map's entries, an interner's table and a signal's slot. Interior mutability does not stop occupancy.

- When `body` returns or fails, the chunks D owns that its result or failure occupies pass to the caller's domain, and D's stacks and other chunks are freed.
- A std operation that moves a value from a task of D into storage another domain O owns passes the chunks D owns that the value occupies, before it completes, to the innermost domain enclosing both D and O. These operations are a channel send, a message sent through a `sys::Cx` or posted through a `sys::Outbox` ([components](26-components.md#components)), an insertion into a std type with interior mutability, `sys::Minter.of`, whose storage the outermost domain owns ([capability kinds](14-authority.md#capability-kinds)), and the end of an `await when` body, for each cown it names whose value lies in chunks that O owns.
- A std operation that a task of D runs and that draws memory for storage whose chunks another domain O owns, such as growing a container or a channel's buffer, passes each chunk it draws, before it completes, to the innermost domain enclosing both D and O.
- A chunk passing to a domain O is charged from then on to O's cell instead of the cell it was charged to, when the chains of both cells end at root cells with one supply and O's cell's chain has room for it. The passing subtracts its bytes from the charge of every open cell on the chain of the cell it was charged to; then, when every cell on the chain of O's cell is open and stays at most its limit with the bytes added, it adds them to the charge of each of those cells, and otherwise it adds them back where it subtracted them and the chunk stays charged to its cell. Only the charge stays: the chunk passes to O either way.
- When D is abandoned, its stacks and chunks are freed. Memory in other domains' chunks that tasks of D owned stays allocated.
- The value of a cown poisoned by D's abandonment is never released; its memory in chunks of other domains stays allocated.

```nan
pub fn squares(count: u32, exec: task::Executor, alloc: mem::Alloc) -> mem::List[u64] fails task::Cancelled uses wait {
    let outcome = try await sys::Domain.run(fn() -> mem::List[u64] {
        var built = mem::List[u64].empty(alloc)
        for index in 0..<count {
            let wide = u64.widen(index)
            mut built.push(wide * wide)
        }
        return built
    }, exec, alloc)
    let Ok(values) = outcome else {
        return mem::List[u64].empty(alloc)
    }
    return values
} squares

test squares_outlive_their_domain for squares {
    let world = testing::world()
    let values = try await squares(4, exec: world.exec, alloc: world.alloc)
    expect values.len() == 4
    expect values[3] == 9
} squares_outlive_their_domain
```

```nan
pub fn sender_charge(exec: task::Executor, alloc: mem::Alloc) -> usize fails task::Cancelled uses wait {
    let sender = alloc.split(1_048_576)
    defer take sender.close()
    let drawn = sender.alloc()
    let outcome = try await sys::Domain.run(fn() -> mem::List[u64] {
        var message = mem::List[u64].empty(drawn)
        mut message.push(7)
        return message
    }, exec, alloc)
    let Ok(message) = outcome else {
        return sender.limit()
    }
    expect message.len() == 1
    return sender.charged()
} sender_charge

test receiver_pays_after_handover for sender_charge {
    let world = testing::world()
    let charge = try await sender_charge(world.exec, alloc: world.alloc)
    expect charge == 0
} receiver_pays_after_handover
```

## Poisoning

**Poisoning** is the permanent state of a cown that a task held when its domain was abandoned ([abandoning a domain](#abandoning-a-domain)). A poisoned cown never becomes usable again; how `await when` treats it is defined in [poisoned cowns](16-concurrency.md#poisoned-cowns).

## Diagnostics

| Code | Class | Rule |
| --- | --- | --- |
| `domain.unavailable` | error | [Domain run](#domain-run) |
| `error.cancel-unmapped` | error | [Cancellation](#cancellation) |
| `error.failure-type` | error | [Failures](#failures) |
| `error.from-ambiguous` | error | [Error enums](#error-enums) |
| `error.from-duplicate` | error | [Error enums](#error-enums) |
| `error.from-shape` | error | [Error enums](#error-enums) |
| `error.marker-unneeded` | error | [Attempt](#attempt) |
| `error.unconvertible` | error | [Fail](#fail), [Joins](16-concurrency.md#joins) |
| `error.unhandled` | error | [Unhandled failures](#unhandled-failures) |
