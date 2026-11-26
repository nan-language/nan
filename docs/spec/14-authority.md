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

pub fn Fs.create_dir(self, path: Str) fails fs::FsError uses wait

pub fn Fs.rename(self, from: Str, to: Str) fails fs::FsError uses wait

pub fn Fs.kind(self, path: Str) -> fs::Kind fails fs::FsError uses wait

pub fn Fs.list(self, path: Str, alloc: mem::Alloc) -> mem::List[fs::Entry] fails fs::FsError uses wait

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

pub fn Clock.served[S: ClockService and Capability and Copy](service: S) -> Clock fails Revoked

pub open struct WallClock {
} WallClock

pub fn WallClock.denied() -> WallClock

pub fn WallClock.served[S: WallClockService and Capability and Copy](service: S) -> WallClock fails Revoked

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

pub open enum StdioError derives Render, Send, Inert {
    Closed
    Revoked(Revoked) from
    Cancelled(task::Cancelled) from
} StdioError
```

```nan-api
// module: sys::fs

pub open struct File {
} File

pub closed enum Kind derives Copy, Eq, Render {
    File
    Directory
    Other
} Kind

pub closed struct Entry {
    pub name: text::String
    pub kind: Kind
} Entry

pub fn File.read(mut self, mut buf: Untrusted[SliceMut[u8]]) -> usize fails FsError uses wait

pub fn File.write(mut self, data: Slice[u8]) -> usize fails FsError uses wait

pub fn File.byte_len(self) -> u64 fails FsError uses wait

pub open enum FsError derives Render, Send, Inert {
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

pub open enum NetError derives Render, Send, Inert {
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
- `open` opens a file for reading and `create` creates or truncates one for writing; the other direction fails with `sys::fs::FsError.Denied`. `read_all` returns a file's contents, drawn through `alloc`, and traps when `alloc` is exhausted (`trap.exhausted`). `write_all` replaces a file's contents, creating it when missing, `remove` deletes a file or an empty directory and `byte_len` returns a file's length in bytes. `create_dir` creates a directory, doing nothing when one is there, and `rename` moves the file or directory `from` names to `to`, replacing a file there. `create`, `write_all`, `create_dir` and `rename` create the missing directories of their path, of `to` for `rename`. `kind` gives what the path names: a file, a directory or another entry. `list` gives the entries of a directory, drawn through `alloc` and trapping when it is exhausted, in byte order of name, each with its name and kind, `.` and `..` excluded; a name that is not UTF-8 fails it with `sys::fs::FsError.Denied`. The empty path names the tree's root for `kind` and `list`, and fails with `sys::fs::FsError.InvalidPath` for every other operation. A missing file fails with `sys::fs::FsError.NotFound`, a full device with `sys::fs::FsError.NoSpace`, and a path naming a directory where a file is expected, a file where a directory is expected, a directory `remove` finds not empty or a refusal by the platform with `sys::fs::FsError.Denied`.
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
    fn fill(self, mut buf: SliceMut[u8]) fails Revoked limits stack 4_096
} RandomService

pub interface ClockService {
    fn elapsed_ns(self) -> u64 fails Revoked limits stack 1_024
    fn sleep_ns(self, ns: u64) fails task::Cancelled uses wait limits stack 1_024
} ClockService

pub interface WallClockService {
    fn now_ns(self) -> u64 limits stack 1_024
} WallClockService

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
    fn create_dir(self, path: Str) fails fs::FsError uses wait limits stack 4_096
    fn rename(self, from: Str, to: Str) fails fs::FsError uses wait limits stack 4_096
    fn kind(self, path: Str) -> fs::Kind fails fs::FsError uses wait limits stack 4_096
    fn list(self, path: Str, alloc: mem::Alloc) -> mem::List[fs::Entry] fails fs::FsError uses wait limits stack 4_096
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
    fn resource(self, id: u64) -> Held fails RunnerError uses wait limits stack 8_192
    fn take_resource(self, id: u64) -> Held fails RunnerError uses wait limits stack 8_192
    fn put_resource(self, id: u64, take resource: Held) fails RunnerError uses wait limits stack 8_192
} RunnerService

pub fn Runner.send_to(self, port: u64, hash: ProtocolHash, bytes: Slice[u8], handles: Slice[Handle]) fails RunnerError

pub fn Runner.call(self, port: u64, hash: ProtocolHash, bytes: Slice[u8], handles: Slice[Handle], alloc: mem::Alloc) -> Encoded fails RunnerError uses wait

pub fn Runner.reply(self, request: u64, bytes: Slice[u8], handles: Slice[Handle]) fails RunnerError

pub fn Runner.publish(self, summary: Slice[u8]) fails RunnerError

pub fn Runner.sync(self) fails RunnerError

pub fn Runner.capability(self, id: u64) -> Handle fails RunnerError

pub fn Runner.drop_capability(self, id: u64) fails RunnerError

pub fn Runner.resource(self, id: u64) -> Held fails RunnerError uses wait

pub fn Runner.take_resource(self, id: u64) -> Held fails RunnerError uses wait

pub fn Runner.put_resource(self, id: u64, take resource: Held) fails RunnerError uses wait
```

- `sys::Runner.denied()` gives a runner whose handle is stale from the start.
- `C.served(service)` of `Fs`, `Net`, `Clock`, `WallClock`, `Log`, `Random`, `Stdio` or `Runner` returns a capability whose entry derives from the entry of `service` ([capability tables](#capability-tables)), so revoking `service`, or closing its minter, revokes it; it fails with `sys::Revoked.Stale` when `service` is stale.
- The operations of a served capability call `service`'s methods on the calling task, on every target: `Log.info`, `warn` and `error` call `log` with their level, their message and the rendering of their value within the budget of 1,024 bytes; `Random.fill` calls `fill`; `Clock.elapsed_ns` calls `elapsed_ns`, its reading rounded down to a multiple of the clock's resolution, and `elapsed_ms` that reading in whole milliseconds; `Clock.sleep` and `sleep_ns` call `sleep_ns` with their duration in nanoseconds; `WallClock.now_ns` calls `now_ns`, its reading rounded down likewise; `Stdio.read_in`, `write_out` and `write_err` call the method of their name; `Fs.open`, `create`, `read_all`, `write_all`, `remove`, `create_dir`, `kind` and `list` call the method of their name with their path, and `rename` with each of its paths, joined, by `/` when the path is not empty, to the paths of the `subdir` calls the capability derives through; and `Net.listen` and `connect` call the method of their name. Each operation fails as its method fails, a count past `buf`'s or `data`'s length counting as that length and a write's count of 0 for a `data` that is not empty as 1. The checks of the capability's own restrictions come first: an operation that the form of its path, `read_only` or `only_host` refuses calls no method.
- A file, listener or connection opened through a served capability holds the number its method returned, and its operations call the method of their name with that number, except `sys::net::Conn.abandon`, which calls `reset`. Releasing a file or listener calls `release`, and [settlement](11-errors-domains.md#settlement) releasing a connection calls `reset`. `release`, `reset` and `close` are called whether or not the resource's entry is revoked.
- The operations of a served `Runner`, `runner.send_to(port, hash, bytes, handles)`, `call`, `reply`, `publish`, `sync`, `capability`, `drop_capability`, `resource`, `take_resource` and `put_resource`, call `service`'s method of their name with their arguments on the calling task, and fail as it fails. [Component contexts, ports and resources](26-components.md#runner-capabilities) act through them, and a kernel relays them for an instance in an isolated space ([component instances](#component-instances)). `RunnerError.Unknown` is how a kind refuses a port, request, capability or resource it does not know.
- Each call of a method above, other than the `release` or `reset` that releasing a resource or `abandon` makes, runs on the calling task's stack when that stack has room for the method's `stack N`, and otherwise on a new [segment](17-suspension-io.md#grown-stacks) of it, of 64 KiB or of that room if more, returned once the method returns. A segment that cannot be drawn, or a call made on a stack other than a running task's, as an interrupt fn's, traps (`trap.exhausted`). A [stack use](10-effects-digests.md#stack-limits) counts such a call as a call of a support routine.
- The `Held` that `resource` or `take_resource` gives passes to the calling task's domain ([domain memory](11-errors-domains.md#domain-memory)), and `put_resource` on a stale `Runner` releases the `Held` it takes. The resource methods of `RunnerService` are bound by 8,192 bytes, as releasing a served resource counts the 4,096 bytes of its own kind's method.
- `coarse` of a served `Clock` or `WallClock` serves the same kind with the coarser resolution. A deadline that `task::within`, `within_ns` or `paced` sets on a served `Clock` passes as on the clock the platform gives, timed from the moment it was served, and `task::deadline_ns` reads deadlines on it so.
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
| `x86_64-linux`, `x86_64-windows`, `x86_64-macos`, `aarch64-linux`, `aarch64-windows`, `aarch64-macos` | `host::World` |
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
    targets x86_64-linux
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

A kernel runs the jobs of bundles it verified for isolated spaces ([isolated spaces](24-kernel.md#isolated-spaces)) in user mode, in a space that the jobs and [component instances](#component-instances) it places together share:

```nan-api
// module: nanos

pub closed struct Services derives Copy {
    pub clock: sys::Clock
    pub log: sys::Log
    pub stdio: sys::Stdio
    pub random: sys::Random
    pub fs: sys::Fs
} Services

pub closed enum IsolatedError derives Render {
    NoJob
    NoChannel
    Unplaced(sys::load::LoadError) from
    Cancelled(task::Cancelled) from
} IsolatedError

pub open linear struct Link derives Send {
} Link

pub closed enum LinkError derives Render {
    NoChannel
    Cancelled(task::Cancelled) from
} LinkError

pub fn Link.of(take bell: hw::PhysFrame[4_096], alloc: mem::Alloc) -> Link

pub fn Link.open(mut self, key: Str, take inbound: hw::PhysFrame[4_096], take outbound: hw::PhysFrame[4_096]) -> usize

pub fn Link.open_port(mut self, port: u64, to: Inbox, take inbound: hw::PhysFrame[4_096], take outbound: hw::PhysFrame[4_096]) -> usize

pub fn Link.open_crossing(mut self, key: Str, operation: u64, hash: sys::ProtocolHash, to: Inbox, take inbound: hw::PhysFrame[4_096], take outbound: hw::PhysFrame[4_096]) -> usize

pub fn Link.ring(self, bits: u64)

pub fn Link.send(self, channel: usize, data: Slice[u8]) -> usize fails LinkError

pub fn Link.receive(self, channel: usize, alloc: mem::Alloc) -> Untrusted[mem::Bytes] fails LinkError uses wait

pub fn Link.release(take self, mut frames: mem::List[hw::PhysFrame[4_096]])

pub open struct Isolation derives Send {
} Isolation

pub fn isolate[E](cpu: hw::Ring0, tenant: u64, services: Services, mut frames: mem::List[hw::PhysFrame[4_096]], mut extended: Option[hw::ExtendedState], exec: task::Executor, alloc: mem::Alloc, body: fn(space: Isolation) fails E uses wait limits stack 131_072) fails E uses wait

pub fn run_isolated(space: Isolation, job: sys::load::Isolated, link: Link, services: Services, caps: Caps, take input: mem::Bytes, mut frames: mem::List[hw::PhysFrame[4_096]], alloc: mem::Alloc) -> Ended fails IsolatedError uses wait

pub open struct SpaceCalls derives Copy {
} SpaceCalls

pub fn SpaceCalls.of_space() -> SpaceCalls

pub fn SpaceCalls.open(self, path: Str) -> u64 fails sys::fs::FsError

pub fn SpaceCalls.create(self, path: Str) -> u64 fails sys::fs::FsError

pub fn SpaceCalls.read(self, file: u64, mut buf: SliceMut[u8]) -> usize fails sys::fs::FsError

pub fn SpaceCalls.write(self, file: u64, data: Slice[u8]) -> usize fails sys::fs::FsError

pub fn SpaceCalls.byte_len(self, file: u64) -> u64 fails sys::fs::FsError

pub fn SpaceCalls.release(self, file: u64)

pub fn SpaceCalls.now_ns(self) -> u64 fails sys::Revoked

pub fn SpaceCalls.read_all(self, path: Str, alloc: mem::Alloc) -> mem::Bytes fails sys::fs::FsError

pub fn SpaceCalls.write_all(self, path: Str, data: Slice[u8]) fails sys::fs::FsError

pub fn SpaceCalls.remove(self, path: Str) fails sys::fs::FsError

pub fn SpaceCalls.create_dir(self, path: Str) fails sys::fs::FsError

pub fn SpaceCalls.rename(self, from: Str, to: Str) fails sys::fs::FsError

pub fn SpaceCalls.kind(self, path: Str) -> sys::fs::Kind fails sys::fs::FsError

pub fn SpaceCalls.list(self, path: Str, alloc: mem::Alloc) -> mem::List[sys::fs::Entry] fails sys::fs::FsError

pub fn SpaceCalls.run_test(self, image: Str, output: Str, texts: Slice[text::String], alloc: mem::Alloc) -> u8 fails sys::fs::FsError

pub fn SpaceCalls.read_in(self, mut buf: SliceMut[u8]) -> usize fails sys::StdioError

pub fn SpaceCalls.write_out(self, data: Slice[u8]) fails sys::StdioError

pub fn SpaceCalls.write_err(self, data: Slice[u8]) fails sys::StdioError
```

- `nanos::isolate(cpu, tenant, services, mut frames, mut extended, exec, alloc, body: b)` calls `b` with an **isolation**, through which `run_isolated` and `hold_isolated` place jobs and instances in one isolated space for `tenant` ([user mode](24-kernel.md#user-mode)), and returns once `b` has returned and the space runs none. A task of `exec` runs the space's thread on `cpu` with the extended state `extended` holds while the space runs a job or instance, serving its system calls, its waits and its readings of time on the clock of `services`: it makes the space from `frames` for the first job or instance placed while the space runs none, maps the bundle of each into it as `sys::load::Isolated.join` does, shares the frames of each one's link with it, and gives the space's frames back to `frames` once the last has ended, the space ending with it. The **isolation's frames** are those `frames` holds, among them, while a job or instance is placed, those it lends.
- The space's runtime keeps time by the clock of `services`. Once a reading of it fails, as every reading of a stale clock does, the runtime revokes its own clock and every clock deriving from it ([revocation](#revocation)), and passes every timer armed then; its time stands still from then on, so its tasks wait for its doorbell and its kernel's orders alone, and the isolation, finding that clock stale, answers its waits on those alone.
- `nanos::run_isolated(space, job, link, services, caps, input, mut frames, alloc)` places the job of `job` in the space of the isolation `space`, its input `input`, its world reaching `services` and holding `caps`, and returns how it ended once the space no longer maps `link`'s frames, as `nanos::run` does. The job **lends** its isolation every frame of `frames` once it is placed, and draws within them; once it has ended, the isolation gives back to `frames` as many frames as it lent from its own before the call returns. When the isolation's frames then hold fewer, its space having drawn the rest for itself, each job and instance of the space ends `Trapped` with a trap of `trap.exhausted`, as on a fault for which they hold no frame, and the isolation gives back as many as they hold once the space's frames have gone back to them. It fails with `IsolatedError.NoJob` when the bundle holds no job and with `Unplaced` when the job cannot be placed: when no space can be made, or the space has no room left for its bundle's image, the frames of its link or the frames that take. Once the call is cancelled, the space abandons the job, and the call then fails with `IsolatedError.Cancelled`.
- A **link** is the [user frames](24-kernel.md#user-frames) a kernel shares with the spaces it runs with it, one after another: a doorbell, and channels. `nanos::Link.of(take bell, alloc)` makes a link of no channel whose doorbell is `bell`, and `link.open(key, take inbound, take outbound)` adds a channel under `key`, the space reading what the kernel writes in `inbound` and writing what the kernel reads in `outbound`, and gives its index; each frame is cleared. `link.open_port(port, to, take inbound, take outbound)` adds a channel through which the instance held with the link calls its port `port` on the instance whose inbox `to` is ([bridges](#bridges)), and `link.open_crossing(key, operation, hash, to, take inbound, take outbound)` one through which the job or instance placed with the link calls that instance through the capability its world holds under `key`, each `call` of the operation `operation` of it being a call of the protocol of `hash`; each gives the channel's index. `link.ring(bits)` sets `bits` in the doorbell's first word and wakes the space if its runtime waits. A channel frame is a ring: the count of bytes its writer has written, the count its reader has read, each a word read with acquire and written with release ordering, then its bytes from offset 64. `link.send(channel, data)` writes as many bytes of `data` as the channel's inbound ring has room for and rings the doorbell's bit of the channel, the last bit for a channel past 63, giving their count, and `link.receive(channel, alloc)` waits until the space has written bytes the kernel has not read in the channel's outbound ring, then gives them, drawn through `alloc`, and rings the channel's bit; both fail with `LinkError.NoChannel` for an index that names no channel, and they read the space's counts as untrusted, a ring whose counts no reader and writer could leave having no room and no bytes. A task waiting in `receive` while another waits there on the same channel traps (`trap.duplicate-access`). `take link.release(mut frames)` gives the link's frames back to `frames`.
- The space holds the capabilities it reaches through `invoke` in a **table of handles**, each handle naming an entry: the log, standard streams, random source and file system of the `services` of `isolate`, the space's own, by the handles 0 to 3, the space's test runs by the handle 4 and its kernel's orders by the handle 5, then, as they are placed, each channel of a job's or instance's link, the runner capability of each ([component instances](#component-instances)) and the clock, log, standard streams, random source and file system of its `services`, each end of a bridge ([bridges](#bridges)), and each capability that crosses into the space, up to 256 entries at once. A capability crossing into the table for a job or instance that an earlier crossing of it added an entry for takes that entry again, the entry counting the crossings that hold it. The table takes back the entries added for a job or instance once it has ended, the entry of the end of a bridge once the end is closed, and an entry a crossing added once the kernel has released each crossing holding it; when it needs an entry while every slot holds one, it first takes back each entry a crossing added whose capability is stale. An entry lies in one of 256 slots, the handle's remainder by 256, and the next entry of a slot whose entry was taken back takes the handle 256 above, so that no handle names two entries. A call naming a handle the table does not hold, or one added for a job or instance that has ended, fails as one naming a stale capability.
- The space's runtime holds each channel of a job's `link` but those `open_port` and `open_crossing` add in the job's `world.caps` as a `sys::Stdio` under the channel's key, whose `read_in` reads from its inbound ring, waiting on the doorbell while it is empty, whose `write_out` writes to its outbound ring, waiting on the doorbell while it is full, and whose `write_err` fails with `StdioError.Closed`; each read or write that moves bytes tells the kernel through the channel's handle, which wakes a task waiting in `link.receive`. A runtime waiting on the doorbell looks at its ring again at least every second while its clock is live.
- The space grows within the isolation's frames: it starts mapping none of its boot area, and its isolation serves each draw and release of its runtime through `space.draw` and `space.give_back` on them, and draws the page of the boot area its thread faults on when the space does not map it. A draw for which they hold too few frames fails, and the allocation needing it with it, as on an exhausted heap; a fault for which they hold no frame ends each job and instance of the space `Trapped` with a trap of `trap.exhausted`. Each job and instance draws within the frames it lends, so the frames of `isolate` serve the space itself: its tables, the images of its bundles, its runtime's own records and the chunks its runtime keeps for later draws, and the frames that move out of it.
- The space's runtime starts each job and instance placed in it as its kernel orders, on a task of its own, and abandons the domain of each one whose holder abandoned it as its kernel orders too: the runtime takes each order through the handle 5, and its kernel answers its next wait as soon as it has one. Its tasks share the space's thread, so a job or instance that never waits holds up the others until it ends.
- A **test run** runs a [test image](22-testing.md#emulator-runs) alone in a space of its own: the isolation reads the image from the file system of the space's services, makes its space for `tenant`, of frames from the isolation's, through `sys::load::Isolated.of_test` ([isolated spaces](24-kernel.md#isolated-spaces)), lays out a start record of the address and the length of the texts naming the run, each NUL-terminated, then those texts, and runs the space's thread on `cpu`, with extended state of its own on a frame from the isolation's, until it ends, the job's thread running nothing meanwhile. It serves the run's draws and releases as the job's, writes what the run writes to its standard output into a file it creates at the output path the request names, and drops what it writes to its standard error. The run ends with the status its space ends with, with 124 once it has run for 10 seconds of the kernel's clock or every job and instance of the space asking for it is abandoned, and with 70 when its thread faults on a page it cannot draw or on another exception; its frames then go back to the isolation's.
- The job runs as the first task of a domain run of the space's runtime, a domain of its own, on a world whose allocator names a cell with a limit of 4,096 bytes for each frame the job lent, whose parent is the cell of that runtime's allocator, and whose executor derives from that runtime's on that allocator, so that the job's allocations through its world and the stacks of its tasks, its first task's among them, draw within the frames it lent: a draw past them is an exhaustion of the job's domain, and a first task's stack that does not fit ends the job `Trapped` with a trap of `trap.exhausted`. The world's clock, log, standard streams, random source and file system reach those of the job's `services` through system calls, its clock and random source each stale from the start when that of `services` is stale as the job starts, its network is denied, and its table holds each capability of `caps` that crosses into the space under its key ([component instances](#component-instances)), then `link`'s channels. A capability of a crossing kind reaches the job as it reaches an instance, but its `call`, unless a [bridge](#bridges) carries it, reaches the kernel through a system call of the job's runner capability that returns at once, the kernel making `Handle.call` on a task of its own, under the header of the job's task making the call when that task has one, and giving its result to the space's runtime as an order, so that the space runs on while it waits; a `call` whose message passes 1 MiB moves as an instance's does. A file a job opens is open in its isolation until the job releases it or the space ends, and each call moves at most 512 KiB of a file's bytes. A host-compatible entry so receives a denied wall clock.
- Each reading of the world's clock reads the clock of the job's `services`, failing with `sys::Revoked.Stale` once that clock is stale, while the space's runtime times the world's sleeps and deadlines, as it does those of a [served](#served-capabilities) clock: a sleep returns at once when the clock of `services` is stale as it starts, and otherwise once the runtime's clock has run its duration. A job whose clock is denied or revoked thus reads no time, while the other jobs and instances of its space and the runtime's own timers keep theirs.
- `nanos::SpaceCalls.of_space()` gives the calls of the code running in an isolated space, which reach what the space's table holds by its handles without waiting: the space's thread runs nothing else until its kernel answers. `read_all`, `write_all`, `remove`, `create_dir`, `rename`, `kind` and `list` act as the `sys::Fs` operations of their names on the file system of the space's services, and `read_in`, `write_out` and `write_err` as the `sys::Stdio` operations of their names on its standard streams. `open` and `create` open a file of that file system as those operations do and give its number, which `read`, `write` and `byte_len` act on as the operations of their names on a `sys::fs::File`, each read or write moving a prefix of its bytes and giving its length, and which `release` releases; `now_ns` reads the clock of the space's services in nanoseconds, failing with `sys::Revoked.Stale` once that clock is stale. `run_test(image, output, texts, alloc)` makes a test run of the image at the path `image` on `texts`, its standard output written to the file at `output`, and gives the status it ends with, failing as `read_all` fails for the image and `create` for the output, and with `sys::fs::FsError.Denied` for a file that is no test image. Only `read_all`, `list` and `run_test` draw memory, through `alloc`. It is a trusted primitive whose soundness condition is that the calling code runs in an isolated space.
- The kernel reads every register and byte the space gives as untrusted: the arguments of a log line or of a write reach `services` only when they are the `mem::wire` encodings the runtime writes, and a call naming bytes the space does not map fails. A trap abandoning a job's domain ends that job alone `Trapped` with a trap of the same kind, and one ending the space's runtime outside every job's domain ends each job and instance of the space so; its report is the one the runtime wrote when that is UTF-8 text of at most 4,096 bytes, and locates the call of `run_isolated` otherwise.
- An exception of the space's thread that its isolation does not serve by drawing a page traps in the space as one in a kernel task does ([interrupt functions](24-kernel.md#interrupt-functions)): the isolation resumes the thread in the space's runtime as if the faulting instruction called the trap, which abandons the domain of the task running it, a job's or one a job runs, and whose report locates that instruction through the line tables of the space's images, with the operand line `vector = v, code = c`, followed by `, address = a` for a page fault, the fields of the thread's `hw::UserFault` ([user mode](24-kernel.md#user-mode)). An exception before the runtime can trap, or while it enters the trap of the one before, ends each job and instance of the space `Trapped` with a trap of `trap.exception`, and a space waiting with no timer armed and no order to take, which nothing can wake, each with a trap of `trap.abandoned`, both located at the call of `run_isolated`.

### Component instances

A bare kernel holds instances of the components that a nanos bundle it loaded exports ([component entries](27-program-model.md#entry)), each in a domain of its own, as the holder of a [stepper](26-components.md#steppers) holds its component:

```nan-api
// module: nanos

pub interface Environment {
    fn build(take world: World) -> Self fails CapError
} Environment

pub open linear struct Instance derives Send {
} Instance

pub open struct Inbox derives Send {
} Inbox

pub closed struct Checkpoint derives Encode {
    pub schema: mem::wire::SchemaHash
    pub bytes: mem::Bytes
} Checkpoint

pub closed struct Stepped {
    pub handled: sys::Handled
    pub encoding: sys::Encoded
} Stepped

pub closed struct SentCall {
    pub operation: u16
    pub header: Option[sys::Header]
    pub delay_ns: u64
    pub encoding: sys::Encoded
} SentCall

pub closed enum InstanceError derives Render {
    Ended
    Cancelled(task::Cancelled) from
} InstanceError

pub fn hold[T, E](load: sys::load::Load, component: Str, take world: World, take checkpoint: Option[Checkpoint], runner: sys::Runner, exec: task::Executor, alloc: mem::Alloc, body: fn(take instance: Instance) -> T fails E uses wait limits stack 65_536) -> Option[T] fails E uses wait

pub fn Instance.domain(self) -> mem::Shared[sys::Abandoner]

pub fn Instance.inbox(self) -> Inbox

pub fn Instance.step(mut self) -> bool fails InstanceError uses wait

pub fn Instance.step_encoded(mut self) -> Option[Stepped] fails InstanceError uses wait

pub fn Instance.take_sent(mut self) -> Option[SentCall] fails InstanceError uses wait

pub fn Instance.held(self) -> mem::List[sys::HeldTask] fails InstanceError uses wait

pub fn Instance.start_held(mut self, key: u64) -> bool fails InstanceError uses wait

pub fn Instance.drop_held(mut self, key: u64) -> bool fails InstanceError uses wait

pub fn Instance.incoming(self) fails InstanceError uses wait

pub fn Instance.ready(self) -> bool fails InstanceError uses wait

pub fn Instance.start_keyed(mut self) -> usize fails InstanceError uses wait

pub fn Instance.idle(self) -> bool fails InstanceError uses wait

pub fn Instance.checkpoint(self) -> Checkpoint fails InstanceError uses wait

pub fn Instance.checkpoint_as(self, schema: mem::wire::SchemaHash) -> Option[Checkpoint] fails InstanceError uses wait

pub fn Instance.resumed(mut self, reason: sys::ResumeReason) fails InstanceError uses wait

pub fn Instance.pressure(mut self, level: u8) fails InstanceError uses wait

pub fn Instance.health(mut self) -> sys::HealthReply fails InstanceError uses wait

pub fn Instance.suspend(mut self) fails InstanceError uses wait

pub fn Instance.wake(mut self) fails InstanceError uses wait

pub fn Instance.end(take self) -> Ended fails task::Cancelled uses wait

pub fn Instance.abandon(take self)

pub fn Inbox.call(self, hash: sys::ProtocolHash, request: u64, take encoding: sys::Encoded, alloc: mem::Alloc) -> Option[sys::Encoded] fails sys::CallError[Never] uses wait

pub fn hold_isolated[T, E](space: Isolation, job: sys::load::Isolated, component: Str, link: Link, services: Services, caps: Caps, take checkpoint: Option[Checkpoint], runner: sys::Runner, mut frames: mem::List[hw::PhysFrame[4_096]], alloc: mem::Alloc, body: fn(take instance: Instance) -> T fails E uses wait limits stack 65_536) -> Option[T] fails E uses wait
```

- The Env of a component entry implements `nanos::Environment`: `Env.build(world)` makes it from the world its kernel grants an instance, failing with `CapError.Missing` when `world.caps` lacks a capability the Env needs.
- `nanos::hold(load, component, world, checkpoint, runner, exec, alloc, body: b)` runs the **instance task** of a new instance of the component entry `component` of `load` as the first task of a [domain run](11-errors-domains.md#domain-run) of a new abandoner on `exec` and `alloc`, whose stack holds what `job.stack()` gives for that entry and [grows](17-suspension-io.md#grown-stacks) beyond it, calls `b` with the instance on the calling task, and returns `Some` of what `b` returns once the instance's domain has ended. It returns `None`, running nothing, when `load` has no component entry of that name. The load is borrowed for the call, so its placement is never reclaimed under the instance.
- `instance.domain()` shares the abandoner running the instance's domain, through which another task abandons an instance whose handler never returns.
- A `nanos::Checkpoint` is a `mem::wire` encoding of a component's state, `bytes`, with the schema hash of the shape it encodes, `schema`.
- The instance task owns `world` and `checkpoint`. It decodes `checkpoint` as `mem::wire::Codec[S].decode_from` decodes its bytes under its schema hash ([migrations](13-text-encode.md#migrations)), S being the component's state, so that a forward migration of S converts a state of an earlier shape, builds the Env from `world`, then calls `sys::Stepper.start`, or `sys::Stepper.resume` with the decoded state when `checkpoint` is `Some`, attaches the runner capability `runner` to the stepper ([runner capabilities](26-components.md#runner-capabilities)), and holds the stepper until the instance ends. The component's handlers thus call the kind serving `runner` on the instance task, its calls through ports and its replies crossing as wire encodings.
- Each operation of an instance named after a `sys::Stepper` operation has the instance task perform that operation, `checkpoint` giving the `mem::wire` encoding of the state with the schema hash of S and `checkpoint_as(schema)` the encoding `mem::wire::Codec[S].encode_as` gives of it under `schema` with that schema hash, `None` when `encode_as` gives false, so that a holder rolls the instance back through an [inverse migration](13-text-encode.md#migrations), so a handler runs in the instance's domain and a trap in it abandons that domain alone. Each waits for the instance task to carry it out, and fails with `InstanceError.Ended` once the instance's domain has ended. A cancelled `incoming` or `ready` also ends the wait of the instance task.
- `mut instance.step_encoded()` has the instance task step the stepper as `sys::Wire[P].step_encoded` does ([wire encoding](26-components.md#wire-encoding)), P being the protocol the component serves, and gives what it handled with the encoding of the call its message makes, whoever posted or sent it, so that the holder records every input of the instance; it gives `None` when the intake is empty.
- `mut instance.take_sent()` has the instance task take the oldest message its component sent itself, as `sys::Stepper.take_sent` does, and gives its operation's number, as `sys::Wire[P].encode_any` gives it, its header, the nanoseconds left before it would enter the intake and the encoding of the call it makes; it gives `None` when no such message waits. `instance.held()` lists the held keyed tasks as `sys::Stepper.held` does, and `mut instance.start_held(k)` and `mut instance.drop_held(k)` start or supersede the held keyed task of key `k` as the stepper's operations do. A holder thus runs a handler on an input with the effects it holds, releasing or dropping each.
- `instance.inbox()` gives an inbox of the instance. `inbox.call(hash, request, encoding, alloc)` has a task of the instance's domain serve the encoded call `encoding`, numbered `request`, as `sys::Wire[P].serve` does ([wire encoding](26-components.md#wire-encoding)) through the stepper's outbox, knowing no peer schemas, P being the protocol the component serves, under the [header](26-components.md#headers) of the task calling `inbox.call` when that task has one, as `sys::run_headed` gives it, so that the call's handler runs under that header, and gives its reply drawn through `alloc` once a step of the instance's holder has handled the call, or `None` when its handler [postponed](26-components.md#runner-capabilities) it, the kernel then keeping it pending for the `reply` of `runner`. It fails as `serve` does, and as closed once the instance has ended.
- `take instance.end()` has the instance task stop serving its inboxes and end the stepper, and returns once the instance's domain has ended, with how the instance ended: `Returned` with the `mem::wire` encoding of its last state; `Failed` with the rendering of the failure building its Env; `Cancelled` when its stepper's start or end met a cancellation; `Abandoned` when the abandoner `instance.domain()` shares abandoned the domain; `Trapped` with the trap that abandoned the domain otherwise; `Undecodable` when `decode_from` fails on `checkpoint`. It is no cancellation point. `take instance.abandon()` abandons the instance's domain without waiting.
- Only std types and `mem::wire` encodings pass between a kernel and an instance task, which runs the code of the bundle: the state, the Env and the messages of the component never leave the bundle's code.
- `nanos::hold_isolated(space, job, component, link, services, caps, checkpoint, runner, mut frames, alloc, body: b)` holds an instance of the component entry `component` of a bundle verified for isolated spaces as `hold` does, its instance task running in a domain of its own in the space of the isolation `space`, placed there as `run_isolated` places a job ([jobs](#jobs)), lending the isolation the frames of `frames` as such a job does, its tasks on the isolation's executor. The instance's world is a job's in a space, reaching `services` and its table holding each capability of `caps` that crosses into the space under its key, then the channels of `link` but the first and those `open_port` and `open_crossing` add. It returns `Some` of what `b` returns once no space maps `link`'s frames, and `None`, running nothing, when the bundle has no component entry of that name. The instance's handlers, keyed tasks and checkpoints thus run in the space, and a trap in a handler ends the instance alone.
- The first channel of `link` carries the instance: the kernel sends down it each request of the holder, the end of each wait the holder no longer waits for, the protocol hash, number, header, bytes and handles of each call of an inbox, the instance's release by `end` and the reply to each call through a port and to each `call` of a crossing kind's capability, and the space's runtime sends up it each answer, each call the instance makes through a port and each such `call`, with the header of the task making it, each as a `mem::wire` encoding after its length as four little-endian bytes. A message whose encoding is longer than 1 MiB **moves** instead: the kernel unmaps the pages holding the encoding from the space, as the writing side's kernel of a [bridge](#bridges) does, the runtime sends up the handle of the instance's runner capability and the token they moved under, and the kernel reads the message from those frames, giving them back to its isolation's frames, as it gives back those it has not read once the instance has ended; a message cannot move once the isolation takes no more frames. The kernel reads what the space sends as untrusted: a message it does not decode or longer than 1 MiB, and a token naming nothing that moved through the instance's runner capability, breach the channel, and the kernel hears no more of the instance and ends it.
- The instance acts through `runner`. The space's runtime attaches to its stepper a runner capability that forwards each operation to `runner`, with the arguments it is given: a call through a port up the first channel of `link`, so that the space runs on while the call waits, failing with `RunnerError.Unknown` when its message there passes 1 MiB and cannot move, and each other operation as a system call the kernel serves at once. The kernel makes each call and `send_to` through a port under the header of the space's task making it when that task has one, as `sys::run_headed` gives it. Their handles cross as a call's and a reply's do, an operation failing with `RunnerError.Unknown` when one cannot, and the capability that `capability` gives for a number crosses into the table once until `drop_capability` drops that number, which releases that crossing. A space's runtime holds no file or listener of its kernel's, so `resource`, `take_resource` and `put_resource` fail with `RunnerError.Unknown` there without reaching `runner`.
- A capability **crosses** into a space when it is a `sys::Log`, a `sys::Stdio`, a `sys::Random` or a live capability of a [crossing kind](#crossing-kinds). Handles cross the space through its table. The kernel adds the capability each handle of a call holds to the table, and the call reaches the instance with a handle of the space's runtime in its place, holding a capability of the same type that reaches that entry through `invoke`; the runtime holds one capability per entry for each job or instance, and revokes it once a later handle of the entry's slot crosses for that job or instance, the table having taken that entry back, while a handle earlier than one of its slot that crossed for the job or instance reaches it as a stale capability. A handle of a reply names the entry the runtime holds its capability for, and reaches the holder as a handle of that entry's capability. A call carrying a handle of a capability that does not cross, or one the full table cannot take, and a call whose reply carries a handle of a capability the runtime holds for no entry or whose answer up the first channel of `link` passes 1 MiB and cannot move, fails with `CallError.Malformed(core::DecodeError.Invalid)`. The handles of the encodings `step_encoded` and `take_sent` give cross as a reply's do, and an encoding carrying a handle that cannot cross, or whose answer passes 1 MiB and cannot move, reaches the holder without bytes or handles.
- A capability of a crossing kind reaches the instance as a capability of the same type that a kind of the space's runtime serves: its `invoke` is a system call the kernel serves at once by `Handle.invoke` on its own capability, failing with `CrossError.Refused` when the `mem::wire` encoding of the result's bytes and of what crosses for its handles is longer than 4,096 bytes, and its `call`, unless a [bridge](#bridges) carries it, goes up the first channel of `link`, the kernel making `Handle.call` on a task of its own, under the header of the space's task making the call when that task has one, and sending its result down, so that the space runs on while it waits, and failing with `CrossError.Refused` when its message there passes 1 MiB and cannot move. The handles of each operation's arguments cross as a call's through a port do, and those of its result as a reply's do, the operation failing with `CrossError.Refused` when one cannot. Through it `sys::reach` gives `None` and `sys::derive` fails, and once the kernel's capability is stale its operations fail with `CrossError.Revoked`.
- An instance in a space ends as one `hold` holds does, `Abandoned` once the abandoner `instance.domain()` shares is triggered and the space has abandoned its domain, or, when no job or instance of the space runs on that its holder has not abandoned, from the space's next exit, `Trapped` as a job in a space does when a trap ends it, `Trapped` with a trap of `trap.bounds` whose report is `trap.bounds writing what no end of a channel writes` once it breaches the first channel of `link` or a bridge ([bridges](#bridges)), the space's other jobs and instances running on, and `Failed` with the rendering of `IsolatedError.NoChannel` when `link` has no channel or `open_port` or `open_crossing` added its first and of `IsolatedError.Unplaced` when it cannot be placed.

### Bridges

An instance in an isolated space calls a port on an instance in another isolated space, or in its own, and a job or instance in an isolated space calls such an instance through a capability of a crossing kind, over a **bridge**: a channel of its link that the kernel maps into both spaces, through which their runtimes exchange the call and its reply without the kernel relaying them.

- When `hold_isolated` places an instance whose `link` has a channel `link.open_port(port, to, inbound, outbound)` opened while the instance of `to` runs in an isolated space, the kernel clears the channel's frames and maps them into the space of each instance before it starts the instance. Each call the instance makes through `port` then passes over the channel rather than through `runner`, its space writing in `outbound` and reading in `inbound`, the other space the other way, and the other instance serves it as `inbox.call` does, under the header the call's record carries, its runtime keeping a postponed call pending for the `reply` of the runner capability that instance acts through. When `to` names an instance of the shared space or one that has ended, or a space has no room for the frames, the channel carries nothing and the calls through `port` reach `runner`.
- When `run_isolated` or `hold_isolated` places a job or instance whose `link` has a channel `link.open_crossing(key, operation, hash, to, inbound, outbound)` opened while the instance of `to` runs in an isolated space, and whose `caps` hold under `key` a capability of a crossing kind that crosses into the space, the kernel maps the channel likewise. Each `call` of `operation` through that capability in the space whose arguments are at most 1 MiB and carry no handles then passes over the channel rather than through the kernel, as a call of the protocol of `hash` whose bytes are those arguments, and gives the bytes and handles of its reply. Before the other instance serves such a call, its space's runtime has the kernel **admit** it: the kernel makes `Handle.invoke` of `operation` on its own capability with the call's bytes, under the header the call's record carries when it carries one, so that the kind decides whether each call may pass, and the call fails with `CrossError.Refused` when that `invoke` fails, with `CrossError.Revoked` when the capability is stale, and with `CrossError.Refused` when the other instance fails it. When `caps` holds no such capability under `key`, or the channel carries nothing as for a port, the calls reach the kernel as without the channel.
- Each side writes into the ring of the frame it writes **records**, each the `mem::wire` encoding of a call, with the number of its wait, its protocol hash, the header of the task making it and its bytes, or of the reply to the call of a number or its failure, through a stale capability or not, after its length as four little-endian bytes, and reads the other side's records from the ring of the frame it reads, both rings laid out as a link's channel's. A side waiting for bytes sets the word at offset 16 of the frame it reads, and one waiting for room the word at offset 24 of the frame it writes; the other side, having written or read, takes that word and asks its kernel to wake the waiting side only when the word was set. A waiting side looks at its rings again at least every second while its runtime's clock is live.
- The bytes of a call or reply travel in its record below 256 KiB. From 256 KiB, the kernel of the writing side unmaps the pages holding them from its space, and the kernel of the reading side maps those frames into its space in place of pages of its own, giving the frames those pages held back to its isolation's frames; once the reading side's end is closed, frames that moved for it without being mapped go back to the frames of its isolation, and pages no longer move to it.
- The handles of a call or reply cross from the table of the writing side's space into the other's: the kernel adds the capability each names to the other space's table, as a call's handles cross into a space, keeping what crossed under a token the record names, which only the reading side's runtime claims; once the reading side's end is closed, the kernel drops what it keeps for it unclaimed, releasing each crossing of it. A call whose handles cannot cross fails with `RunnerError.Unknown`, and so does a call whose reply's handles cannot, or with `CrossError.Refused` through a capability.
- Each side reads the other's frame as untrusted. Counts no reader and writer could leave, a record longer than 260 KiB, one it does not decode, a call carrying handles over a bridge whose calls the kernel admits, and a token naming nothing that crossed or moved are a **breach** of the bridge by the other side, whose job or instance its kernel ends as `Trapped` with a trap of `trap.bounds`; the reading side's end closes.
- A bridge closes once either side's job or instance ends or breaches it. The calls waiting on it then fail with `RunnerError.Unknown`, or with `CrossError.Refused` through a capability, and later calls through `port` reach `runner`, and those through a capability the kernel; `run_isolated` or `hold_isolated` of the calling job or instance returns only once the other space no longer maps the channel's frames.

## Attenuation

**Attenuation** is deriving a capability with less authority through an ordinary method. The std attenuations are `sys::Fs.subdir`, which reaches only the tree under `path`; `sys::Fs.read_only`, whose `create`, `write_all`, `remove`, `create_dir` and `rename` fail with `sys::fs::FsError.Denied`; `sys::Net.only_host`, whose `connect` fails with `sys::net::NetError.Denied` unless its `host` equals, byte for byte, the `host` that `only_host` received and the source's `connect` does not deny that `host`, and whose `listen` always fails with it; `sys::Clock.coarse` and `sys::WallClock.coarse`, whose clock has a coarser resolution; `task::Executor.coarse`, whose delays end on a coarser resolution ([executors](16-concurrency.md#executors)); and `mem::Alloc.split` ([budgets](12-memory.md#budgets)).

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

- A kind is a struct without fields, generic parameters, derives or a modifier other than `open`, and its record type is a scalar, a layout, an `hw::Ptr` or an `Array` of these. A `sys::Kind` impl line naming any other type or record type is an error (`authority.kind-form`), reported on the impl line. A construction of a kind is that error too: a kind's values come only from `mint` and `derive`.
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

A record as long as its layout attenuates by more than flags, such as a path:

```nan
// file: names/package.nan
//! Names handed out by subtree.

package names {
    language 1
    targets any
    allow capability in names
} names

// file: names/names.nan
//! A capability over a tree of names, attenuated to a subtree.

pub open struct Tree {
} Tree

pub layout Below: bytes 64 le {
    length: u8 at 0
    path: Array[u8, 63] at 1
} Below

impl Tree: sys::Kind[u8, Below]

pub fn open_names(alloc: mem::Alloc) -> sys::Minter[Tree, u8, Below] {
    return sys::Minter.of(0, alloc)
} open_names

pub fn Tree.below(self, segment: Slice[u8]) -> Tree fails sys::Revoked {
    return try sys::derive(self, narrow: fn(state: u8, record: Below) -> Below {
        var below = record
        var path = record.path
        var at = usize.widen(record.length)
        for byte in segment {
            if at < 63 {
                path[at] = byte
                at += 1
            }
        }
        below.path = path
        below.length = u8.truncate(at)
        return below
    })
} Tree.below

pub fn Tree.ends_with(self, byte: u8) -> bool {
    let ends = sys::reach(self, body: fn(state: u8, record: Below) -> bool {
        if record.length == 0 {
            return false
        }
        return record.path[usize.widen(record.length) - 1] == byte
    })
    return ends == Some(true)
} Tree.ends_with

test subtrees_keep_their_paths for Tree.below {
    let world = testing::world()
    let minter = open_names(world.alloc)
    defer take minter.close()
    let root = minter.mint(Below(length: 0, path: Array[u8, 63].filled(0)))
    let data = try root.below(b"data/")
    let agent = try data.below(b"agent")
    let revoker = try sys::revocable(agent)
    let copy = revoker.capability()
    let data_ends = data.ends_with(b'/')
    let agent_ends = agent.ends_with(b't')
    let copy_ends = copy.ends_with(b't')
    let root_ends = root.ends_with(b'/')
    take revoker.revoke()
    let revoked_copy_ends = copy.ends_with(b't')
    minter.revoke(data)
    let revoked_agent_ends = agent.ends_with(b't')
    expect data_ends and agent_ends and copy_ends
    expect not root_ends
    expect not revoked_copy_ends
    expect not revoked_agent_ends
} subtrees_keep_their_paths
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

### Crossing kinds

A capability kind **crosses** when its module implements `sys::Crossing` for it: its operations take and give bytes and handles, which a `sys::Handle` carries to the code of the image that minted the capability, so that the capability acts alike in every closure holding it and in [isolated spaces](#component-instances).

```nan-api
// module: sys

pub interface Crossing {
    fn invoke(self, operation: u64, args: Untrusted[Slice[u8]], handles: Slice[Handle], alloc: mem::Alloc) -> Encoded fails CrossError limits stack 4_096
    fn call(self, operation: u64, args: Untrusted[Slice[u8]], handles: Slice[Handle], alloc: mem::Alloc) -> Encoded fails CrossError uses wait limits stack 4_096
} Crossing

pub closed enum CrossError derives Render {
    Refused
    Revoked(Revoked) from
    Cancelled(task::Cancelled) from
} CrossError

pub fn Handle.crosses(self) -> bool

pub fn Handle.invoke(self, operation: u64, args: Slice[u8], handles: Slice[Handle], alloc: mem::Alloc) -> Encoded fails CrossError

pub fn Handle.call(self, operation: u64, args: Slice[u8], handles: Slice[Handle], alloc: mem::Alloc) -> Encoded fails CrossError uses wait
```

- `invoke` and `call` are a crossing kind's operations, one that never waits and one that may: each takes the number of an operation, the bytes of its arguments, which it reads as untrusted, and the handles of the capabilities they pass, and gives the bytes of its result drawn through `alloc` with the handles of the capabilities it passes back. A kind reads the capability a handle holds through `sys::CapabilityId.of_handle` ([wire encoding](26-components.md#wire-encoding)), and fails its operations with `CrossError.Refused` for an operation or arguments it refuses, and with `CrossError.Revoked` through a stale capability.
- An entry of a crossing kind records the kind's methods for `sys::Crossing` in the image minting it, and an entry deriving from it, through `sys::derive` or `sys::revocable`, records the same.
- `handle.crosses()` is true when the capability `handle` holds is live and of a crossing kind. `handle.invoke(operation, args, handles, alloc)` and `handle.call(operation, args, handles, alloc)` call the method of their name of the kind of that capability, with the capability as `self`, on the calling task as a [served capability](#served-capabilities) calls its kind's methods, and fail as it fails; they fail with `CrossError.Revoked(Revoked.Stale)` when the capability is stale and otherwise with `CrossError.Refused` when it is not of a crossing kind. Their digests are those of `sys::Handle`'s operations, whatever the methods do.

```nan
// file: meters/package.nan
//! Meters a kernel hands to the components it holds.

package meters {
    language 1
    targets any
    allow capability in meters
} meters

// file: meters/meters.nan
//! Meters adding the amounts their operations carry as bytes, moving them to the meters they are handed and handing out meters deriving from them.

pub open struct Meter {
} Meter

impl Meter: sys::Kind[core::atomic::AtomicU64, u64]

impl Meter: sys::Crossing

/// The operations of a meter: adding an amount and giving its total, moving an amount to the meter
/// its handles hold and giving a meter deriving from it.
const ADD: u64 = 0

const MOVE: u64 = 1

const SPLIT: u64 = 2

pub fn open_meters(alloc: mem::Alloc) -> sys::Minter[Meter, core::atomic::AtomicU64, u64] {
    return sys::Minter.of(core::atomic::AtomicU64.of(0), alloc)
} open_meters

pub fn Meter.add(self, amount: u64, alloc: mem::Alloc) -> u64 fails sys::CrossError {
    let none = mem::List[sys::Handle].empty(alloc)
    let total = try sys::Handle.of(self).invoke(ADD, args: amount_bytes(amount, alloc).view(), handles: none.view(), alloc)
    match attempt mem::wire::Codec[u64].decode(Untrusted.of(total.bytes.view()), alloc) {
        Ok(sum) => return sum
        Err(_) => fail sys::CrossError.Refused
    }
} Meter.add

pub fn Meter.move_to(self, to: Meter, amount: u64, alloc: mem::Alloc) fails sys::CrossError {
    let given = Array[sys::Handle, 1].filled(sys::Handle.of(to))
    discard try sys::Handle.of(self).invoke(MOVE, args: amount_bytes(amount, alloc).view(), handles: given.view(), alloc)
} Meter.move_to

pub fn Meter.split(self, alloc: mem::Alloc) -> Meter fails sys::CrossError {
    let none = mem::List[sys::Handle].empty(alloc)
    let derived = try sys::Handle.of(self).invoke(SPLIT, args: b"", handles: none.view(), alloc)
    let Some(handle) = derived.handles.get(0) else {
        fail sys::CrossError.Refused
    }
    let Some(meter) = sys::CapabilityId[Meter](id: 0).of_handle(handle) else {
        fail sys::CrossError.Refused
    }
    return meter
} Meter.split

pub fn Meter.invoke(
    self,
    operation: u64,
    args: Untrusted[Slice[u8]],
    handles: Slice[sys::Handle],
    alloc: mem::Alloc,
    ) -> sys::Encoded fails sys::CrossError limits stack 4_096 {
    var result = sys::Encoded.empty(alloc)
    if operation == SPLIT {
        let derived = try sys::derive(self, narrow: fn(state: core::atomic::AtomicU64, record: u64) => record)
        mut result.handles.push(sys::Handle.of(derived))
        return result
    }
    let Ok(amount) = attempt mem::wire::Codec[u64].decode(args, alloc) else {
        fail sys::CrossError.Refused
    }
    if operation == MOVE {
        if handles.len() != 1 {
            fail sys::CrossError.Refused
        }
        let Some(to) = sys::CapabilityId[Meter](id: 0).of_handle(handles[0]) else {
            fail sys::CrossError.Refused
        }
        let Some(taken) = sys::reach(self, body: fn(state: core::atomic::AtomicU64, record: u64) => state.fetch_sub(amount)) else {
            fail sys::CrossError.Revoked(sys::Revoked.Stale)
        }
        if taken.is_none() {
            fail sys::CrossError.Refused
        }
        discard sys::reach(to, body: fn(state: core::atomic::AtomicU64, record: u64) => state.fetch_add(amount))
        return result
    }
    if operation != ADD {
        fail sys::CrossError.Refused
    }
    let added = sys::reach(self, body: fn(state: core::atomic::AtomicU64, record: u64) => state.fetch_add(amount))
    let Some(Some(before)) = added else {
        fail sys::CrossError.Revoked(sys::Revoked.Stale)
    }
    mut result.bytes.append(amount_bytes(before.wrapping_add(amount), alloc).view())
    return result
} Meter.invoke

pub fn Meter.call(
    self,
    operation: u64,
    args: Untrusted[Slice[u8]],
    handles: Slice[sys::Handle],
    alloc: mem::Alloc,
    ) -> sys::Encoded fails sys::CrossError limits stack 4_096 {
    return try self.invoke(operation, args, handles, alloc)
} Meter.call

fn amount_bytes(amount: u64, alloc: mem::Alloc) -> mem::Bytes {
    var bytes = mem::Bytes.empty(alloc)
    mem::wire::Codec[u64].encode(amount, out: mut bytes)
    return bytes
} amount_bytes

test meters_add_through_handles_until_revoked for Meter.add {
    let world = testing::world()
    let minter = open_meters(world.alloc)
    defer take minter.close()
    let meter = minter.mint(0)
    discard attempt meter.add(2, alloc: world.alloc)
    let total = match attempt meter.add(3, alloc: world.alloc) {
        Ok(sum) => sum
        Err(_) => 0
    }
    let crosses = sys::Handle.of(meter).crosses()
    let clock = sys::Handle.of(world.clock)
    let clock_crosses = clock.crosses()
    let none = mem::List[sys::Handle].empty(world.alloc)
    let refused = match attempt clock.invoke(ADD, args: b"", handles: none.view(), alloc: world.alloc) {
        Err(sys::CrossError.Refused) => true
        else => false
    }
    minter.revoke(meter)
    let revoked = match attempt meter.add(1, alloc: world.alloc) {
        Err(sys::CrossError.Revoked(_)) => true
        else => false
    }
    let still_crosses = sys::Handle.of(meter).crosses()
    expect total == 5
    expect crosses and not clock_crosses and refused
    expect revoked and not still_crosses
} meters_add_through_handles_until_revoked

test meters_move_to_and_hand_out_meters_through_handles for Meter.move_to {
    let world = testing::world()
    let minter = open_meters(world.alloc)
    defer take minter.close()
    let others = open_meters(world.alloc)
    defer take others.close()
    let meter = minter.mint(0)
    let other = others.mint(0)
    discard attempt meter.add(5, alloc: world.alloc)
    let moved = attempt meter.move_to(other, amount: 3, alloc: world.alloc)
    let short = attempt meter.move_to(other, amount: 3, alloc: world.alloc)
    let received = match attempt other.add(0, alloc: world.alloc) {
        Ok(sum) => sum
        Err(_) => 0
    }
    let Ok(derived) = attempt meter.split(world.alloc) else {
        expect false
        return
    }
    let split_total = match attempt derived.add(1, alloc: world.alloc) {
        Ok(sum) => sum
        Err(_) => 0
    }
    minter.revoke(meter)
    let split_revoked = match attempt derived.add(1, alloc: world.alloc) {
        Err(sys::CrossError.Revoked(_)) => true
        else => false
    }
    expect moved.is_ok() and short.is_err() and received == 3
    expect split_total == 3 and split_revoked
} meters_move_to_and_hand_out_meters_through_handles
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
| `confined` | the confined operations of [register windows](23-low-level.md#register-windows), [DMA](23-low-level.md#dma), [I/O domains](23-low-level.md#io-domains), [locks](24-kernel.md#locks), [PCI functions](24-kernel.md#pci-functions) and [PCI ports](24-kernel.md#pci-ports) |
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

```nan
// file: uart/package.nan
//! 16550 serial registers.

package uart {
    language 1
    targets x86_64-bare
    allow device in uart, uart_regs // error: authority.unknown-name
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
```

```nan
// file: uart/package.nan
//! 16550 serial registers.

package uart {
    language 1
    targets x86_64-bare
    allow device in uart // incomplete: unused.grant
} uart

// file: uart/uart.nan
//! 16550 status reads.

pub layout LineStatus: u8 {
    data_ready: bit 0
    reserved: rest
} LineStatus

pub fn has_byte(status: LineStatus) -> bool {
    return status.data_ready
} has_byte
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

The **closure summary** of a [final-link root](27-program-model.md#final-builds) is the set of pairs (P, e) where P is a package of its final closure other than the root and the std packages, and e an entry of P's authority summary. The root's [lock](29-identity-versioning.md#lock) records an approved closure summary, which only [approvals](29-identity-versioning.md#approvals), a first lock's included, extend. A closure summary holding a pair the approved one lacks is an error (`authority.widening-unapproved`), reported on the root's `package` line, naming the command `nan dep approve` of the root and listing each such pair; `nan check`, `nan test`, `nan build` and `nan run` of the root report it.

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
