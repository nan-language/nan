# Standard library 1.0

This chapter defines std: its packages, modules and layers, the index of every pub std item with the section defining it, the std items no other chapter defines (array and slice members, Option and Result members, ordered lookup), the supplied operations, the contracts and evidence pub std items carry, the authority each std package reaches, the platform packages, the controlled vocabulary and naming conventions of std names, and the reserved std names.

## Std layers

**Std**, the standard library, is the nine packages that the reserved std aliases name ([aliases](03-packages-modules.md#aliases)) and the three [platform packages](#platform-packages). Each of the nine is named like its alias.

| Package | Holds | Modules below the root |
| --- | --- | --- |
| `core` | scalar operations, `Option`, `Result`, arrays and slices, `Str`, ranges, cursors, `Untrusted`, `Dyn`, the property, comparison, operator, render, encoding and generation interfaces, total float orders, wide products, signatures | `core::atomic`, `core::crypto` |
| `mem` | allocators, budgets, stores, ids, handles, boxes, shared values, collections, once maps | `mem::wire` |
| `text` | owned and inline text, interning, writers, templates | `text::utf8`, `text::json`, `text::construction` |
| `task` | task scopes, joins, deadlines, races, cowns, channels, cancellation, poisoning, executors, completions | none |
| `sys` | the sys capabilities, revocation, domains, components | `sys::fs`, `sys::net`, `sys::sim` |
| `hw` | pointers, address types, register windows, DMA, privileged intrinsics, the anchor, interrupts, signals, spin locks, scheduling, physical memory, platform registries | `hw::sim` |
| `boot` | the uefi and bare root grants, firmware services | none |
| `host` | the host root grant, the component runner | none |
| `testing` | the simulated world, structural comparison | none |

- These are the only std modules. A std path has at most one module segment after the alias, as in `sys::net::Conn`; every other std item is in its package's root module.
- Each of the nine packages declares `targets any` and has no target variants, so each of its items has one interface and one meaning on every [target](27-program-model.md#targets). Each platform package declares exactly the targets whose descriptors name it.

A **layer** is a position in this order, lowest first: `core`, `mem`, `text`, `task`, `sys`, `hw`, `boot`, `host`, `testing`, the platform packages.

- Outside test items and example clauses, a std item names and calls only items of its own and lower layers, except these references to [base types](05-properties.md#structural-properties) of higher layers and their members: `core` names `mem::Alloc`, `mem::Bytes`, `mem::OutOfMemory` and `hw::Dma[T]` in `Encode`, `Generate` and the [content-blind operations](15-untrusted.md#content-blind-operations); `mem` names `hw::Region` in `over`; `task` names `sys::Clock` in `task::within` and `task::within_ns`; and the type of `hw::mint_root` is a root grant of `boot` or `host`.
- The checker checks std one layer at a time, lowest first, having read the heads of the base types these exceptions name.

Std ships with the toolchain:

- A toolchain carries exactly one release of each std package, and the [toolchain hash](36-builds.md#toolchain-hash) covers their sources. No `dep` directive and no line of a [lock](29-identity-versioning.md#lock) names a std package.
- In every other respect a std package is a package: written in NaN, with an [artefact](30-artefacts.md#artefacts) holding its interfaces, digests, ledger and seals, and recorded in the [requires-sets](29-identity-versioning.md#requires-sets) of the packages using its items.
- Std passes `nan verify`: it holds no incompleteness, and each of its trust escapes, its package items granting `device` or `priv` included, carries a current [seal](31-trust.md#seals). An implementation whose std fails `nan verify` does not conform.

```nan
pub fn is_text(data: Slice[u8]) -> bool {
    return text::utf8::is_valid(data)
} is_text
```

## Std index

The table names every pub std type, interface, free function and registry with the section defining it. A type's members are defined in its section, except members a row names.

| Package | Items | Defined in |
| --- | --- | --- |
| `core` | the scalars, `Unit`, `Never` | [Scalars](04-types.md#scalars), [Unit and Never](04-types.md#unit-and-never) |
| `core` | integer and float members, the named and bit conversions included as built-in members, `core::Overflowing`, `core::WideProduct`, `core::TotalF32`, `core::TotalF64` | [Numbers](09-numbers.md) |
| `core` | `char` members, `Str`, `core::CharCursor` | [Text types](13-text-encode.md#text-types), [Lengths and slicing](13-text-encode.md#lengths-and-slicing) |
| `core` | `Option`, `Some`, `None`, `Result`, `Ok`, `Err` | [Option and Result](04-types.md#option-and-result); members, `core::Option.extract` included, in [Option and Result members](#option-and-result-members) |
| `core` | `Array`, `Slice`, `SliceMut` | [Arrays and slices](04-types.md#arrays-and-slices); `at` and `at_mut` in [Indexing](07-ownership.md#indexing); `cursor`, `cursor_mut` and `into_cursor` in [Iteration](07-ownership.md#iteration); other members in [Array and slice members](#array-and-slice-members) |
| `core` | `core::Range` | [Ranges](06-statements-expressions.md#ranges); `next` in [Ranges](09-numbers.md#ranges) |
| `core` | `core::Cursor`, `core::SliceCursor`, `core::SliceCursorMut`, `core::ArrayCursor` | [Iteration](07-ownership.md#iteration) |
| `core` | `Any`, `AnyBits`, `Copy`, `Inert`, `Send` | [Property interfaces](05-properties.md#property-interfaces) |
| `core` | `Eq`, `Ord`, `Hash`, `Default`, `core::Ordering`, `core::Hasher` | [Equality, ordering, hashing and defaults](08-generics-interfaces.md#equality-ordering-hashing-and-defaults) |
| `core` | `core::Add`, `core::Sub`, `core::Mul`, `core::Div`, `core::Rem`, `core::BitAnd`, `core::BitOr`, `core::BitXor`, `core::Shl`, `core::Shr`, `core::Neg`, `core::BitNot` | [Operator interfaces](08-generics-interfaces.md#operator-interfaces) |
| `core` | `Dyn` | [Dynamic dispatch](08-generics-interfaces.md#dynamic-dispatch) |
| `core` | `Render`, `core::Renderer` | [Render](13-text-encode.md#render) |
| `core` | `Encode`, `core::Encoder`, `core::Decoder`, `core::DecodeError` | [Encode](13-text-encode.md#encode), [Encoders and decoders](13-text-encode.md#encoders-and-decoders) |
| `core` | `Untrusted`, `core::Parts` | [Untrusted values](15-untrusted.md#untrusted-values), [Content-blind operations](15-untrusted.md#content-blind-operations) |
| `core` | `Generate`, `core::Choices` | [Generation](22-testing.md#generation) |
| `core` | `core::atomic::AtomicU8`, `core::atomic::AtomicU16`, `core::atomic::AtomicU32`, `core::atomic::AtomicU64`, `core::atomic::AtomicUsize`, `core::atomic::AtomicBool` | [Atomics](16-concurrency.md#atomics) |
| `core` | `core::crypto::PublicKey`, `core::crypto::Signature`, `core::crypto::SecretKey` | [Signatures](31-trust.md#signatures) |
| `mem` | `mem::Alloc`, `mem::Budget`, `mem::OutOfMemory` | [Allocators](12-memory.md#allocators), [Budgets](12-memory.md#budgets), [Growth](12-memory.md#growth) |
| `mem` | `mem::Heap`, `mem::Arena`, `mem::Id`, `mem::Pool`, `mem::Handle` | [Stores](12-memory.md#stores) |
| `mem` | `mem::Box` | [Box](07-ownership.md#box); `from_box` in [Dynamic dispatch](08-generics-interfaces.md#dynamic-dispatch) |
| `mem` | `mem::Shared` | [Shared values](12-memory.md#shared-values) |
| `mem` | `mem::Entry`, `mem::KeyValue`, `mem::List`, `mem::Bytes`, `mem::Map`, `mem::HashMap`, `mem::Set`, `mem::Bounded` | [Collections](12-memory.md#collections); `get_by`, `get_by_mut` and `contains_by` in [Ordered lookup](#ordered-lookup); `mem::Map.at`, `mem::Map.at_mut`, `mem::HashMap.at` and `mem::HashMap.at_mut` in [Maps and sets](12-memory.md#maps-and-sets) |
| `mem` | `mem::ArenaCursor`, `mem::PoolCursor`, `mem::ListCursor`, `mem::BoundedCursor`, `mem::MapCursor`, `mem::MapCursorMut`, `mem::MapIntoCursor`, `mem::HashMapCursor`, `mem::HashMapCursorMut`, `mem::HashMapIntoCursor`, `mem::SetCursor`, `mem::SetIntoCursor` | [Memory types](12-memory.md#memory-types) |
| `mem` | `mem::OnceMap` | [Once maps](12-memory.md#once-maps) |
| `mem` | `mem::wire::Codec` | [Wire format](13-text-encode.md#wire-format) |
| `text` | `text::String`, `text::Fixed` | [Text types](13-text-encode.md#text-types) |
| `text` | `text::utf8::Utf8Error`, `text::utf8::valid_prefix`, `text::utf8::is_valid`, `text::utf8::char_at`, `text::utf8::write_char` | [UTF-8](13-text-encode.md#utf-8) |
| `text` | `text::Symbol`, `text::Interner` | [Interning](13-text-encode.md#interning) |
| `text` | `text::Writer`, `text::fmt` | [Writers](13-text-encode.md#writers), [Templates](13-text-encode.md#templates) |
| `text` | `text::json::Codec` | [JSON](13-text-encode.md#json) |
| `text` | `text::construction::Codec` | [Construction text](13-text-encode.md#construction-text) |
| `task` | `task::scope`, `task::Scope` | [Task scopes](16-concurrency.md#task-scopes); `spawn` and `spawn_sized` in [Joins](16-concurrency.md#joins) |
| `task` | `task::Join` | [Joins](16-concurrency.md#joins) |
| `task` | `task::within`, `task::within_ns`, `task::race` | [Deadlines and races](16-concurrency.md#deadlines-and-races) |
| `task` | `task::Cown`, `task::Poisoned` | [Cowns](16-concurrency.md#cowns), [Poisoned cowns](16-concurrency.md#poisoned-cowns) |
| `task` | `task::Channel`, `task::Sender`, `task::Receiver`, `task::SendError` | [Channels](16-concurrency.md#channels) |
| `task` | `task::Executor` | [Executors](16-concurrency.md#executors) |
| `task` | `task::Cancelled` | [Cancellation](11-errors-domains.md#cancellation) |
| `task` | `task::Completion` | [Completion I/O](17-suspension-io.md#completion-io) |
| `sys` | `sys::Fs`, `sys::Net`, `sys::Clock`, `sys::WallClock`, `sys::Random`, `sys::Log`, `sys::Stdio`, `sys::StdioError`, `sys::fs::File`, `sys::fs::FsError`, `sys::net::Listener`, `sys::net::Conn`, `sys::net::NetError` | [Std capabilities](14-authority.md#std-capabilities) |
| `sys` | `sys::Revoked`, `sys::Capability`, `sys::revocable`, `sys::Revoker` | [Revocation](14-authority.md#revocation) |
| `sys` | `sys::Domain`, `sys::Trap`, `sys::TrapKind` | [Domain run](11-errors-domains.md#domain-run) |
| `sys` | `sys::Component` | [Components](26-components.md#components) |
| `sys` | `sys::Cx` | [Component context](26-components.md#component-context); `spawn` and `cancel` in [Keyed tasks](26-components.md#keyed-tasks) |
| `sys` | `sys::Outbox` | [Keyed tasks](26-components.md#keyed-tasks) |
| `sys` | `sys::sim::fs`, `sys::sim::net`, `sys::sim::random`, `sys::sim::stdio` | [Sim constructors](22-testing.md#sim-constructors) |
| `hw` | `hw::Ptr` | [Pointers](23-low-level.md#pointers); atomic members in [Trusted atomics](23-low-level.md#trusted-atomics) |
| `hw` | `hw::Order`, `hw::fence` | [Trusted atomics](23-low-level.md#trusted-atomics) |
| `hw` | `hw::PhysAddr`, `hw::VirtAddr`, `hw::BusAddr`, `hw::UserAddr`, `hw::UserAddrError` | [Address types](23-low-level.md#address-types) |
| `hw` | `hw::Register` | [Registers](23-low-level.md#registers) |
| `hw` | `hw::Mmio`, `hw::Ports` | [Register windows](23-low-level.md#register-windows) |
| `hw` | `hw::Dma`, `hw::DmaShared`, `hw::dma_fence` | [DMA](23-low-level.md#dma) |
| `hw` | `hw::Ring0`, `hw::Idt`, `hw::IdtInstalled`, `hw::PageTableRoot`, `hw::SysReg` | [Privileged intrinsics](23-low-level.md#privileged-intrinsics) |
| `hw` | `hw::mint_root` | [Landing functions](23-low-level.md#landing-functions) |
| `hw` | `hw::Anchor`, `hw::cpu_count` | [Anchor](24-kernel.md#anchor) |
| `hw` | `hw::IrqContext`, `hw::TrapFrame`, `hw::NoCode`, `hw::ErrorCode`, `hw::PageFault`, `hw::IrqExit` | [Interrupt functions](24-kernel.md#interrupt-functions) |
| `hw` | `hw::IrqsDisabled`, `hw::without_irqs` | [Interrupt state](24-kernel.md#interrupt-state) |
| `hw` | `hw::Signal` | [Signals](24-kernel.md#signals) |
| `hw` | `hw::SpinLock`, `hw::SpinGuard`, `hw::IrqShared` | [Locks](24-kernel.md#locks) |
| `hw` | `hw::RunQueue`, `hw::SavedContext`, `hw::Resumable`, `hw::switch_to`, `hw::PerCpu` | [Scheduling](24-kernel.md#scheduling) |
| `hw` | `hw::Region`, `hw::PhysFrame`, `hw::Offset` | [Physical memory](24-kernel.md#physical-memory) |
| `hw` | `hw::vectors`, `hw::Vector`, `hw::VectorHandler`, `hw::syscall_entry`, `hw::SyscallEntry`, `hw::preempt`, `hw::PreemptEntry` | [Platform registries](25-registries.md#platform-registries) |
| `hw` | `hw::sim::Registers` | [Sim constructors](22-testing.md#sim-constructors) |
| `boot` | `boot::Machine`, `boot::Kernel` | [Root grants](14-authority.md#root-grants); members of `boot::Kernel` in [Anchor](24-kernel.md#anchor) and of `boot::Machine` in [Firmware services](24-kernel.md#firmware-services) |
| `boot` | `boot::Services`, `boot::ServiceError` | [Firmware services](24-kernel.md#firmware-services) |
| `host` | `host::World` | [Root grants](14-authority.md#root-grants) |
| `host` | `host::Runner` | [Runner](26-components.md#runner) |
| `testing` | `testing::world`, `testing::same` | [Simulated capabilities](22-testing.md#simulated-capabilities) |

## Array and slice members

These members of `Array[T, N]`, `Slice[T]` and `SliceMut[T]` exist for every `T`:

```nan-api
// module: core

pub fn Array.len(self) -> usize = N

pub proj fn Array.view(self) -> Slice[T] limits pure, notrap, terminates

pub proj fn Array.view_mut(mut self) -> SliceMut[T] limits pure, notrap, terminates

pub fn Slice.len(self) -> usize limits pure, notrap, terminates
    example b"abc".len() == 3

pub fn Slice.is_empty(self) -> bool = self.len() == 0

pub proj fn Slice.prefix(self, count: usize) -> Option[Slice[T]] limits pure, notrap, terminates

pub proj fn Slice.split_at(self, at: usize) -> Option[core::Parts[Slice[T]]] limits pure, notrap, terminates

pub fn SliceMut.len(self) -> usize limits pure, notrap, terminates

pub fn SliceMut.is_empty(self) -> bool = self.len() == 0

pub proj fn SliceMut.view(self) -> Slice[T] limits pure, notrap, terminates

pub proj fn SliceMut.prefix_mut(mut self, count: usize) -> Option[SliceMut[T]] limits pure, notrap, terminates

pub proj fn SliceMut.split_at_mut(mut self, at: usize) -> Option[core::Parts[SliceMut[T]]] limits pure, notrap, terminates
```

Each member of this table exists only for the instantiations its row gives, a type parameter counting as Copy only when a bound names `Copy`. On any other instantiation the member name is unknown (`name.unknown`).

| Head | Exists when |
| --- | --- |
| `pub fn Array.filled(value: T) -> Array[T, N] limits pure, notrap, terminates` | `T` is Copy |
| `pub fn SliceMut.fill(mut self, value: T) limits notrap, terminates` | `T` is Copy |
| `pub fn SliceMut.copy_from(mut self, source: Slice[T]) limits notrap, terminates` | `T` is Copy |
| `pub fn Slice.u16_le(self, at: usize) -> u16 limits pure, notrap, terminates`, and likewise `u32_le`, `u64_le`, `u16_be`, `u32_be` and `u64_be`, each also on `SliceMut` | `T` is `u8` |
| `pub fn SliceMut.write_u16_le(mut self, at: usize, value: u16) limits notrap, terminates`, and likewise `write_u32_le`, `write_u64_le`, `write_u16_be`, `write_u32_be` and `write_u64_be` | `T` is `u8` |

The facts below are the ensures and requires of these members, available to the [prover](20-prover.md#callee-knowledge):

- `len` is the number of elements; `Array.len` is `N`. `view` and `view_mut` cover every element, so their results have the receiver's `len`.
- `prefix(count)` and `prefix_mut(count)` are `Some` exactly when `count <= self.len()`, holding the first `count` elements. `split_at(at)` and `split_at_mut(at)` are `Some` exactly when `at <= self.len()`; `head` holds the elements before index `at` and `tail` the rest.
- `filled(value)` holds `N` copies of `value`. `fill` writes `value` to every element.
- `copy_from` has `requires source.len() == self.len()` and writes the elements of `source` in index order.
- A reader of an `n`-byte integer (`u16`, `u32` or `u64`) has `requires at <= self.len() and self.len() - at >= n` and returns the integer whose bytes are the elements from index `at`, least significant first for `_le` and most significant first for `_be`. A writer has the same requires and stores the bytes of `value` in that order.

```nan
pub closed struct Header derives Eq {
    pub kind: u8
    pub length: u16
} Header

pub fn read_header(frame: Slice[u8]) -> Option[Header]
    example read_header(b"\x02\x00\x10") == Some(Header(kind: 2, length: 16))
    example read_header(b"\x02") == None
{
    let Some(head) = frame.prefix(3) else {
        return None
    }
    return Some(Header(kind: head[0], length: head.u16_be(1)))
} read_header
```

```nan
pub fn first_half(words: Slice[u32]) -> u16 {
    return words.u16_le(0) // error: name.unknown
} first_half
```

## Option and Result members

```nan-api
// module: core

pub fn Option.is_some(self) -> bool limits pure, notrap, terminates
    example Some(3u8).is_some()

pub fn Option.is_none(self) -> bool limits pure, notrap, terminates
    example not Some(3u8).is_none()

pub fn Option.extract(mut self) -> Option[T] limits pure, notrap, terminates

pub fn Result.is_ok(self) -> bool limits pure, notrap, terminates

pub fn Result.is_err(self) -> bool limits pure, notrap, terminates
```

- `is_some` and `is_ok` are true exactly for `Some` and `Ok`; `is_none` and `is_err` are their negations.
- `pub fn Option.value_or(self, fallback: T) -> T limits pure, notrap, terminates` returns the payload of `Some`, and `fallback` for `None`. It exists only when `T` is Copy, as for [array and slice members](#array-and-slice-members); on any other instantiation the name is unknown (`name.unknown`).
- `mut o.extract()` returns the value `o` held and leaves `None` in it; it moves the payload out of a place that [modes](07-ownership.md#modes) never move.

```nan
pub fn port_or_default(port: Option[u16]) -> u16
    example port_or_default(Some(8_080)) == 8_080
    example port_or_default(None) == 80
{
    return port.value_or(80)
} port_or_default
```

```nan
pub fn next_job(mut pending: Option[u32]) -> u32 {
    let Some(job) = mut pending.extract() else {
        return 0
    }
    return job
} next_job
```

## Ordered lookup

**Ordered lookup** finds a key of a `mem::Map[K, V]` or a value of a `mem::Set[T]` through a function placing keys relative to a sought key that need not be a value of the key type, such as a `Str` sought among `text::String` keys.

```nan-api
// module: mem

pub proj fn Map.get_by(self, order: fn(key: K) -> core::Ordering) -> Option[V]

pub proj fn Map.get_by_mut(mut self, order: fn(key: K) -> core::Ordering) -> Option[V]

pub fn Set.contains_by(self, order: fn(value: T) -> core::Ordering) -> bool
```

`order(k)` returns `core::Ordering.Less` when key `k` comes before the sought key, `Equal` when `k` matches it and `Greater` when `k` comes after it. With `k_0` to `k_(n−1)` the receiver's keys (a set's values) in ascending order, `n` being its `len`, each of these members runs this search:

1. `lo` is 0 and `hi` is `n`.
2. While `lo < hi`, it calls `order(k_mid)` with `mid` equal to `(lo + hi) / 2` rounded down: `Less` sets `lo` to `mid + 1`, `Greater` sets `hi` to `mid`, and `Equal` ends the search, finding `k_mid`.
3. When `lo` equals `hi`, the search ends, finding nothing.

`get_by` and `get_by_mut` yield the value of the found key, or `None`; `contains_by` returns whether the search found one.

```nan
pub fn port_of(ports: mem::Map[text::String, u16], name: Str) -> Option[u16] {
    return ports.get_by(fn(key: text::String) => key.as_str().cmp(name))
} port_of

test finds_port_by_name for port_of {
    let world = testing::world()
    var ports = mem::Map[text::String, u16].empty(world.alloc)
    discard mut ports.insert(text::String.of("http", alloc: world.alloc), value: 80)
    expect port_of(ports, name: "http") == Some(80)
    expect port_of(ports, name: "ftp") == None
} finds_port_by_name
```

## Supplied operations

A **supplied operation** is a std function without a body, whose item ends at its head line or after its clause region ([heads](02-items.md#heads)). The toolchain implements it as its head, its clauses and the section defining it state, and the [prover](20-prover.md#callee-knowledge) takes its requires and ensures without proving them. The section defining it states its facts ([digests](10-effects-digests.md#digests)), which are exactly these:

- wait when its head states `uses wait`, and terminates otherwise; float use when a parameter or its result has a float type; and the device, priv and sync facts that its section states for pointers, register windows, DMA and privileged intrinsics.
- Unless its head demands `pure`: sync when it is a member of a type with [interior mutability](12-memory.md#interior-mutability), or is `mem::Shared.share` or `mem::Shared.make_mut`, which change a count; and, in its capabilities set, the primitive capabilities among its parameters' types.
- Unless its head demands `pure` or `noalloc`, alloc when it draws memory: when its name starts with `try_`, when the type it is a member of has a member of its name prefixed with `try_`, or when its section states that it draws memory, but never for the members of `text::Fixed` and `mem::Bounded`, which store their contents inline.
- Unless its head demands `notrap`, may-trap when it draws memory, its name does not start with `try_` and its section does not state that it fails when that memory is exhausted, or when its section names a trap it raises.

## Std evidence

Pub std items carry the contracts and evidence that consumers rely on:

- Apart from stack overflow (`trap.stack-overflow`), a pub std function traps only on conditions its requires state or the section defining it names as traps.
- Each contract term of a pub std item names only items and members a consumer sees, so no part of it is [opaque](18-contracts.md#opacity) to a consumer.
- Every pub std function carries an `example` clause or is the target of a test of its std package ([test items](22-testing.md#test-items)), except the functions that no test or example on `x86_64-host` can call: the members of `boot::Machine`, `boot::Kernel`, `boot::Services`, `hw::Ring0`, `hw::Region`, `hw::PhysFrame[S]`, `hw::PageTableRoot`, `hw::IrqContext`, `hw::TrapFrame[V]`, `hw::SpinGuard[T]` and `hw::SavedContext`, whose values only machine targets give, and the functions with a parameter of one of these types; `hw::Mmio.map`, `hw::Ports.map` and `hw::Idt.of_vectors`, which need the devices and vector seams of a machine target; `hw::Anchor.with`, which only interrupt and landing functions call; and `Untrusted.inspect`, which only validators call.
- Each fact that the section defining a std item states as its requires or ensures, and that a contract term over the item's parameters and result can state, is a `requires` or `ensures` clause of that item, which `nan show <path> --api` prints.

## Std authority

The primitive capabilities ([capability types](05-properties.md#capability-types)) are exactly `mem::Alloc`, `mem::Budget` and `mem::Heap`; `task::Executor`; `sys::Fs`, `sys::Net`, `sys::Clock`, `sys::WallClock`, `sys::Random`, `sys::Log`, `sys::Stdio`, `sys::fs::File`, `sys::net::Listener`, `sys::net::Conn` and `sys::Revoker[C]`; `boot::Machine`, `boot::Kernel` and `boot::Services` ([kernel types](24-kernel.md#kernel-types)); and the hardware capabilities `hw::Mmio[R]`, `hw::Ports[R]`, `hw::Dma[T]`, `hw::DmaShared[T]`, `hw::Ring0`, `hw::PageTableRoot`, `hw::Region`, `hw::PhysFrame[S]` and `hw::IrqShared[T]` ([kernel types](24-kernel.md#kernel-types)). The **sys capabilities** are `sys::Fs`, `sys::Net`, `sys::Clock`, `sys::WallClock`, `sys::Random`, `sys::Log` and `sys::Stdio`.

The [authority summary](14-authority.md#authority-summaries) of each std package, published in its artefact, holds only entries of its row. "Trusted code" covers trusted blocks and expressions and `assume`; "decoders" are the decoders that the package's types derive.

| Package | Capabilities | Facts | Validators | Trust escapes |
| --- | --- | --- | --- | --- |
| `core` | none | `alloc` | decoders | trusted code, declassifying decoders |
| `mem` | `mem::Budget`, `mem::Heap`, `hw::Region` | `alloc` | decoders, the `core::Decoder` methods of `mem::wire` | trusted code, declassifying decoders and validators |
| `text` | none | `alloc` | decoders, the `core::Decoder` methods of `text::json` and `text::construction` | trusted code, declassifying decoders and validators |
| `task` | `task::Executor`, `sys::Clock` | `alloc` | none | trusted code |
| `sys` | the sys capabilities and the resources they open, `sys::Revoker[C]`, `task::Executor` | `alloc` | none | trusted code |
| `hw` | the hardware capabilities | `alloc`, `device`, `priv` | none | trusted code, `device` and `priv` grants |
| `boot` | `boot::Machine`, `boot::Kernel`, `boot::Services`, the hardware capabilities, `task::Executor`, `sys::Clock`, `sys::WallClock` | `alloc`, `device`, `priv` | none | trusted code, `device` and `priv` grants |
| `host` | the sys capabilities and the resources they open, `task::Executor` | `alloc` | none | none |
| `testing` | the sys capabilities and the resources they open, `task::Executor` | `alloc` | none | trusted code |
| platform packages | every primitive capability | `alloc`, `device`, `priv` | none | trusted code, foreign fns, landing fns, its `priv` grant |

```nan
pub fn header_length(frame: Slice[u8]) -> Option[u16] limits pure, notrap, terminates {
    let Some(head) = frame.prefix(4) else {
        return None
    }
    return Some(head.u16_be(2))
} header_length
```

## Platform packages

A **platform package** is the package that mints a target's [root grant](14-authority.md#root-grants) and implements the target-dependent operations of the capabilities that root grant yields. Exactly three exist; each concrete target's [descriptor](27-program-model.md#descriptors) names one:

| Platform package | Targets | Root grant | Implements |
| --- | --- | --- | --- |
| `host_platform` | the host targets | `host::World` | the sys capabilities, through Linux system calls and the [host I/O backend](27-program-model.md#host-io-backend) |
| `uefi_platform` | the UEFI targets | `boot::Machine` | `boot::Services`, through the firmware's boot services |
| `bare_platform` | the bare targets | `boot::Kernel` | none; it builds the root grant from the start information, the PVH start information on `x86_64-bare` and the device tree on `aarch64-bare` |

- A platform package holds the entry [landing fn](23-low-level.md#landing-functions) of its targets, the one item that calls `hw::mint_root`, and every dependence of std on Linux, ELF, glibc, UEFI, PVH or device trees; no other std item has one.
- No alias and no `dep` directive names a platform package, so no path outside it reaches its items. The [final closure](27-program-model.md#final-builds) of a program also holds the platform package its descriptor names, and no other.
- Each platform package's manifest grants `priv` to its modules, so its package item is sealed ([device and priv grants](14-authority.md#device-and-priv-grants)); that of `host_platform`, whose modules hold trusted code, also grants them `trusted`.
- `uefi_platform` and `bare_platform` each contribute the `hw::preempt` entry of their targets ([preemption](24-kernel.md#preemption)).

A **target-dependent operation** is an operation of a sys capability, of a resource one opens, or of `boot::Services`. Its std item reaches the platform function through the capability-table [support routine](27-program-model.md#runtime-support), which calls the platform function recorded in the table entry of the capability, or of the entry a resource records ([resources](14-authority.md#resources)): the platform package's for an entry its root grant minted, and the sim's, which `sys` implements on every target, for an entry a [sim](22-testing.md#simulated-capabilities) constructor minted. That call contributes exactly these facts to the item's [digest](10-effects-digests.md#digests): wait exactly when the operation's head states `uses wait`; alloc exactly when the operation takes a `mem::Alloc`; may-trap exactly when it takes a `mem::Alloc` or adds a [capability table](14-authority.md#capability-tables) entry; terminates exactly when wait does not hold. A capability that no root grant of a target yields has values there only as sims.

```nan
// file: pause/package.nan
//! Measures a short pause on the host.

package pause {
    language 1
    targets x86_64-host
    entry main
} pause

// file: pause/pause.nan
//! Entry of the pause program.

pub closed enum PauseError derives Render {
    Revoked(sys::Revoked) from
    Cancelled(task::Cancelled) from
} PauseError

fn main(take world: host::World) fails PauseError uses wait {
    let started = try world.clock.elapsed_ms()
    try await world.clock.sleep(10)
    let finished = try world.clock.elapsed_ms()
    world.log.info("paused at least 10 ms", value: finished - started >= 10)
} main
```

```nan
pub fn uptime_ms() -> u64 {
    return host_platform::monotonic_ms() // error: name.unknown
} uptime_ms
```

## Controlled vocabulary

The **controlled vocabulary** is the set of verbs that std names use, one per concept, given in this table for the concepts that recur across std; a verb used by one family of operations is the one its defining section gives. A function computing a property without changing anything is named by a noun for that property (`len`, `capacity`, `kind`, `elapsed_ms`).

- A std function performing a concept of the table is named by that concept's verb, after any checked prefix ([name prefixes](03-packages-modules.md#name-prefixes)) or arithmetic prefix ([overflow](09-numbers.md#overflow)) and with the affixes of [std naming](#std-naming); other words may follow the verb.
- No snake_case std name contains a synonym of the table as a run of its underscore-separated words. The Synonyms column feeds the ranking of [unknown names](03-packages-modules.md#unknown-names).
- A projection of the whole contents as a view of another type is named `as_` followed by that type's noun: `Str.as_bytes`, `text::String.as_str`.

| Verb | Concept | Synonyms |
| --- | --- | --- |
| `empty` | make a value holding no elements | `new`, `init` |
| `of` | make a value holding given contents | `wrap`, `from_value` |
| `filled` | make an array of copies of one value | `repeat` |
| `zeroed` | make bytes of a given length, all zero | `zero_bytes` |
| `push` | add one element at the end | `push_back`, `enqueue` |
| `append` | add a run of elements or text at the end | `extend`, `concat` |
| `insert` | add an element at a key or position | `put`, `add_entry` |
| `pop` | remove and return the last element | `pop_back`, `dequeue` |
| `remove` | remove the element at a key or position, or a file | `delete`, `erase` |
| `retain` | keep the elements a function accepts | `filter`, `keep_if` |
| `truncate` | keep the leading elements, dropping the rest | `shorten` |
| `clear` | remove all contents, keeping storage | `reset`, `wipe` |
| `fill` | write one value to every element | `set_all` |
| `copy_from` | overwrite elements with those of another sequence | `assign`, `clone_from` |
| `contains` | test membership | `has`, `includes` |
| `position` | index of the first element a function accepts | `find_index` |
| `find` | position of a sought value or text | `index_of`, `search` |
| `at` | project the element at an index, key or id, trapping when absent | `item`, `element` |
| `get` | project the element at an index, key or id, or `None` | `retrieve`, `get_or_none` |
| `view` | project the whole contents as a slice of the same elements | `as_slice` |
| `cursor` | iterate | `iter` |
| `extract` | move the payload out of an `Option` place, leaving `None` | `replace`, `swap_out` |
| `contents` | reach the value inside a box, shared value or buffer | `deref`, `unbox`, `inner` |
| `boxed` | place a value in a store's box | `allocate`, `box_value` |
| `share` | make another handle to the same shared value | `clone`, `dup` |
| `split` | divide a capability, sender or frame into parts | `fork`, `divide` |
| `merge` | rejoin frames | `combine` |
| `close` | release a value gracefully, waiting when the release waits | `shutdown`, `dispose` |
| `abandon` | release a resource without waiting | `abort`, `drop`, `kill` |
| `revoke` | end every copy of a capability | `disable`, `withdraw` |
| `spawn` | start a task | `go`, `launch` |
| `cancel` | cancel a scope or keyed task | `stop` |
| `send` | deliver a value into a channel or into the component's own intake | `emit`, `dispatch` |
| `post` | deliver a value into an outbox or a signal | `notify`, `deliver` |
| `receive` | take the next value of a channel | `recv` |
| `drain` | consume every remaining value | `consume` |
| `wait` | suspend until a signal holds a value | `block`, `park` |
| `encode` | write the encoded form of a value | `serialize` |
| `decode` | read a value from its encoded form | `deserialize` |
| `render` | write a value as text within a render budget | `to_string`, `format`, `display` |
| `intern` | map text to a symbol, adding it when absent | `symbol_of` |
| `resolve` | map a symbol to its text | `text_of` |
| `inspect` | read the contents of an Untrusted value | `unwrap`, `reveal` |

```nan
pub fn reading_count(readings: mem::List[u32]) -> usize {
    return readings.len()
} reading_count
```

```nan
pub fn reading_count(readings: mem::List[u32]) -> usize {
    return readings.size() // error: name.unknown
} reading_count
```

## Std naming

Std names follow the [naming conventions](03-packages-modules.md#naming-conventions), the [controlled vocabulary](#controlled-vocabulary) and these conventions:

- An associated function returning a new value of its type is named by its verb (`List.empty`, `Shared.of`, `Bytes.zeroed`, `Channel.bounded`), never by its type's name.
- When std offers an operation in a trapping and a failing form, the failing form is named `try_` followed by the trapping form's name and has the same parameters: `push` and `try_push`, `of` and `try_of`.
- A `mut` projection beside a read one adds the suffix `_mut` (`at_mut`, `view_mut`, `get_by_mut`). A function taking a function that orders or selects elements adds the suffix `_by` (`sort_by`, `get_by`). The consuming form of a read projection adds the prefix `into_` (`into_cursor`, `into_contents`).
- An error enum is named after its subject followed by `Error` (`FsError`, `NetError`, `DecodeError`). A failure type of one condition is named after that condition (`mem::OutOfMemory`, `sys::Revoked`, `task::Cancelled`, `task::Poisoned`), and a function whose only failure is that condition fails with that type directly.
- A variant of a std error enum holding `task::Cancelled` is `Cancelled(task::Cancelled) from`, and one holding `sys::Revoked` is `Revoked(sys::Revoked) from`.
- A duration or time is a `u64` count of milliseconds whose name ends in `ms` (`ms`, `elapsed_ms`) or of nanoseconds whose name ends in `ns` (`ns`, `elapsed_ns`, `now_ns`).

```nan
pub fn record_all(mut history: mem::List[u32], values: Slice[u32]) fails mem::OutOfMemory {
    try mut history.try_reserve(values.len())
    for value in values {
        mut history.push(value)
    }
} record_all
```

## Reserved std names

The paths `sys::infer`, `sys::load` and `core::simd` are reserved: no std module or item has them, and a path whose first two segments are one of them is an error (`name.reserved`). The alias `ui` is reserved as [aliases](03-packages-modules.md#aliases) states.

```nan
pub fn lane_count(data: Slice[u32]) -> usize {
    return core::simd::lanes(data) // error: name.reserved
} lane_count
```
