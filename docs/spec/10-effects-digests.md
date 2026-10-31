# Effects, digests and limits

This chapter defines waiting, with the `uses wait` clause and the `await` marker; digests and their facts, with reversibility classes and undo clauses; how facts propagate through direct calls, function arguments, indirect calls, foreign fns and trusted code; limits, their promises and the stack check of final builds; function types and the fit rule; higher-order calls and failure polymorphism; and purity.

## Waiting

**Waiting** is the suspension of a task at a call until the operation the call performs can proceed. It is the only effect a head declares: `uses wait` in a function head, an interface method head, a closure head or a [function type](#function-types) states that calling the function may wait.

```ebnf
uses_clause = "uses" ( 'wait' | IDENT {!effect.uses-unknown} ) ;
```

- `uses` followed by any word other than `wait` is an error (`effect.uses-unknown`); the exact fix deletes the clause.
- [Heads](02-items.md#heads) fix which function heads state `uses wait`, and [closures](07-ownership.md#closures) which closure heads do.

### Awaited calls

A call **may wait** exactly when one of these holds:

- its callee is a function, an interface method or a function value whose head or type states `uses wait`;
- one of its arguments for a parameter of a plain `fn` type is a function, closure or function value whose head or type states `uses wait`.

An argument for a `task fn` parameter runs in a new task ([task closures](16-concurrency.md#task-closures)), so it never makes the call wait.

- A call that may wait carries the `await` marker, in the marker order of [calls](06-statements-expressions.md#calls): `try await mut conn.read(mut buf.view_mut())`. A call that may wait without it is an error (`effect.await-missing`), whose exact fix inserts `await`.
- `await` before a call that cannot wait is an error (`effect.await-unneeded`), whose exact fix deletes it.
- `await` marks only the call it precedes; a call that may wait inside an argument carries its own.
- [Cowns](16-concurrency.md#cowns) define `await when`; `effect.await-missing` and `effect.await-unneeded` judge only calls.

Evaluating `await f(x)` performs the call `f(x)`. The call is a [suspension point](17-suspension-io.md#stackful-tasks): while the callee waits, its task is suspended and other tasks run. The call then yields its result, or fails, exactly as an unawaited call would.

```nan
pub closed enum ReadError derives Render {
    Net(sys::net::NetError) from
    Cancelled(task::Cancelled) from
} ReadError

pub fn read_frame(mut conn: sys::net::Conn, alloc: mem::Alloc) -> Untrusted[mem::Bytes] fails ReadError uses wait {
    var buf = Untrusted.of(mem::Bytes.zeroed(1_500, alloc))
    let count = try await mut conn.read(mut buf.view_mut())
    return buf.copy_prefix(count, alloc)
} read_frame
```

```nan
pub closed enum ReadError derives Render {
    Net(sys::net::NetError) from
    Cancelled(task::Cancelled) from
} ReadError

pub fn read_frame(mut conn: sys::net::Conn, alloc: mem::Alloc) -> Untrusted[mem::Bytes] fails ReadError uses wait {
    var buf = Untrusted.of(mem::Bytes.zeroed(1_500, alloc))
    let count = try mut conn.read(mut buf.view_mut()) // error: effect.await-missing
    return buf.copy_prefix(count, alloc)
} read_frame
```

```nan
pub closed enum ReadError derives Render {
    Net(sys::net::NetError) from
    Cancelled(task::Cancelled) from
} ReadError

pub fn frame_length(count: usize) -> usize {
    return count.saturating_add(4)
} frame_length

pub fn read_length(mut conn: sys::net::Conn, alloc: mem::Alloc) -> usize fails ReadError uses wait {
    var buf = Untrusted.of(mem::Bytes.zeroed(1_500, alloc))
    let count = try await mut conn.read(mut buf.view_mut())
    return await frame_length(count) // error: effect.await-unneeded
} read_length
```

```nan
pub fn reset(mut level: u32) uses alloc { // error: effect.uses-unknown
    level = 0
} reset
```

## Digests

The **digest** of a function, closure, const initializer or const assert is the coarse record of facts that the checker computes from its code and from the digests of what it reaches. A **digest fact** is one field of a digest; the fields are exactly the rows of this table, in this order. Source never states a digest: [limits](#limits) demand facts, and tools print them. A digest holds no sites and no methods; `nan explain` ([commands](33-toolchain.md#commands)) and [publish summaries](29-identity-versioning.md#publish-summaries) list them.

| Field | Kind | Holds when the code, or what it reaches |
| --- | --- | --- |
| wait | boolean | contains an `await when`, or a call that may wait other than through its own parameters ([heads](02-items.md#heads)); for a function or closure, exactly when its head states `uses wait` |
| alloc | boolean | obtains memory through a `mem::Alloc` ([allocators](12-memory.md#allocators)) |
| device | boolean | performs an operation that an `allow device` [manifest grant](14-authority.md#manifest-grants) covers |
| priv | boolean | performs an operation that an `allow priv` grant covers |
| sync | boolean | performs an `await when`, an atomic operation or fence, a memory access through `hw::Ptr` or `hw::UserAddr` ([pointers](23-low-level.md#pointers)), or an operation of a type with [interior mutability](12-memory.md#interior-mutability) |
| may-trap | boolean | contains a trap site |
| terminates | boolean | ends every execution, as [Propagation](#propagation) computes it |
| float use | boolean | operates on a float value or converts from or to a float type ([floats](09-numbers.md#floats)) |
| capabilities | set | calls functions of these [primitive capabilities](05-properties.md#capability-types), `mem::Alloc` excepted |
| validators | set | calls these [validators](15-untrusted.md#validators) and [decoders](13-text-encode.md#decoders-as-validators) |
| reversibility | class | changes state outside the program at most as this [class](#reversibility) states |

- The **effect facts** are wait, alloc, device, priv and sync.
- A **trap site** is an [obligation](18-contracts.md#obligations) that keeps a [run-time check](18-contracts.md#run-time-checks), or an operation that its owning chapter defines as trapping without an obligation, such as exhausting a [growable container](12-memory.md#growth). An obligation discharged by `assume` keeps no check and is no trap site. A `todo` expression is [incompleteness](18-contracts.md#todo) and no trap site. Stack overflow is governed by [stack limits](#stack-limits) and is no trap site.
- A function `C.f` of a primitive capability `C` has `C` in its capabilities set unless its head demands `pure`. A validator or decoder has itself in its validators set.
- A function `C.f` of a [capability kind](14-authority.md#capability-kinds) `C` whose head does not demand `pure` hides how the module declaring `C` acts: its digest is the one its code gives without the device, priv and sync facts, with `C` alone in its capabilities set and an empty validators set. Its package's [authority summary](14-authority.md#authority-summaries) reads both.
- An operation that the toolchain supplies (a built-in operator, an intrinsic, a [supplied operation](28-stdlib.md#supplied-operations), a compiler stub or a support routine) has the facts its owning chapter states for it, and no others, the dispatch of a [target-dependent operation](28-stdlib.md#platform-packages) included.
- The **worst digest** has alloc, device, priv, sync and may-trap true, wait, terminates and float use false, the unbounded set, which holds every element, as capabilities and as validators, and the greatest class, `irreversible(emit, actuate, erase, spend)`.

The checker computes one digest per target that the package declares. A digest is visible wherever its item may be named, as part of the [closure set](00-charter.md#guarantees) of the naming line. A consumer whose acceptance depends, through a limit, a fit or purity, on a dependency's digest holding terminates, lacking another fact or a set element, or having a class at most a bound, records that reliance in its [requires-set](29-identity-versioning.md#requires-sets). Digests are recorded beside the [interface hash](29-identity-versioning.md#hashes), never in it.

`nan show <path> --api` prints a digest as one line, `digest: effects E | authority A | may-trap M | T`:

- E is the true facts among `wait`, `alloc`, `sync` and `float`, in that order, then the class when it is not `traceless`, or `none`.
- A is `device` and `priv` when true, then the capabilities' paths, then the validators' paths, each set in byte order, or `none` when all are absent. The two sets are unbounded together, as in the worst digest, and are then written once, as `any`.
- M is `yes` when may-trap holds and `no` otherwise; T is `terminates` or `may-not-terminate`.
- Entries within E and within A are separated by one space.

```nan
pub fn percent_of(ratio: f64) -> u8 {
    return u8.fit(ratio * 100.0)
} percent_of
```

```text
$ nan show example::percent_of --api
ok show example::percent_of --api: records 1
api example::percent_of example/example.nan:2
    pub fn percent_of(ratio: f64) -> u8
    digest: effects float | authority none | may-trap yes | terminates
ok show example::percent_of --api: records 1
```

### Reversibility

The **class** of a digest bounds what its code does to state outside the program, such as files, devices and peers. The classes are, from least to greatest:

| Class | The code changes outside state at most by |
| --- | --- |
| `traceless` | nothing |
| `recorded` | appending to a record that only grows, such as a log or a standard stream |
| `restorable` | changes that later code can put back |
| `irreversible(K)` | effects that no later code undoes, of the kinds K |

K is a non-empty set of **kinds**, written in this order and separated by `, `: `emit` (data or a request leaves the machine), `actuate` (a physical effect), `erase` (outside information is lost) and `spend` (a resource is consumed for good). `irreversible(K)` is at most `irreversible(L)` exactly when K is a subset of L. The join of classes is the greatest of them, irreversible classes joining into `irreversible` of the union of their kinds.

- An operation has the class its owning chapter states, and `traceless` when it states none; [Reversibility classes](28-stdlib.md#reversibility-classes) states those of the std operations.
- A **device access**, a [volatile call](23-low-level.md#registers) or a [DMA access call](23-low-level.md#dma), has the greatest class, except under a **device premise**: in a function or closure whose own limits clause holds a class, each device access among its own operations has that class. A device premise is a [trust escape](31-trust.md#trust-escapes), which the ledger records as a `class` entry.

```nan
pub fn report(log: sys::Log, count: u32) limits recorded {
    log.info("count", value: count)
} report
```

```nan
pub fn clear_draft(fs: sys::Fs) fails sys::fs::FsError uses wait limits restorable {
    try await fs.remove("draft") // error: limit.reversibility-exceeded
} clear_draft
```

```nan
// file: kernel/package.nan
//! Doorbell of a bare kernel's device.

package kernel {
    language 1
    targets x86_64-bare
    allow device in kernel
} kernel

// file: kernel/kernel.nan
//! Rings the doorbell of a block device's queue.

pub registers Doorbell {
    ring: u32 at 0x0 write
} Doorbell

pub fn notify(mut bell: hw::Mmio[Doorbell]) limits restorable {
    mut bell.write(Doorbell.ring, value: 1)
} notify
```

The ledger of this package holds a `class` entry on `notify` with its class, `restorable`.

### Compensation

An **undo clause** in the clause region of a function item claims that another function puts back what the function changes:

```ebnf
undo_clause = 'undo' path ( "." IDENT )? TEXT NL ;
```

- The path names the **undoer** as a [test target](22-testing.md#test-items) names its function: a function item of the claiming function's package, other than that function, visible wherever it is visible. A path naming anything else is an error (`effect.undo-target`).
- The undoer's class is at most `restorable`; otherwise the clause is an error (`effect.undo-irreversible`).
- The TEXT is the clause's premise: why the undoer puts back what the function changes. The claim is a [trust escape](31-trust.md#trust-escapes), which the ledger records as an `undo` entry.
- The digest of a function with an undo clause is **compensated**: its class C, as [Propagation](#propagation) computes it, becomes `irreversible(emit)` when C holds `emit`, and the lesser of C and `restorable` otherwise. Each site of its text is judged against limits with its class compensated so.

```nan
pub fn enable(fs: sys::Fs) fails sys::fs::FsError uses wait limits restorable
    undo disable "the flag holds 0 before enable runs, and disable writes 0 back"
{
    try await fs.write_all("flag", data: b"1")
} enable

pub fn disable(fs: sys::Fs) fails sys::fs::FsError uses wait limits restorable
    undo enable "the flag holds 1 before disable runs, and enable writes 1 back"
{
    try await fs.write_all("flag", data: b"0")
} disable
```

Each function's digest is compensated from `irreversible(erase)` to `restorable`:

```text
$ nan show example::enable --api
ok show example::enable --api: records 1
api example::enable example/example.nan:2
    pub fn enable(fs: sys::Fs) fails sys::fs::FsError uses wait limits restorable
        undo disable "the flag holds 0 before enable runs, and disable writes 0 back"
    digest: effects wait restorable | authority sys::Fs | may-trap no | may-not-terminate
ok show example::enable --api: records 1
```

```nan
pub fn enable(fs: sys::Fs) fails sys::fs::FsError uses wait limits restorable
    undo forget "forget removes the flag that enable wrote" // error: effect.undo-irreversible
{
    try await fs.write_all("flag", data: b"1")
} enable

pub fn forget(fs: sys::Fs) fails sys::fs::FsError uses wait {
    try await fs.remove("flag")
} forget
```

## Propagation

Facts propagate through direct calls. The **own operations** of a function or closure are the operations of its body outside the bodies of the [closures](07-ownership.md#closures) it holds. Its digest is the join of the facts of its own operations with:

- the digest of each function it calls directly;
- the [declared digest](#indirect-calls) of each indirect call it makes and of each call it makes to a method of a generic parameter's [bound](08-generics-interfaces.md#generic-parameters);
- what each [function argument](#function-arguments) it supplies charges.

A join holds a boolean fact other than terminates when any part holds it, holds terminates when every part holds it, and holds the union of the parts' sets and the join of their classes. A generic item's digest is computed once, at its definition, and every instantiation has it. A consumer uses a dependency's digest as checking the dependency in the consumer's closure derives it ([releases](30-artefacts.md#releases)); the digest an artefact records is the one computed in its publisher's closure.

The **dependencies** of a function or closure are the functions it calls directly and the functions and closures it supplies as function arguments. A **cycle** is a maximal set of functions and closures each of which reaches every member, itself included, through dependencies. Every member of a cycle has the same facts other than wait and terminates: the join of the parts that this section joins into any member, members excepted.

A **finite cursor** is a cursor ([iteration](07-ownership.md#iteration)) whose type a std package declares; every std cursor yields finitely many elements. A function or closure has terminates exactly when these hold for it and, when it belongs to a cycle, for every member of its cycle:

- its wait fact is false;
- its own operations include no `while` or `loop` statement, and the cursor of each `for` among them is a finite cursor;
- every part joined into its digest, other than a member of its cycle, has terminates;
- when it belongs to a cycle, it is a function with a `decreases` clause, whose obligations [Decreases](18-contracts.md#decreases) defines, and each of its dependencies on a member of the cycle is a direct call.

An execution of code that has terminates returns, fails or traps. Terminates does not exclude a recursion through indirect calls or bound methods, which ends in [stack overflow](17-suspension-io.md#stack-overflow) when it does not return.

```nan
pub fn fill(mut buf: SliceMut[u8], value: u8) limits terminates, noalloc {
    for byte in mut buf {
        byte = value
    }
} fill

pub fn clear(mut buf: SliceMut[u8]) limits terminates, noalloc {
    fill(mut buf, value: 0)
} clear
```

```nan
pub fn record(mut log: mem::List[u32], value: u32) limits noalloc {
    mut log.push(value) // error: limit.allocates
} record
```

### Function arguments

A **function argument** is an argument for a parameter whose declared type is a function type.

- In the body of a function or closure, a call through one of its own function-typed parameters adds nothing to its digest, and neither does passing such a parameter as a function argument. A function value that a closure captures is not one of the closure's own parameters.
- Every other function argument is charged to the call that supplies it: that call joins the digest of the closure supplied, of the named function supplied as reached through its [checking entry](18-contracts.md#checking-entries), or the declared digest of the type of the function value supplied, without its wait fact for a `task fn` parameter.

```nan
pub fn count_where(values: Slice[u32], keep: fn(x: u32) -> bool) -> usize limits noalloc {
    var count: usize = 0
    for value in values {
        if keep(value) {
            count += 1
        }
    }
    return count
} count_where

pub fn count_above(values: Slice[u32], floor: u32) -> usize limits noalloc {
    return count_where(values, keep: fn(x: u32) => x > floor)
} count_above
```

```nan
pub fn count_where(values: Slice[u32], keep: fn(x: u32) -> bool) -> usize limits noalloc {
    var count: usize = 0
    for value in values {
        if keep(value) {
            count += 1
        }
    }
    return count
} count_where

pub fn collect_above(values: Slice[u32], floor: u32, mut seen: mem::List[u32]) -> usize limits noalloc {
    return count_where(values, keep: fn[mut seen](x: u32) -> bool {
        mut seen.push(x) // error: limit.allocates
        return x > floor
    })
} collect_above
```

## Indirect calls

An **indirect call** is a call through a function value, a [registry](25-registries.md#registries) field or a [call through `Dyn`](18-contracts.md#clauses), other than a call through the calling function's or closure's own function-typed parameter.

The **declared digest** of a function type, registry field type or interface method head is the worst digest, except that wait holds exactly when it states `uses wait` and the [limits](#limits) it demands, or that [interrupt functions](24-kernel.md#interrupt-functions) imply for an `interrupt fn` type, set facts: `pure` makes alloc, device, priv and sync false, both sets empty and the class `traceless`, `notrap` makes may-trap false, `terminates` makes terminates true, `noalloc` makes alloc false and a class makes the class that class. An indirect call, and a call to a method of a generic parameter's bound, has the declared digest of the type or head it calls through.

```nan
pub closed struct Filter {
    pub keep: fn(byte: u8) -> bool limits noalloc
} Filter

pub fn count_kept(filter: Filter, data: Slice[u8]) -> usize limits noalloc {
    let keep = filter.keep
    var count: usize = 0
    for byte in data {
        if keep(byte) {
            count += 1
        }
    }
    return count
} count_kept
```

```nan
pub closed struct Filter {
    pub keep: fn(byte: u8) -> bool
} Filter

pub fn count_kept(filter: Filter, data: Slice[u8]) -> usize limits noalloc {
    let keep = filter.keep
    var count: usize = 0
    for byte in data {
        if keep(byte) { // error: limit.allocates
            count += 1
        }
    }
    return count
} count_kept
```

## Foreign functions and trusted code

- A [foreign fn](23-low-level.md#foreign-functions) never waits. A foreign fn head, or a function type with a convention clause, that states `uses wait` is an error (`effect.foreign-wait`), whose exact fix deletes the clause.
- A foreign fn's digest is the worst digest, except that the limits it demands set facts as in a [declared digest](#indirect-calls). Its limits belong to the foreign fn's [trust escape](31-trust.md#trust-escapes), so its seal covers them.
- [Trusted code](23-low-level.md#trusted-code) contributes the facts of the operations inside it like any other code, so the device, priv and alloc facts of the primitives it calls reach the enclosing function's digest. Its trust is no digest fact: the [ledger](31-trust.md#ledger) records it.

```nan
// file: procinfo/package.nan
//! Process facts on the host.

package procinfo {
    language 1
    targets x86_64-host
    allow trusted in procinfo
} procinfo

// file: procinfo/procinfo.nan
//! Reads the process id from the C library.

foreign fn getpid() -> i32 limits notrap, terminates, noalloc convention "sysv"

pub fn process_id() -> i32 limits noalloc {
    return getpid()
} process_id
```

```nan
// file: firmware/package.nan
//! Firmware delays for a UEFI loader.

package firmware {
    language 1
    targets x86_64-uefi
} firmware

// file: firmware/firmware.nan
//! Busy-wait delays through the firmware's stall service.

pub closed struct StallService {
    pub stall: fn(microseconds: usize) -> usize limits noalloc convention "win64"
} StallService

pub fn settle(service: StallService) -> usize limits noalloc {
    let stall = service.stall
    return stall(1_000)
} settle
```

```nan
// file: procinfo/package.nan
//! Process facts on the host.

package procinfo {
    language 1
    targets x86_64-host
    allow trusted in procinfo
} procinfo

// file: procinfo/procinfo.nan
//! Reads the process id from the C library.

foreign fn getpid() -> i32 convention "sysv"

pub fn process_id() -> i32 limits noalloc {
    return getpid() // error: limit.allocates
} process_id
```

```nan
// file: procinfo/package.nan
//! Process facts on the host.

package procinfo {
    language 1
    targets x86_64-host
    allow trusted in procinfo
} procinfo

// file: procinfo/procinfo.nan
//! Reads the process id from the C library.

pub foreign fn getpid() -> i32 uses wait convention "sysv" // error: effect.foreign-wait
```

```nan
// file: board/package.nan
//! Address-space switching for one board.

package board {
    language 1
    targets x86_64-bare
    allow priv in board
    allow trusted in board
} board

// file: board/board.nan
//! The CR3 switch.

pub fn activate(take root: hw::PageTableRoot, cpu: hw::Ring0) -> Option[hw::PageTableRoot] limits pure {
    return trusted "root maps the running kernel image and the current stack" { cpu.write_cr3(take root) } // error: limit.impure
} activate
```

## Limits

A **limit** is a demand in the `limits` clause of a function head, a closure head, an interface method head or a function type: a digest fact, a bound on the class, or a bound on stack use.

```ebnf
limits_clause = "limits" limit ( "," limit )* ;
limit = 'pure' | 'notrap' | 'terminates' | 'noalloc' | 'stack' INT | reversibility ;
reversibility = 'traceless' | 'recorded' | 'restorable' | 'irreversible' "(" irreversible_kind ( "," irreversible_kind )* ")" ;
irreversible_kind = 'emit' | 'actuate' | 'erase' | 'spend' ;
```

| Limit | Demands |
| --- | --- |
| `pure` | the code is [pure](#purity) |
| `notrap` | may-trap is false |
| `terminates` | terminates is true |
| `noalloc` | alloc is false |
| a class C | the class is at most C ([Reversibility](#reversibility)) |
| `stack N` | the stack use is at most N bytes, checked at the final build ([Stack limits](#stack-limits)) |

- In a limits clause, a `,` continues the clause exactly when the token after it is `pure`, `notrap`, `terminates`, `noalloc`, `stack`, `traceless`, `recorded`, `restorable` or `irreversible`; any other `,` belongs to the enclosing list. A parameter named like one of these words therefore never directly follows a function type that has a limits clause.
- A limit appears at most once in a clause, a clause holds at most one class, and a class holds each kind at most once (`limit.duplicate`); `noalloc` or a class beside `pure` is an error (`limit.redundant`). Each exact fix deletes the extra limit or kind.
- N is an `INT` without a literal suffix whose value is from 1 to 4,294,967,295; any other is an error (`limit.stack-size`).
- `nan fmt` writes limits in the order `pure`, `notrap`, `terminates`, `noalloc`, the class, `stack`, and the kinds of a class in their order.
- A limit of a function or closure covers its digest as [Propagation](#propagation) computes it, so calls through its own function-typed parameters are charged to its callers.
- A head that states `uses wait` and demands `pure` is an error (`limit.impure`), and one that states `uses wait` and demands `terminates` is an error (`limit.may-not-terminate`), each reported at the limit.
- A function or closure whose digest lacks a fact its limits demand is an error at each site of its text, closure bodies included, that brings a fact or set element breaking a limit, the wait fact excepted: `limit.impure` for `pure`, `limit.may-trap` for `notrap`, `limit.may-not-terminate` for `terminates`, `limit.allocates` for `noalloc` and `limit.reversibility-exceeded` for a class. Its because-chain holds a `called` reason per call from the reported site to the operation carrying the fact and a `raised` reason for that operation, whose subject is the operation's class for a class, or a `declared` reason for the declared digest carrying it. A site that several enclosing functions and closures report is reported once, for the innermost.
- Under `notrap`, an obligation in the text that the prover does not prove is reported with the prover's [outcome](20-prover.md#outcomes), `proof.unknown` or `proof.refuted`, instead of `limit.may-trap`.
- The limits of an interface method head are met by its implementing functions ([conformance](08-generics-interfaces.md#conformance)), and those of a function type by each function that fits it ([Fit](#fit)).
- Apart from the size of a task's stack ([task stacks](17-suspension-io.md#task-stacks)), a limits clause changes no behaviour.

```nan
pub fn level_of(value: u32) -> u8 {
    return u8.fit(value)
} level_of

pub fn level_or_max(value: u32) -> u8 limits notrap {
    if value > 255 {
        return 255
    }
    return level_of(value) // error: limit.may-trap
} level_or_max
```

```nan
pub fn find_zero(data: Slice[u8]) -> usize limits terminates {
    var index: usize = 0
    while index < data.len() { // error: limit.may-not-terminate
        if data[index] == 0 {
            return index
        }
        index += 1
    }
    return index
} find_zero
```

```nan
pub fn announce(log: sys::Log, count: u32) -> u32 limits pure {
    log.info("count", value: count) // error: limit.impure
    return count
} announce
```

```nan
pub fn checksum(data: Slice[u8]) -> u64 limits pure, noalloc { // error: limit.redundant
    var total: u64 = 0
    for byte in data {
        total = total.wrapping_add(u64.widen(byte))
    }
    return total
} checksum
```

### Limits as promises

On a `pub` item, each limit other than `stack N` is a [promise](05-properties.md#promises): every later release keeps the demanded fact, and a release whose item loses the limit is breaking ([compatibility](29-identity-versioning.md#compatibility)). `stack N` binds each final build that lowers the item and promises nothing to consumers.

### Stack limits

The **stack use** of a lowered function is its frame size plus the largest stack use among its call sites. Each function-typed parameter of the function has a **stack bound**: when the function is judged on its own, the N of the `stack N` its type demands, or none; inside the stack use of a direct call site, the stack use of the argument that site supplies for it.

- A direct call site uses the callee's stack use computed with each of the callee's function-typed parameters that receives a function argument at that site bound to that argument's stack use: a closure's own stack use, a named function's stack use through its [checking entry](18-contracts.md#checking-entries), the stack bound of the calling function's own parameter when the argument is that parameter, or the N of the `stack N` that a function value's type demands, none when it demands none. In lowered code, a call to a method of a generic parameter's bound is a direct call to the implementing function that the instantiation selects.
- A call through the function's own function-typed parameter uses that parameter's stack bound.
- An indirect call site uses the N of the `stack N` that its function type, registry field type or interface method head demands; a call to a foreign fn the N of its `stack N`; a call to a compiler stub or support routine the stub call bound that [descriptors](27-program-model.md#descriptors) give.
- A call site is **unbounded** when it is a call through the function's own function-typed parameter whose stack bound is none, an indirect call whose type or head demands no `stack` limit, a call to a foreign fn whose head demands none, or a direct call whose callee reaches itself through [dependencies](#propagation) or holds a call site that is unbounded under the bounds that call gives it.

At every [final build](27-program-model.md#final-builds), each function, closure and instantiation that the build lowers is judged against each `stack N` demanded of it by its own limits, by the interface method it implements or by a function type it fits:

1. Each unbounded call site in its text is an error (`limit.stack-unbounded`), with a `called` reason per call on the path to the cause and a `raised` reason for the unbounded call site.
2. Otherwise the build computes its stack use, and a stack use above N is an error (`limit.stack-exceeded`), with a `called` reason per call of the deepest call path.

A demand of its own limits or of an implemented interface method is reported at the unbounded call site, or on the line of its head for an exceeded bound. A demand of a function type that a named function or function value fits is reported at the use where it fits ([Fit](#fit)), the because-chain naming the call site or call path; a closure is judged as if the type's limits were its own. A function value fitting such a type has as its stack use the N of the `stack N` its own type demands, and is unbounded when its type demands none.

The frame size of a lowered function is the size the backend reports for its frame. When the backend reports none, it is the conservative frame bound: 16 bytes, plus the callee-saved register area of the function's calling convention, plus each [NIR](19-nir.md#nir) value and slot of the function at the size of its type in the final closure's [type layout](30-artefacts.md#type-layout), rounded up to 8 bytes, plus its largest outgoing argument area. Lowering must make every execution of a function occupy at most its stack use.

```nan
// file: brackets/package.nan
//! Bracket nesting report.

package brackets {
    language 1
    targets x86_64-host
    entry main
} brackets

// file: brackets/brackets.nan
//! Reports how deeply a bracket string nests.

pub fn max_nesting(text: Slice[u8]) -> u32 limits stack 4_096 {
    var level: u32 = 0
    var deepest: u32 = 0
    for byte in text {
        if byte == b'(' {
            level = level.saturating_add(1)
        }
        if byte == b')' {
            level = level.saturating_sub(1)
        }
        if level > deepest {
            deepest = level
        }
    }
    return deepest
} max_nesting

fn main(take world: host::World) fails task::Cancelled uses wait {
    world.log.info("nesting", value: max_nesting(b"(()(()))"))
} main
```

```nan
// file: steps/package.nan
//! Repeated step report.

package steps {
    language 1
    targets x86_64-host
    entry main
} steps

// file: steps/steps.nan
//! Reports a value after two steps.

pub fn apply_twice(value: u32, step: fn(x: u32) -> u32) -> u32 {
    let once = step(value)
    return step(once)
} apply_twice

pub fn bumped_twice(value: u32) -> u32 limits stack 4_096 {
    return apply_twice(value, step: fn(x: u32) => x.saturating_add(1))
} bumped_twice

fn main(take world: host::World) fails task::Cancelled uses wait {
    world.log.info("bumped", value: bumped_twice(40))
} main
```

```nan
// file: numbers/package.nan
//! Digit count report.

package numbers {
    language 1
    targets x86_64-host
    entry main
} numbers

// file: numbers/numbers.nan
//! Reports how many decimal digits a number has.

pub fn digits(value: u64) -> u32 limits stack 4_096 {
    if value < 10 {
        return 1
    }
    return 1 + digits(value / 10) // error: limit.stack-unbounded
} digits

fn main(take world: host::World) fails task::Cancelled uses wait {
    world.log.info("digits", value: digits(1_234))
} main
```

## Function types

A **function type** is the type of a function value. It carries a full head without a name or generic parameters, and no requires or ensures: a function reached through a value checks its unproven requires in its [checking entry](18-contracts.md#checking-entries).

```ebnf
fn_type = fn_kind "fn" fn_type_head | "fn" fn_type_head convention_clause? ;
fn_kind = 'task' | "interrupt" ;
fn_type_head = "(" fn_type_param_list? ")" fn_result? fails_clause? uses_clause? limits_clause? ;
fn_type_param_list = fn_type_param ( "," fn_type_param_list? )? ;
fn_type_param = mode? IDENT ":" type_expr ;
```

- The parts follow the order of a [head](02-items.md#heads): parameters, result, `fails`, `uses wait`, `limits`, and last the convention clause of [foreign fns](23-low-level.md#foreign-functions), which only a plain `fn` type may have. A plain `fn` type with a convention clause is a **foreign function type**.
- When the result of a head or of a function type is a function type, the `fails`, `uses`, `limits` and convention clauses that follow belong to that result type ([one-token lookahead](41-grammar.md#one-token-lookahead)).
- The parameter names are the argument labels of calls through a value of the type. Such a call is written, marked and evaluated as a direct call to a function with the type's head.
- The type of a named function or closure is the function type of its head, with the convention clause of a foreign fn. Its kind is `interrupt fn` for an interrupt fn, `task fn` for a task closure and plain `fn` otherwise.
- A `task fn` type is the type of [task closures](16-concurrency.md#task-closures). An `interrupt fn` type is the type of [interrupt fns](24-kernel.md#interrupt-functions), which fix the heads it may have.
- [Structural properties](05-properties.md#structural-properties) give the properties of function types, and [function values](07-ownership.md#function-values) where a closure or function-typed parameter may appear.

```nan
pub closed enum ProbeError derives Render {
    Net(sys::net::NetError) from
    Cancelled(task::Cancelled) from
} ProbeError

pub closed struct Prober {
    pub probe: fn(mut conn: sys::net::Conn) -> bool fails ProbeError uses wait limits noalloc
} Prober

pub fn probe_with(prober: Prober, mut conn: sys::net::Conn) -> bool fails ProbeError uses wait {
    let probe = prober.probe
    return try await probe(mut conn)
} probe_with
```

### Fit

A function or function value **fits** a function type when each of these holds, checked in this order:

1. Kind: a plain `fn` type without a convention clause accepts named functions other than foreign and interrupt fns, closures, and values of such types. A `task fn` type accepts the same named functions, closures, and values of `task fn` types. An `interrupt fn` type accepts interrupt fns and values of `interrupt fn` types. A foreign function type accepts foreign fns and values of foreign function types with the same convention. Otherwise it is an error (`effect.fit-kind`).
2. Head: the same number of parameters, a `self` parameter counting as one of its type, with equal modes and types in order, and an equal result; parameter names may differ. Otherwise it is an error (`effect.fit-head`).
3. Failures: the function cannot fail, or its failure type equals the type's. Otherwise it is an error (`effect.fit-fails`).
4. Waiting: the function does not state `uses wait`, or the type does. Otherwise it is an error (`effect.fit-wait`).
5. Limits: the digest of the named function as reached through its [checking entry](18-contracts.md#checking-entries), the digest of the closure, or the declared digest of the function value's type has every fact the type's limits demand; `stack N` is checked at the final build ([Stack limits](#stack-limits)).

The fit rule applies wherever a function or function value becomes a value of a function type: an argument, an initialised or assigned binding, a construction, a contribution and a returned value. Only the first unmet condition is reported. Conditions 1 to 4 are reported at the use. For condition 5, a closure is judged as if the type's limits were its own, with the sites and codes of [Limits](#limits); any other function or value is reported at the use with the code of [Limits](#limits) for the missing fact, and the because-chain names the site that brings it.

```nan
pub fn count_matching(data: Slice[u8], matches: fn(byte: u8) -> bool limits notrap) -> usize {
    var count: usize = 0
    for byte in data {
        if matches(byte) {
            count += 1
        }
    }
    return count
} count_matching

pub fn is_vowel(byte: u8) -> bool limits pure, notrap {
    return match byte {
        b'a' or b'e' or b'i' or b'o' or b'u' => true
        else => false
    }
} is_vowel

pub fn vowel_count(data: Slice[u8]) -> usize {
    return count_matching(data, matches: is_vowel)
} vowel_count
```

```nan
pub fn count_matching(data: Slice[u8], matches: fn(byte: u8) -> bool limits notrap) -> usize {
    var count: usize = 0
    for byte in data {
        if matches(byte) {
            count += 1
        }
    }
    return count
} count_matching

pub closed enum FlagError derives Eq, Render {
    Unknown(u8)
} FlagError

pub fn is_set(byte: u8) -> bool fails FlagError {
    return match byte {
        b'y' => true
        b'n' => false
        else => fail FlagError.Unknown(byte)
    }
} is_set

pub fn set_count(data: Slice[u8]) -> usize {
    return count_matching(data, matches: is_set) // error: effect.fit-fails
} set_count
```

## Higher-order calls

A function-typed parameter's type is an upper bound on the functions it receives. Calls through the parameter are written as its type requires: `await` when the type states `uses wait`, and `try` or `attempt` when it may fail.

- A call of a higher-order function, the outer call, carries `await` exactly when it [may wait](#awaited-calls): when the callee states `uses wait` or a function argument for a plain `fn` parameter does.
- The outer call may fail with the failure type of the callee's head after its generic parameters are fixed.

### Failure polymorphism

A `fails` clause may name a generic parameter E of its item, as `task::within` does: `pub fn within[T, E](clock: sys::Clock, ms: u64, body: fn() -> T fails E uses wait) -> Option[T] fails E uses wait`. The item then fails exactly with values of type E, and its body propagates a failure of type E with `try` as [failures](11-errors-domains.md#failures) define.

- [Inference](08-generics-interfaces.md#inference) matches a function argument against a parameter type failing with E by matching the argument's failure type with E. An argument that cannot fail matches without fixing E.
- An E that only arguments that cannot fail matched is fixed as `Never` instead of being uninferred, except where [failure bounds](11-errors-domains.md#failure-bounds) fix it otherwise.
- An instantiation failing with `Never` cannot fail, so its calls carry no `try` or `attempt` ([attempt](11-errors-domains.md#attempt)).

```nan
pub fn retry[E](attempts: u32, op: fn() -> bool fails E uses wait) -> bool fails E {
    for _ in 0..<attempts {
        if try await op() {
            return true
        }
    }
    return false
} retry

pub closed enum PingError derives Render {
    Net(sys::net::NetError) from
    Cancelled(task::Cancelled) from
} PingError

pub fn has_data(mut conn: sys::net::Conn, alloc: mem::Alloc) -> bool fails PingError uses wait {
    var buf = Untrusted.of(mem::Bytes.zeroed(64, alloc))
    let count = try await mut conn.read(mut buf.view_mut())
    return count > 0
} has_data

pub fn wait_for_data(mut conn: sys::net::Conn, alloc: mem::Alloc) -> bool fails PingError uses wait {
    return try await retry(3, op: fn[mut conn]() -> bool fails PingError uses wait {
        return try await has_data(mut conn, alloc)
    })
} wait_for_data
```

```nan
pub fn retry[E](attempts: u32, op: fn() -> bool fails E uses wait) -> bool fails E {
    for _ in 0..<attempts {
        if try await op() {
            return true
        }
    }
    return false
} retry

pub closed enum PingError derives Render {
    Net(sys::net::NetError) from
    Cancelled(task::Cancelled) from
} PingError

pub fn has_data(mut conn: sys::net::Conn, alloc: mem::Alloc) -> bool fails PingError uses wait {
    var buf = Untrusted.of(mem::Bytes.zeroed(64, alloc))
    let count = try await mut conn.read(mut buf.view_mut())
    return count > 0
} has_data

pub fn wait_for_data(mut conn: sys::net::Conn, alloc: mem::Alloc) -> bool fails PingError uses wait {
    return try retry(3, op: fn[mut conn]() -> bool fails PingError uses wait { // error: effect.await-missing
        return try await has_data(mut conn, alloc)
    })
} wait_for_data
```

## Purity

A function, closure or function value is **pure** when its digest, or the declared digest of its type, has no effect fact, empty capabilities and validators sets and the class `traceless`. Pure code may trap, run without end and use floats unless other limits forbid it.

Purity is required:

- by `limits pure`;
- of [transparent function](02-items.md#transparent-functions) bodies, and of const initializers and const assert conditions ([constants](02-items.md#constants));
- of [contract terms](18-contracts.md#contract-terms) and [expect](18-contracts.md#expect) conditions;
- of the implementing functions of the operator interfaces and of `Eq`, `Ord`, `Hash` and `Default`, through the limits of their interface method heads ([operator interfaces](08-generics-interfaces.md#operator-interfaces)).

```nan
pub fn weight(byte: u8) -> u32 limits pure, notrap, terminates {
    return u32.widen(byte).wrapping_mul(3)
} weight

pub fn is_heavy(byte: u8) -> bool = weight(byte) > 300
```

## Diagnostics

| Code | Class | Rule |
| --- | --- | --- |
| `effect.await-missing` | error | [Awaited calls](#awaited-calls) |
| `effect.await-unneeded` | error | [Awaited calls](#awaited-calls) |
| `effect.fit-fails` | error | [Fit](#fit) |
| `effect.fit-head` | error | [Fit](#fit) |
| `effect.fit-kind` | error | [Fit](#fit) |
| `effect.fit-wait` | error | [Fit](#fit) |
| `effect.foreign-wait` | error | [Foreign functions and trusted code](#foreign-functions-and-trusted-code) |
| `effect.undo-irreversible` | error | [Compensation](#compensation), [Servers](26-components.md#servers) |
| `effect.undo-target` | error | [Compensation](#compensation), [Servers](26-components.md#servers) |
| `effect.uses-unknown` | error | [Waiting](#waiting) |
| `limit.allocates` | error | [Limits](#limits) |
| `limit.duplicate` | error | [Limits](#limits) |
| `limit.impure` | error | [Limits](#limits) |
| `limit.may-not-terminate` | error | [Limits](#limits) |
| `limit.may-trap` | error | [Limits](#limits) |
| `limit.redundant` | error | [Limits](#limits) |
| `limit.reversibility-exceeded` | error | [Limits](#limits) |
| `limit.stack-exceeded` | error | [Stack limits](#stack-limits) |
| `limit.stack-size` | error | [Limits](#limits) |
| `limit.stack-unbounded` | error | [Stack limits](#stack-limits), [Static bounds](17-suspension-io.md#static-bounds) |
