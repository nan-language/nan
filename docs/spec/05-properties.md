# Structural properties

This chapter defines the six structural properties and how the checker computes them, then linear, droppable and affine types with the `Any` bound, views with their positions and arguments, state tokens, Copy, Send, Inert and AnyBits, capability types, promises and interface facts, and the core interfaces that name the properties.

## Structural properties

A **structural property** is one of linear, view, Copy, Send, Inert and AnyBits. The checker computes each property from a type's structure. Source states only `linear`, `view`, `derives Copy` and the [promises](#promises) of open types.

The **constituent types** of a struct or enum are its field types and variant payload types, with the instantiation's type arguments substituted. A type **contains** its constituent types, its type arguments and every type these contain. A **base type** is a std type whose owning chapter declares it a base type and fixes its six properties; the checker identifies base types by canonical path and never computes their properties from fields.

| Type | linear | view | Copy | Send | Inert | AnyBits |
| --- | --- | --- | --- | --- | --- | --- |
| `u8` to `u64`, `i8` to `i64`, `usize`, `isize`, `f32`, `f64`, `Unit` | no | no | yes | yes | yes | yes |
| `bool`, `char`, `Never` | no | no | yes | yes | yes | no |
| `Array[T, N]` | as `T` | as `T` | as `T` | as `T` | as `T` | as `T` |
| `Option[T]`, `Result[T, E]` | any argument | any argument | all arguments | all arguments | all arguments | no |
| `Untrusted[T]` | as `T` | as `T` | as `T` | as `T` | as `T` | no |
| `Slice[T]`, `SliceMut[T]` | no | yes | no | as `T` | as `T` | no |
| `Str` | no | yes | no | yes | yes | no |
| `Dyn[I]` | no | yes | no | no | no | no |
| state token | [as a struct](#linear-types) | yes | no | no | no | no |
| function type other than `task fn` | no | no | yes | no | yes | no |
| struct or enum | [declared or any argument](#linear-types) | [declared](#views) | [derived](#copy) | [all constituents](#send) | [all constituents](#inert) | [rule](#anybits) |
| layout | no | no | yes | yes | yes | [rule](#anybits) |
| base type | owning chapter | owning chapter | owning chapter | owning chapter | owning chapter | owning chapter |

- "as `T`" means exactly when `T` has the property, "any argument" when at least one type argument has it, and "all arguments" when every type argument has it.
- A state token has the properties of its own row, whose linear cell follows the struct row.
- A `task fn` type has the properties that [task closures](16-concurrency.md#task-closures) state.
- Inside a generic item, a type parameter is Copy, Send, Inert or AnyBits exactly when a bound names that property's interface; it is linear as [generic parameters](#generic-parameters) state and a view as [view arguments](#view-arguments) state.
- The checker decides Copy, Send, Inert and AnyBits of a generic struct or enum once, at its definition, assuming that every type parameter has the property.
- A type of the table has a **dependent parameter** of one of these properties wherever the property's cell reads "as `T`" or "all arguments", and a base type wherever its owning chapter makes the property follow a type argument. A **dependent position** of a property is a whole constituent type, or a type argument at a dependent parameter of a type at a dependent position. A type parameter of a struct or enum is a dependent parameter of the property when it occurs at a dependent position.
- An instantiation of a generic struct or enum is Copy, Send, Inert or AnyBits exactly when its definition is and every type argument at a dependent parameter of the property has it.

```nan
pub closed struct Sample derives Copy {
    pub channel: u8
    pub level: u16
} Sample

pub fn latest[T: Copy and Send](history: Slice[T]) -> Option[T] {
    if history.len() == 0 {
        return None
    }
    return Some(history[history.len() - 1])
} latest

pub fn latest_sample(history: Slice[Sample]) -> Option[Sample] {
    return latest(history)
} latest_sample
```

## Linear types

A **linear** type is one whose values must be consumed before their owner's scope ends. An owner consumes a linear value by moving it ([ownership](07-ownership.md#ownership)), which passes the obligation to the new owner, by destructuring it with a pattern, or, inside the type's defining module and when the type has a linear field, by moving every linear field out of it. [Linear consumption](07-ownership.md#linear-consumption) reports a value that is not consumed (`own.linear-dropped`).

A **droppable** type is any type that is not linear; its values may reach the end of their owner's scope unconsumed. An **affine** type is one that is neither Copy nor linear: its values move on use and may be dropped.

- A struct or enum is linear when it is declared with the `linear` [modifier](02-items.md#modifiers) or when a type argument of its instantiation is linear.
- A struct or enum with a constituent type that is linear when every type parameter of the item is droppable must be declared `linear`; otherwise it is an error (`prop.linear-undeclared`), reported at the first such field or variant in source order, whose exact fix inserts `linear`.
- `linear` may be declared on any struct or enum.

```nan
linear struct Lease {
    slot: u32
} Lease

linear struct Booking {
    lease: Lease
    guests: u8
} Booking

fn Lease.release(take self) -> u32 {
    let Lease(slot: slot) = self
    return slot
} Lease.release

pub fn book(slot: u32, guests: u8) -> u32 {
    let booking = Booking(lease: Lease(slot: slot), guests: guests)
    let Booking(lease: lease, guests: count) = booking
    let freed = take lease.release()
    return freed + u32.widen(count)
} book

pub fn longer(take left: mem::List[u8], take right: mem::List[u8]) -> mem::List[u8] {
    if left.len() >= right.len() {
        return left
    }
    return right
} longer
```

```nan
pub closed linear struct Lease {
    pub slot: u32
} Lease

pub closed struct Booking {
    pub lease: Lease // error: prop.linear-undeclared
    pub guests: u8
} Booking
```

### Generic parameters

- A type parameter bounded by `Any` accepts linear type arguments. Every other type parameter is droppable, and a linear type argument for it is an error (`prop.linear-argument`).
- The type parameters of `Array`, `Option`, `Result`, `Untrusted`, `Slice` and `SliceMut` are bounded by `Any`.
- Inside its item, a type parameter bounded by `Any` and not by `Copy` is linear, so the item consumes its values whatever the instantiation.
- A member of a base type counts a type parameter bounded by `Any` as droppable where its owning chapter says so. Calling it on an instantiation whose argument for that parameter is linear is an error (`prop.linear-argument`).

```nan
pub fn either[T](take primary: Option[T], take backup: T) -> T {
    let Some(value) = primary else {
        return backup
    }
    return value
} either

pub closed struct Labelled[T: Any] {
    pub label: u32
    pub item: T
} Labelled

pub fn Labelled.into_item(take self) -> T {
    let Labelled(label: _, item: item) = self
    return item
} Labelled.into_item
```

```nan
pub closed linear struct Lease {
    pub slot: u32
} Lease

pub closed linear struct Booking {
    pub lease: Lease
    pub guests: u8
} Booking

pub fn either[T](take primary: Option[T], take backup: T) -> T {
    let Some(value) = primary else {
        return backup
    }
    return value
} either

pub fn pick(take primary: Option[Booking], take backup: Booking) -> Booking {
    return either(take primary, take backup) // error: prop.linear-argument
} pick
```

The [record](08-generics-interfaces.md#instantiation) names the parameter, the argument and the field making it linear:

```text
$ nan check example
failed check example: errors 1, incomplete 0
error prop.linear-argument example/example.nan:19 example::pick
        return either(take primary, take backup) // error: prop.linear-argument
    expected: a droppable type for T of either
    found: Booking
    because declared example/example.nan:7 example::Booking lease: Lease
        pub lease: Lease
    because declared example/example.nan:2 example::Lease Lease
    pub closed linear struct Lease {
failed check example: errors 1, incomplete 0
```

## Views

A **view** is a value referring into memory it does not own. The view types are exactly `Slice[T]`, `SliceMut[T]`, `Str`, `Dyn[I]`, state tokens, structs and enums declared with the `view` [modifier](02-items.md#modifiers), the instantiations of `Array`, `core::ArrayCursor`, `Option`, `Result` and `Untrusted` with a view type argument, the base types their owning chapters declare views, and type parameters as [view arguments](#view-arguments) state.

- A struct or enum not declared `view` with a constituent type that is a view is an error (`prop.view-undeclared`), reported at the first such field or variant in source order, whose exact fix inserts `view`.
- `view` may be declared on any struct or enum, with or without `linear`.

```nan
pub closed view struct Field {
    pub name: Str
    pub value: Str
} Field

pub fn Field.is_named(self, name: Str) -> bool = self.name == name

pub proj fn Field.as_value(self) -> Str {
    return self.value
} Field.as_value
```

```nan
pub closed struct Entry {
    pub key: Str // error: prop.view-undeclared
    pub hits: u32
} Entry
```

### View positions

A view type appears only as:

- the type of a parameter or a local binding;
- the result type of a `proj fn` ([projections](07-ownership.md#projections)), or of any function when the view comes from a type parameter that accepts view arguments;
- a constituent type of a view type;
- the type of a `const` item ([constants](02-items.md#constants)), whose views then refer only to literal data and the data of consts and contributions ([const evaluation](21-interpreter.md#const-evaluation));
- the entry type of a [registry](25-registries.md#registries), under the same restriction;
- a type argument, as [view arguments](#view-arguments) permit.

- A function head, closure or function type whose result type is a view is an error unless it is a `proj fn` head (`prop.view-result`). A type parameter that accepts view arguments counts as no view in this rule.
- When a call of a function that is not a projection returns a view because a type argument is one, its result is derived from each argument and receiver of a view type, as a value they move into is ([holds](07-ownership.md#holds)).

```nan
pub fn default_name() -> Str { // error: prop.view-result
    return "unnamed"
} default_name
```

### View arguments

- A type parameter accepts view type arguments exactly when it is `Self` or a parameter of an interface, a parameter of a struct or enum declared `view`, or a parameter of `Array`, `core::ArrayCursor`, `Option`, `Result`, `Untrusted`, `Slice` or `SliceMut`, and no bound of it names `Copy`, `Send` or `AnyBits`. A view type argument for any other type parameter is an error (`prop.view-argument`), except `Dyn[I]` as the argument of `mem::Box`, which forms `mem::Box[Dyn[I]]` ([dynamic dispatch](08-generics-interfaces.md#dynamic-dispatch)).
- Inside its item, a type parameter that accepts view arguments is a view; every other type parameter is not.

```nan
pub closed struct Pair[T] {
    pub left: T
    pub right: T
} Pair

pub fn pair_matches(text: Str) -> bool {
    let pair = Pair(left: "key", right: "value") // error: prop.view-argument
    return pair.left == text or pair.right == text
} pair_matches
```

### Mutable views

Writing through a view, by an indexed assignment `v[i] = x` (which calls `at_mut`), by another `mut` projection of it or by passing it to a `mut` parameter, is a `mut` access to the place holding the view, so writing through a binding that [modes](07-ownership.md#modes) never access by `mut`, such as a `let` binding or a read parameter, is an error (`mode.immutable-binding`).

```nan
pub fn fill(mut out: SliceMut[u8], value: u8) {
    for i in 0..<out.len() {
        out[i] = value
    }
} fill
```

```nan
pub fn fill(out: SliceMut[u8], value: u8) {
    for i in 0..<out.len() {
        out[i] = value // error: mode.immutable-binding
    }
} fill
```

## State tokens

A **state token** is a struct declared `view` with no fields. It stands for a machine state that holds while its holder runs, such as `hw::IrqsDisabled` for maskable interrupts being disabled ([interrupt state](24-kernel.md#interrupt-state)).

- A function head, closure or function type with `uses wait` and a parameter whose type is or contains a state token is an error (`prop.token-wait`).
- A type that is or contains a state token is an error (`prop.token-argument`) as a type argument for a parameter that accepts [view arguments](#view-arguments), other than a parameter of `Array`, `core::ArrayCursor`, `Option`, `Result`, `Untrusted`, `Slice` or `SliceMut`, and as the target of an impl line.
- A state token live at a suspension point is an error that [views across suspension points](17-suspension-io.md#views-across-suspension-points) defines.

```nan
// file: ticker/package.nan
//! Tick counting for a bare kernel.

package ticker {
    language 1
    targets x86_64-bare
} ticker

// file: ticker/ticker.nan
//! An IRQ-safe tick counter.

pub closed struct Ticks {
    pub count: u64
} Ticks

pub fn bump(t: hw::IrqsDisabled, lock: hw::SpinLock[Ticks]) {
    lock.with(t, body: fn(mut ticks: Ticks) limits notrap, terminates, noalloc {
        ticks.count = ticks.count.saturating_add(1)
    })
} bump
```

```nan
// file: ticker/package.nan
//! Tick counting for a bare kernel.

package ticker {
    language 1
    targets x86_64-bare
} ticker

// file: ticker/ticker.nan
//! An IRQ-safe tick counter.

pub closed struct Ticks {
    pub count: u64
} Ticks

pub fn bump(t: hw::IrqsDisabled, lock: hw::SpinLock[Ticks], tick: mem::Shared[hw::Signal[u64]]) fails task::Cancelled uses wait { // error: prop.token-wait
    lock.with(t, body: fn(mut ticks: Ticks) limits notrap, terminates, noalloc {
        ticks.count = ticks.count.saturating_add(1)
    })
    discard try await tick.contents().wait()
} bump
```

```nan
pub closed view struct Quiet {
} Quiet

pub closed view struct Pinned[T] {
    pub value: T
} Pinned

pub fn is_pinned(pinned: Pinned[Quiet]) -> bool { // error: prop.token-argument
    return true
} is_pinned
```

## Copy

A **Copy** type is one whose values are copied, not moved, when assigned or passed.

- A struct or enum is Copy exactly when its `derives` clause ([derives](08-generics-interfaces.md#derives)) lists `Copy`.
- `derives Copy` on a struct or enum that is declared `linear` or `view`, or that has a constituent type that is not Copy when every type parameter is Copy, is an error (`prop.copy-ineligible`). It is the only code a `Copy` entry produces.
- Linear types and views are never Copy.

```nan
pub closed struct Point derives Copy, Eq {
    pub x: i32
    pub y: i32
} Point

pub fn mirror(p: Point) -> Point {
    let copy = p
    return Point(x: copy.y, y: p.x)
} mirror
```

```nan
pub closed struct Frame derives Copy { // error: prop.copy-ineligible
    pub seq: u32
    pub payload: mem::List[u8]
} Frame
```

## Send

A **Send** type is one whose values may pass to another task.

- A struct or enum is Send when all its constituent types are Send.
- A base type bound to one task or one CPU is not Send; its owning chapter says which base types are.
- A view reaches another task only by [lending](16-concurrency.md#lending), and a type parameter bounded by `Send` accepts no view ([view arguments](#view-arguments)).

```nan
pub closed struct Job {
    pub id: u32
    pub payload: mem::List[u8]
} Job

pub fn queue_len[T: Send](pending: Slice[T]) -> usize {
    return pending.len()
} queue_len

pub fn jobs_waiting(pending: Slice[Job]) -> usize {
    return queue_len(pending)
} jobs_waiting
```

## Inert

An **Inert** type is one whose values hold no capability other than the allocators that std base types store ([memory types](12-memory.md#memory-types)), and no [interior mutability](12-memory.md#interior-mutability).

- A struct or enum is Inert when all its constituent types are Inert.
- No primitive capability and no type with interior mutability is Inert.
- Inert is required by [const items](02-items.md#constants), `Encode` ([requirements](13-text-encode.md#requirements)), `Generate` ([generation](22-testing.md#generation)) and [component state](26-components.md#component-state); each owner names the code its violation produces.

```nan
pub closed struct Window {
    pub low: u32
    pub high: u32
} Window

pub const DEFAULT_WINDOW: Window = Window(low: 0, high: 512)
```

## AnyBits

An **AnyBits** type is one for which every bit pattern of its size is a valid value.

- A struct is AnyBits when it is declared neither `linear` nor `view`, has no [predicate](04-types.md#predicates) and every constituent type is AnyBits. An enum is never AnyBits, and neither is a linear type.
- A [layout](23-low-level.md#layouts) is AnyBits when it is not a tags layout and each of its `T at` fields has an AnyBits type `T`.
- AnyBits is required by `T.from_bits` ([bit patterns](09-numbers.md#bit-patterns)), `hw::UserAddr` ([address types](23-low-level.md#address-types)), [DMA buffers](23-low-level.md#dma), [register types](23-low-level.md#registers) and [foreign fn](23-low-level.md#foreign-functions) heads; each owner names the code its violation produces.

```nan
pub closed struct Reading {
    pub channel: u16
    pub level: i32
} Reading

pub fn raw_count[T: AnyBits](records: Slice[T]) -> usize {
    return records.len()
} raw_count

pub fn reading_count(readings: Slice[Reading]) -> usize {
    return raw_count(readings)
} reading_count
```

## Capability types

A **primitive capability** is a base type that its owning chapter declares a [capability](14-authority.md#capabilities); [Standard library 1.0](28-stdlib.md) lists every one. A **capability type** is a primitive capability, a struct or enum with a capability type among its constituent types, or an instantiation of a type of the table or a base type whose type argument at a dependent parameter of Inert is a capability type. An allocator that a std base type stores is no constituent type, so it makes no capability type.

- `derives Default`, `derives Encode` or `derives Generate` on a capability type is an error (`prop.capability-derive`). It is the only code that entry produces.

```nan
pub closed struct Reporter {
    pub log: sys::Log
    pub sent: u32
} Reporter

pub fn Reporter.report(mut self, reading: u32) {
    self.log.info("reading", value: reading)
    self.sent += 1
} Reporter.report
```

```nan
pub closed struct Reporter derives Default { // error: prop.capability-derive
    pub log: sys::Log
    pub sent: u32
} Reporter
```

## Promises

A **promise** is a property or [limit](10-effects-digests.md#limits-as-promises) of a `pub` item that consumers may rely on; losing one is breaking ([compatibility](29-identity-versioning.md#compatibility)).

- Consumers know every property of a [closed type](04-types.md#closed-and-open-types) from its head and from its constituent types as they see them, and every property of a base type from its owning chapter. Each of these properties is a promise.
- Of an [open type](04-types.md#closed-and-open-types) that is not a base type, consumers see its [interface facts](#interface-facts). They see Send, Inert and AnyBits only when its `derives` clause lists them; a property the clause does not list is absent for consumers, whatever the computation gives. Its promises are `Copy` when it derives `Copy` and each property its `derives` clause lists.
- Inside its package, an open type's properties are computed from its fields.
- A `derives` clause listing Send, Inert or AnyBits on an open type whose computation does not give that property is an error (`prop.promise-unmet`). It is the only code that entry produces. On a generic open type, the promise covers each instantiation whose type arguments at the property's dependent parameters have the property.
- A `derives` clause listing Send, Inert or AnyBits on a type that is not a `pub` open type is an error (`prop.promise-not-open`), whose exact fix deletes the entry.

```nan
pub open struct Counter derives Send, Inert {
    total: u64
} Counter

pub fn Counter.zero() -> Counter = Counter(total: 0)

pub fn Counter.value(self) -> u64 = self.total
```

```nan
pub open struct Level derives AnyBits { // error: prop.promise-unmet
    percent: u8 where percent <= 100
} Level
```

### Interface facts

The **interface facts** of a type are whether it is linear, a view, a state token or a type containing a state token, whether it derives `Copy`, its promises when it is `pub`, whether it is a [parsing type](15-untrusted.md#validator-classes) when it is a `pub` struct or enum, and the dependent parameters on which each of these rests. They enter the type's [interface hash](29-identity-versioning.md#hashes).

## Property interfaces

The **property interfaces** are the core interfaces `Copy`, `Send`, `Inert` and `AnyBits`, which name their properties in [bounds](08-generics-interfaces.md#generic-parameters), and `Any`, which lets a type parameter accept linear arguments ([generic parameters](#generic-parameters)). They have no methods.

```nan-api
// module: core

pub interface Any {
} Any

pub interface AnyBits {
} AnyBits

pub interface Copy {
} Copy

pub interface Inert {
} Inert

pub interface Send {
} Send
```

- Every type satisfies `Any`. A type satisfies `Copy`, `Send`, `Inert` or `AnyBits` exactly when it has that property where the bound is checked, except that no view satisfies `Send`.
- An impl line naming a property interface is an error (`prop.property-impl`).

```nan
pub closed struct Span derives Copy {
    pub low: u32
    pub high: u32
} Span
```

```nan
pub closed struct Span {
    pub low: u32
    pub high: u32
} Span

impl Span: Copy // error: prop.property-impl
```

## Diagnostics

| Code | Class | Rule |
| --- | --- | --- |
| `prop.capability-derive` | error | [Capability types](#capability-types), [Requirements](13-text-encode.md#requirements), [Generation](22-testing.md#generation) |
| `prop.copy-ineligible` | error | [Copy](#copy) |
| `prop.linear-argument` | error | [Generic parameters](#generic-parameters), [Lists and bytes](12-memory.md#lists-and-bytes), [Components](26-components.md#components) |
| `prop.linear-undeclared` | error | [Linear types](#linear-types) |
| `prop.promise-not-open` | error | [Promises](#promises) |
| `prop.promise-unmet` | error | [Promises](#promises) |
| `prop.property-impl` | error | [Property interfaces](#property-interfaces) |
| `prop.token-argument` | error | [State tokens](#state-tokens) |
| `prop.token-wait` | error | [State tokens](#state-tokens) |
| `prop.view-argument` | error | [View arguments](#view-arguments) |
| `prop.view-result` | error | [View positions](#view-positions) |
| `prop.view-undeclared` | error | [Views](#views) |
