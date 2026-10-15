# Items

This chapter defines the item: its shape and the column-0 rule, the fourteen item kinds with the `item` and `source_file` rules, resynchronisation, order and names, modifiers, function heads, methods, transparent functions, the clause region, constants, and the reserved forms of item heads.

## Item shape

An **item** is a construct of one of the fourteen [item kinds](#item-kinds) that starts at [column 0](01-lexical.md#lines). It reads `[pub|pkg] [modifiers] kind Name` followed by what its kind defines: an optional [visibility](03-packages-modules.md#visibility), the [modifiers](#modifiers), the kind keyword and the item's name. A disputed test starts with its `todo` marker ([todo](18-contracts.md#todo)). An item's **name** is the identifier its kind's rule declares, and a function's name is the whole `item_label` after `fn`. Impl lines, const asserts and tombstones have no name; [Order and names](#order-and-names) gives their identity.

Every item is braced or single-line:

- A braced item ends with its closing line: `}` at column 0 followed by the **closing label**, as in `} Ring.pop`. Structs, enums, interfaces, layouts, registers blocks, tests, packages and functions with a body block are braced.
- A **single-line item** is written on one [logical line](01-lexical.md#lines): impl lines, const items, const asserts, registries, contributions, tombstones, transparent functions and foreign fns. Its only `NL` is its last token: an `NL` inside a `{` that the item opens is a syntax failure (`item.multiple-lines`), reported on the item's first line.

```ebnf
item_label = IDENT ( "." IDENT | "::" IDENT ( "::" IDENT )? "." IDENT )? ;
```

The rule of each braced kind ends with `COL0 "}" item_label NL`.

- The closing label is the item's name, spelt as the item's head spells it. A missing or different label is an error (`item.label-mismatch`); its exact fix writes the name.
- Exactly these tokens are at column 0: a file's `//!` summary; each [doc comment](32-docs.md#doc-comments) line of an item, and the token after the last one or, without a doc comment, the item's first token; the `}` of a closing line; and the `{` that opens a function body after a [clause region](#clause-region).
- A [syntax failure](#resynchronisation) at the first token of a physical line, not at column 0, that the grammar accepts at that position after a `COL0` is an error (`item.indented`); its exact fix removes the indentation of the line.
- A syntax failure at a token at column 0, or at the `COL0` before it, is an error (`item.stray-column-zero`). When the grammar accepts the token at that position without the `COL0`, the exact fix gives the line its [canonical indentation](35-format.md#depth).
- A rule naming its own code for a syntax failure, such as the body rules of [Heads](#heads), takes precedence over both.
- Items never nest: every item is a direct part of its source file.

```nan
pub const STEP: u32 = 5

pub closed struct Gauge {
    pub level: u32
} Gauge

pub fn Gauge.empty() -> Gauge = Gauge(level: 0)

pub fn Gauge.raise(mut self) {
    self.level = self.level.saturating_add(STEP)
} Gauge.raise
```

```nan
pub fn double(level: u32) -> u32 {
    return level * 2
} doubled // error: item.label-mismatch
```

```nan
pub closed enum Mode {
    Idle
    Busy
} Mode

pub fn Mode.is_busy(self) -> bool = match self { // error: item.multiple-lines
    Mode.Idle => false
    Mode.Busy => true
}
```

## Item kinds

The **item kinds** are exactly `fn`, `struct`, `enum`, `interface`, `impl`, `layout`, `registers`, `const`, `const assert`, `registry`, `contribute`, `test`, `retired` and `package`.

| Kind | Shape | Visibility | Modifiers | Defined in |
| --- | --- | --- | --- | --- |
| `fn` | braced; single-line when transparent or foreign | yes | function | [Heads](#heads) |
| `struct` | braced | yes | type | [Data types](04-types.md) |
| `enum` | braced | yes | type | [Data types](04-types.md) |
| `interface` | braced | yes | none | [Generics and interfaces](08-generics-interfaces.md) |
| `impl` | single-line | no | none | [Generics and interfaces](08-generics-interfaces.md) |
| `layout` | braced | yes | none | [Low-level programming](23-low-level.md) |
| `registers` | braced | yes | none | [Low-level programming](23-low-level.md) |
| `const` | single-line | yes | none | [Constants](#constants) |
| `const assert` | single-line | no | none | [Constants](#constants) |
| `registry` | single-line | yes | none | [Registries](25-registries.md) |
| `contribute` | single-line | no | none | [Registries](25-registries.md) |
| `test` | braced | no | none | [Testing and evidence](22-testing.md) |
| `retired` | single-line | no | none | [Identity and versioning](29-identity-versioning.md) |
| `package` | braced | no | none | [Packages, modules and names](03-packages-modules.md) |

```ebnf
source_file = ( COL0 ( FILE_DOC NL | item_start ) item* )? EOF ;
item = COL0 item_start ;
item_start = ( DOC NL COL0 )* visibility? item_modifier* item_kind ;
item_modifier = type_modifier | fn_modifier | reserved_head_word ;
item_kind = fn_item
    | struct_item
    | enum_item
    | interface_item
    | impl_item
    | layout_item
    | registers_item
    | "const" ( const_item | const_assert_item )
    | registry_item
    | contribute_item
    | test_item
    | retired_item
    | package_item ;
```

- `item` reads the item's column-0 token, its doc comment lines, its visibility and its modifiers. The rule of each kind starts at its kind keyword, except that `test_item` starts with its optional disputed marker and that `item_kind` reads the `const` the two constant kinds share.
- A `FILE_DOC` line other than the first token of its file, and a `DOC` line that documents nothing, are not syntax failures: the parser reads them as plain comments ([doc comments](32-docs.md#doc-comments)).
- Visibility on an item of a kind marked "no" in the table is an error (`item.visibility-not-allowed`); its exact fix deletes it.

```nan
pub const MAX_LEVEL: u8 = 10

const assert MAX_LEVEL > 0

pub closed enum Mode derives Eq {
    Idle
    Busy(u8)
} Mode

pub fn Mode.level(self) -> u8 {
    return match self {
        Mode.Idle => 0
        Mode.Busy(level) => level
    }
} Mode.level

test idle_has_no_level for Mode.level {
    let mode = Mode.Idle
    expect mode.level() == 0
} idle_has_no_level
```

```nan
pub fn is_idle(level: u32) -> bool = level == 0

pub test zero_is_idle for is_idle { // error: item.visibility-not-allowed
    expect is_idle(0)
} zero_is_idle
```

## Resynchronisation

The parser recovers from a syntax failure at the next item, so one damaged item never hides the others.

1. A **syntax failure** is a token the grammar does not accept at its position, as for the [first dead token](41-grammar.md#prefix-viability), or a rejected alternative once parsed. The parser reports it with the code a rule of the specification names for it, or else with a code of [Syntax failures](34-diagnostics.md#syntax-failures), and abandons the item it is reading.
2. Starting at the failing token, or at the `COL0` immediately before it, the parser discards tokens until it reaches `EOF` or a `COL0` followed by a token that can start an item: `///`, `pub`, `pkg`, `todo`, a modifier, a reserved item-head word or a kind keyword.
3. It resumes by reading an item at that `COL0`.

- A [conflict block](33-toolchain.md#conflicts) is read as one abandoned item: the parser abandons the item it is reading, if any, at the block's first line, discards every token through its last line, and continues as step 2 states.
- An abandoned item reports no diagnostic beyond its syntax failure and the lexical diagnostics of its lines.
- Every other item is checked as if the abandoned item were absent, with three exceptions while a module holds an abandoned item: a use that [names nothing](03-packages-modules.md#unknown-names) reports nothing when it is looked up in that module: a one-segment name used there, a path whose module part names that module, or a member of a type declared there or targeted by an impl line there; an impl line of that module reports no [missing method](08-generics-interfaces.md#conformance); and no [unused code](31-trust.md#unused-code) is reported in its package.

```nan
pub fn double(level: u32) -> u32 {
return level * 2 // error: item.stray-column-zero
} double

pub fn triple(level: u32) -> u32 {
    return level * 3
} triple
```

## Order and names

Item order carries no meaning. An item may name any item of its module and any item of another module visible to it, wherever that item is written. Moving an item within its file or to another source file of the same module changes the meaning and acceptance of no item.

- Two named items of one module with the same name are an error (`item.duplicate`), whatever their kinds. Two function names `Type.name` are the same when they name the same type and the same member name.
- Two functions named after one type with the same member name in two modules are an error (`item.duplicate`), reported on the one outside the type's module.
- A function named `Type.name` whose `name` is a field or variant of `Type` is an error (`item.duplicate`).
- Two parameters of one head with the same name are an error (`item.duplicate`).
- The identity of a named item is its name. An impl line's identity is its impl target and interface ([coherence](08-generics-interfaces.md#coherence)), a tombstone's is its old form ([tombstones](29-identity-versioning.md#tombstones)), and a const assert's is its condition, normalised as for its [item hash](29-identity-versioning.md#hashes). Two tombstones or two const asserts of one module with the same identity are an error (`item.duplicate`).
- `item.duplicate` is reported on each member of a set of equal names or identities except the first, ordered by file path in byte order and then by position in the file.

```nan
// file: example/package.nan
//! Window defaults.

package example {
    language 1
    targets any
} example

// file: example/example.nan
//! Window construction.

pub fn default_window() -> Window = Window(low: 0, high: WIDTH)

// file: example/window.nan
//! The window type and its default width.

const WIDTH: u32 = 64

pub closed struct Window {
    pub low: u32
    pub high: u32
} Window
```

```nan
pub fn retries() -> u32 = 3

pub fn retries() -> u32 = 5 // error: item.duplicate
```

## Modifiers

A **modifier** is a word between the visibility and the kind keyword that changes the rules of an item.

```ebnf
type_modifier = 'closed' | 'open' | "linear" | 'view' ;
fn_modifier = "proj" | "interrupt" | "landing" | "foreign" | "validator" ;
```

- Type modifiers precede only `struct` and `enum`, and function modifiers only `fn`; any other placement is an error (`item.modifier-misplaced`).
- A repeated type modifier, `closed` beside `open`, and a second function modifier are errors (`item.modifier-conflict`); for a repeated modifier, the exact fix deletes the repetition.
- A `pub` struct or enum states `closed` or `open`; one stating neither is an error (`item.openness-missing`). A private or `pkg` struct or enum stating either is an error (`item.openness-not-pub`); its exact fix deletes the word.
- `nan fmt` writes type modifiers in the order `closed` or `open`, `linear`, `view`.
- `closed` and `open` are defined in [Data types](04-types.md#closed-and-open-types); `linear` in [Structural properties](05-properties.md#linear-types); `view` in [Structural properties](05-properties.md#views); `proj` in [Ownership and access](07-ownership.md#projections); `interrupt` in [Kernel execution model](24-kernel.md#interrupt-functions); `landing` and `foreign` in [Low-level programming](23-low-level.md#landing-functions); `validator` in [Untrusted data](15-untrusted.md#validators).

```nan
pub open linear struct Lease {
    slot: u32
} Lease

pub fn Lease.grant(slot: u32) -> Lease = Lease(slot: slot)

pub fn Lease.release(take self) -> u32 {
    let Lease(slot: freed) = self
    return freed
} Lease.release
```

```nan
pub struct Point { // error: item.openness-missing
    pub x: i32
    pub y: i32
} Point
```

## Heads

The **head** of an item is its text before its clause region, body or members. A function head states its visibility, modifier, name, generic parameters, parameters, result, `fails`, `uses wait` and `limits`, in that order, and a foreign fn's head ends with its calling convention. A **parameter** is a named, typed input of a function with a [mode](07-ownership.md#modes). The **body** of a function is its braced statements, or the expression after `=` of a [transparent function](#transparent-functions).

```ebnf
fn_item = "fn" fn_head fn_end ;
fn_head = item_label generic_params? "(" param_list? ")" fn_result? fails_clause? uses_clause? limits_clause? ;
fn_result = "->" type_expr ;
param_list = param ( "," param_list? )? ;
param = mode? ( IDENT ":" type_expr | "self" self_field_set? ) ;
fn_end = convention_clause NL
    | "=" expr NL
    | "{" fn_body
    | NL clause_region COL0 "{" fn_body ;
fn_body = stmt* COL0 "}" item_label NL ;
```

Rules on the parts of a head:

- Every parameter other than `self` writes its name and type; its mode is read unless `mut` or `take` precedes it.
- `-> R` is omitted exactly when the result is `Unit`. Writing `-> Unit` is an error (`item.unit-result`); its exact fix deletes it.
- Nothing in a head is inferred: the checker never computes a parameter type, a result, a failure type, waiting or a limit from the body.
- A head whose result is a function type has no `fails`, `uses wait`, `limits` or convention clause of its own: those written after the result belong to the result type ([function types](10-effects-digests.md#function-types)). Such a function that may fail or may wait is reported as `item.fails-missing` or `item.wait-missing` without an exact fix.
- Generic parameters are defined in [Generics and interfaces](08-generics-interfaces.md#generic-parameters), `fails` clauses in [Errors, traps and domains](11-errors-domains.md#failures), `uses wait` and `limits` in [Effects, digests and limits](10-effects-digests.md#waiting), and the convention clause in [Low-level programming](23-low-level.md#foreign-functions).

Rules on bodies:

- A function with the `foreign` modifier is a [foreign fn](23-low-level.md#foreign-functions): its head ends with a convention clause, and it has no clause region and no body. A foreign fn with a clause region or a body or without a convention clause, and a convention clause on any other function, are errors (`item.foreign-form`).
- A function of a std package may have no body: its item then ends at its head line or after its clause region, and it is a [supplied operation](28-stdlib.md#supplied-operations). Every other function has a body; one without is an error (`item.missing-body`).
- When the function has no clause region, the `{` opening its body block is the last token of the last physical line of the head; otherwise it is the first token at column 0 after the last clause. A `{` at column 0 after a head without a clause region, a `{` ending a clause line, a `{` after a clause region that is not at column 0, and a clause line after the `{` that ends the head line are errors (`item.body-opener`); the exact fix moves the `{`.

`fails` and `uses wait` are written exactly when the body needs them. These rules read the body outside the bodies of the [closures](07-ownership.md#closures) it holds, and judge each call whether or not it carries its markers.

- A function **may fail** when its body holds a `fail`, or a call not marked `attempt` of a function or function value whose head or type fails with a type other than `Never` once the call fixes its generic parameters ([failure polymorphism](10-effects-digests.md#failure-polymorphism)). One that may fail and has no `fails` clause is an error (`item.fails-missing`), reported on the line holding `fn`; when every such `fail` and call raises a value of one type, the exact fix adds `fails` with that type.
- A function **may wait** when its body holds an `await when`, or a call that [may wait](10-effects-digests.md#awaited-calls) other than through the function's own parameters. A call waits only through them when the function value it calls is one of them, or when its callee's head has no `uses wait` and each argument whose head or type states `uses wait` is one of them. One that may wait and has no `uses wait` is an error (`item.wait-missing`), reported on the line holding `fn`; the exact fix adds `uses wait`, preceded by `fails task::Cancelled` when the head has no `fails` clause.
- A function that cannot fail and has a `fails` clause is an error (`item.fails-unneeded`), and one that cannot wait and has `uses wait` is an error (`item.wait-unneeded`), unless its body holds a `todo` expression. The exact fix deletes the clause. A `fails` clause beside `uses wait` is never unneeded ([cancellation](11-errors-domains.md#cancellation)).
- These rules apply to every function with a body. For the [entry function](27-program-model.md#entry), whose head forms are fixed, `item.fails-unneeded` and `item.wait-unneeded` do not apply, and when its target's forms admit no `fails` or `uses wait` clause, `item.fails-missing` and `item.wait-missing` have no exact fix and their because-chain quotes those forms. Foreign fn heads follow [Low-level programming](23-low-level.md#foreign-functions), and interface method heads [Generics and interfaces](08-generics-interfaces.md#interfaces).

A call binds each parameter to its argument in its mode and runs the body. It returns the value of the `return` that ends the run, or `Unit` when a function whose result is `Unit` reaches the end of its body.

```nan
pub closed enum ReadError derives Eq, Render {
    Empty
} ReadError

pub fn first_byte(data: Slice[u8]) -> u8 fails ReadError {
    if data.len() == 0 {
        fail ReadError.Empty
    }
    return data[0]
} first_byte

pub fn fill(mut buf: SliceMut[u8], value: u8) {
    for byte in mut buf {
        byte = value
    }
} fill

pub fn twice[E](op: fn() -> u32 fails E uses wait) -> u32 fails E {
    let earlier = try await op()
    let later = try await op()
    return earlier.saturating_add(later)
} twice
```

```nan
pub fn clear(mut buf: SliceMut[u8]) -> Unit { // error: item.unit-result
    for byte in mut buf {
        byte = 0
    }
} clear
```

```nan
pub closed enum ReadError derives Eq, Render {
    Empty
} ReadError

pub fn byte_count(data: Slice[u8]) -> usize fails ReadError { // error: item.fails-unneeded
    return data.len()
} byte_count
```

```nan
pub fn pause(clock: sys::Clock) fails task::Cancelled { // error: item.wait-missing
    try await clock.sleep(ms: 10)
} pause
```

## Methods

A function named `Type.name` is a member of `Type`. A **method** is one whose first parameter is `self`; an **associated function** is one without a `self` parameter. Methods are called receiver-first and associated functions as `Type.name(...)`, as [calls](06-statements-expressions.md#calls) define.

- `Type` is written as a [path](03-packages-modules.md#paths) from the function's module, without generic arguments. It names a struct, enum or layout declared in the function's module, a [scalar](04-types.md#scalars) or `Unit` when the function's module is the root module of `core`, or the target of an [impl line](08-generics-interfaces.md#impl-lines) of the function's module whose interface declares `name`. Any other function named `Type.name` is an error (`item.method-misplaced`).
- `self` is written `self`, `mut self` or `take self`, without a type. It is legal only as the first parameter of a function named `Type.name`, of an [interface method](08-generics-interfaces.md#interfaces) or of the old head of a [tombstone](29-identity-versioning.md#tombstones); anywhere else, closures and tests included, it is an error (`item.self-misplaced`).
- The type of `self` is `Type` applied to its own generic parameters. Those parameters and their bounds are in scope in the head, clause region and body of every function named `Type.name`, which may add generic parameters of its own after its name.

```nan
pub closed struct Gauge {
    pub level: u32
    pub peak: u32
} Gauge

pub fn Gauge.empty() -> Gauge = Gauge(level: 0, peak: 0)

pub fn Gauge.raise(mut self, amount: u32) {
    self.level = self.level.saturating_add(amount)
    if self.level > self.peak {
        self.peak = self.level
    }
} Gauge.raise

pub fn Gauge.into_peak(take self) -> u32 {
    return self.peak
} Gauge.into_peak

pub closed struct Pair[T: Copy] {
    pub left: T
    pub right: T
} Pair

pub fn Pair.swapped(self) -> Pair[T] = Pair(left: self.right, right: self.left)
```

In the next example the impl line sits in the interface's module, so the method names its type by path.

```nan
// file: example/package.nan
//! Shape codes.

package example {
    language 1
    targets any
} example

// file: example/example.nan
//! Codes for plane shapes.

pub interface Describe {
    fn code(self) -> u32
} Describe

impl geo::Square: Describe

pub fn geo::Square.code(self) -> u32 = self.side

// file: example/geo/geo.nan
//! Plane shapes.

pub closed struct Square {
    pub side: u32
} Square
```

```nan
pub closed struct Gauge {
    pub level: u32
} Gauge

pub fn level_of(self) -> u32 { // error: item.self-misplaced
    return self.level
} level_of
```

## Transparent functions

A **transparent function** is a function whose body is `= expr`. It is pure, terminating and notrap, and its body is its contract.

- Its digest is checked as if its head stated `limits pure, notrap, terminates` ([limits](10-effects-digests.md#limits)): each site breaking one of these is reported with that section's code, and each obligation of the body the prover does not prove with the prover's [outcome](20-prover.md#outcomes), `proof.unknown` or `proof.refuted`.
- It has no clause region and no `limits` clause; a `limits` clause on one is an error (`item.transparent-limits`), whose exact fix deletes it.
- Its only permitted modifier is `proj`; `interrupt`, `landing` or `validator` on one is an error (`item.modifier-misplaced`), and `foreign` on one is an error (`item.foreign-form`).
- A call evaluates the body with the parameters bound to the arguments and returns its value.

Code **unfolds** a call to a transparent function when every field, item and member that the body names is visible to that code. Where it unfolds, the prover knows that the call's value is the body with the arguments substituted, and the transparent calls inside the body unfold by the same rule. Elsewhere the prover knows only the function's head and that it is pure, terminating and notrap. [Hashes](29-identity-versioning.md#hashes) and [requires-sets](29-identity-versioning.md#requires-sets) record what a [consumer](03-packages-modules.md#visibility) unfolds.

```nan
pub closed struct Span {
    pub start: u32
    pub end: u32
} Span

pub fn Span.is_empty(self) -> bool = self.start == self.end

pub fn Span.length(self) -> u32 = self.end.saturating_sub(self.start)
```

```nan
pub fn announce(log: sys::Log, count: u32) = log.info("count", value: count) // error: limit.impure
```

In the next example `Meter.value` names the private field `count`, so the root module cannot unfold it and knows nothing of its result.

```nan
// file: example/package.nan
//! Counter headroom.

package example {
    language 1
    targets any
} example

// file: example/meter/meter.nan
//! A counter that stops at its limit.

pub const LIMIT: u32 = 100

pub open struct Meter {
    count: u32 where count <= LIMIT
} Meter

pub fn Meter.value(self) -> u32 = self.count

// file: example/example.nan
//! Headroom checks.

pub fn headroom(m: meter::Meter) -> u32 limits notrap {
    return meter::LIMIT - m.value() // error: proof.unknown
} headroom
```

## Clause region

The **clause region** is the logical lines between a function head and its body that hold the function's requires, ensures, example, decreases, todo and waive clauses.

```ebnf
clause_region = clause_line+ ;
clause_line = requires_clause
    | ensures_clause
    | example_clause
    | decreases_clause
    | todo_clause
    | waive_clause ;
```

- Every clause rule ends with `NL`. Requires, ensures, example, decreases and todo clauses are defined in [Contracts](18-contracts.md#clauses), and waive clauses in [Statements and expressions](06-statements-expressions.md#thresholds).
- Clause lines are never at column 0; the [canonical format](35-format.md#canonical-format) fixes their indentation.
- Clause lines form groups in the order requires, ensures, example, decreases, todo, waive. A clause marked [disputed](18-contracts.md#todo) by a `todo` prefix belongs to the group of the clause it marks.
- A clause line following a line of a later group is an error (`item.clause-order`); the exact fix moves it to just before the first line of a later group.
- A clause region belongs only to a function with a body block or to an interface method.

```nan
pub fn average(total: u64, count: u64) -> u64
    requires count > 0
    example average(9, count: 3) == 3
{
    return total / count
} average
```

```nan
pub fn average(total: u64, count: u64) -> u64
    example average(9, count: 3) == 3
    requires count > 0 // error: item.clause-order
{
    return total / count
} average
```

```nan
pub fn average(total: u64, count: u64) -> u64
    requires count > 0 { // error: item.body-opener
    return total / count
} average
```

## Constants

A **const item** is a named value of an Inert type computed at check time, written `const NAME: T = expr`. A **const assert** is an item stating a condition that the interpreter must find true at check time, written `const assert expr`.

```ebnf
const_item = IDENT ( ":" type_expr "=" expr NL | "=" expr NL {!item.const-untyped} ) ;
const_assert_item = "assert" expr NL ;
```

- A const without a type is an error (`item.const-untyped`).
- A const whose type is not [Inert](05-properties.md#inert), or is [linear](05-properties.md#linear-types), is an error (`item.const-type`).
- A const's initializer has the const's type, and a const assert's condition has type `bool`. Each is checked as the body of a function whose result type is the const's type and whose head states `limits pure, terminates` ([limits](10-effects-digests.md#limits)), exempt from `prop.view-result`; [const evaluation](21-interpreter.md#const-evaluation) restricts the views of its value.
- The [const evaluation](21-interpreter.md#const-evaluation) of a const or [contribution](25-registries.md#contributions) **needs** the value of each const and contribution that its run reads, or that checking reads in an item whose unit the run executes, and whatever their evaluations need in turn. A const or contribution whose evaluation needs its own value is an error (`item.const-cycle`), reported once for each set of consts and contributions that need one another's values, at the member whose canonical path sorts first in byte order.
- The interpreter computes each const and checks each const assert by [const evaluation](21-interpreter.md#const-evaluation), which reports an evaluation that traps (`const.trapped`) and a condition that is false (`const.assert-false`).
- A const names a value, never a [place](07-ownership.md#places). Each use yields the computed value as a call result would, so a use passes to a `mut` or `take` parameter unmarked.

```nan
pub const MAX_FRAME: u16 = 1_500

const HEADER_BYTES: u16 = 10

pub const MAX_PAYLOAD: u16 = MAX_FRAME - HEADER_BYTES

const assert MAX_PAYLOAD >= 1_024
```

```nan
pub const RETRIES = 3 // error: item.const-untyped
```

```nan
pub const SPAN: u32 = SPAN + 1 // error: item.const-cycle
```

```nan
pub const LIMIT: u32 = capped(100) // error: item.const-cycle

fn capped(value: u32) -> u32 {
    return match value {
        LIMIT => value
        else => 0
    }
} capped
```

## Reserved forms

Language 1 reserves two forms of item heads.

```ebnf
self_field_set = "." "{" IDENT ( "," IDENT )* "}" {!item.self-field-set} ;
reserved_head_word = ( 'stable' | 'flexible' | 'migrate' | 'machine' | 'protocol' | 'ipc' ) {!item.reserved-word} ;
```

- A `self` parameter followed by a field set, such as `mut self.{level, peak}`, is an error (`item.self-field-set`); the exact fix replaces it with `self` in the same mode.
- The words `stable`, `flexible`, `migrate`, `machine`, `protocol` and `ipc` are reserved in modifier and kind position of an item; there they are an error (`item.reserved-word`). Everywhere else they are identifiers.

```nan
pub closed struct Reading {
    pub stable: bool
    pub machine: u32
} Reading
```

```nan
pub closed struct Gauge {
    pub level: u32
    pub peak: u32
} Gauge

pub fn Gauge.reset(mut self.{level, peak}) { // error: item.self-field-set
    self.level = 0
    self.peak = 0
} Gauge.reset
```

```nan
pub stable struct Reading { // error: item.reserved-word
    pub value: u32
} Reading
```

## Diagnostics

| Code | Class | Rule |
| --- | --- | --- |
| `item.body-opener` | error | [Heads](#heads), [Interfaces](08-generics-interfaces.md#interfaces) |
| `item.clause-order` | error | [Clause region](#clause-region) |
| `item.const-cycle` | error | [Constants](#constants) |
| `item.const-type` | error | [Constants](#constants) |
| `item.const-untyped` | error | [Constants](#constants) |
| `item.duplicate` | error | [Order and names](#order-and-names), [Interfaces](08-generics-interfaces.md#interfaces) |
| `item.fails-missing` | error | [Heads](#heads) |
| `item.fails-unneeded` | error | [Heads](#heads) |
| `item.foreign-form` | error | [Heads](#heads), [Transparent functions](#transparent-functions) |
| `item.indented` | error | [Item shape](#item-shape) |
| `item.label-mismatch` | error | [Item shape](#item-shape), [Interfaces](08-generics-interfaces.md#interfaces) |
| `item.method-misplaced` | error | [Methods](#methods) |
| `item.missing-body` | error | [Heads](#heads) |
| `item.modifier-conflict` | error | [Modifiers](#modifiers) |
| `item.modifier-misplaced` | error | [Modifiers](#modifiers), [Transparent functions](#transparent-functions) |
| `item.multiple-lines` | error | [Item shape](#item-shape) |
| `item.openness-missing` | error | [Modifiers](#modifiers) |
| `item.openness-not-pub` | error | [Modifiers](#modifiers) |
| `item.reserved-word` | error | [Reserved forms](#reserved-forms) |
| `item.self-field-set` | error | [Reserved forms](#reserved-forms) |
| `item.self-misplaced` | error | [Methods](#methods) |
| `item.stray-column-zero` | error | [Item shape](#item-shape) |
| `item.transparent-limits` | error | [Transparent functions](#transparent-functions) |
| `item.unit-result` | error | [Heads](#heads) |
| `item.visibility-not-allowed` | error | [Item kinds](#item-kinds) |
| `item.wait-missing` | error | [Heads](#heads) |
| `item.wait-unneeded` | error | [Heads](#heads) |
