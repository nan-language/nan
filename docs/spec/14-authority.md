# Capabilities and authority

This chapter defines authority and the absence of ambient authority, capabilities and the capability tables behind Copy capabilities, the std capabilities, revocation, resources, root grants, attenuation, capability kinds, protocol phases, manifest grants, denied dependencies, the trust that device and priv grants carry, and authority summaries with their approval by final-link roots.

## Authority

**Authority** is the power to act outside a function's own values: I/O, time, randomness, allocation, devices and privileged machine state. **Ambient authority** is authority reachable without a capability value; NaN has none.

- Code acts outside its own values only through [capabilities](#capabilities) it holds as parameters, captures or values reached from them. A [manifest grant](#manifest-grants) confers no capability; it permits the operations its authority covers.
- No item holds mutable state: a [const item](02-items.md#constants) is immutable and Inert, so it holds no capability, and every other value an item holds, such as a registry entry, is const data. The one value reachable without being passed is the [kernel anchor](24-kernel.md#anchor), reached only from entry contexts.
- Every std operation that performs I/O, reads a clock, draws randomness or allocates reaches a capability: its receiver, an argument, or a capability its receiver stores. A device operation that reaches no capability, such as `hw::IrqShared.install`, is permitted only by an `allow device` grant; `hw::Mmio.map` and `hw::Ports.map` are also [trusted primitives](23-low-level.md#trusted-primitives), called inside trusted code of a module that an `allow trusted` grant names as well.
- Capabilities come only from [root grants](#root-grants), from operations on other capabilities, from [sim](22-testing.md#simulated-capabilities) constructors and `testing::world()`, from the minters of [capability kinds](#capability-kinds) and from [trusted primitives](23-low-level.md#trusted-primitives); no operation of safe code builds one from bits, integers or text.

```nan
pub fn report_level(log: sys::Log, level: u32) {
    log.info("level", value: level)
} report_level
```

## Capabilities

A **capability** is a value conferring authority. The [primitive capabilities](05-properties.md#capability-types) this chapter declares are base types with the properties of this table; none is a view, Inert or AnyBits, and none implements `Eq`, `Ord`, `Hash` or `Render`.

| Type | Linear | Copy | Send |
| --- | --- | --- | --- |
| `sys::Fs`, `sys::Net`, `sys::Clock`, `sys::WallClock`, `sys::Random`, `sys::Log`, `sys::Stdio` | no | yes | yes |
| `sys::fs::File`, `sys::net::Listener` | no | no | yes |
| `sys::net::Conn`, `sys::Revoker[C]`, `sys::Minter[C, S, R]` | yes | no | yes |
| `boot::Machine`, `boot::Kernel` | yes | no | no |

[`mem::Alloc`](12-memory.md#allocators) and [`task::Executor`](16-concurrency.md#executors) are the other Copy capabilities of std, and every [capability kind](#capability-kinds) is Copy and Send. [Standard library 1.0](28-stdlib.md#std-authority) lists every primitive capability.

### Capability tables

Every Copy capability other than `mem::Alloc` is a [generational handle](12-memory.md#generational-handles) naming an entry of a **capability table**; `mem::Alloc` names a cell ([budgets](12-memory.md#budgets)). A capability table is owned by its **minter**: the root grant minted with it, a `sys::Revoker`, a `sys::Minter` ([capability kinds](#capability-kinds)), or the test or example run whose [sims](22-testing.md#simulated-capabilities) it holds.

- An entry records the authority it confers and the entry it derives from, if any. An executor's entry also records a clock: the `sys::Clock` of the root grant that minted it, the virtual clock of the test or example run whose world holds it, or for an executor made by `sys::revocable` the clock of its source; the kernel executor records the kernel's clock ([anchor](24-kernel.md#anchor)). Every copy of a handle names the same entry.
- Revoking an entry advances its slot's generation, so every handle to it is stale. An operation through a handle checks at its start that the handle is live; an operation already past its check completes as if the entry were live.
- Tables are shared by every task and CPU. Checks and revocations are atomic: a check that starts after a revocation completes finds the handle stale.
- Entries are memory of the [support routines](27-program-model.md#runtime-support), charged to no cell and drawn through no allocator. An operation adding an entry when that memory is exhausted traps (`trap.exhausted`).
- A table's entries are freed when its minter ends: a `sys::Revoker` at its revocation, a `sys::Minter` when it closes or its domain is settled, a test or example run at its end, and a root grant's table when the program ends.

### Std capabilities

```nan-api
// module: sys

pub open struct Fs {
} Fs

pub fn Fs.subdir(self, path: Str) -> Fs fails fs::FsError

pub fn Fs.read_only(self) -> Fs fails Revoked

pub fn Fs.open(self, path: Str) -> fs::File fails fs::FsError uses wait

pub fn Fs.create(self, path: Str) -> fs::File fails fs::FsError uses wait

pub fn Fs.read_all(self, path: Str, alloc: mem::Alloc) -> Untrusted[mem::Bytes] fails fs::FsError uses wait

pub fn Fs.write_all(self, path: Str, data: Slice[u8]) fails fs::FsError uses wait

pub fn Fs.remove(self, path: Str) fails fs::FsError uses wait

pub open struct Net {
} Net

pub fn Net.only_host(self, host: Str) -> Net fails Revoked

pub fn Net.listen(self, port: u16) -> net::Listener fails net::NetError uses wait

pub fn Net.connect(self, host: Str, port: u16) -> net::Conn fails net::NetError uses wait

pub open struct Clock {
} Clock

pub fn Clock.elapsed_ms(self) -> u64 fails Revoked

pub fn Clock.elapsed_ns(self) -> u64 fails Revoked

pub fn Clock.sleep(self, ms: u64) fails task::Cancelled uses wait

pub fn Clock.sleep_ns(self, ns: u64) fails task::Cancelled uses wait

pub fn Clock.coarse(self, ns: u64) -> Clock fails Revoked

pub open struct WallClock {
} WallClock

pub fn WallClock.now_ns(self) -> u64 fails Revoked

pub fn WallClock.coarse(self, ns: u64) -> WallClock fails Revoked

pub open struct Random {
} Random

pub fn Random.fill(self, mut buf: SliceMut[u8]) fails Revoked

pub fn Random.next_u64(self) -> u64 fails Revoked

pub open struct Log {
} Log

pub fn Log.info[T: Render](self, message: Str, value: T)

pub fn Log.warn[T: Render](self, message: Str, value: T)

pub fn Log.error[T: Render](self, message: Str, value: T)

pub open struct Stdio {
} Stdio

pub fn Stdio.read_in(self, mut buf: Untrusted[SliceMut[u8]]) -> usize fails StdioError uses wait

pub fn Stdio.write_out(self, data: Slice[u8]) fails StdioError uses wait

pub fn Stdio.write_err(self, data: Slice[u8]) fails StdioError uses wait

pub open enum StdioError derives Render {
    Closed
    Revoked(Revoked) from
    Cancelled(task::Cancelled) from
} StdioError
```

```nan-api
// module: sys::fs

pub open struct File {
} File

pub fn File.read(mut self, mut buf: Untrusted[SliceMut[u8]]) -> usize fails FsError uses wait

pub fn File.write(mut self, data: Slice[u8]) -> usize fails FsError uses wait

pub fn File.byte_len(self) -> u64 fails FsError uses wait

pub open enum FsError derives Render {
    NotFound
    Denied
    InvalidPath
    NoSpace
    Revoked(sys::Revoked) from
    Cancelled(task::Cancelled) from
} FsError
```

```nan-api
// module: sys::net

pub open struct Listener {
} Listener

pub fn Listener.accept(mut self) -> Conn fails NetError uses wait

pub open linear struct Conn {
} Conn

pub fn Conn.read(mut self, mut buf: Untrusted[SliceMut[u8]]) -> usize fails NetError uses wait

pub fn Conn.write(mut self, data: Slice[u8]) -> usize fails NetError uses wait

pub fn Conn.close(take self) fails NetError uses wait

pub fn Conn.abandon(take self)

pub open enum NetError derives Render {
    Refused
    Reset
    Unreachable
    AddressInUse
    Denied
    Revoked(sys::Revoked) from
    Cancelled(task::Cancelled) from
} NetError
```

- A `sys::Fs` reaches one directory tree. A path is relative, its segments separated by `/`, and names the entry its segments reach from the tree's root; an empty segment, `.`, `..` or a segment holding the char `'\0'` fails with `sys::fs::FsError.InvalidPath`. Resolution never leaves the tree: a symbolic link leading outside it fails with `sys::fs::FsError.Denied`. `subdir` checks only the path's form.
- `open` opens a file for reading and `create` creates or truncates one for writing; the other direction fails with `sys::fs::FsError.Denied`. `read_all` returns a file's contents, drawn through `alloc`, and traps when `alloc` is exhausted (`trap.exhausted`). `write_all` replaces a file's contents, creating it when missing, `remove` deletes a file and `byte_len` returns a file's length in bytes. `create` and `write_all` create the missing directories of their path. A missing file fails with `sys::fs::FsError.NotFound`, a full device with `sys::fs::FsError.NoSpace`, and a path naming a directory where a file is expected or a refusal by the platform with `sys::fs::FsError.Denied`.
- `sys::Net.listen` accepts TCP connections on `port` at every local address, failing with `sys::net::NetError.AddressInUse` when the port is taken. `sys::Net.connect` opens one to `host`, an IPv4 or IPv6 address literal or a name the platform resolves, failing with `sys::net::NetError.Refused` when the peer refuses it and with `sys::net::NetError.Unreachable` when the host does not resolve or cannot be reached. Port 0 and a refusal by the platform fail with `sys::net::NetError.Denied`. A read of a connection returns 0 once the peer has closed it and every byte it sent has been read, and fails with `sys::net::NetError.Reset` once the peer has reset it and every byte that reached it has been read; a write on a connection the peer closed or reset fails with `sys::net::NetError.Reset`. `sys::net::Conn.close` sends the data not yet sent, shuts the connection down and returns once the platform reports it closed; `sys::net::Conn.abandon` discards the data not yet sent and resets the connection.
- A `sys::Clock` counts monotonic time in nanoseconds from the minting of its root grant, and has a **resolution**, 1 ns for a root grant's clock. `elapsed_ns` returns the nanoseconds elapsed, rounded down to a multiple of the resolution, and never decreases; `elapsed_ms` returns them divided by 1,000,000. `sleep_ns` returns once at least `ns` nanoseconds have elapsed and the elapsed nanoseconds are a multiple of the resolution. `sleep(ms)` is `sleep_ns` of `ms` × 1,000,000, or of the largest `u64` when that product exceeds it; every millisecond form of the clock's waits converts its duration so.
- `c.coarse(ns)` returns a clock reading the same time with the greater of `c`'s resolution and `ns` ([attenuation](#attenuation)), and fails with `sys::Revoked.Stale` when `c` is stale. Code given only a clock of resolution r can time nothing finer than r.
- A `sys::WallClock` reads wall-clock time, which may jump: `now_ns` returns the nanoseconds since the Unix epoch, 1970-01-01T00:00:00 UTC, rounded down to a multiple of its resolution. `coarse` derives an entry as `sys::Clock.coarse` does. On the host the root grant's wall clock reads the platform's real-time clock.
- `sys::Random` draws from the platform's cryptographically secure source.
- A `sys::Log` call writes one record and never waits or fails. On the host, a record is one line on standard error: the level (`info`, `warn` or `error`), `: `, the message, a space, the rendering of `value` within a [render budget](13-text-encode.md#render) of 1,024 bytes and a line feed, each line feed of the message or rendering written as the two chars `\n`.
- `read` and `read_in` return the number of bytes written into `buf`, 0 at the end of input. `write` returns the number of bytes written, at least 1 when `data` is not empty. `write_out` and `write_err` write all of `data`. A file keeps one position, starting at 0, that each read and write advances by its count.
- `sys::StdioError.Closed` reports a closed standard stream.

## Revocation

**Revocation** ends every copy of a capability through the linear value that minted it: a `sys::Revoker`, a `sys::Minter`, or, for an allocator, its [budget](12-memory.md#budgets).

```nan-api
// module: sys

pub closed enum Revoked derives Copy, Eq, Hash, Render {
    Stale
} Revoked

pub interface Capability {
} Capability

impl Fs: Capability

impl Net: Capability

impl Clock: Capability

impl WallClock: Capability

impl Random: Capability

impl Log: Capability

impl Stdio: Capability

impl mem::Alloc: Capability

impl task::Executor: Capability

pub fn revocable[C: Capability and Copy](cap: C) -> Revoker[C] fails Revoked

pub open linear struct Revoker[C: Capability and Copy] {
} Revoker

pub fn Revoker.capability(self) -> C

pub fn Revoker.revoke(take self)
```

- `sys::revocable(cap)` adds an entry deriving from `cap`'s entry to a new table owned by the returned `sys::Revoker`, and fails with `sys::Revoked.Stale` when `cap` is stale. `r.capability()` returns a handle to that entry, the same on every call.
- `take r.revoke()` revokes that entry and every entry deriving from it, transitively and in any table, then frees the revoker's table.
- For a `mem::Alloc`, `sys::revocable(cap)` adds a cell instead of an entry and never fails, and revoking closes that cell ([budgets](12-memory.md#budgets)).
- `sys::revocable`, `sys::Revoker.capability` and `sys::Revoker.revoke` terminate and have no effect fact. Their only trap site is the entry or cell that `sys::revocable` adds ([capability tables](#capability-tables), [budgets](12-memory.md#budgets)).
- An impl line naming `sys::Capability` outside std is an error (`authority.capability-impl`).

An operation through a stale handle behaves as follows:

| Capability | Operation through a stale handle |
| --- | --- |
| `sys::Fs`, `sys::Net`, `sys::Random`, `sys::Stdio` | fails with `sys::Revoked.Stale`, directly or through the `Revoked` variant of its error enum |
| `sys::Clock` | `elapsed_ms`, `elapsed_ns` and `coarse` fail with `sys::Revoked.Stale`; `sleep` and `sleep_ns` return at once |
| `sys::WallClock` | `now_ns` and `coarse` fail with `sys::Revoked.Stale` |
| `sys::Log` | the record is discarded |
| a [capability kind](#capability-kinds) | `sys::reach` returns `None` and `sys::derive` fails with `sys::Revoked.Stale` |
| `mem::Alloc` | draws through it are exhaustion ([budgets](12-memory.md#budgets)) |
| `task::Executor` | `task::scope`, `task::race`, the [domain runs](11-errors-domains.md#domains) and `host::Runner.run` start no task and fail at once with the cancellation of their failure type; a task that `task::Scope.spawn` or `spawn_sized` would start fails at once with it without running its closure. No cancelling construct is the origin of these cancellations ([origin and absorption](11-errors-domains.md#origin-and-absorption)). `coarse` fails with `sys::Revoked.Stale`. |

```nan
pub fn read_config(fs: sys::Fs, alloc: mem::Alloc) -> Untrusted[mem::Bytes] fails sys::fs::FsError uses wait {
    return try await fs.read_all("app.conf", alloc)
} read_config

test revoked_copy_fails for read_config {
    let world = testing::world()
    let revoker = try sys::revocable(world.fs)
    let fs = revoker.capability()
    take revoker.revoke()
    let outcome = attempt await read_config(fs, alloc: world.alloc)
    let revoked = match outcome {
        Err(sys::fs::FsError.Revoked(_)) => true
        else => false
    }
    expect revoked
} revoked_copy_fails
```

```nan
pub fn still_derives(clock: sys::Clock) -> bool {
    match attempt sys::revocable(clock) {
        Ok(revoker) => {
            take revoker.revoke()
            return true
        }
        Err(_) => return false
    }
} still_derives

test a_stale_clock_derives_nothing for still_derives {
    let world = testing::world()
    let revoker = try sys::revocable(world.clock)
    let clock = revoker.capability()
    take revoker.revoke()
    let derived = still_derives(clock)
    expect not derived
} a_stale_clock_derives_nothing
```

```nan
pub closed struct Meter {
    pub reads: u32
} Meter

impl Meter: sys::Capability // error: authority.capability-impl
```

## Resources

A **resource** is an affine or linear value opened through a capability, such as a file or a connection; every resource is a primitive capability.

- A resource records the entry of the capability that opened it or, when another resource opened it, the entry that resource records. Once that entry is revoked, each operation on the resource fails with `sys::Revoked.Stale` through its error enum, except its releases, which complete.
- An affine resource reaching its [end of scope](07-ownership.md#ownership) is released by the runtime: its platform object is closed without waiting and without running user code.
- A resource whose graceful release waits is linear. Its graceful release, such as `sys::net::Conn.close`, and its [abandon method](17-suspension-io.md#abandoning-resources) consume it.
- Every primitive capability giving access to hardware is linear, so a type holding one is [linear](05-properties.md#linear-types).

```nan
pub closed enum GreetError derives Render {
    Net(sys::net::NetError) from
    Cancelled(task::Cancelled) from
} GreetError

pub fn send_greeting(net: sys::Net) fails GreetError uses wait {
    var conn = try await net.connect("10.0.0.2", port: 7)
    defer take conn.abandon()
    discard try await mut conn.write(b"hello\n")
    undefer conn
    try await take conn.close()
} send_greeting

pub fn config_size(fs: sys::Fs) -> u64 fails sys::fs::FsError uses wait {
    let file = try await fs.open("app.conf")
    return try await file.byte_len()
} config_size
```

## Root grants

A **root grant** is the capability a platform [landing fn](23-low-level.md#landing-functions) mints with `hw::mint_root` and passes to the [entry function](27-program-model.md#entry). A program run receives exactly one, by target:

| Target | Root grant |
| --- | --- |
| `x86_64-host`, `aarch64-host` | `host::World` |
| `x86_64-uefi`, `aarch64-uefi` | `boot::Machine` |
| `x86_64-bare`, `aarch64-bare` | `boot::Kernel` |

```nan-api
// module: host

pub open struct World derives Send {
    pub fs: sys::Fs
    pub net: sys::Net
    pub clock: sys::Clock
    pub wall: sys::WallClock
    pub random: sys::Random
    pub log: sys::Log
    pub stdio: sys::Stdio
    pub alloc: mem::Alloc
    pub exec: task::Executor
    pub args: Untrusted[mem::List[mem::Bytes]]
    pub environment: Untrusted[mem::Map[mem::Bytes, mem::Bytes]]
} World
```

```nan-api
// module: boot

pub open linear struct Machine {
} Machine

pub open linear struct Kernel {
} Kernel
```

- `host::World` is affine; the table it was minted with lives as long as the process. `fs` reaches the process's working directory, `alloc` is the process domain's allocator and `exec` runs its tasks. `args` holds the command-line arguments after the program name and `environment` the environment variables, both [Untrusted](15-untrusted.md#untrusted-values).
- `boot::Machine` and `boot::Kernel` are linear; their tables live as long as the machine runs, and [Kernel execution model](24-kernel.md) defines their operations.

```nan
// file: greet/package.nan
//! Prints a greeting.

package greet {
    language 1
    targets x86_64-host
    entry main
} greet

// file: greet/greet.nan
//! Entry of the greeting program.

pub closed enum GreetError derives Render {
    Stdio(sys::StdioError) from
    Cancelled(task::Cancelled) from
} GreetError

fn main(take world: host::World) fails GreetError uses wait {
    try await world.stdio.write_out(b"hello\n")
} main
```

## Attenuation

**Attenuation** is deriving a capability with less authority through an ordinary method. The std attenuations are `sys::Fs.subdir`, which reaches only the tree under `path`; `sys::Fs.read_only`, whose `create`, `write_all` and `remove` fail with `sys::fs::FsError.Denied`; `sys::Net.only_host`, whose `connect` fails with `sys::net::NetError.Denied` unless its `host` equals, byte for byte, the `host` that `only_host` received and the source's `connect` does not deny that `host`, and whose `listen` always fails with it; `sys::Clock.coarse` and `sys::WallClock.coarse`, whose clock has a coarser resolution; `task::Executor.coarse`, whose delays end on a coarser resolution ([executors](16-concurrency.md#executors)); and `mem::Alloc.split` ([budgets](12-memory.md#budgets)).

- A std attenuation other than `split` adds an entry deriving from its source's entry to its source's table and returns a handle to it. The new entry keeps every restriction of its source, and revoking the source revokes it. An implementation may instead return a handle to an existing entry with the same source and restriction.
- Any code may attenuate: a type whose private field holds a capability and whose methods expose less authority attenuates it.

```nan
pub open struct ReportDir {
    fs: sys::Fs
} ReportDir

pub fn reports_in(fs: sys::Fs) -> ReportDir fails sys::fs::FsError {
    let reports = try fs.subdir("reports")
    return ReportDir(fs: reports)
} reports_in

pub fn ReportDir.save(self, name: Str, data: Slice[u8]) fails sys::fs::FsError uses wait {
    try await self.fs.write_all(name, data)
} ReportDir.save
```

```nan
pub fn millisecond_reading(clock: sys::Clock) -> u64 fails sys::Revoked {
    let coarse = try clock.coarse(1_000_000)
    return try coarse.elapsed_ns()
} millisecond_reading

test reading_rounds_down for millisecond_reading {
    let world = testing::world()
    try await world.clock.sleep_ns(1_500_000)
    let reading = try millisecond_reading(world.clock)
    expect reading == 1_000_000
} reading_rounds_down
```

```nan
pub fn denies(net: sys::Net, host: Str) -> bool fails task::Cancelled uses wait {
    let outcome = attempt await net.connect(host, port: 7)
    var denied = false
    match outcome {
        Ok(conn) => take conn.abandon()
        Err(failure) => {
            denied = match failure {
                sys::net::NetError.Denied => true
                else => false
            }
        }
    }
    return denied
} denies

test narrowing_keeps_the_host for denies {
    let world = testing::world()
    let narrow = try world.net.only_host("localhost")
    let same = try narrow.only_host("localhost")
    let other = try narrow.only_host("127.0.0.1")
    let kept = try await denies(same, host: "localhost")
    let moved = try await denies(other, host: "127.0.0.1")
    expect not kept
    expect moved
} narrowing_keeps_the_host
```

## Capability kinds

A **capability kind** is a primitive capability that a package outside std declares, such as the ports, store views or CPU shares of a kernel. An impl line `impl C: sys::Kind[S, R]` declares the struct C a kind: each value of C is a handle naming an entry of a [capability table](#capability-tables) that keeps a **record** of type R, and each entry belongs to a **minter**, a `sys::Minter[C, S, R]` keeping a **state** of type S. Code acts through a kind only by calling the functions of the module declaring it, which alone reaches the states and records behind its handles.

```nan-api
// module: sys

pub interface Kind[S: Send, R: AnyBits and Copy] {
} Kind

pub open linear struct Minter[C: Kind[S, R], S: Send, R: AnyBits and Copy] {
} Minter

pub fn Minter.of(take state: S, alloc: mem::Alloc) -> Minter[C, S, R]

pub fn Minter.mint(self, record: R) -> C

pub fn Minter.revoke(self, capability: C)

pub fn Minter.close(take self)

pub fn reach[C: Kind[S, R], S: Send, R: AnyBits and Copy, T: Any, E](capability: C, body: fn(state: S, record: R) -> T fails E) -> Option[T] fails E

pub fn derive[C: Kind[S, R], S: Send, R: AnyBits and Copy](capability: C, narrow: fn(state: S, record: R) -> R) -> C fails Revoked
```

- A kind is a struct without fields, generic parameters, derives or a modifier other than `open`, and its record type is a scalar, a layout, an `hw::Ptr` or an `Array` of these, at most 16 bytes long. A `sys::Kind` impl line naming any other type or record type is an error (`authority.kind-form`), reported on the impl line. A construction of a kind is that error too: a kind's values come only from `mint` and `derive`.
- A kind is Copy and Send, and never linear, a view, Inert or AnyBits. It implements `sys::Capability`, so `sys::revocable` derives entries from its entries.
- A use of `sys::Minter.of`, `sys::Minter.mint`, `sys::Minter.revoke`, `sys::Minter.close`, `sys::reach` or `sys::derive` whose C is not a kind that the module of the code using it declares is an error (`authority.kind-foreign`).
- A module declaring a kind performs `capability` directly ([manifest grants](#manifest-grants)).

A minter owns the table of the entries it mints:

- `sys::Minter.of(state, alloc)` returns a minter keeping `state` in storage drawn through `alloc`, trapping when it is exhausted (`trap.exhausted`). That storage, with the chunks `state` occupies, passes to the outermost domain, the process or the kernel ([domain memory](11-errors-domains.md#domain-memory)).
- `m.mint(record)` adds an entry recording `record` and returns a handle to it. An entry **belongs** to the minter that minted it, or to the one the entry it derives from belongs to.
- `m.revoke(c)` revokes `c`'s entry and every entry deriving from it when that entry belongs to `m`, and does nothing otherwise.
- `take m.close()` revokes every entry belonging to `m`, then releases its state once no `reach` or `derive` holds it. [Settlement](11-errors-domains.md#settlement) revokes them too, but never releases the state.

Only the declaring module reaches through a handle:

- `sys::reach(c, body)` returns `None` when `c` is stale. Otherwise it calls `body` with a read of the state of the minter `c`'s entry belongs to and with the entry's record, holding the state until `body` returns, even once `c` is revoked, and returns `Some` of `body`'s result or fails with its failure.
- `sys::derive(c, narrow)` calls `narrow` as `reach` calls `body`, then adds an entry deriving from `c`'s entry to its table, recording the record `narrow` returned, and returns a handle to it ([attenuation](#attenuation)). It fails with `sys::Revoked.Stale` when `c` is stale before `narrow` runs or once it has returned.
- `sys::reach` and `sys::derive` have no effect fact and no capability of their own ([supplied operations](28-stdlib.md#supplied-operations)); `body` and `narrow` charge what they do. `mint` and `derive` trap when the capability tables are full ([capability tables](#capability-tables)).
- Kinds have no [sims](22-testing.md#simulated-capabilities): tests and dry runs act through handles that the declaring module mints from a minter whose state records what the kind's operations do instead of doing it.

The digest of a function of a kind holds that kind as its capability and hides how the module acts ([digests](10-effects-digests.md#digests)):

```nan
// file: ports/package.nan
//! Ports a kernel hands to components.

package ports {
    language 1
    targets any
    allow capability in ports
} ports

// file: ports/ports.nan
//! Ports counting the messages sent through them.

pub open struct Port {
} Port

pub layout Grant: u64 le {
    slot: u32 at 0
    send: bit 32
    reserved: rest
} Grant

impl Port: sys::Kind[core::atomic::AtomicU64, Grant]

pub fn open_ports(alloc: mem::Alloc) -> sys::Minter[Port, core::atomic::AtomicU64, Grant] {
    return sys::Minter.of(core::atomic::AtomicU64.of(0), alloc)
} open_ports

pub fn Port.send(self) -> bool {
    let sent = sys::reach(self, body: fn(state: core::atomic::AtomicU64, record: Grant) -> bool {
        if not record.send {
            return false
        }
        discard state.fetch_add(1)
        return true
    })
    return sent == Some(true)
} Port.send

pub fn Port.receive_only(self) -> Port fails sys::Revoked {
    return try sys::derive(self, narrow: fn(state: core::atomic::AtomicU64, record: Grant) => Grant(slot: record.slot, send: false, reserved: 0))
} Port.receive_only

test narrowed_port_cannot_send for Port.receive_only {
    let world = testing::world()
    let minter = open_ports(world.alloc)
    defer take minter.close()
    let port = minter.mint(Grant(slot: 1, send: true, reserved: 0))
    let narrowed = try port.receive_only()
    let revoker = try sys::revocable(port)
    let copy = revoker.capability()
    take revoker.revoke()
    let sent = port.send()
    let narrowed_sent = narrowed.send()
    let copy_sent = copy.send()
    minter.revoke(port)
    let revoked_sent = port.send()
    expect sent
    expect not narrowed_sent
    expect not copy_sent
    expect not revoked_sent
} narrowed_port_cannot_send
```

```text
$ nan show ports::Port.send --api
ok show ports::Port.send --api: records 1
api ports::Port.send ports/ports.nan:18
    pub fn Port.send(self) -> bool
    digest: effects none | authority ports::Port | may-trap no | terminates
ok show ports::Port.send --api: records 1
```

```nan
// file: ports/package.nan
//! Ports a kernel hands to components.

package ports {
    language 1
    targets any
    allow capability in ports
} ports

// file: ports/ports.nan
//! Ports naming their slot in a field.

pub open struct Port {
    pub slot: u32
} Port

impl Port: sys::Kind[u32, u64] // error: authority.kind-form
```

```nan
// file: ports/package.nan
//! Ports a kernel hands to components.

package ports {
    language 1
    targets any
} ports

// file: ports/ports.nan
//! Ports declared without a grant.

pub open struct Port {
} Port

impl Port: sys::Kind[u32, u64] // error: authority.ungranted
```

```nan
// file: ports/package.nan
//! Ports a kernel hands to components.

package ports {
    language 1
    targets any
    allow capability in ports
} ports

// file: ports/ports.nan
//! Ports a kernel hands to components.

pub open struct Port {
} Port

impl Port: sys::Kind[u32, u64]

// file: forger/package.nan
//! Mints ports it does not declare.

package forger {
    language 1
    targets any
    dep ports "../ports"
} forger

// file: forger/forger.nan
//! Mints a port outside the module declaring it.

pub fn forge(minter: sys::Minter[ports::Port, u32, u64]) -> ports::Port {
    return minter.mint(7) // error: authority.kind-foreign
} forge
```

## Protocol phases

A capability valid only while a linear parent stays in one phase of its protocol is a [view](05-properties.md#views): std passes it to a closure that the parent's method calls, or returns it from a projection, so it never outlives the phase. `boot::Machine.with_services` passes `boot::Services` this way ([firmware services](24-kernel.md#firmware-services)).

```nan
// file: loader/package.nan
//! Firmware-stage helpers for a UEFI loader.

package loader {
    language 1
    targets x86_64-uefi
} loader

// file: loader/loader.nan
//! Firmware stage records.

pub closed view struct Stage {
    pub services: boot::Services
    pub step: u32
} Stage
```

```nan
// file: loader/package.nan
//! Firmware-stage helpers for a UEFI loader.

package loader {
    language 1
    targets x86_64-uefi
} loader

// file: loader/loader.nan
//! Firmware stage records.

pub closed struct Stage {
    pub services: boot::Services // error: prop.view-undeclared
    pub step: u32
} Stage
```

## Manifest grants

A **manifest grant** is an `allow <authority> in <names>` [directive](03-packages-modules.md#manifest), granting an authority no value carries. Each name a `device`, `priv`, `trusted` or `validate` grant lists is a module of the package or an alias a `dep` directive of the manifest declares, and each name a `capability` grant lists a module of the package; [contributors](25-registries.md#contributors) fixes the names of a `contribute` grant. In a `deny a -> b` directive, `a` is a module of the package and `b` a module of the package, an alias a `dep` directive declares or a std alias. Any other name is an error (`authority.unknown-name`), whose candidates are ranked as for `name.unknown` ([unknown names](03-packages-modules.md#unknown-names)).

A module performs an authority **directly** when one of its items, under a declared target, contains an operation of this table:

| Authority | Operations |
| --- | --- |
| `device` | the device operations of [register windows](23-low-level.md#register-windows), [DMA](23-low-level.md#dma) and [locks](24-kernel.md#locks) |
| `priv` | calls of [privileged intrinsics](23-low-level.md#privileged-intrinsics) and declarations of landing fns |
| `trusted` | [trusted code](23-low-level.md#trusted-code), `assume` and declarations of [foreign fns](23-low-level.md#foreign-functions) |
| `validate` | the operations that [validate grants](15-untrusted.md#validate-grants) list |
| `capability` | declarations of [capability kinds](#capability-kinds) |

- A call of a function whose body performs an authority is not itself an operation of that authority.
- A module performing `device`, `priv`, `trusted`, `validate` or `capability` directly that no grant of that authority names is an error (`authority.ungranted`), reported on the line of each such operation. Its exact fix adds the module to the authority's `allow` directive, writing the directive when the manifest has none.
- A package is **granted** `device`, `priv`, `trusted` or `validate` when its manifest grants it to any name. A dependency granted one of these whose alias the dependent's manifest does not list for it is an error (`authority.dependency-ungranted`), reported on the dependency's `dep` directive, with an exact fix adding the alias as for `authority.ungranted`. A std package needs no grant from its dependents.
- `allow contribute R in <names>` lists the aliases of packages whose contributions to the registry `R` the package admits, as contributors defines.
- A name a `device`, `priv`, `trusted`, `validate` or `capability` grant lists is incompleteness (`unused.grant`), reported on the name, when it is a module performing the authority directly under no declared target or the alias of a dependency not granted the authority. Contributors states the rule for `contribute`.

```nan
// file: uart/package.nan
//! 16550 serial registers.

package uart {
    language 1
    targets x86_64-bare
    allow device in uart
} uart

// file: uart/uart.nan
//! 16550 status reads.

pub layout LineStatus: u8 {
    data_ready: bit 0
    reserved: rest
} LineStatus

pub registers UartRegs {
    data: u8 at 0x0 read write
    line_status: LineStatus at 0x5 read
} UartRegs

pub fn has_byte(mut regs: hw::Mmio[UartRegs]) -> bool {
    let status = mut regs.read(UartRegs.line_status)
    return status.data_ready
} has_byte

// file: kernel/package.nan
//! Serial polling for a bare kernel.

package kernel {
    language 1
    targets x86_64-bare
    dep uart "../uart"
    allow device in uart
} kernel

// file: kernel/kernel.nan
//! Polls the serial port.

pub fn poll_once(mut regs: hw::Mmio[uart::UartRegs]) -> bool {
    return uart::has_byte(mut regs)
} poll_once
```

```nan
// file: uart/package.nan
//! 16550 serial registers.

package uart {
    language 1
    targets x86_64-bare
} uart

// file: uart/uart.nan
//! 16550 status reads.

pub layout LineStatus: u8 {
    data_ready: bit 0
    reserved: rest
} LineStatus

pub registers UartRegs {
    data: u8 at 0x0 read write
    line_status: LineStatus at 0x5 read
} UartRegs

pub fn has_byte(mut regs: hw::Mmio[UartRegs]) -> bool {
    let status = mut regs.read(UartRegs.line_status) // error: authority.ungranted
    return status.data_ready
} has_byte
```

```nan
// file: uart/package.nan
//! 16550 serial registers.

package uart {
    language 1
    targets x86_64-bare
    allow device in uart
} uart

// file: uart/uart.nan
//! 16550 status reads.

pub layout LineStatus: u8 {
    data_ready: bit 0
    reserved: rest
} LineStatus

pub registers UartRegs {
    data: u8 at 0x0 read write
    line_status: LineStatus at 0x5 read
} UartRegs

pub fn has_byte(mut regs: hw::Mmio[UartRegs]) -> bool {
    let status = mut regs.read(UartRegs.line_status)
    return status.data_ready
} has_byte

// file: kernel/package.nan
//! Serial polling for a bare kernel.

package kernel {
    language 1
    targets x86_64-bare
    dep uart "../uart" // error: authority.dependency-ungranted
} kernel

// file: kernel/kernel.nan
//! Polls the serial port.

pub fn poll_once(mut regs: hw::Mmio[uart::UartRegs]) -> bool {
    return uart::has_byte(mut regs)
} poll_once
```

## Denied dependencies

`deny a -> b` forbids module `a` from depending on `b`: an item of `a` naming an item or member by a path whose first segment is `b` is an error (`authority.denied-dependency`), reported on each line holding such a path. Only direct dependence, as [modules](03-packages-modules.md#modules) defines it, is denied.

```nan
// file: report/package.nan
//! Share reports.

package report {
    language 1
    targets any
    deny totals -> sys
} report

// file: report/report.nan
//! Logs report shares.

pub fn log_share(log: sys::Log, part: u64, whole: u64) {
    log.info("share", value: totals::share_percent(part, whole))
} log_share

// file: report/totals/totals.nan
//! Arithmetic over report totals.

pub fn share_percent(part: u64, whole: u64) -> u64 {
    if whole == 0 {
        return 0
    }
    return part * 100 / whole
} share_percent
```

```nan
// file: report/package.nan
//! Share reports.

package report {
    language 1
    targets any
    deny totals -> sys
} report

// file: report/report.nan
//! Logs report totals.

pub fn log_grand_total(log: sys::Log, total: u64) {
    totals::log_total(log, total)
} log_grand_total

// file: report/totals/totals.nan
//! Arithmetic over report totals.

pub fn log_total(log: sys::Log, total: u64) { // error: authority.denied-dependency
    log.info("total", value: total)
} log_total
```

## Device and priv grants

The device and privileged operations that `device` and `priv` grants permit reach memory and machine state no type describes ([systems guarantees](00-charter.md#systems-guarantees)), and the kinds that `capability` grants permit stand for authority that no std capability states. A manifest granting `device`, `priv` or `capability` to any name gives its `package` item a [ledger](31-trust.md#ledger) entry that is a [trust escape](31-trust.md#trust-escapes), so `nan verify` requires a current [seal](31-trust.md#seals) on that item.

## Authority summaries

The **authority summary** of a package is the set of these entries, over the [digests](10-effects-digests.md#digests) of its items under every declared target, the digests that the code of the functions of its [capability kinds](#capability-kinds) gives, and its items' ledger entries:

- each primitive capability type in a digest's capabilities set;
- `alloc`, `device` and `priv`, each when a digest has that fact;
- each validator and decoder in a digest's validators set;
- each pair of a [ledger](31-trust.md#ledger) kind and the canonical path of an item holding a trust escape of that kind, so that all trust escapes of one kind in one item are one entry.

A **widening** is a change after which a package's authority summary holds an entry it did not hold before. [Compatibility](29-identity-versioning.md#facts-outside-the-interface-hash) states which consumers a widening in a release breaks.

The **closure summary** of a [final-link root](27-program-model.md#final-builds) is the set of pairs (P, e) where P is a package of its final closure other than the root and the std packages, and e an entry of P's authority summary. The root's [lock](29-identity-versioning.md#lock) records an approved closure summary, which only [approvals](29-identity-versioning.md#approvals), a first lock's included, extend. A closure summary holding a pair the approved one lacks is an error (`authority.widening-unapproved`), reported on the root's `package` line and listing each such pair; `nan check`, `nan test`, `nan build` and `nan run` of the root report it.

## Diagnostics

| Code | Class | Rule |
| --- | --- | --- |
| `authority.capability-impl` | error | [Revocation](#revocation) |
| `authority.denied-dependency` | error | [Denied dependencies](#denied-dependencies) |
| `authority.dependency-ungranted` | error | [Manifest grants](#manifest-grants) |
| `authority.kind-foreign` | error | [Capability kinds](#capability-kinds) |
| `authority.kind-form` | error | [Capability kinds](#capability-kinds) |
| `authority.ungranted` | error | [Manifest grants](#manifest-grants), [Capability kinds](#capability-kinds), [Trusted code](23-low-level.md#trusted-code), [Register windows](23-low-level.md#register-windows), [DMA](23-low-level.md#dma), [Privileged intrinsics](23-low-level.md#privileged-intrinsics), [Landing functions](23-low-level.md#landing-functions), [Foreign functions](23-low-level.md#foreign-functions) |
| `authority.unknown-name` | error | [Manifest grants](#manifest-grants) |
| `authority.widening-unapproved` | error | [Authority summaries](#authority-summaries), [Approvals](29-identity-versioning.md#approvals) |
