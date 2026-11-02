# Registries

This chapter defines registry items with their key and `unique` fields, contribution items, the judging root and the contributions it admits through `allow contribute`, function-typed entry fields, the tables a registry holds with their duplicate rules, reads and emission, loaded registries, whose tables grow as a kernel loads bundles, the platform registries `hw::vectors` and `hw::preempt`, and the `registry` diagnostic family.

## Registries

A **registry** is a typed table declared by a `registry` item and filled by [contributions](#contributions) across packages. Its **entry type** is the type after `:`, its **key field** is the field named after `keyed by`, and its **unique fields** are the fields its `unique` list names.

```ebnf
registry_item = 'loaded'? "registry" IDENT ":" type_expr 'keyed' 'by' IDENT unique_list? NL ;
unique_list = 'unique' IDENT ( "," IDENT )* ;
```

- A registry is a [single-line item](02-items.md#item-shape) with an optional [visibility](03-packages-modules.md#visibility) and no modifiers. Its name is the `IDENT` after `registry`. A registry written `loaded` is a [loaded registry](#loaded-registries).
- The entry type is a struct type that a [const item](02-items.md#constants) may have: [Inert](05-properties.md#inert) and not [linear](05-properties.md#linear-types). It may be a [view](05-properties.md#views), whose views in each entry then refer only to literal data and to the data of consts and contributions. Any other entry type is an error (`registry.entry-type`).
- The key field and each unique field name a field of the entry type (`registry.unknown-field`) that is visible in the registry's module (`name.private`).
- The key field's type implements `Ord` (`registry.key-type`). Each unique field's type implements `Eq` (`registry.unique-type`).
- A unique list naming the key field, or naming a field twice, is an error (`registry.unique-repeated`); the exact fix deletes the repeated name.

```nan
pub closed struct Command {
    pub opcode: u8
    pub run: fn(arg: u32) -> u32 limits notrap
} Command

pub registry commands: Command keyed by opcode

contribute commands double_cmd = Command(opcode: 1, run: double)

contribute commands halve_cmd = Command(opcode: 2, run: halve)

fn double(arg: u32) -> u32 limits notrap {
    return arg.saturating_mul(2)
} double

fn halve(arg: u32) -> u32 limits notrap {
    return arg / 2
} halve

pub fn dispatch(opcode: u8, arg: u32) -> Option[u32] {
    for command in commands {
        if command.opcode == opcode {
            return Some((command.run)(arg))
        }
    }
    return None
} dispatch

test dispatch_reaches_double for dispatch {
    let doubled = dispatch(1, arg: 21)
    let missing = dispatch(9, arg: 21)
    expect doubled == Some(42)
    expect missing == None
} dispatch_reaches_double
```

```nan
pub closed struct Gain {
    pub level: f32
    pub apply: fn(sample: u32) -> u32
} Gain

pub registry gains: Gain keyed by level // error: registry.key-type
```

## Contributions

A **contribution** is a `contribute` item adding one entry to a registry.

```ebnf
contribute_item = "contribute" path IDENT "=" expr NL ;
```

- The path names a registry visible to the contribution's module (`name.private`); a path naming another item is an error (`registry.not-registry`). The `IDENT` is the contribution's name.
- The expression is the contribution's value. Its expected type is the registry's entry type, and it is a [const context](21-interpreter.md#const-evaluation): it is checked as a const initializer is and computed by const evaluation.
- A path naming a contribution is an expression yielding its value, as the path of a const does.
- A contribution exists under each target its module exists under ([target variants](03-packages-modules.md#target-variants)).

```nan
pub closed struct Port {
    pub number: u16
    pub irq: u8
} Port

pub registry ports: Port keyed by number

contribute ports com1 = Port(number: 0x3F8, irq: 4)

pub fn first_serial_irq() -> u8 = com1.irq
```

```nan
pub const MAX_OPCODE: u8 = 8

contribute MAX_OPCODE first_opcode = 1 // error: registry.not-registry
```

## Contributors

Registry rules are judged against a **judging root**. At a [final build](27-program-model.md#final-builds), the judging root is the final-link root; at the build of a test image, it is the package whose tests the image runs. Under `nan check`, `nan test` and `nan verify`, it is each package of the scope, and for `nan find --contributors` the package it names ([reading commands](33-toolchain.md#reading-commands)). A contribution to a registry `R` is **admitted** by the judging root exactly when the contribution's package declares `R`, is the judging root, or is named, by an alias its `dep` directive declares, in the judging root's `allow contribute R in <aliases>` directive. A judging root also admits every contribution of the [platform package](28-stdlib.md#platform-packages) that its target's descriptor names. The manifests of other packages admit nothing.

- In an `allow contribute R in <names>` directive, `R` is a path resolved as in the package's root module and naming a registry (`registry.not-registry`), and each name is an alias that a `dep` directive of the manifest declares (`registry.grant-not-alias`).
- A contribution in the judging root's dependency closure that is not admitted is an error (`registry.ungranted`), reported once for each registry and contributing package, on the judging root's `dep` directive for that package, or on its `package` line when it declares no alias for it.
- The exact fix of `registry.ungranted` adds the contributing package's alias to the `allow contribute R` directive, writing the directive when the manifest has none. For the contributing package and for the package declaring `R`, when the manifest declares no alias, the fix first adds a `dep` directive whose alias is the package's name and whose source is the one the [lock](29-identity-versioning.md#lock) records. When that alias is already declared for another package, the diagnostic offers no exact fix.
- A name in an `allow contribute R` directive whose package declares `R`, or makes no contribution to `R` under any declared target, is incompleteness (`unused.grant`).

```nan
// file: codecs/package.nan
//! Frame codec registry.

package codecs {
    language 1
    targets any
} codecs

// file: codecs/codecs.nan
//! The table of frame codecs, keyed by frame tag.

pub closed struct Codec {
    pub tag: u8
    pub payload_len: fn(header: u16) -> u16 limits notrap
} Codec

pub registry formats: Codec keyed by tag

// file: framing/package.nan
//! Short-frame codec.

package framing {
    language 1
    targets any
    dep codecs "../codecs"
} framing

// file: framing/framing.nan
//! Decodes the payload length of short frames.

pub const SHORT_TAG: u8 = 1

contribute codecs::formats short_frames = codecs::Codec(tag: SHORT_TAG, payload_len: short_len)

fn short_len(header: u16) -> u16 limits notrap {
    return header & 0x00FF
} short_len

// file: monitor/package.nan
//! Frame codec census.

package monitor {
    language 1
    targets x86_64-host
    entry main
    dep codecs "../codecs"
    dep framing "../framing"
    allow contribute codecs::formats in framing
} monitor

// file: monitor/monitor.nan
//! Logs how many frame codecs the image holds.

fn main(take world: host::World) fails task::Cancelled uses wait {
    world.log.info("codecs", value: codecs::formats.len())
    world.log.info("short frame tag", value: framing::SHORT_TAG)
} main
```

```nan
// file: codecs/package.nan
//! Frame codec registry.

package codecs {
    language 1
    targets any
} codecs

// file: codecs/codecs.nan
//! The table of frame codecs, keyed by frame tag.

pub closed struct Codec {
    pub tag: u8
    pub payload_len: fn(header: u16) -> u16 limits notrap
} Codec

pub registry formats: Codec keyed by tag

// file: framing/package.nan
//! Short-frame codec.

package framing {
    language 1
    targets any
    dep codecs "../codecs"
} framing

// file: framing/framing.nan
//! Decodes the payload length of short frames.

pub const SHORT_TAG: u8 = 1

contribute codecs::formats short_frames = codecs::Codec(tag: SHORT_TAG, payload_len: short_len)

fn short_len(header: u16) -> u16 limits notrap {
    return header & 0x00FF
} short_len

// file: monitor/package.nan
//! Frame codec census.

package monitor {
    language 1
    targets x86_64-host
    entry main
    dep codecs "../codecs"
    dep framing "../framing" // error: registry.ungranted
} monitor

// file: monitor/monitor.nan
//! Logs how many frame codecs the image holds.

fn main(take world: host::World) fails task::Cancelled uses wait {
    world.log.info("codecs", value: codecs::formats.len())
    world.log.info("short frame tag", value: framing::SHORT_TAG)
} main
```

## Function fields

A **function field** is a field of an entry type whose type is a [function type](10-effects-digests.md#function-types), or holds one.

- The [fit rule](10-effects-digests.md#fit) applies to each function value in a contribution's value and its field's type, and reports at the contribution.
- A call through a function field of a table entry is an [indirect call](10-effects-digests.md#indirect-calls), with the declared digest of the field's type.
- A function stored by a contribution is reached through its [checking entry](18-contracts.md#checking-entries).

```nan
pub closed struct Command {
    pub opcode: u8
    pub run: fn(arg: u32) -> u32 limits notrap
} Command

pub registry commands: Command keyed by opcode

contribute commands per_hundred_cmd = Command(opcode: 3, run: per_hundred) // error: limit.may-trap

fn per_hundred(arg: u32) -> u32 {
    return 100 / arg
} per_hundred
```

## Tables

The **table** of a registry under a target is the sequence of the values of the admitted contributions to it in the judging root's dependency closure that exist under that target. The checker builds it by taking those contributions in byte order of their [canonical paths](29-identity-versioning.md#canonical-paths) and inserting each value immediately before the first entry already in the table whose key field is greater by the key field type's `Ord.cmp`, or at the end when none is. The checker computes `Ord.cmp` and `Eq.eq` on these values by [const evaluation](21-interpreter.md#const-evaluation). A diagnostic ending such an evaluation ([run ends](21-interpreter.md#run-ends)) is reported where a duplicate would be: on the contribution being inserted when it lies in the judging root, and otherwise on the judging root's `dep` directive for its package, or on its `package` line when it declares no alias for that package.

- Two admitted contributions to one registry that exist under a common target the judging root declares, or under the target of its final build, are an error when their key fields compare `Equal` by `Ord.cmp` (`registry.duplicate-key`), or when their values of a unique field are equal by `Eq.eq` (`registry.duplicate-unique`).
- Each such diagnostic quotes every contribution of the set and is reported for every contribution of the set except the one whose canonical path sorts first in byte order: on the contribution when it lies in the judging root, and otherwise on the judging root's `dep` directive for its package, or on its `package` line when it declares no alias for that package.
- A registry keyed by a field of type `Unit` therefore holds at most one entry.
- A path naming a registry other than a loaded one is an expression of type `Slice[E]`, where `E` is the entry type, viewing the table in read-only memory of the image. Every read of it yields the same slice. It names a value, never a [place](07-ownership.md#places), and reading it adds no [digest fact](10-effects-digests.md#digests).
- An [interpreter](21-interpreter.md#interpreter) run holds no table: reading a registry in one is uninterpretable ([run ends](21-interpreter.md#run-ends)).
- A final build, and the build of a test image, emits the table of each registry in its reach as read-only data of the image: an array of the entries in table order, each laid out as [lowering](30-artefacts.md#lowering) lays out the entry type. A function field holds the function's checking entry when it has one.

```nan
pub closed struct Port {
    pub number: u16
    pub irq: u8
} Port

pub registry ports: Port keyed by number unique irq

contribute ports com2 = Port(number: 0x2F8, irq: 3)

contribute ports com1 = Port(number: 0x3F8, irq: 4)

pub fn lowest_port() -> Option[u16] {
    let table = ports
    if table.len() == 0 {
        return None
    }
    return Some(table[0].number)
} lowest_port

test ports_sort_by_number for lowest_port {
    expect lowest_port() == Some(0x2F8)
} ports_sort_by_number
```

```nan
pub closed struct Port {
    pub number: u16
    pub irq: u8
} Port

pub registry ports: Port keyed by number unique irq

contribute ports com1 = Port(number: 0x3F8, irq: 4)

contribute ports com3 = Port(number: 0x3E8, irq: 4) // error: registry.duplicate-unique
```

```nan
pub closed struct Port {
    pub number: u16
    pub irq: u8
} Port

pub registry ports: Port keyed by number

contribute ports com1 = Port(number: 0x3F8, irq: 4)

contribute ports com1_alias = Port(number: 0x3F8, irq: 3) // error: registry.duplicate-key
```

```nan
pub closed struct Port {
    pub number: u16
    pub irq: u8
} Port

pub registry ports: Port keyed by number

pub const PORT_COUNT: usize = ports.len() // error: const.uninterpretable
```

## Loaded registries

A **loaded registry** is a registry whose table a kernel extends while it runs: [loading](24-kernel.md#loading) a bundle adds the entries of its contributions, and unloading it removes them.

- Loading compares key and unique fields itself: the key field of a loaded registry has an integer type, `bool`, `char` or `Str`, any other type being an error (`registry.key-type`), and so has each of its unique fields (`registry.unique-type`). It orders them as `Ord.cmp` of these types does: integers, `bool` and `char` by value, `Str` by its bytes and then its length.
- Its entry type holds a function type only as the type of one of its fields (`registry.loaded-entry`). Such a field's type states neither `pure` nor `notrap`, since a call through the field of an unloaded bundle's entry traps ([loading](24-kernel.md#loading)); each of its parameter, result and failure types is a **boundary type**; and none of its `mut` parameters has a view type (`registry.loaded-field`). A boundary type is the unit type, `Never`, a type implementing `core::Encode`, a [capability kind](14-authority.md#capability-kinds), `sys::Outbox[P]` or a primitive capability other than `boot::Machine`, `boot::Kernel`, `boot::Services` and `hw::IrqShared[T]`, through which code installs functions that outlive a call.
- A path naming a loaded registry is an expression of type `sys::load::Table[E]`, where `E` is the entry type, naming the registry's table. It names a value, never a place, and reading it adds no digest fact.
- The table's **versions** are the successive sequences of its entries: its first is the table that [Tables](#tables) gives the final build, and loading publishes each later one. A version never changes.

```nan-api
// module: sys::load

pub open struct Table[E: Any] {
} Table

pub fn Table.read[T: Any](self, body: fn(entries: Slice[E]) -> T) -> T
```

- `table.read(body)` gives what `body` returns for the entries of the current version, in table order, which the read holds until `body` returns, or until the settlement of its domain when `body` never does. It traps (`trap.exhausted`) when runtime memory cannot record the read. A version a later one replaced is reclaimed once no read holds it ([loading](24-kernel.md#loading)).
- `sys::load::Table[E]` is a base type: Copy and Send, never linear, a view, Inert or AnyBits.
- An [interpreter](21-interpreter.md#interpreter) run calling `read` is uninterpretable ([run ends](21-interpreter.md#run-ends)).

```nan
pub closed struct Driver {
    pub vendor: u16
    pub class: u8
    pub probe: fn(device: u32) -> bool
} Driver

pub loaded registry drivers: Driver keyed by vendor unique class

contribute drivers serial = Driver(vendor: 0x1AF4, class: 7, probe: accepts)

fn accepts(device: u32) -> bool {
    return device != 0
} accepts

pub fn probed(vendor: u16, device: u32) -> bool {
    let table = drivers
    return table.read(fn(entries: Slice[Driver]) -> bool {
        for driver in entries {
            if driver.vendor == vendor {
                return (driver.probe)(device)
            }
        }
        return false
    })
} probed
```

```nan
pub closed struct Codec {
    pub tag: f32
    pub decode: fn(byte: u8) -> u8
} Codec

pub loaded registry codecs: Codec keyed by tag // error: registry.key-type
```

```nan
pub closed struct Hook {
    pub slot: u8
    pub run: Option[fn(value: u32) -> u32]
} Hook

pub loaded registry hooks: Hook keyed by slot // error: registry.loaded-entry
```

```nan
pub closed struct Gauge {
    pub slot: u8
    pub read: fn(raw: u32) -> u32 limits notrap
} Gauge

pub loaded registry gauges: Gauge keyed by slot // error: registry.loaded-field
```

```nan
pub closed struct Maker {
    pub slot: u8
    pub make: fn(seed: u32) -> fn(value: u32) -> u32
} Maker

pub loaded registry makers: Maker keyed by slot // error: registry.loaded-field
```

## Platform registries

A **platform registry** is a std registry binding [compiler stubs](23-low-level.md#machine-seams) to handlers. The platform registries are exactly `hw::vectors` and `hw::preempt`:

```nan-api
// module: hw

pub registry vectors: Vector keyed by vector

pub closed struct Vector {
    pub vector: u8
    pub ist: u8 where ist <= 7
    pub handler: VectorHandler
} Vector

pub closed enum VectorHandler {
    Irq(interrupt fn(take irq: IrqContext) -> IrqExit)
    Trap(interrupt fn(mut frame: TrapFrame[NoCode]) -> IrqExit)
    TrapWithCode(interrupt fn(mut frame: TrapFrame[ErrorCode]) -> IrqExit)
    PageFault(interrupt fn(mut frame: TrapFrame[PageFault]) -> IrqExit)
} VectorHandler

pub registry preempt: PreemptEntry keyed by slot

pub closed struct PreemptEntry {
    pub slot: Unit
    pub handler: fn()
} PreemptEntry
```

- The **landing field** is `hw::PreemptEntry.handler`.
- The target's [descriptor](27-program-model.md#descriptors) assigns each vector a class, one of the four `hw::VectorHandler` variant names or none, and lists the vectors that need a nonzero `ist` or that every vector takes `ist` 0.
- These are errors (`registry.platform-entry`): an `hw::Vector` whose vector has no class or a class other than its handler's variant name, or whose `ist` is 0 for a vector that needs a nonzero one or nonzero for a vector that takes 0; a landing field holding a function that is not a [landing fn](23-low-level.md#landing-functions); and a contribution to `hw::preempt` outside a [platform package](28-stdlib.md#platform-packages).
- A contribution to `hw::vectors` or `hw::preempt` under a declared target without vector seams ([machine seams](23-low-level.md#machine-seams)) is an error (`registry.platform-target`).

The `hw::vectors` entry whose key field is a vector **binds** that vector. The seams use the tables as follows:

- On x86_64 the gate of each vector holds the entry's `ist` as its interrupt stack table index, 0 selecting none; on AArch64 every vector's seam runs on the CPU's interrupt stack of `ist` 0.
- The seam of a bound vector calls the interrupt fn of `handler` ([interrupt functions](24-kernel.md#interrupt-functions)), that of `hw::SIGNAL_VECTOR` once the CPU has served its shootdowns ([cross-CPU operations](24-kernel.md#cross-cpu-operations)), except for an exception raised in user mode ([user mode](24-kernel.md#user-mode)). Interrupt functions fixes what the seam of an unbound vector does.
- The vector seams call the `handler` of the `hw::preempt` entry, which the platform package contributes, where [preemption](24-kernel.md#preemption) has the interrupted thread yield.

```nan
// file: kernel/package.nan
//! Timer interrupts for a bare kernel.

package kernel {
    language 1
    targets x86_64-bare
} kernel

// file: kernel/kernel.nan
//! Binds the timer vector.

interrupt fn on_timer(take irq: hw::IrqContext) -> hw::IrqExit limits stack 1_024 {
    take irq.complete()
    return hw::IrqExit.Reschedule
} on_timer

contribute hw::vectors timer = hw::Vector(vector: 0x20, ist: 0, handler: hw::VectorHandler.Irq(on_timer))
```

```nan
// file: kernel/package.nan
//! Timer interrupts for a bare kernel.

package kernel {
    language 1
    targets x86_64-bare
} kernel

// file: kernel/kernel.nan
//! Binds the timer handler to the page-fault vector.

interrupt fn on_timer(take irq: hw::IrqContext) -> hw::IrqExit limits stack 1_024 {
    take irq.complete()
    return hw::IrqExit.Reschedule
} on_timer

contribute hw::vectors timer = hw::Vector(vector: 14, ist: 0, handler: hw::VectorHandler.Irq(on_timer)) // error: registry.platform-entry
```

## Diagnostics

| Code | Class | Rule |
| --- | --- | --- |
| `registry.duplicate-key` | error | [Tables](#tables) |
| `registry.duplicate-unique` | error | [Tables](#tables) |
| `registry.entry-type` | error | [Registries](#registries) |
| `registry.grant-not-alias` | error | [Contributors](#contributors) |
| `registry.key-type` | error | [Registries](#registries), [Loaded registries](#loaded-registries) |
| `registry.loaded-entry` | error | [Loaded registries](#loaded-registries) |
| `registry.loaded-field` | error | [Loaded registries](#loaded-registries) |
| `registry.not-registry` | error | [Contributions](#contributions), [Contributors](#contributors) |
| `registry.platform-entry` | error | [Platform registries](#platform-registries) |
| `registry.platform-target` | error | [Platform registries](#platform-registries) |
| `registry.ungranted` | error | [Contributors](#contributors) |
| `registry.unique-repeated` | error | [Registries](#registries) |
| `registry.unique-type` | error | [Registries](#registries), [Loaded registries](#loaded-registries) |
| `registry.unknown-field` | error | [Registries](#registries) |
