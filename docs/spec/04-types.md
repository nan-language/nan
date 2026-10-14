# Data types

This chapter defines the scalars, `Unit` and `Never`, struct and enum items, `Option` and `Result`, arrays and slices, niches, closed and open types, construction, predicates and their obligations, derived `Default`, expected types and the typing of literals, and patterns.

## Scalars

A **scalar** is one of the 14 built-in types of the table. Their names belong to the [core vocabulary](03-packages-modules.md#core-vocabulary).

| Type | Values | Bits |
| --- | --- | --- |
| `u8`, `u16`, `u32`, `u64` | the integers 0 to 2^n − 1, for n the bit count | 8, 16, 32, 64 |
| `i8`, `i16`, `i32`, `i64` | the integers −2^(n−1) to 2^(n−1) − 1, in two's complement | 8, 16, 32, 64 |
| `usize` | the integers 0 to 2^64 − 1 | 64 |
| `isize` | the integers −2^63 to 2^63 − 1, in two's complement | 64 |
| `f32` | the IEEE 754 binary32 values, including both zeros, both infinities and NaNs | 32 |
| `f64` | the IEEE 754 binary64 values, including both zeros, both infinities and NaNs | 64 |
| `bool` | `true`, represented as 1, and `false`, represented as 0 | 8 |
| `char` | the Unicode scalar values U+0000 to U+D7FF and U+E000 to U+10FFFF | 32 |

- The Bits column is the width of the bit pattern that `to_bits` and `from_bits` exchange ([bit patterns](09-numbers.md#bit-patterns)).
- Every scalar is a distinct type: `usize` is not `u64` and `isize` is not `i64`, although they have the same values. A value changes type only through a [named conversion](09-numbers.md#conversions).
- Operations on numbers are defined in [Numbers](09-numbers.md), on `bool` in [logical operators](06-statements-expressions.md#logical-operators) and on `char` in [text types](13-text-encode.md#text-types).

```nan
pub fn total_bytes(count: usize, size: u64) -> u64 {
    let items = u64.widen(count)
    return items * size
} total_bytes
```

```nan
pub fn total_bytes(count: usize, size: u64) -> u64 {
    let items: u64 = count // error: type.mismatch
    return items * size
} total_bytes
```

## Unit and Never

- `Unit` is the type with exactly one value, written `Unit`. That value occupies no memory.
- `Never` is the type with no values: control never leaves an expression of type `Never` normally. `return`, `break` and `continue` ([returns](06-statements-expressions.md#returns)), `fail` ([fail](11-errors-domains.md#fail)) and every call of a function whose result type is `Never` have type `Never`.
- An expression of type `Never` is accepted wherever an expression of any type is expected.

```nan
pub closed enum LevelError derives Eq, Render {
    TooHigh(u8)
} LevelError

pub fn level(raw: u8) -> u8 fails LevelError {
    let checked = match raw {
        0..=9 => raw
        else => fail LevelError.TooHigh(raw)
    }
    return checked
} level

pub fn confirm(ready: bool) -> Result[Unit, u8] {
    if ready {
        return Ok(Unit)
    }
    return Err(1)
} confirm
```

## Structs

A struct item declares a type whose values hold one value of each of its named fields. It takes the type [modifiers](02-items.md#modifiers) and a [derives](08-generics-interfaces.md#derives) clause.

```ebnf
struct_item = "struct" IDENT generic_params? derives_clause? "{" struct_member* COL0 "}" item_label NL ;
struct_member = ( DOC NL )* ( field | invariant_line ) ;
field = visibility? IDENT ":" type_expr where_clause? NL ;
type_expr = fn_type | path ( generic_args | "." IDENT generic_args? {!name.dot-for-path} )? ;
```

- A `type_expr` path names a type: a scalar, `Unit`, `Never`, a struct, an enum, a layout, another std type or a type parameter in scope. A path naming any other item is an error (`type.not-a-type`), except where a const argument ([instantiation](08-generics-interfaces.md#instantiation)), the argument of `Dyn` ([dynamic dispatch](08-generics-interfaces.md#dynamic-dispatch)) or a register window ([registers](23-low-level.md#registers)) takes one under its own rule.
- A struct may have no fields; it then has exactly one value, constructed and matched as `S()`.
- Two fields of one struct with the same name are an error (`type.duplicate-member`).
- The declaration order of fields, `invariant` lines and variants matters only to the derives that follow it: `Ord`, `Render` and `Generate` ([derives](08-generics-interfaces.md#derives)).
- A field is private, `pkg` or `pub`, with the meanings of [visibility](03-packages-modules.md#visibility), and is visible only where its struct also is. Naming a field where it is not visible is an error (`type.hidden-field`), and so is a construction or struct pattern of a struct that has a field not visible there.
- A type **holds inline** its [constituent types](05-properties.md#structural-properties) and every type they hold inline; `Array[T, N]` and `Untrusted[T]` hold `T` inline. `Slice[T]`, `SliceMut[T]`, `Str`, `Dyn[I]`, function types and `mem::Box[T]` ([box](07-ownership.md#box)) hold nothing inline, and every other base type what its owning chapter states.
- A struct or enum that holds an instantiation of itself inline is an error (`type.recursive`). It is reported once per cycle, on the first field or variant in source order through which the type of the cycle whose canonical path sorts first in byte order holds itself.
- Field `where` clauses and `invariant` lines are [predicates](#predicates).

```nan
pub open struct Counter {
    pub hits: u64
    misses: u64
} Counter

pub fn Counter.empty() -> Counter = Counter(hits: 0, misses: 0)

pub fn Counter.record(mut self, hit: bool) {
    if hit {
        self.hits += 1
    } else {
        self.misses += 1
    }
} Counter.record

pub fn Counter.miss_count(self) -> u64 = self.misses

pub closed struct Node {
    pub value: u32
    pub next: Option[mem::Box[Node]]
} Node
```

```nan
pub closed struct Node {
    pub value: u32
    pub next: Option[Node] // error: type.recursive
} Node
```

## Enums

An enum item declares a type whose every value is exactly one of its **variants**, together with that variant's payload. It takes the type [modifiers](02-items.md#modifiers) and a [derives](08-generics-interfaces.md#derives) clause.

```ebnf
enum_item = "enum" IDENT generic_params? derives_clause? "{" enum_member+ COL0 "}" item_label NL ;
enum_member = ( DOC NL )* variant ;
variant = IDENT variant_payload? 'from'? NL ;
variant_payload = "(" payload_list ")" ;
payload_list = payload_field ( "," payload_list? )? ;
payload_field = type_expr ( ":" type_expr )? ;
```

- In a `payload_field` of the form `a: T`, `a` is the field's label, a single identifier; any other type expression before `:` is an error (`type.unlabelled-payload`).
- A variant has exactly one of three forms: a unit variant `V` with no payload, `V(T)` with one unlabelled payload of type `T`, or `V(a: T, b: U)` with one or more labelled payload fields. A payload with two or more unlabelled types, or mixing labelled and unlabelled ones, is an error (`type.unlabelled-payload`).
- Two variants of one enum with the same name, or two payload fields of one variant with the same label, are an error (`type.duplicate-member`).
- Variant `V` of enum `E` is the [member](03-packages-modules.md#members) `E.V`: `Kind.Data`, `wire::Kind.Data`. The type arguments of a generic enum may be written before the `.`, as in `Tree[u8].Leaf`; when omitted they are [inferred](08-generics-interfaces.md#inference).
- Variants and their payload fields have the visibility of their enum.
- `from` after a variant marks it as a [from variant](11-errors-domains.md#error-enums).

```nan
pub closed enum Shape derives Eq {
    Dot
    Segment(u32)
    Rect(width: u32, height: u32)
} Shape

pub fn Shape.area(self) -> u64 {
    return match self {
        Shape.Dot or Shape.Segment(_) => 0
        Shape.Rect(width: w, height: h) => u64.widen(w) * u64.widen(h)
    }
} Shape.area
```

```nan
pub closed enum Move {
    Step(i8, i8) // error: type.unlabelled-payload
    Stay
} Move
```

## Option and Result

`Option` and `Result` are closed core enums. Their members are defined in [Option and Result members](28-stdlib.md#option-and-result-members).

```nan-api
// module: core
pub closed enum Option[T: Any] derives Copy, Eq, Hash, Render, Encode, Generate {
    Some(T)
    None
} Option

pub closed enum Result[T: Any, E: Any] derives Copy, Eq, Hash, Render, Encode, Generate {
    Ok(T)
    Err(E)
} Result
```

- `Option[T]` holds either `Some(v)`, a value of `T`, or `None`. It marks a value as possibly absent.
- `Result[T, E]` holds either `Ok(v)` or `Err(e)`. `attempt` turns a failing call into one ([attempt](11-errors-domains.md#attempt)).
- `Some`, `None`, `Ok` and `Err` are core vocabulary and are written without their enum's name. Writing `Option` or `Result`, with or without type arguments, before `.Some`, `.None`, `.Ok` or `.Err` is an error (`type.qualified-core-variant`) whose exact fix removes the qualifier.

```nan
pub fn half(value: u32) -> Option[u32] {
    if value % 2 == 1 {
        return None
    }
    return Some(value / 2)
} half
```

```nan
pub fn half(value: u32) -> Option[u32] {
    if value % 2 == 1 {
        return Option.None // error: type.qualified-core-variant
    }
    return Some(value / 2)
} half
```

## Arrays and slices

- `Array[T, N]` holds exactly `N` values of `T` stored inline, in index order, with no indirection. `N` is a const parameter of type `usize` ([generic parameters](08-generics-interfaces.md#generic-parameters)) and may be 0.
- `Slice[T]` is a [view](05-properties.md#views) of a run of contiguous values of `T` whose length is fixed when the view is made; its elements are read through it and never written. `SliceMut[T]` is a view of such a run whose elements may also be written. Neither owns the elements it reaches.
- The elements of an array or slice of length `n` have the indices 0 to `n` − 1. [Indexing](07-ownership.md#indexing) reaches them; the other members are defined in [array and slice members](28-stdlib.md#array-and-slice-members).

```nan
pub fn peak(samples: Array[u16, 8]) -> u16 {
    var highest: u16 = 0
    for sample in samples {
        if sample > highest {
            highest = sample
        }
    }
    return highest
} peak

pub fn first_byte(data: Slice[u8]) -> Option[u8] {
    if data.len() == 0 {
        return None
    }
    return Some(data[0])
} first_byte
```

## Niches

A **niche** is a bit pattern of a type's representation that is no value of the type. Safe code never produces a value holding a niche ([safe code](23-low-level.md#safe-code)). The niches are exactly:

- in `bool`, every byte other than 0 and 1;
- in `char`, every 32-bit pattern that is not a Unicode scalar value;
- in a `layout` with `tags`, every tag value it does not list ([layouts](23-low-level.md#layouts));
- in `Slice[T]`, `SliceMut[T]`, `Str`, `Dyn[I]` and `mem::Box[T]`, the patterns holding the zero address;
- in an enum, every pattern its [type layout](30-artefacts.md#type-layout) gives no value;
- in a struct, an array or a payload, every pattern in which the bits of some field or element are a niche of that part's type;
- in a base type of another chapter, what that chapter states.

Integers and floats have no niches, and predicates create none. Lowering represents some enums inside the niches of a payload ([niche packing](30-artefacts.md#niche-packing)).

```nan
pub fn flag(raw: u8) -> Option[bool]
    example flag(1) == Some(true)
    example flag(2) == None
{
    return bool.checked_from_bits(raw)
} flag
```

## Closed and open types

The modifiers `closed` and `open` ([modifiers](02-items.md#modifiers)) fix what a [consumer](03-packages-modules.md#visibility) may rely on in the structure of a `pub` struct or enum. Inside its package, a type is constructed, destructured and matched exhaustively without `else` whichever modifier it states.

- A **closed type** is a `pub` struct or enum that states `closed`. Every field of a closed struct is `pub`; any other field is an error (`type.closed-hidden-field`).
- Consumers construct and destructure a closed struct, and match a closed enum exhaustively. A closed type's fields, variants and payloads are part of its interface ([hashes](29-identity-versioning.md#hashes)).
- An **open type** is a `pub` struct or enum that states `open`. An open struct may gain fields and an open enum variants as a [compatible](29-identity-versioning.md#compatibility) change.
- Constructing an open struct outside its package is an error (`type.open-construction`), and destructuring one there is an error (`pattern.open-struct`); each is reported instead of `type.hidden-field`. Consumers obtain its values from its functions, and read and write its `pub` fields.
- Consumers construct the variants of an open enum. Outside its package, an open enum has values matching none of its declared variants, which [exhaustiveness](06-statements-expressions.md#disjointness-and-exhaustiveness) and [refutability](#refutability) count. A `match` outside an open enum's package that would be exhaustive if that enum had only its declared variants, and is not, is an error (`pattern.open-enum-no-else`), reported instead of `expr.non-exhaustive`.
- A consumer's checked code depends on no part of an open type's representation: its size, field offsets, field count and non-`pub` fields stay symbolic in the consumer's [NIR](19-nir.md#symbolic-fields) and are fixed only by the [lowering](30-artefacts.md#lowering) of the final closure. Consumers see an open type's [interface facts](05-properties.md#interface-facts), its promises among them.

```nan
// file: meter/package.nan
//! Running totals.

package meter {
    language 1
    targets any
} meter

// file: meter/meter.nan
//! An open total and an open scale.

pub open struct Total {
    pub sum: u64
    pub samples: u32
} Total

pub fn Total.empty() -> Total = Total(sum: 0, samples: 0)

pub open enum Scale derives Eq {
    Bytes
    Kibibytes
} Scale

// file: report/package.nan
//! Reports meter totals.

package report {
    language 1
    targets any
    dep meter "../meter"
} report

// file: report/report.nan
//! Reads totals and scales from meter.

pub fn fresh_sum() -> u64 {
    let total = meter::Total.empty()
    return total.sum
} fresh_sum

pub fn shift_for(scale: meter::Scale) -> u32 {
    return match scale {
        meter::Scale.Bytes => 0
        meter::Scale.Kibibytes => 10
        else => 0
    }
} shift_for
```

```nan
// file: meter/package.nan
//! Running totals.

package meter {
    language 1
    targets any
} meter

// file: meter/meter.nan
//! An open total.

pub open struct Total {
    pub sum: u64
    pub samples: u32
} Total

// file: report/package.nan
//! Reports meter totals.

package report {
    language 1
    targets any
    dep meter "../meter"
} report

// file: report/report.nan
//! Builds a total by hand.

pub fn fresh_sum() -> u64 {
    let total = meter::Total(sum: 0, samples: 0) // error: type.open-construction
    return total.sum
} fresh_sum
```

```nan
// file: meter/package.nan
//! Running totals.

package meter {
    language 1
    targets any
} meter

// file: meter/meter.nan
//! An open scale.

pub open enum Scale derives Eq {
    Bytes
    Kibibytes
} Scale

// file: report/package.nan
//! Reports meter totals.

package report {
    language 1
    targets any
    dep meter "../meter"
} report

// file: report/report.nan
//! Matches a scale without an else arm.

pub fn shift_for(scale: meter::Scale) -> u32 {
    return match scale { // error: pattern.open-enum-no-else
        meter::Scale.Bytes => 0
        meter::Scale.Kibibytes => 10
    }
} shift_for
```

## Construction

A **construction** is an expression that builds a struct value, `S(f: e, ...)`, or a variant value with a payload, `E.V(e)` or `E.V(a: e, ...)`. A unit variant is written `E.V`, without an argument list.

```ebnf
construction_expr = path generic_args? ( "." IDENT )? "(" arg_list? ")" ;
```

- A construction has the syntax of a call: the parser reads it as a `postfix_expr` ([field access and method calls](06-statements-expressions.md#field-access-and-method-calls)), and it is a construction exactly when its path names a struct or its last segment or member names a variant ([calls](06-statements-expressions.md#calls)). Omitted type arguments are [inferred](08-generics-interfaces.md#inference).
- A struct construction and a labelled variant construction write every value, the first included, as `label: value`, the label naming a field or payload field ([unknown names](03-packages-modules.md#unknown-names) otherwise); labels are never punned. A value without a label is an error (`type.unlabelled-field`); when it is a binding named like a field not otherwise named, the exact fix writes that name as its label. Each field is named exactly once: an omitted field is an error (`type.missing-field`) and a repeated one is an error (`type.duplicate-label`). Fields may be named in any order.
- A `V(T)` variant is constructed with exactly one unlabelled value. A unit variant with an argument list, a `V(T)` variant with any other argument list, and a labelled variant with an empty one are errors (`type.payload-shape`).
- Each value is marked as a `take` argument is ([marking](07-ownership.md#marking)): `Uart(regs: take regs, dropped: 0)`.
- The values are evaluated in the order written ([evaluation order](06-statements-expressions.md#evaluation-order)), and the value is then built in its [result location](07-ownership.md#result-location).
- The struct's predicates are obligations at each construction ([obligations](#obligations)).
- **Destructuring** takes values apart with the struct and variant [patterns](#patterns), which mirror construction.

```nan
pub closed struct Span derives Copy, Eq {
    pub start: u32
    pub end: u32 where start <= end
} Span

pub fn Span.point(position: u32) -> Span = Span(start: position, end: position)

pub fn Span.length(self) -> u32 {
    let Span(start: low, end: high) = self
    return high - low
} Span.length
```

```nan
pub closed struct Span derives Copy, Eq {
    pub start: u32
    pub end: u32 where start <= end
} Span

pub fn Span.point(position: u32) -> Span {
    return Span(start: position) // error: type.missing-field
} Span.point
```

## Predicates

A **predicate** is a condition that every value of a struct type satisfies: a field `where P` or a struct line `invariant P`.

```ebnf
where_clause = "where" expr ;
invariant_line = "invariant" expr NL ;
```

- `P` is a `bool` [contract term](18-contracts.md#contract-terms). It names the struct's fields by their bare names, a bare name spelt like a field naming that field, and may also name consts, the struct's const generic parameters and the functions contract terms may call.
- A predicate **mentions** each field it names; a field's `where P` also mentions that field and otherwise constrains values as a line `invariant P` does.
- Enum payloads carry no predicates.
- Predicates are part of the struct's interface. A predicate naming what a consumer cannot see is [opaque](18-contracts.md#opacity) to that consumer.
- The **defining module** of a struct is the module that declares it.
- A value is **reachable** from another when it is that value, or a field, payload or `Array` element of a value reachable from it. Reachability never enters a value of a type with [interior mutability](12-memory.md#interior-mutability).

### Module boundary

Only the defining module writes the fields its predicates mention.

- Outside the defining module, assigning, moving or accessing by `mut` a mentioned field, or any place within one, is an error (`predicate.outside-write`). Reading such a field, and constructing, assigning, passing or destructuring a whole value, are not restricted by predicates.
- A **boundary function** of the defining module is a `pub` or `pkg` function of that module, a function of it implementing an interface method, a private function of it used as a [function value](07-ownership.md#function-values), or a closure written in it. A **helper** is any other function of the defining module.
- Helpers neither assume predicates nor re-establish them, except at the ends of holds that [obligations](#obligations) names; a predicate may be false while one runs.
- A boundary function that is a mut [projection](07-ownership.md#projections) returning a result derived from the receiver through a mentioned field is an error (`predicate.exposed-field`), reported on the `return` statement.

### Obligations

For a value whose type's defining module is M, the predicate is an [obligation](18-contracts.md#obligations):

- at the value's construction;
- in M, at each call other than a call naming a helper, for the values reachable from the call's receiver and arguments;
- in M, at each closure expression, for the values reachable from its captures;
- at each exit of a boundary function of M, failures included, for the values reachable from its result, its failure value, its `mut` parameters and its `mut` captures;
- in M, at the end of the [hold](07-ownership.md#holds) of each writable part that a call of a mut [projection](07-ownership.md#projections) returns, for the values reachable from that part;
- in M, at each exit of each `await when` body ([cowns](16-concurrency.md#cowns)), for the values reachable from the cowns it acquired.

The predicate is a fact:

- everywhere outside M;
- for the value of a const;
- after each point where it is an obligation, for the values that obligation covers;
- in M, at entry to a boundary function, for the values reachable from its parameters and captures; at the start of an `await when` body, for the values reachable from its cowns; and after each call other than a call naming a helper, for the values reachable from the call's result and `mut` arguments.

A fact about a value ends at the first assignment, move or `mut` access of a field its predicate mentions, and at the first call passing by `mut` to a helper a value from which it is reachable.

The prover discharges an obligation from facts, from contracts and from the [frame rule](20-prover.md#frame-rule). An obligation not proven is a [run-time check](18-contracts.md#run-time-checks) that evaluates the predicate and traps when it is false (`trap.predicate`).

```nan
// file: example/package.nan
//! Gauge levels.

package example {
    language 1
    targets any
} example

// file: example/gauge/gauge.nan
//! A bounded level that remembers its peak.

pub const CAPACITY: u32 = 64

pub closed struct Gauge {
    pub level: u32 where level <= CAPACITY
    pub peak: u32
    invariant level <= peak
} Gauge

pub fn Gauge.empty() -> Gauge = Gauge(level: 0, peak: 0)

pub fn Gauge.fill(mut self, amount: u32)
    requires amount <= CAPACITY - self.level
{
    self.level += amount
    raise_peak(mut self)
} Gauge.fill

fn raise_peak(mut reading: Gauge)
    ensures reading.level == old(reading.level)
    ensures reading.peak >= reading.level
{
    if reading.peak < reading.level {
        reading.peak = reading.level
    }
} raise_peak

// file: example/example.nan
//! Reads gauges from outside their module.

pub fn gap(meter: gauge::Gauge) -> u32 {
    return meter.peak - meter.level
} gap
```

```nan
// file: example/package.nan
//! Gauge levels.

package example {
    language 1
    targets any
} example

// file: example/gauge/gauge.nan
//! A bounded level.

pub const CAPACITY: u32 = 64

pub closed struct Gauge {
    pub level: u32 where level <= CAPACITY
} Gauge

// file: example/example.nan
//! Writes a gauge from outside its module.

pub fn drain(mut meter: gauge::Gauge) {
    meter.level = 0 // error: predicate.outside-write
} drain
```

```nan
pub closed struct Band {
    pub low: u32
    pub high: u32
    invariant low <= high
} Band

pub proj fn Band.high_mut(mut self) -> u32 {
    return self.high // error: predicate.exposed-field
} Band.high_mut
```

## Derived Default

A struct that lists `Default` in its [derives](08-generics-interfaces.md#derives) has as its default the value whose every field holds the default of the field's type. When the struct has predicates, the prover must prove every predicate of that value ([outcomes](20-prover.md#outcomes)); a predicate it does not prove makes the derive an error (`predicate.default-unproven`), reported on the struct's first line. A derived default carries no run-time check.

```nan
pub closed struct Window derives Default {
    pub low: u32
    pub high: u32 where low <= high
} Window
```

```nan
pub closed struct Retry derives Default { // error: predicate.default-unproven
    pub attempts: u8 where attempts >= 1
} Retry
```

## Expected types and literals

The **expected type** of an expression is the type its position fixes. The rule owning a position states its expected type, as [conditionals and loops](06-statements-expressions.md#conditionals-and-loops) does for the condition of `if` and [value-yielding if and match](06-statements-expressions.md#value-yielding-if-and-match) for branches; the general positions are:

- the declared type of the `let` or `var` binding or the const item it initialises;
- the type of the place it is assigned to by `=`;
- the type of the parameter it is the argument of, with the type arguments fixed so far ([inference](08-generics-interfaces.md#inference));
- the type of the field or payload field it initialises, with the type arguments fixed so far;
- the result type of the enclosing function or closure, after `return`;
- the failure type of the innermost function or closure, after `fail` ([failures](11-errors-domains.md#failures)), and none in a test body;
- for the right operand of a binary arithmetic, bitwise, shift or comparison operator, or the right side of a compound assignment, the right operand type that the left operand's or the place's type fixes ([operator interfaces](08-generics-interfaces.md#operator-interfaces)), which for a number type is that number type, or `u32` for a shift ([number operators](09-numbers.md#number-operators));
- for the left operand of such an operator whose type is not fixed without an expected type, the right operand's type when the operator is not a shift and that type is fixed without an expected type, and otherwise, unless the operator is a comparison, the operator's expected type;
- for a bound of a range expression, the other bound's type when that type is fixed without an expected type, and otherwise `T` when the range's expected type is `core::Range[T]`;
- for the operand of a prefix `-` or `~`, the operator's expected type.

An expression whose type differs from its expected type is an error (`type.mismatch`), except an expression of type `Never`, a function or function value where a function type is expected, which the [fit](10-effects-digests.md#fit) rule judges, and the operand of `fail`, which [failure conversion](11-errors-domains.md#failure-conversion) judges. Its expected is the expected type and its found the expression's type. No conversion is implicit.

The types of literals:

- A number literal with a [literal suffix](01-lexical.md#number-literals) has the suffix's type.
- A number literal without a suffix has its expected type. An `INT` requires an integer type and a `FLOAT` a float type; any other expected type is `type.mismatch`. A number literal with neither a suffix nor an expected type is an error (`type.literal-untyped`).
- A prefix `-` whose operand is a number literal, unparenthesised, forms one **negative literal**, whose value is the negated value of the literal: `-128i8` and `let low: i8 = -128` are valid, and `-0.0` is negative zero.
- An integer literal denotes its mathematical value, in every base: `0xFF` is 255. A value outside its type's range is an error (`type.literal-range`).
- A float literal denotes its value rounded to its type as IEEE 754 rounds to nearest, ties to even. A literal that rounds to an infinity is an error (`type.literal-range`).
- A text literal or text run is a `Str`, a byte-string literal a `Slice[u8]`, a char literal a `char` and a byte literal a `u8` ([text literals](01-lexical.md#text-literals)). `true` and `false` are `bool` and `Unit` is `Unit`. These types do not depend on the position.

```nan
pub fn scaled(reading: u16) -> u32 {
    let offset = 40u32
    let factor: u32 = 3
    return u32.widen(reading) * factor + offset
} scaled
```

```nan
pub fn limit_reached(count: u32) -> bool {
    let limit = 10 // error: type.literal-untyped
    return count >= limit
} limit_reached
```

```nan
pub fn clamp_small(value: u8) -> u8 {
    let ceiling: u8 = 300 // error: type.literal-range
    if value > ceiling {
        return ceiling
    }
    return value
} clamp_small
```

## Patterns

A **pattern** is the form a value is tested against in a `match` arm, a `let` statement or a `for` loop ([Statements and expressions](06-statements-expressions.md)). A value matches a pattern or does not, and a match binds the names the pattern introduces.

```ebnf
pattern = "_" | literal_pattern | named_pattern ;
literal_pattern = pattern_literal range_tail? ;
pattern_literal = "-"? ( INT | FLOAT ) | CHAR | BYTE | TEXT | BYTES | "true" | "false" ;
range_tail = ( "..<" | "..=" ) range_bound ;
range_bound = pattern_literal | path ;
named_pattern = path ( "." IDENT )? ( "(" field_pattern_list? ")" | range_tail )? ;
field_pattern_list = field_pattern ( "," field_pattern_list? )? ;
field_pattern = pattern ( ":" pattern )? ;
```

The **scrutinee** of a pattern is the value it tests; a pattern's type is its scrutinee's type, and a pattern inside a struct or variant pattern has the type of the field or payload it tests. In a `field_pattern` of the form `a: p`, `a` is the label, a single identifier; any other pattern before `:` is an error (`type.unlabelled-field`). The pattern forms are exactly these:

- Wildcard: `_` matches every value and binds nothing.
- Binding: a named pattern that is one identifier, without a member, field patterns or range tail, other than `None`, `Unit` and a SCREAMING_SNAKE name ([naming conventions](03-packages-modules.md#naming-conventions)), matches every value and binds it to that name.
- Literal: a number, char, byte, text or byte-string literal, `true` or `false` matches the value equal to it. A number literal takes the pattern's type as its expected type ([expected types](#expected-types-and-literals)); every other literal has its own type. A float literal is never a pattern (`expr.float-pattern`, [match arms](06-statements-expressions.md#match-arms)).
- Unit: `Unit` matches the value `Unit`.
- Constant: a SCREAMING_SNAKE identifier alone, or a path naming a const item ([constants](02-items.md#constants)), matches the value equal to the const's. A const whose type is not an integer scalar, `bool`, `char`, `Str` or `Slice[u8]` is an error here (`pattern.const-kind`).
- Range: `lo..<hi` matches the values from `lo` up to but excluding `hi`, and `lo..=hi` those from `lo` to `hi` inclusive. Each bound is a number, char or byte literal or a constant. The bounds and the pattern have one integer scalar type or `char`; any other type is an error (`pattern.range-kind`). A range matching no value is an error (`pattern.empty-range`).
- Variant: `E.V`, `E.V(p)` or `E.V(a: p, ...)`, path-prefixed when the enum lives elsewhere, and `None`, `Some(p)`, `Ok(p)` and `Err(p)`, matches a value of that variant whose payload matches the payload patterns. Payload patterns follow the variant's declared form as constructions do (`type.payload-shape`, `type.unlabelled-field`, `type.missing-field`, `type.duplicate-label`).
- Struct: `S(f: p, ...)` names every field of struct `S` exactly once with the field's pattern after its label (`type.unlabelled-field`, `type.missing-field`, `type.duplicate-label`), and matches when every field matches. An open struct is destructured only inside its package ([closed and open types](#closed-and-open-types)).

A named pattern that is not a binding and whose path names something other than a const, a variant, or a struct followed by field patterns, and a range bound that is not a literal or a constant, are errors (`pattern.path-kind`). A variant of another enum, a struct other than the pattern's type, or a literal or constant of another type is `type.mismatch`. Text and byte-string literals and constants match by length and bytes; integers, `bool` and `char` by value. Matching never runs user code and never traps.

```nan
const ESC_BYTE: u8 = 0x1B

pub closed enum Key derives Eq {
    Letter(u8)
    Escape
    Arrow(dx: i8, dy: i8)
    Other
} Key

pub fn decode_key(byte: u8) -> Key {
    return match byte {
        ESC_BYTE => Key.Escape
        b'a'..=b'z' or b'A'..=b'Z' => Key.Letter(byte)
        else => Key.Other
    }
} decode_key

pub fn is_horizontal(key: Key) -> bool {
    return match key {
        Key.Arrow(dx: _, dy: 0) => true
        else => false
    }
} is_horizontal

pub fn command_code(name: Str) -> Option[u8] {
    return match name {
        "quit" => Some(1)
        "help" => Some(2)
        else => None
    }
} command_code
```

```nan
pub fn is_small(size: u32, limit: u32) -> bool {
    return match size {
        0..=limit => true // error: pattern.path-kind
        else => false
    }
} is_small
```

### Pattern bindings

The names a pattern binds are distinct (`pattern.duplicate-binding`). What the names of a `for` pattern bind is fixed by [iteration](07-ownership.md#iteration). In a `let` statement or a `match` arm, each name is a pattern binding ([bindings](06-statements-expressions.md#bindings)) and binds its part of the scrutinee as follows:

- A Copy part is copied ([Copy](05-properties.md#copy)).
- A non-Copy part of an owned scrutinee is moved out of it, by a [partial move](07-ownership.md#ownership) when the scrutinee is a place. The scrutinee is **owned** when it is a temporary other than a [part or wrapped part](07-ownership.md#projection-results), or a place that owns its value as [ownership](07-ownership.md#ownership) states.
- A non-Copy part of any other scrutinee is bound as a [view binding](07-ownership.md#view-bindings), writable only as that section states. While the binding is live, a scrutinee that is a projection result keeps its [hold](07-ownership.md#holds), frozen or held as its projection gives, and a scrutinee that is a place is frozen.
- Parts matched by a wildcard, literal, range or constant are neither moved nor copied.

### Refutability

A pattern is **irrefutable** when every value of its type matches it, decided exactly as [exhaustiveness](06-statements-expressions.md#disjointness-and-exhaustiveness) is, and **refutable** otherwise. A refutable pattern in a `let` without `else`, or in a `for` loop, is an error (`pattern.refutable`).

```nan
pub fn level_or_zero(level: Option[u8]) -> u8 {
    let Some(value) = level else {
        return 0
    }
    return value
} level_or_zero
```

```nan
pub fn level_or_zero(level: Option[u8]) -> u8 {
    let Some(value) = level // error: pattern.refutable
    return value
} level_or_zero
```

## Diagnostics

| Code | Class | Rule |
| --- | --- | --- |
| `pattern.const-kind` | error | [Patterns](#patterns) |
| `pattern.duplicate-binding` | error | [Pattern bindings](#pattern-bindings) |
| `pattern.empty-range` | error | [Patterns](#patterns) |
| `pattern.open-enum-no-else` | error | [Closed and open types](#closed-and-open-types) |
| `pattern.open-struct` | error | [Closed and open types](#closed-and-open-types) |
| `pattern.path-kind` | error | [Patterns](#patterns) |
| `pattern.range-kind` | error | [Patterns](#patterns) |
| `pattern.refutable` | error | [Refutability](#refutability) |
| `predicate.default-unproven` | error | [Derived Default](#derived-default) |
| `predicate.exposed-field` | error | [Module boundary](#module-boundary) |
| `predicate.outside-write` | error | [Module boundary](#module-boundary) |
| `type.closed-hidden-field` | error | [Closed and open types](#closed-and-open-types) |
| `type.duplicate-label` | error | [Construction](#construction), [Patterns](#patterns) |
| `type.duplicate-member` | error | [Structs](#structs), [Enums](#enums), [Layouts](23-low-level.md#layouts), [Registers](23-low-level.md#registers) |
| `type.hidden-field` | error | [Structs](#structs) |
| `type.literal-range` | error | [Expected types and literals](#expected-types-and-literals) |
| `type.literal-untyped` | error | [Expected types and literals](#expected-types-and-literals), [Bindings](06-statements-expressions.md#bindings) |
| `type.mismatch` | error | [Expected types and literals](#expected-types-and-literals), [Patterns](#patterns), [Assignment](06-statements-expressions.md#assignment), [Conditionals and loops](06-statements-expressions.md#conditionals-and-loops), [Value-yielding if and match](06-statements-expressions.md#value-yielding-if-and-match), [Inference](08-generics-interfaces.md#inference), [Clauses](18-contracts.md#clauses), [Examples](18-contracts.md#examples), [Quantifiers](18-contracts.md#quantifiers), [Expect](18-contracts.md#expect) |
| `type.missing-field` | error | [Construction](#construction), [Patterns](#patterns) |
| `type.not-a-type` | error | [Structs](#structs) |
| `type.open-construction` | error | [Closed and open types](#closed-and-open-types) |
| `type.payload-shape` | error | [Construction](#construction), [Patterns](#patterns) |
| `type.qualified-core-variant` | error | [Option and Result](#option-and-result) |
| `type.recursive` | error | [Structs](#structs) |
| `type.unlabelled-field` | error | [Construction](#construction), [Patterns](#patterns) |
| `type.unlabelled-payload` | error | [Enums](#enums) |
