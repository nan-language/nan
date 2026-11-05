# Capabilities and authority

This chapter defines authority and the absence of ambient authority, capabilities and the capability tables behind Copy capabilities, the std capabilities, revocation, resources, root grants, attenuation, capability kinds, protocol phases, manifest grants, denied dependencies, the trust that device and priv grants carry and confined grants do not, and authority summaries with their approval by final-link roots.

## Authority

**Authority** is the power to act outside a function's own values: I/O, time, randomness, allocation, devices and privileged machine state. **Ambient authority** is authority reachable without a capability value; NaN has none.

- Code acts outside its own values only through [capabilities](#capabilities) it holds as parameters, captures or values reached from them. A [manifest grant](#manifest-grants) confers no capability; it permits the operations its authority covers.
- No item holds mutable state: a [const item](02-items.md#constants) is immutable and Inert, so it holds no capability, and every other value an item holds, such as a registry entry, is const data. The one value reachable without being passed is the [kernel anchor](24-kernel.md#anchor), reached only from entry contexts.
- Every std operation that performs I/O, reads a clock, draws randomness or allocates reaches a capability: its receiver, an argument, or a capability its receiver stores. A device operation that reaches no capability, such as `hw::IrqShared.install`, is permitted only by an `allow device` grant, and a confined one by an `allow confined` or `allow device` grant; `hw::Mmio.map` and `hw::Ports.map` are also [trusted primitives](23-low-level.md#trusted-primitives), called inside trusted code of a module that an `allow trusted` grant names as well.
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
| `sys::Fs`, `sys::Net`, `sys::Clock`, `sys::WallClock`, `sys::Random`, `sys::Log`, `sys::Stdio`, `sys::Runner` | no | yes | yes |
| `sys::fs::File`, `sys::net::Listener` | no | no | yes |
| `sys::net::Conn`, `sys::Revoker[C]`, `sys::Minter[C, S, R]` | yes | no | yes |
| `boot::Machine`, `boot::Kernel` | yes | no | no |

[`mem::Alloc`](12-memory.md#allocators), [`task::Executor`](16-concurrency.md#executors) and [`sys::Handle`](26-components.md#wire-encoding) are the other Copy capabilities of std, and every [capability kind](#capability-kinds) is Copy and Send. [Standard library 1.0](28-stdlib.md#std-authority) lists every primitive capability.

### Capability tables

Every Copy capability other than `mem::Alloc` and [`sys::Handle`](26-components.md#wire-encoding) is a [generational handle](12-memory.md#generational-handles) naming an entry of a **capability table**; `mem::Alloc` names a cell ([budgets](12-memory.md#budgets)), and a `sys::Handle` holds another capability. A capability table is owned by its **minter**: the root grant minted with it, a `sys::Revoker`, a `sys::Minter` ([capability kinds](#capability-kinds)), or the test or example run whose [sims](22-testing.md#simulated-capabilities) it holds.

- An entry records the authority it confers and the entry it derives from, if any. An executor's entry also records a clock: the `sys::Clock` of the root grant that minted it, the virtual clock of the test or example run whose world holds it, or for an executor made by `sys::revocable` the clock of its source; the kernel executor records the kernel's clock ([anchor](24-kernel.md#anchor)). An executor's entry also records its share and serial executor and counts the time its tasks ran ([executors](16-concurrency.md#executors)). Every copy of a handle names the same entry.
- Revoking an entry advances its slot's generation, so every handle to it is stale. An operation through a handle checks at its start that the handle is live; an operation already past its check completes as if the entry were live.
- Tables are shared by every task and CPU. Checks and revocations are atomic: a check that starts after a revocation completes finds the handle stale.
- Entries are memory of the [support routines](27-program-model.md#runtime-support), charged to no cell and drawn through no allocator. An operation adding an entry when that memory is exhausted traps (`trap.exhausted`).
- A table's entries are freed when its minter ends: a `sys::Revoker` at its revocation, a `sys::Minter` when it closes or its domain is settled, a test or example run at its end, and a root grant's table when the program ends.

### Std capabilities

```nan-api
// module: sys

pub open struct Fs {
} Fs

pub fn Fs.denied() -> Fs

pub fn Fs.served[S: FsService and Capability and Copy](service: S) -> Fs fails Revoked

pub fn Fs.subdir(self, path: Str) -> Fs fails fs::FsError

pub fn Fs.read_only(self) -> Fs fails Revoked

pub fn Fs.open(self, path: Str) -> fs::File fails fs::FsError uses wait

pub fn Fs.create(self, path: Str) -> fs::File fails fs::FsError uses wait

pub fn Fs.read_all(self, path: Str, alloc: mem::Alloc) -> Untrusted[mem::Bytes] fails fs::FsError uses wait

pub fn Fs.write_all(self, path: Str, data: Slice[u8]) fails fs::FsError uses wait

pub fn Fs.remove(self, path: Str) fails fs::FsError uses wait

pub open struct Net {
} Net

pub fn Net.denied() -> Net

pub fn Net.served[S: NetService and Capability and Copy](service: S) -> Net fails Revoked

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

pub fn WallClock.denied() -> WallClock

pub fn WallClock.now_ns(self) -> u64 fails Revoked

pub fn WallClock.coarse(self, ns: u64) -> WallClock fails Revoked

pub open struct Random {
} Random

pub fn Random.served[S: RandomService and Capability and Copy](service: S) -> Random fails Revoked

pub fn Random.fill(self, mut buf: SliceMut[u8]) fails Revoked

pub fn Random.next_u64(self) -> u64 fails Revoked

pub open struct Log {
} Log

pub fn Log.served[S: LogService and Capability and Copy](service: S) -> Log fails Revoked

pub fn Log.info[T: Render](self, message: Str, value: T)

pub fn Log.warn[T: Render](self, message: Str, value: T)

pub fn Log.error[T: Render](self, message: Str, value: T)

pub open struct Stdio {
} Stdio

pub fn Stdio.served[S: StdioService and Capability and Copy](service: S) -> Stdio fails Revoked

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
- `sys::Fs.denied()`, `sys::Net.denied()` and `sys::WallClock.denied()` give a capability whose handle is stale from the start, so that each operation through it fails as through a revoked one.
- On a machine target, and in a bundle of a nanos target ([bundles](30-artefacts.md#bundles)), which runs on its kernel's runtime, the operations of a `sys::Fs` or `sys::Net` that no kind [serves](#served-capabilities) fail with their error's `Denied` once their handle is found live: no file system or network of the platform serves them.

### Served capabilities

A kernel serves `sys::Fs`, `sys::Net`, `sys::Log`, `sys::Random` and `sys::Stdio` to the code it runs, and the [runner capabilities](26-components.md#runner-capabilities) of the components it holds, through a [capability kind](#capability-kinds) of its own implementing the service interface of each:

```nan-api
// module: sys

pub closed enum LogLevel derives Copy, Eq, Render {
    Info
    Warn
    Error
} LogLevel

pub interface LogService {
    fn log(self, level: LogLevel, message: Str, value: Str) limits stack 4_096
} LogService

pub interface RandomService {
    fn fill(self, mut buf: SliceMut[u8]) limits stack 4_096
} RandomService

pub interface StdioService {
    fn read_in(self, mut buf: SliceMut[u8]) -> usize fails StdioError uses wait limits stack 4_096
    fn write_out(self, data: Slice[u8]) fails StdioError uses wait limits stack 4_096
    fn write_err(self, data: Slice[u8]) fails StdioError uses wait limits stack 4_096
} StdioService

pub interface FsService {
    fn open(self, path: Str) -> u64 fails fs::FsError uses wait limits stack 4_096
    fn create(self, path: Str) -> u64 fails fs::FsError uses wait limits stack 4_096
    fn read_all(self, path: Str, alloc: mem::Alloc) -> mem::Bytes fails fs::FsError uses wait limits stack 4_096
    fn write_all(self, path: Str, data: Slice[u8]) fails fs::FsError uses wait limits stack 4_096
    fn remove(self, path: Str) fails fs::FsError uses wait limits stack 4_096
    fn read(self, file: u64, mut buf: SliceMut[u8]) -> usize fails fs::FsError uses wait limits stack 4_096
    fn write(self, file: u64, data: Slice[u8]) -> usize fails fs::FsError uses wait limits stack 4_096
    fn byte_len(self, file: u64) -> u64 fails fs::FsError uses wait limits stack 4_096
    fn release(self, file: u64) limits stack 4_096
} FsService

pub interface NetService {
    fn listen(self, port: u16) -> u64 fails net::NetError uses wait limits stack 4_096
    fn connect(self, host: Str, port: u16) -> u64 fails net::NetError uses wait limits stack 4_096
    fn accept(self, listener: u64) -> u64 fails net::NetError uses wait limits stack 4_096
    fn read(self, conn: u64, mut buf: SliceMut[u8]) -> usize fails net::NetError uses wait limits stack 4_096
    fn write(self, conn: u64, data: Slice[u8]) -> usize fails net::NetError uses wait limits stack 4_096
    fn close(self, conn: u64) fails net::NetError uses wait limits stack 4_096
    fn reset(self, conn: u64) limits stack 4_096
    fn release(self, listener: u64) limits stack 4_096
} NetService

pub open struct Runner {
} Runner

pub fn Runner.denied() -> Runner

pub fn Runner.served[S: RunnerService and Capability and Copy](service: S) -> Runner fails Revoked

pub closed enum RunnerError derives Render {
    Unknown
    Revoked(Revoked) from
    Cancelled(task::Cancelled) from
} RunnerError

pub interface RunnerService {
    fn send_to(self, port: u64, hash: ProtocolHash, bytes: Slice[u8], handles: Slice[Handle]) fails RunnerError limits stack 4_096
    fn call(self, port: u64, hash: ProtocolHash, bytes: Slice[u8], handles: Slice[Handle], alloc: mem::Alloc) -> Encoded fails RunnerError uses wait limits stack 4_096
    fn reply(self, request: u64, bytes: Slice[u8], handles: Slice[Handle]) fails RunnerError limits stack 4_096
    fn publish(self, summary: Slice[u8]) fails RunnerError limits stack 4_096
    fn sync(self) fails RunnerError limits stack 4_096
    fn capability(self, id: u64) -> Handle fails RunnerError limits stack 4_096
    fn drop_capability(self, id: u64) fails RunnerError limits stack 4_096
} RunnerService

pub fn Runner.send_to(self, port: u64, hash: ProtocolHash, bytes: Slice[u8], handles: Slice[Handle]) fails RunnerError

pub fn Runner.call(self, port: u64, hash: ProtocolHash, bytes: Slice[u8], handles: Slice[Handle], alloc: mem::Alloc) -> Encoded fails RunnerError uses wait

pub fn Runner.reply(self, request: u64, bytes: Slice[u8], handles: Slice[Handle]) fails RunnerError

pub fn Runner.publish(self, summary: Slice[u8]) fails RunnerError

pub fn Runner.sync(self) fails RunnerError

pub fn Runner.capability(self, id: u64) -> Handle fails RunnerError

pub fn Runner.drop_capability(self, id: u64) fails RunnerError
```

- `sys::Runner.denied()` gives a runner whose handle is stale from the start.
- `C.served(service)` of `Fs`, `Net`, `Log`, `Random`, `Stdio` or `Runner` returns a capability whose entry derives from the entry of `service` ([capability tables](#capability-tables)), so revoking `service`, or closing its minter, revokes it; it fails with `sys::Revoked.Stale` when `service` is stale.
- The operations of a served capability call `service`'s methods on the calling task, on every target: `Log.info`, `warn` and `error` call `log` with their level, their message and the rendering of their value within the budget of 1,024 bytes; `Random.fill` calls `fill`; `Stdio.read_in`, `write_out` and `write_err` call the method of their name; `Fs.open`, `create`, `read_all`, `write_all` and `remove` call the method of their name with their path joined, by `/`, to the paths of the `subdir` calls the capability derives through; and `Net.listen` and `connect` call the method of their name. Each operation fails as its method fails, a count past `buf`'s or `data`'s length counting as that length and a write's count of 0 for a `data` that is not empty as 1. The checks of the capability's own restrictions come first: an operation that the form of its path, `read_only` or `only_host` refuses calls no method.
- A file, listener or connection opened through a served capability holds the number its method returned, and its operations call the method of their name with that number, except `sys::net::Conn.abandon`, which calls `reset`. Releasing a file or listener calls `release`, and [settlement](11-errors-domains.md#settlement) releasing a connection calls `reset`. `release`, `reset` and `close` are called whether or not the resource's entry is revoked.
- The operations of a served `Runner`, `runner.send_to(port, hash, bytes, handles)`, `call`, `reply`, `publish`, `sync`, `capability` and `drop_capability`, call `service`'s method of their name with their arguments on the calling task, and fail as it fails. [Component contexts and ports](26-components.md#runner-capabilities) act through them, and a kernel relays them for an instance in an isolated space ([component instances](#component-instances)). `RunnerError.Unknown` is how a kind refuses a port, request or capability it does not know.
- Their digests are those of the served capability's operations, whatever the methods do.

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

impl Runner: Capability

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
| `sys::Runner` | the operations of [component contexts](26-components.md#runner-capabilities) acting through it fail with `sys::RunnerError.Revoked` holding `sys::Revoked.Stale`, and `sys::Port.call` fails as closed |
| a [capability kind](#capability-kinds) | `sys::reach` returns `None` and `sys::derive` fails with `sys::Revoked.Stale` |
| `mem::Alloc` | draws through it are exhaustion ([budgets](12-memory.md#budgets)) |
| `task::Executor` | `task::scope`, `task::race`, the [domain runs](11-errors-domains.md#domains), `host::Runner.run` and the [stepper](26-components.md#steppers) constructors `start` and `resume` start no task and fail at once with the cancellation of their failure type; a task that `task::Scope.spawn` or `spawn_sized` would start fails at once with it without running its closure. No cancelling construct is the origin of these cancellations ([origin and absorption](11-errors-domains.md#origin-and-absorption)). `coarse` fails with `sys::Revoked.Stale`, and `split` and `serial` return an account of the stale executor. |

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
- An affine resource reaching its [end of scope](07-ownership.md#ownership) is released by the runtime without waiting: its platform object is closed without running user code, or the kind serving it releases it ([served capabilities](#served-capabilities)).
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
| `x86_64-nanos`, `aarch64-nanos` | `nanos::World` |

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

The root grant of the nanos targets holds the capabilities of a host program's but the wall clock, with the command-line arguments and environment replaced by a table of further capabilities:

```nan-api
// module: nanos

pub closed struct World {
    pub alloc: mem::Alloc
    pub exec: task::Executor
    pub clock: sys::Clock
    pub random: sys::Random
    pub log: sys::Log
    pub stdio: sys::Stdio
    pub fs: sys::Fs
    pub net: sys::Net
    pub caps: Caps
} World

pub open struct Caps derives Send {
} Caps

pub open struct Cap[C: sys::Capability and Copy] {
} Cap

pub closed enum CapError derives Copy, Eq, Render {
    Missing
} CapError

pub fn Caps.empty(alloc: mem::Alloc) -> Caps

pub fn Cap.put(mut caps: Caps, key: Str, capability: C)

pub fn Cap.get(caps: Caps, key: Str) -> C fails CapError
```

- A `nanos::Caps` holds capabilities of any Copy capability type, each under a text key; `Caps.empty(alloc)` draws its entries through `alloc`.
- `mut nanos::Cap[C].put(caps, key, capability)` holds `capability` under `key`, in place of what `caps` held under it. `nanos::Cap[C].get(caps, key)` gives the capability held under `key` when it is a `C`, and fails with `CapError.Missing` otherwise. Every closure lowered by one toolchain tells capability types apart alike, so a table built in one closure is read by another.

```nan
pub fn storage_clock(caps: nanos::Caps) -> sys::Clock fails nanos::CapError {
    return try nanos::Cap[sys::Clock].get(caps, key: "storage")
} storage_clock

test held_clock_is_found for storage_clock {
    let world = testing::world()
    var caps = nanos::Caps.empty(world.alloc)
    mut nanos::Cap[sys::Clock].put(mut caps, key: "storage", capability: world.clock)
    let found = attempt storage_clock(caps)
    expect found.is_ok()
} held_clock_is_found
```

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

### Jobs

A bare kernel runs the program of a nanos bundle it loaded ([loading](24-kernel.md#loading)) as a **job**:

```nan-api
// module: nanos

pub closed enum Ended {
    Returned(mem::Bytes)
    Failed(text::String)
    Cancelled
    Abandoned
    Trapped(sys::Trap)
    Undecodable
} Ended

pub closed enum RunError derives Render {
    NoJob
    Cancelled(task::Cancelled) from
} RunError

pub closed struct HostInput derives Encode {
    pub args: mem::List[mem::Bytes]
    pub environment: mem::Map[mem::Bytes, mem::Bytes]
} HostInput

pub fn run(load: sys::load::Load, take world: World, take input: mem::Bytes, domain: sys::Abandoner, exec: task::Executor, alloc: mem::Alloc) -> Ended fails RunError uses wait
```

- `nanos::run(load, world, input, domain, exec, alloc)` runs the job of `load` as the first task of a [domain run](11-errors-domains.md#domain-run) of the abandoner `domain` on `exec` and `alloc`, whose stack holds what the `limits stack N` of the program's entry function states, or [grows](17-suspension-io.md#grown-stacks) from what `job.stack()` gives when it states none, and returns how the job ended: `Returned` with the `mem::wire` encoding of a typed entry's result, or of a host-compatible entry's `u8` exit status, 0 for an entry returning nothing; `Failed` with the rendering of the entry's failure within a budget of 1,024 bytes, or `Cancelled` when that failure is a [cancellation](11-errors-domains.md#cancellation), such as one of a `task::within` deadline whose region holds the call; `Abandoned` when `domain.trigger()` abandoned the domain; `Trapped` with the trap that abandoned the domain otherwise; `Undecodable` when `input` is not the `mem::wire` encoding of a typed entry's input type, or of a `HostInput` for a host-compatible entry. It fails with `RunError.NoJob` when `load` holds no job, as the load of a library does.
- `load` is borrowed for the call, which returns once the job's domain has ended and, abandoned, been settled, so the load's placement is never reclaimed under the job ([loading](24-kernel.md#loading)).
- The job owns `world` and `input`. A typed entry receives `world`. A host-compatible entry receives a `host::World` holding `world`'s capabilities, the `sys::WallClock` that `world.caps` holds under the key `wall`, or `sys::WallClock.denied()` when it holds none, and the arguments and environment of the `HostInput`.

A kernel runs the job of a bundle it verified for isolated spaces ([isolated spaces](24-kernel.md#isolated-spaces)) in a space of its own, in user mode:

```nan-api
// module: nanos

pub closed struct Services derives Copy {
    pub clock: sys::Clock
    pub log: sys::Log
    pub stdio: sys::Stdio
    pub random: sys::Random
} Services

pub closed enum IsolatedError derives Render {
    NoJob
    NoChannel
    Unplaced(sys::load::LoadError) from
    Cancelled(task::Cancelled) from
} IsolatedError

pub open linear struct Link {
} Link

pub closed enum LinkError derives Render {
    NoChannel
    Cancelled(task::Cancelled) from
} LinkError

pub fn Link.of(take bell: hw::PhysFrame[4_096], alloc: mem::Alloc) -> Link

pub fn Link.open(mut self, key: Str, take inbound: hw::PhysFrame[4_096], take outbound: hw::PhysFrame[4_096]) -> usize

pub fn Link.ring(self, bits: u64)

pub fn Link.send(self, channel: usize, data: Slice[u8]) -> usize fails LinkError

pub fn Link.receive(self, channel: usize, alloc: mem::Alloc) -> Untrusted[mem::Bytes] fails LinkError uses wait

pub fn Link.release(take self, mut frames: mem::List[hw::PhysFrame[4_096]])

pub fn run_isolated(cpu: hw::Ring0, job: sys::load::Isolated, services: Services, link: Link, take input: mem::Bytes, mut frames: mem::List[hw::PhysFrame[4_096]], mut extended: Option[hw::ExtendedState], alloc: mem::Alloc) -> Ended fails IsolatedError uses wait
```

- `nanos::run_isolated(cpu, job, services, link, input, mut frames, mut extended, alloc)` makes a space of `job` from `frames`, shares `link`'s frames with it, copies `input` into it and runs the space's thread on `cpu` with the extended state `extended` holds, serving its system calls, until the job ends; it then gives the space's frames back to `frames` and returns how the job ended, as `nanos::run` does. It fails with `IsolatedError.NoJob` when the bundle holds no job and with `Unplaced` when the space cannot be made.
- A **link** is the [user frames](24-kernel.md#user-frames) a kernel shares with the spaces it runs with it, one after another: a doorbell, and channels. `nanos::Link.of(take bell, alloc)` makes a link of no channel whose doorbell is `bell`, and `link.open(key, take inbound, take outbound)` adds a channel under `key`, the space reading what the kernel writes in `inbound` and writing what the kernel reads in `outbound`, and gives its index; each frame is cleared. `link.ring(bits)` sets `bits` in the doorbell's first word and wakes the space if its runtime waits. A channel frame is a ring: the count of bytes its writer has written, the count its reader has read, each a word read with acquire and written with release ordering, then its bytes from offset 64. `link.send(channel, data)` writes as many bytes of `data` as the channel's inbound ring has room for and rings the doorbell's bit of the channel, the last bit for a channel past 63, giving their count, and `link.receive(channel, alloc)` waits until the space has written bytes the kernel has not read in the channel's outbound ring, then gives them, drawn through `alloc`, and rings the channel's bit; both fail with `LinkError.NoChannel` for an index that names no channel, and they read the space's counts as untrusted, a ring whose counts no reader and writer could leave having no room and no bytes. A task waiting in `receive` while another waits there on the same channel traps (`trap.duplicate-access`). `take link.release(mut frames)` gives the link's frames back to `frames`.
- The space holds the capabilities it reaches through `invoke` in a **table of handles**, a handle being the index of its entry: the log, standard streams and random source of `services`, then each channel of `link`, then the runner capability of an instance's space ([component instances](#component-instances)), then each capability that crosses into the space, up to 256 entries. A call naming a handle the table does not hold fails as one naming a stale capability.
- The space's runtime holds each channel of `link` in the job's `world.caps` as a `sys::Stdio` under the channel's key, whose `read_in` reads from its inbound ring, waiting on the doorbell while it is empty, whose `write_out` writes to its outbound ring, waiting on the doorbell while it is full, and whose `write_err` fails with `StdioError.Closed`; each read or write that moves bytes tells the kernel through the channel's handle, which wakes a task waiting in `link.receive`. A runtime waiting on the doorbell looks at its ring again at least every second.
- The space grows within `frames`: it starts mapping none of its boot area, and `run_isolated` serves each draw and release of its runtime through `space.draw` and `space.give_back` on `frames`, and draws the page of the boot area its thread faults on when the space does not map it. A draw for which `frames` holds too few frames fails, and the allocation needing it with it, as on an exhausted heap; a fault for which `frames` holds no frame ends the job `Trapped` with a trap of `trap.exhausted`.
- The job runs as the first task of a domain run of the space's runtime, on a world of that runtime's allocator, executor and clock, whose log, standard streams and random source reach `services` through system calls, whose file system and network are denied, and whose table holds `link`'s channels alone. A host-compatible entry so receives a denied wall clock.
- The kernel reads every register and byte the space gives as untrusted: the arguments of a log line or of a write reach `services` only when they are the `mem::wire` encodings the runtime writes, and a call naming bytes the space does not map fails. A trap of the job's domain ends it `Trapped` with a trap of the same kind, another exception of its thread with a trap of `trap.exception`, and a space waiting with no timer armed, which nothing can wake, with a trap of `trap.abandoned`, each located at the call of `run_isolated`.

### Component instances

A bare kernel holds instances of the components that a nanos bundle it loaded exports ([component entries](27-program-model.md#entry)), each in a domain of its own, as the holder of a [stepper](26-components.md#steppers) holds its component:

```nan-api
// module: nanos

pub interface Environment {
    fn build(take world: World) -> Self fails CapError
} Environment

pub open linear struct Instance {
} Instance

pub open struct Inbox {
} Inbox

pub closed enum InstanceError derives Render {
    Ended
    Cancelled(task::Cancelled) from
} InstanceError

pub fn hold[T, E](load: sys::load::Load, component: Str, take world: World, take checkpoint: Option[mem::Bytes], runner: sys::Runner, exec: task::Executor, alloc: mem::Alloc, body: fn(take instance: Instance) -> T fails E uses wait limits stack 65_536) -> Option[T] fails E uses wait

pub fn Instance.domain(self) -> mem::Shared[sys::Abandoner]

pub fn Instance.inbox(self) -> Inbox

pub fn Instance.step(mut self) -> bool fails InstanceError uses wait

pub fn Instance.incoming(self) fails InstanceError uses wait

pub fn Instance.ready(self) -> bool fails InstanceError uses wait

pub fn Instance.start_keyed(mut self) -> usize fails InstanceError uses wait

pub fn Instance.idle(self) -> bool fails InstanceError uses wait

pub fn Instance.checkpoint(self) -> mem::Bytes fails InstanceError uses wait

pub fn Instance.resumed(mut self, reason: sys::ResumeReason) fails InstanceError uses wait

pub fn Instance.pressure(mut self, level: u8) fails InstanceError uses wait

pub fn Instance.health(mut self) -> sys::HealthReply fails InstanceError uses wait

pub fn Instance.suspend(mut self) fails InstanceError uses wait

pub fn Instance.wake(mut self) fails InstanceError uses wait

pub fn Instance.end(take self) -> Ended fails task::Cancelled uses wait

pub fn Instance.abandon(take self)

pub fn Inbox.call(self, hash: sys::ProtocolHash, take request: sys::Encoded, alloc: mem::Alloc) -> sys::Encoded fails sys::CallError[Never] uses wait

pub fn hold_isolated[T, E](cpu: hw::Ring0, job: sys::load::Isolated, component: Str, services: Services, link: Link, take checkpoint: Option[mem::Bytes], runner: sys::Runner, mut frames: mem::List[hw::PhysFrame[4_096]], mut extended: Option[hw::ExtendedState], exec: task::Executor, alloc: mem::Alloc, body: fn(take instance: Instance) -> T fails E uses wait limits stack 65_536) -> Option[T] fails E uses wait
```

- The Env of a component entry implements `nanos::Environment`: `Env.build(world)` makes it from the world its kernel grants an instance, failing with `CapError.Missing` when `world.caps` lacks a capability the Env needs.
- `nanos::hold(load, component, world, checkpoint, runner, exec, alloc, body: b)` runs the **instance task** of a new instance of the component entry `component` of `load` as the first task of a [domain run](11-errors-domains.md#domain-run) of a new abandoner on `exec` and `alloc`, whose stack holds what `job.stack()` gives for that entry and [grows](17-suspension-io.md#grown-stacks) beyond it, calls `b` with the instance on the calling task, and returns `Some` of what `b` returns once the instance's domain has ended. It returns `None`, running nothing, when `load` has no component entry of that name. The load is borrowed for the call, so its placement is never reclaimed under the instance.
- `instance.domain()` shares the abandoner running the instance's domain, through which another task abandons an instance whose handler never returns.
- The instance task owns `world` and `checkpoint`. It decodes `checkpoint`, builds the Env from `world`, then calls `sys::Stepper.start`, or `sys::Stepper.resume` with the decoded state when `checkpoint` is `Some`, attaches the runner capability `runner` to the stepper ([runner capabilities](26-components.md#runner-capabilities)), and holds the stepper until the instance ends. The component's handlers thus call the kind serving `runner` on the instance task, its calls through ports and its replies crossing as wire encodings.
- Each operation of an instance named after a `sys::Stepper` operation has the instance task perform that operation, `checkpoint` giving the `mem::wire` encoding of the state, so a handler runs in the instance's domain and a trap in it abandons that domain alone. Each waits for the instance task to carry it out, and fails with `InstanceError.Ended` once the instance's domain has ended. A cancelled `incoming` or `ready` also ends the wait of the instance task.
- `instance.inbox()` gives an inbox of the instance. `inbox.call(hash, request, alloc)` has a task of the instance's domain serve the encoded call `request` as `sys::Wire[P].serve` does ([wire encoding](26-components.md#wire-encoding)) through the stepper's outbox, knowing no peer schemas, P being the protocol the component serves, and gives its reply drawn through `alloc`, once a step of the instance's holder has handled the call. It fails as `serve` does, and as closed once the instance has ended.
- `take instance.end()` has the instance task stop serving its inboxes and end the stepper, and returns once the instance's domain has ended, with how the instance ended: `Returned` with the `mem::wire` encoding of its last state; `Failed` with the rendering of the failure building its Env; `Cancelled` when its stepper's start or end met a cancellation; `Abandoned` when the abandoner `instance.domain()` shares abandoned the domain; `Trapped` with the trap that abandoned the domain otherwise; `Undecodable` when `checkpoint` is not the `mem::wire` encoding of a state of the component. It is no cancellation point. `take instance.abandon()` abandons the instance's domain without waiting.
- Only std types and `mem::wire` encodings pass between a kernel and an instance task, which runs the code of the bundle: the state, the Env and the messages of the component never leave the bundle's code.
- `nanos::hold_isolated(cpu, job, component, services, link, checkpoint, runner, mut frames, mut extended, exec, alloc, body: b)` holds an instance of the component entry `component` of a bundle verified for isolated spaces as `hold` does, its instance task running in a space of its own that it makes from `frames` and runs on `cpu` with the extended state `extended` holds, as `run_isolated` runs a job ([jobs](#jobs)). The instance's world is a job's in a space, its table holding the channels of `link` but the first. It returns `Some` of what `b` returns once the space has ended and its frames are back in `frames`, and `None`, running nothing, when the bundle has no component entry of that name. The instance's handlers, keyed tasks and checkpoints thus run in the space, and a trap in a handler ends the instance and its space alone.
- The first channel of `link` carries the instance: the kernel sends down it each request of the holder, the end of each wait the holder no longer waits for, the protocol hash, bytes and handles of each call of an inbox, the instance's release by `end` and the reply to each call through a port, and the space's runtime sends up it each answer and each call the instance makes through a port, each as a `mem::wire` encoding after its length as four little-endian bytes. The kernel reads what the space sends as untrusted, and hears no more of an instance that sends a message it does not decode or longer than 1 MiB.
- The instance acts through `runner`. The space's runtime attaches to its stepper a runner capability that forwards each operation to `runner`, with the arguments it is given: a call through a port up the first channel of `link`, so that the space runs on while the call waits, and each other operation as a system call the kernel serves at once. Their handles cross as a call's and a reply's do, an operation failing with `RunnerError.Unknown` when one cannot, and the capability that `capability` gives for a number crosses into the table once until `drop_capability` drops that number.
- Handles cross the space through its table. The kernel adds the capability each handle of a call holds to the table, and the call reaches the instance with a handle of the space's runtime in its place, holding a capability of the same type that reaches that entry through `invoke`; the runtime holds one capability per entry. A handle of a reply names the entry the runtime holds its capability for, and reaches the holder as a handle of that entry's capability. A call carrying a handle of a type other than `sys::Log`, `sys::Stdio` and `sys::Random`, or one the full table cannot take, and a call whose reply carries a handle of a capability the runtime holds for no entry, fails with `CallError.Malformed(core::DecodeError.Invalid)`.
- An instance in a space ends as one `hold` holds does, `Abandoned` from its space's next exit once the abandoner `instance.domain()` shares is triggered, `Trapped` as a job in a space does when its thread takes another exception, and `Failed` with the rendering of `IsolatedError.NoChannel` when `link` has no channel and of `IsolatedError.Unplaced` when no space can be made.

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

A **manifest grant** is an `allow <authority> in <names>` [directive](03-packages-modules.md#manifest), granting an authority no value carries. Each name a `device`, `confined`, `priv`, `trusted` or `validate` grant lists is a module of the package or an alias a `dep` directive of the manifest declares, and each name a `capability` grant lists a module of the package; [contributors](25-registries.md#contributors) fixes the names of a `contribute` grant. In a `deny a -> b` directive, `a` is a module of the package and `b` a module of the package, an alias a `dep` directive declares or a std alias. Any other name is an error (`authority.unknown-name`), whose candidates are ranked as for `name.unknown` ([unknown names](03-packages-modules.md#unknown-names)).

A module performs an authority **directly** when one of its items, under a declared target, contains an operation of this table:

| Authority | Operations |
| --- | --- |
| `device` | the device operations of [register windows](23-low-level.md#register-windows), [I/O domains](23-low-level.md#io-domains), [locks](24-kernel.md#locks) and [PCI functions](24-kernel.md#pci-functions), and every confined operation |
| `confined` | the confined operations of [register windows](23-low-level.md#register-windows), [DMA](23-low-level.md#dma), [I/O domains](23-low-level.md#io-domains), [locks](24-kernel.md#locks) and [PCI functions](24-kernel.md#pci-functions) |
| `priv` | calls of [privileged intrinsics](23-low-level.md#privileged-intrinsics) and declarations of landing fns |
| `trusted` | [trusted code](23-low-level.md#trusted-code), `assume` and declarations of [foreign fns](23-low-level.md#foreign-functions) |
| `validate` | the operations that [validate grants](15-untrusted.md#validate-grants) list |
| `capability` | declarations of [capability kinds](#capability-kinds) |

- A call of a function whose body performs an authority is not itself an operation of that authority.
- A module performing `device`, `confined`, `priv`, `trusted`, `validate` or `capability` directly that no grant of that authority names is an error (`authority.ungranted`), reported on the line of each such operation, a `device` grant also naming the modules that perform `confined`. Its exact fix adds the module to the authority's `allow` directive, writing the directive when the manifest has none.
- A package is **granted** `device`, `confined`, `priv`, `trusted` or `validate` when its manifest grants it to any name. A dependency granted one of these whose alias the dependent's manifest does not list for it, under `device` or `confined` for `confined`, is an error (`authority.dependency-ungranted`), reported on the dependency's `dep` directive, with an exact fix adding the alias as for `authority.ungranted`. A std package needs no grant from its dependents.
- `allow contribute R in <names>` lists the aliases of packages whose contributions to the registry `R` the package admits, as contributors defines.
- A name a `device`, `confined`, `priv`, `trusted`, `validate` or `capability` grant lists is incompleteness (`unused.grant`), reported on the name, when it is a module performing the authority directly under no declared target or the alias of a dependency not granted the authority. A module performing `confined` alone uses a `device` grant naming it when no `confined` grant names it, and the alias of a dependency granted `confined` alone uses one. Contributors states the rule for `contribute`.

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

The confined operations that a `confined` grant permits reach, through a device, only the memory its [I/O domain](23-low-level.md#io-domains) maps, so a confined driver programming its device wrongly harms its device and its own buffers alone. A manifest granting `confined` gives its `package` item a ledger entry that is no trust escape.

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
