# Generics and interfaces

This chapter defines interfaces and their methods, impl lines and conformance, generic parameters and bounds, instantiation (generic arguments, checking at definition, inference and monomorphisation), method resolution and qualified calls, coherence, the core interfaces `Eq`, `Ord`, `Hash` and `Default`, the operator interfaces and built-in float comparison, dynamic dispatch through `Dyn[I]` and `mem::Box[Dyn[I]]`, and derives.

## Interfaces

An **interface** is a nominal set of method heads with optional default bodies, declared by an interface item. Its members are its **interface methods**. Inside the item, `Self` is an implicit [generic parameter](#generic-parameters) standing for the implementing type, bounded by the interface with its own parameters as arguments and by `Any`, and `self` has type `Self`. Outside interface items, `Self` is an ordinary identifier.

```ebnf
interface_item = "interface" IDENT generic_params? "{" interface_member* COL0 "}" item_label NL ;
interface_member = ( DOC NL )* "proj"? "fn" IDENT generic_params? "(" param_list? ")" fn_result? fails_clause? uses_clause? limits_clause? method_end ;
method_end = method_body | NL ( clause_region method_body? )? ;
method_body = "{" stmt* "}" IDENT NL ;
```

- Interface methods have the interface's visibility. Their names are distinct within the interface (`item.duplicate`), and the `IDENT` closing a default body equals the method's name (`item.label-mismatch`).
- An interface method whose first parameter is `self`, `mut self` or `take self` is called on a value; one without `self` is called on a type, as `T.default()`. Parameters, results, `fails` and `uses wait` follow [heads](02-items.md#heads), except that an interface method is never `item.fails-unneeded` or `item.wait-unneeded`.
- A `proj` method is a [projection](07-ownership.md#projections) at every call through the interface, and its default body is checked as one.
- The clause region of an interface method holds `requires`, `ensures` and `todo` clauses, and, when the method has a default body, `decreases` and `waive` clauses. Any other clause there is an error (`interface.method-clause`). A `todo` clause of an interface method holding an example clause is that error too. How callers and implementing functions use interface contracts is defined in [interface contracts](18-contracts.md#interface-contracts).
- A default body opens with `{` at the end of the head line when the method has no clause region, and with `{` alone on the line after its last clause otherwise; any other placement is an error (`item.body-opener`).
- A `limits` clause on an interface method is a demand on every implementing function ([Conformance](#conformance)).
- A default body is checked once, at the interface, with `Self` and the interface's own parameters as type parameters having only their bounds ([checking at definition](#checking-at-definition)), and meets the method's limits like any implementing function.
- The implicit `Self` is compared with the interface's other generic parameters and its module's types as every generic parameter is ([naming conventions](03-packages-modules.md#naming-conventions)), so a type of that module or a generic parameter of the interface named `Self` is an error (`name.clash`).

```nan
pub interface Shape {
    fn area(self) -> u64
    fn is_empty(self) -> bool {
        return self.area() == 0
    } is_empty
} Shape

pub closed struct Square derives Copy {
    pub side: u32
} Square

impl Square: Shape

pub fn Square.area(self) -> u64 {
    let side = u64.widen(self.side)
    return side * side
} Square.area

pub fn has_area(square: Square) -> bool {
    return not square.is_empty()
} has_area
```

```nan
pub interface Gauge {
    fn level(self) -> u32
        ensures result <= 100
        example true // error: interface.method-clause
} Gauge
```

## Impl lines

An **impl line** is the single-line item `impl Type: Interface` declaring that a type implements an interface.

```ebnf
impl_item = "impl" path impl_params? ":" bound NL ;
impl_params = "[" impl_param ( "," impl_param )* "]" ;
impl_param = IDENT ( ":" bound ( "and" bound )* )? ;
```

- The path after `impl` is the **impl target**. It names a struct, an enum, a layout, a [scalar](04-types.md#scalars), `Unit`, `Str`, `Array`, `Slice`, `SliceMut`, `Option`, `Result`, `Untrusted`, or a std [base type](05-properties.md#structural-properties). Any other impl target, such as a type parameter, `Dyn`, a registers block, an interface or a const, is an error (`interface.impl-target`).
- The bound after `:` names an interface (`interface.not-interface`) with all its generic arguments.
- Without `impl_params`, the impl line covers every instantiation of the impl target. With them, it lists exactly the impl target's generic parameters, by their declared names and in order, and each type parameter may add bounds; const parameters add none. Any other list, such as type arguments in place of parameter names, is an error (`interface.impl-parameters`). The impl line **applies** to an instantiation exactly when its arguments satisfy the impl target's bounds and the added ones, and each argument for a parameter droppable for the impl line is droppable. A std impl line that its owning chapter restricts to some instantiations applies only to those.
- A type parameter that the impl target bounds by `Any` and that `impl_params` lists without `Any` among its added bounds is **droppable for the impl line**: the line applies only to instantiations whose argument for it is droppable ([generic parameters](05-properties.md#generic-parameters)), and the functions implementing its methods count it as droppable.
- An impl line is in the module declaring its impl target or the module declaring its interface; anywhere else it is an error (`interface.impl-misplaced`). A misplaced impl line is reported once and otherwise not checked.
- An impl line naming a [property interface](05-properties.md#property-interfaces) is rejected as that section states.
- An impl line is visible wherever both its impl target and its interface are. The impl target's generic parameters are in scope in the interface's arguments, with or without `impl_params`.

### Conformance

For an impl line of impl target `T` and interface `I` in module `M`, the **implementing function** of each method `m` of `I` is the function item `T.m` declared in `M` ([methods](02-items.md#methods)), or else the default body of `m` with `Self` replaced by `T`.

- A method with neither is an error (`interface.missing-method`), reported on the impl line.
- `T.m` matches the method's head read with `Self` replaced by `T` and the interface's parameters by the impl line's arguments: its modifier is the method's, or none when the method is `proj` and its result type is droppable ([projections](07-ownership.md#projections)); it has as many generic parameters of its own, with the same bounds in order; its parameters have the method's names, modes and types in order; its result is the method's; its failure type is the method's or it has no `fails` clause; and it states `uses wait` only when the method does. A mismatch is an error (`interface.head-mismatch`), reported on the head of `T.m`.
- The method's limits apply to `T.m` as if its own head stated them. A digest lacking a demanded fact is reported as [limits](10-effects-digests.md#limits) states, with a `declared` reason for the interface method.
- The visibility of `T.m` is the less visible of `T` and `I`, private counting least and `pub` most. Any other visibility is an error (`interface.impl-visibility`), whose exact fix writes that one.
- The bounds an impl line adds hold inside each function implementing one of its methods and are bounds of that function: calling it on an instantiation the impl line does not apply to is an error (`generic.unsatisfied-bound`).
- A function `T.m` in the module of `T`, a field or a variant of `T` named `m`, where `m` is a method of an interface that `T` [derives](#derives), is an error (`interface.method-collision`), reported on that function's head, field or variant.
- An impl line outside the module of `T` whose interface is declared in the package of `T` and has a method `m`, while the module of `T` declares a function `T.m`, is an error (`interface.method-collision`), reported on the impl line.

```nan
pub interface Describe {
    fn code(self) -> u32
} Describe

pub closed struct Pair[T] {
    pub left: T
    pub right: T
} Pair

impl Pair[T: Describe]: Describe

pub fn Pair.code(self) -> u32 {
    return self.left.code().wrapping_add(self.right.code())
} Pair.code

impl u32: Describe

pub fn u32.code(self) -> u32 = self

pub fn pair_code(low: u32, high: u32) -> u32 {
    let pair = Pair(left: low, right: high)
    return pair.code()
} pair_code
```

```nan
pub interface Describe {
    fn code(self) -> u32
} Describe

pub closed struct Pair[T] {
    pub left: T
    pub right: T
} Pair

impl Pair[u32]: Describe // error: interface.impl-parameters

pub fn Pair.code(self) -> u32 {
    return 0
} Pair.code
```

```nan
pub interface Shape {
    fn area(self) -> u64
    fn perimeter(self) -> u64
} Shape

pub closed struct Square derives Copy {
    pub side: u32
} Square

impl Square: Shape // error: interface.missing-method

pub fn Square.area(self) -> u64 {
    let side = u64.widen(self.side)
    return side * side
} Square.area
```

```nan
pub interface Settle {
    fn settled(self) -> u32 limits terminates
} Settle

pub closed struct Level derives Copy {
    pub value: u32
} Level

impl Level: Settle

pub fn Level.settled(self) -> u32 {
    var value = self.value
    while value > 10 { // error: limit.may-not-terminate
        value = value / 2
    }
    return value
} Level.settled
```

```nan
pub closed struct Tag derives Copy, Eq {
    pub id: u32
} Tag

pub fn Tag.eq(self, other: Tag) -> bool { // error: interface.method-collision
    return self.id == other.id
} Tag.eq
```

```nan
// file: example/package.nan
//! Shapes and their codes.

package example {
    language 1
    targets any
} example

// file: example/geo/geo.nan
//! Plane shapes.

pub closed struct Square derives Copy {
    pub side: u32
} Square

// file: example/codes/codes.nan
//! Numeric codes.

pub interface Describe {
    fn code(self) -> u32
} Describe

// file: example/example.nan
//! Codes for shapes.

impl geo::Square: codes::Describe // error: interface.impl-misplaced
```

## Generic parameters

A **generic parameter** is a type parameter or a const parameter of a struct, enum, interface, interface method or function, listed in square brackets after the item's name. A **bound** is an interface that every argument of a type parameter implements, or a condition that every argument of a const parameter satisfies.

```ebnf
generic_params = "[" generic_param ( "," generic_param )* "]" ;
generic_param = IDENT ( ":" bound ( "and" bound )* where_clause? )? ;
bound = path generic_args? ;
```

- A generic parameter whose first bound names a scalar is a **const parameter** of that type: `N: usize where N > 0`. Its type is an integer scalar or `bool`, and it has no other bound (`generic.const-type`). Inside the item it is a value of its type, usable in expressions, predicates, contracts and generic arguments.
- Every other generic parameter is a type parameter; its bounds name interfaces (`interface.not-interface`), joined by `and`. A type parameter without bounds is droppable and has no members ([generic parameters](05-properties.md#generic-parameters)).
- A `where` clause follows only a const parameter (`generic.where-misplaced`). Its condition is a `bool` [contract term](18-contracts.md#contract-terms) over the const parameters in scope, consts and literals.
- A generic parameter is in scope in the head, clauses and body of its item, and those of an interface in its methods.
- Each generic parameter of a function's or interface method's own list occurs in the type of at least one of its parameters (`generic.parameter-unused`).

```nan
pub fn largest[T: Ord and Copy](values: Slice[T], floor: T) -> T {
    var best = floor
    for value in values {
        if value > best {
            best = value
        }
    }
    return best
} largest

pub closed struct Window[N: usize where N > 0] {
    pub samples: Array[u32, N]
    pub next: usize where next < N
} Window

pub fn Window.capacity(self) -> usize = N
```

```nan
pub fn zero_of[T: Default]() -> T { // error: generic.parameter-unused
    return T.default()
} zero_of
```

## Instantiation

An **instantiation** is a generic item with every generic parameter fixed by an argument.

```ebnf
generic_args = "[" generic_arg ( "," generic_arg )* "]" ;
generic_arg = type_expr | "-"? INT | "true" | "false" ;
```

- An argument list has one argument per parameter, in order (`generic.arity`). A type parameter takes a type. A const parameter takes a literal, a const item or a const parameter in scope, the last two written as a `type_expr` path; any other argument kind is an error (`generic.argument-kind`). The parameter's type is a const argument's [expected type](04-types.md#expected-types-and-literals).
- A type expression, a bound and the argument of `Dyn` write all the arguments of the generic type or interface they name (`generic.arity`). A construction, a variant path and a member path `T.f` write all of a type's arguments or none, and omitted ones are [inferred](#inference). An impl target, a function name `Type.name` and a closing label write none. A function's own arguments are never written ([field access and method calls](06-statements-expressions.md#field-access-and-method-calls)).
- Each argument satisfies its parameter's bounds. A type argument not implementing a bound, a property interface included, is an error (`generic.unsatisfied-bound`); linear and view arguments have the codes of [generic parameters](05-properties.md#generic-parameters) and [view arguments](05-properties.md#view-arguments).
- A const parameter's `where` condition holds for the arguments: the interpreter evaluates it by [const evaluation](21-interpreter.md#const-evaluation) when every argument is a value, and otherwise the [prover](20-prover.md#procedure) proves it from the `where` conditions in scope. A condition that is false or unproven is an error (`generic.where-unmet`).
- Two instantiations are the same type exactly when they instantiate the same item with pairwise equal arguments: equal types, and const arguments that are equal values or the same const parameter.

```nan
pub const WIDTH: usize = 2 * 2

pub fn same(values: Array[u8, WIDTH]) -> Array[u8, 4] {
    return values
} same

pub fn doubled(values: Array[u8, WIDTH]) -> Array[u8, 8] {
    return values // error: type.mismatch
} doubled
```

```nan
pub const DEPTH: u32 = 4

pub fn layered(values: Array[u8, DEPTH]) -> u8 = 0 // error: type.mismatch
```

```nan
pub closed struct Window[N: usize where N > 0] {
    pub samples: Array[u32, N]
} Window

pub fn Window.capacity(self) -> usize = N

pub fn empty_capacity(window: Window[0]) -> usize { // error: generic.where-unmet
    return window.capacity()
} empty_capacity
```

### Checking at definition

A generic item is checked once, at its definition. There each type parameter has exactly the members, operators and properties its bounds give, each const parameter satisfies its `where` conditions as facts, and a call to a bound's method has the digest that method's limits give ([indirect calls](10-effects-digests.md#indirect-calls)). Instantiating a checked generic item produces only the diagnostics of the [instantiation](#instantiation) rules, the `prop` codes for type arguments, `error.from-ambiguous` ([error enums](11-errors-domains.md#error-enums)) and the checks of a [final build](27-program-model.md#final-builds); no diagnostic arises inside its body.

### Inference

A call infers the generic arguments of the function it calls from its arguments, receiver first, in order. For each argument, the parameter's type is taken with the arguments fixed so far substituted:

- When the argument's type is fixed without an expected type, it is matched against that type, part by part, and each unfixed parameter is fixed to the part at its position. A part that differs is an error (`type.mismatch`).
- Otherwise, such as for an unsuffixed literal, the argument takes that type as its expected type when it holds no unfixed parameter, and has no expected type otherwise.

The omitted arguments of a type in a construction, variant path or member path `T.f(...)` are fixed first by matching the value's type, or the function's result type, against the expected type, then by the arguments, as for a function's own parameters. A generic function used as a value takes its arguments from the expected function type. Failure parameters are fixed as [failure polymorphism](10-effects-digests.md#failure-polymorphism) and [failure bounds](11-errors-domains.md#failure-bounds) state, and an argument for a function-typed parameter is judged by [fit](10-effects-digests.md#fit), not by matching. A parameter left unfixed is an error (`generic.uninferred`).

```nan
pub fn first_or[T: Copy](values: Slice[T], fallback: T) -> T {
    if values.len() == 0 {
        return fallback
    }
    return values[0]
} first_or

pub fn first_level(levels: Slice[u8]) -> u8 {
    return first_or(levels, fallback: 0)
} first_level
```

```nan
pub fn fallback_or[T: Copy](fallback: T, values: Slice[T]) -> T {
    if values.len() == 0 {
        return fallback
    }
    return values[0]
} fallback_or

pub fn first_level(levels: Slice[u8]) -> u8 {
    return fallback_or(0, values: levels) // error: type.literal-untyped
} first_level
```

### Monomorphisation

Each instantiation behaves exactly as its generic item with every parameter replaced by its argument. Lowering emits code per instantiation reachable in the final closure, one copy per [instantiation key](29-identity-versioning.md#instantiation-keys); no behaviour depends on whether two instantiations share code.

## Method resolution

**Method resolution** finds the function a call `x.f(...)` names. Let `R` be the type of `x`. When `R` is a type parameter, the candidates are the methods `f` of its bounds. When `R` is `Dyn[I]` or `mem::Box[Dyn[I]]`, they are the method `f` of `I` and the box members that [dynamic dispatch](#dynamic-dispatch) gives. Otherwise, with `T` the type `R` is or instantiates, they are exactly the function `T.f` declared in the module that declares `T` ([methods](02-items.md#methods)) and the methods `f` of the interfaces that `R` implements and that are declared in the package declaring `T` or in `core`. A std member that its owning chapter declares only for some instantiations is a candidate only when `R` is one of them.

- A candidate that is an interface method stands for its implementing function for `R`, and for itself when `R` is a type parameter. The same candidates serve a member path `R.f(...)`, which also reaches functions without `self`.
- Two candidates standing for different functions are an error (`interface.ambiguous-method`), with one exact fix per candidate interface `I` writing the qualified call `I.f(x, ...)`.
- When no candidate exists and `f` is a method of interfaces visible at the call that `R` implements, the call is an error (`interface.unqualified-call`), with one exact fix per such interface `I` writing the qualified call `I.f(x, ...)`; otherwise it is `name.unknown`.
- The call `x.f(a, b: c)` passes `x` as the first argument, marked as [calls](06-statements-expressions.md#calls) state.

A **qualified call** `I.f(x, ...)`, with `I` a path to an interface, calls method `f` of `I`. `Self` is fixed by the arguments as in [inference](#inference), then by the expected type, and the interface's arguments by the impl line or bound through which `Self` implements `I`. A `Self` that is `Dyn[I]` or `mem::Box[Dyn[I]]` counts as implementing `I`, and the call dispatches. Any other `Self` not implementing `I`, and arguments written after `I` that differ from those, are an error (`interface.not-implemented`).

```nan
// file: geo/package.nan
//! Plane measures.

package geo {
    language 1
    targets any
} geo

// file: geo/geo.nan
//! The measure interface.

pub interface Measure {
    fn size(self) -> u64
    fn is_small(self) -> bool {
        return self.size() < 10
    } is_small
} Measure

// file: plot/package.nan
//! Plot strips.

package plot {
    language 1
    targets any
    dep geo "../geo"
} plot

// file: plot/plot.nan
//! Strips measured through geo.

pub closed struct Strip derives Copy {
    pub length: u32
} Strip

impl Strip: geo::Measure

pub fn Strip.size(self) -> u64 = u64.widen(self.length)

pub fn strip_size(strip: Strip) -> u64 {
    return strip.size()
} strip_size

pub fn is_short(strip: Strip) -> bool {
    return geo::Measure.is_small(strip)
} is_short
```

```nan
// file: geo/package.nan
//! Plane measures.

package geo {
    language 1
    targets any
} geo

// file: geo/geo.nan
//! The measure interface.

pub interface Measure {
    fn size(self) -> u64
    fn is_small(self) -> bool {
        return self.size() < 10
    } is_small
} Measure

// file: plot/package.nan
//! Plot strips.

package plot {
    language 1
    targets any
    dep geo "../geo"
} plot

// file: plot/plot.nan
//! Strips measured through geo.

pub closed struct Strip derives Copy {
    pub length: u32
} Strip

impl Strip: geo::Measure

pub fn Strip.size(self) -> u64 = u64.widen(self.length)

pub fn is_short(strip: Strip) -> bool {
    return strip.is_small() // error: interface.unqualified-call
} is_short
```

## Coherence

A type has at most one impl line per interface, whatever the interface's arguments, and a [derive](#derives) of an interface counts as one, located on the type's first line. An impl line is identified by its impl target and its interface without arguments. A second one is an error (`interface.duplicate-impl`), reported on each except the one whose file path, then line, sorts first; a reported impl line is otherwise not checked.

Conformance comes only from impl lines, derives, and the built-in conformances that owning chapters declare, such as those of layouts and `hw::Ptr` ([layout values](23-low-level.md#layout-values), [pointers](23-low-level.md#pointers)) and of base types. An impl line applies to one impl target type and never to a type parameter, so no impl line covers another's instantiations. An interface requires no other interface and inherits no method, and no function is overloaded: two functions of one module and two members of one type never share a name ([order and names](02-items.md#order-and-names)).

```nan
pub closed struct Square derives Copy, Eq {
    pub side: u32
} Square

impl Square: Eq // error: interface.duplicate-impl
```

## Equality, ordering, hashing and defaults

Core declares the interfaces `Eq`, `Ord`, `Hash` and `Default`, the result type `core::Ordering` of `Ord.cmp` and the interface `core::Hasher` that `Hash.hash` writes to.

```nan-api
// module: core

pub interface Eq {
    fn eq(self, other: Self) -> bool limits pure, notrap, terminates
} Eq

pub interface Ord {
    fn cmp(self, other: Self) -> Ordering limits pure, notrap, terminates
} Ord

pub closed enum Ordering derives Copy, Eq, Hash, Render {
    Less
    Equal
    Greater
} Ordering

pub interface Hasher {
    fn write(mut self, bytes: Slice[u8]) limits pure, notrap, terminates
} Hasher

pub interface Hash {
    fn hash[H: Hasher](self, mut hasher: H) limits pure, notrap, terminates
} Hash

pub interface Default {
    fn default() -> Self limits pure, terminates
} Default
```

- Core implements `Eq`, `Ord`, `Hash` and `Default` for the integer scalars, `bool`, `char` and `Unit`. Integers and `char` compare by value, `false` is less than `true`, and the one `Unit` value equals itself. The float types implement only `Default` ([float comparison](#float-comparison)).
- `Slice[T]`, `SliceMut[T]` and `Array[T, N]` implement each of `Eq`, `Ord` and `Hash` when `T` does. Two of them are equal when their lengths and elements are equal, and they are ordered lexicographically in index order, a proper prefix being less. `Str` implements `Eq`, `Ord` and `Hash` as `Slice[u8]` does on its bytes. `Array[T, N]` implements `Default` when `T` does.
- Core hashing writes to the hasher: for an integer, its `to_bits` value in little-endian order, one byte per 8 bits of width; for `bool`, one byte, 0 or 1; for `char`, its scalar value as 4 bytes little-endian; for `Unit`, nothing; for `Slice[T]` and `SliceMut[T]`, the length as a `u64` in 8 bytes little-endian, then each element's hash in index order; for `Str`, the length the same way, then its bytes; for `Array[T, N]`, each element's hash in index order.
- The defaults are 0 for integers, `+0.0` for floats, `false`, U+0000, `Unit`, and arrays of element defaults.

```nan
pub closed struct Version derives Copy, Eq {
    pub major: u16
    pub minor: u16
} Version

impl Version: Ord

pub fn Version.cmp(self, other: Version) -> core::Ordering {
    if self.major != other.major {
        return self.major.cmp(other.major)
    }
    return self.minor.cmp(other.minor)
} Version.cmp

pub fn is_newer(candidate: Version, current: Version) -> bool {
    return candidate > current
} is_newer
```

## Operator interfaces

The **operator interfaces** are `core::Add` to `core::Shr` of the table, `Eq` and `Ord`. Every arithmetic, bitwise, shift and comparison operator calls a method of one of them, except float comparison; `and`, `or`, `not` and the range operators are defined in [Statements and expressions](06-statements-expressions.md#operators). In the table, `R` is the right operand's type and `O` the result's.

| Interface | Method | Operator |
| --- | --- | --- |
| `core::Add[R, O]`, `core::Sub[R, O]`, `core::Mul[R, O]`, `core::Div[R, O]`, `core::Rem[R, O]` | `add`, `sub`, `mul`, `div`, `rem` | `a + b`, `a - b`, `a * b`, `a / b`, `a % b` |
| `core::BitAnd[R, O]`, `core::BitOr[R, O]`, `core::BitXor[R, O]`, `core::Shl[R, O]`, `core::Shr[R, O]` | `bit_and`, `bit_or`, `bit_xor`, `shl`, `shr` | `a & b`, `a \| b`, `a ^ b`, `a << b`, `a >> b` |
| `core::Neg[O]`, `core::BitNot[O]` | `neg`, `bit_not` | `-a`, `~a` |

```nan-api
// module: core

pub interface Add[R, O] {
    fn add(self, rhs: R) -> O limits pure, terminates
} Add

pub interface Neg[O] {
    fn neg(self) -> O limits pure, terminates
} Neg
```

- Each other binary interface is declared exactly as `Add`, and `BitNot` exactly as `Neg`, with the interface and method names of the table. A binary operator `a op b` means `I.m(a, b)` and a unary one `I.m(a)`, except a [negative literal](04-types.md#expected-types-and-literals).
- `a == b` means `Eq.eq(a, b)` and `a != b` means `not Eq.eq(a, b)`. With `c` the value of `Ord.cmp(a, b)`, `a < b` is `c == core::Ordering.Less`, `a > b` is `c == core::Ordering.Greater`, `a <= b` is `c != core::Ordering.Greater` and `a >= b` is `c != core::Ordering.Less`.
- The left operand's type fixes the impl line or bound, and with it `R` and `O`. `R`, and for `Eq` and `Ord` the left operand's type, is the right operand's expected type. A left operand type that does not implement the interface is an error (`interface.not-implemented`), except for a number type whose cell in [number operators](09-numbers.md#number-operators) names a code, which is reported instead; when that type is a type parameter of the enclosing item, the exact fix adds the bound to it.
- Operands are evaluated left to right, then the method runs ([evaluation order](06-statements-expressions.md#evaluation-order)).
- The implementing functions of the number types and their traps are defined in [Numbers](09-numbers.md#number-operators).

```nan
pub closed struct Offset derives Copy {
    pub bytes: u64
} Offset

pub closed struct Address derives Copy, Eq, Ord {
    pub value: u64
} Address

impl Address: core::Add[Offset, Address]

pub fn Address.add(self, rhs: Offset) -> Address {
    return Address(value: self.value + rhs.bytes)
} Address.add

pub fn page_end(start: Address) -> Address {
    return start + Offset(bytes: 4_096)
} page_end
```

```nan
pub closed struct Offset derives Copy {
    pub bytes: u64
} Offset

pub closed struct Address derives Copy, Eq, Ord {
    pub value: u64
} Address

pub fn page_start(end: Address) -> Address {
    return end - Offset(bytes: 4_096) // error: interface.not-implemented
} page_start
```

### Float comparison

`f32` and `f64` implement neither `Eq`, `Ord` nor `Hash`. The six comparison operators on two operands of the same float type are built in and follow IEEE 754: `==` is true exactly when neither operand is NaN and the values are equal, `+0.0` equalling `-0.0`; `!=` is its negation; `<`, `<=`, `>` and `>=` are false when either operand is NaN and otherwise compare values. They never trap. Total orders over floats are defined in [Numbers](09-numbers.md#total-order).

```nan
pub fn clamp_unit(value: f64) -> f64
    example clamp_unit(2.5) == 1.0
{
    if value < 0.0 {
        return 0.0
    }
    if value > 1.0 {
        return 1.0
    }
    return value
} clamp_unit
```

```nan
pub fn count_equal[T: Eq](values: Slice[T], target: T) -> usize {
    var count: usize = 0
    for value in values {
        if value == target {
            count += 1
        }
    }
    return count
} count_equal

pub fn count_half(levels: Slice[f64]) -> usize {
    return count_equal(levels, target: 0.5) // error: generic.unsatisfied-bound
} count_half
```

## Dynamic dispatch

`Dyn[I]` is a [view](05-properties.md#views) of a value of some type implementing interface `I`, through which calls to `I`'s methods dispatch to that type's implementing functions; `mem::Box[Dyn[I]]` owns such a value in a [box](07-ownership.md#box). The argument of `Dyn` is an interface with its arguments (`interface.not-interface`).

An interface is **Dyn-usable** when it is not a [property interface](05-properties.md#property-interfaces) and each of its methods has a first parameter `self` or `mut self`, has no generic parameters of its own, and names `Self` nowhere else in its head. Dyn-usability is computed, never declared, and enters the interface's [interface hash](29-identity-versioning.md#item-hash-and-interface-hash). `Dyn[I]` with an interface that is not Dyn-usable is an error (`interface.not-dyn-usable`), whose because-chain names the method.

These core forms make the values, with `x` of a type `T` implementing `I`:

| Form | Result | Rule |
| --- | --- | --- |
| `Dyn[I].view(x)` | `Dyn[I]` | a read projection with receiver `x`; legal exactly when no method of `I` takes `mut self` |
| `Dyn[I].view_mut(mut x)` | `Dyn[I]` | a mut projection with receiver `x`; legal exactly when a method of `I` takes `mut self` |
| `mem::Box[Dyn[I]].from_box(take b)` | `mem::Box[Dyn[I]]` | `b` is a `mem::Box[T]` with `T` droppable; the contents stay in place |

- Of the members of `mem::Box`, `mem::Box[Dyn[I]]` has exactly `contents`, legal exactly where `Dyn[I].view` is, and `contents_mut`, legal exactly where `Dyn[I].view_mut` is. Their results are `Dyn[I]` views of the boxed value.
- Using the other projection form than these rules allow is an error (`interface.dyn-view-mode`), whose exact fix writes the allowed one. `T` not implementing `I` is `interface.not-implemented`. The result holds its source as a projection result does ([holds](07-ownership.md#holds)).
- A call `d.m(...)` on a `Dyn[I]` or `mem::Box[Dyn[I]]` runs the implementing function of `m` for `T`. A `mut self` method marks the receiver `mut`.
- `Dyn[I]` and `mem::Box[Dyn[I]]` implement no interface, so they satisfy no bound other than `Any`. `mem::Box[Dyn[I]]` is affine and is not a view, Copy, Send, Inert or AnyBits.
- The digest of a dispatched call is fixed by the method's limits ([indirect calls](10-effects-digests.md#indirect-calls)).

```nan
pub interface Counter {
    fn bump(mut self)
    fn total(self) -> u32
} Counter

pub closed struct Clicks {
    pub count: u32
} Clicks

impl Clicks: Counter

pub fn Clicks.bump(mut self) {
    self.count = self.count.saturating_add(1)
} Clicks.bump

pub fn Clicks.total(self) -> u32 = self.count

pub fn bump_twice(mut counter: Dyn[Counter]) -> u32 {
    mut counter.bump()
    mut counter.bump()
    return counter.total()
} bump_twice

pub fn bump_clicks(mut clicks: Clicks) -> u32 {
    return bump_twice(Dyn[Counter].view_mut(mut clicks))
} bump_clicks

pub fn erase_clicks(take clicks: mem::Box[Clicks]) -> mem::Box[Dyn[Counter]] {
    return mem::Box[Dyn[Counter]].from_box(take clicks)
} erase_clicks
```

```nan
pub interface Counter {
    fn bump(mut self)
} Counter

pub closed struct Clicks {
    pub count: u32
} Clicks

impl Clicks: Counter

pub fn Clicks.bump(mut self) {
    self.count = self.count.saturating_add(1)
} Clicks.bump

pub fn bump_once(mut counter: Dyn[Counter]) {
    mut counter.bump()
} bump_once

pub fn bump_clicks(clicks: Clicks) {
    bump_once(Dyn[Counter].view(clicks)) // error: interface.dyn-view-mode
} bump_clicks
```

```nan
pub fn count_views(values: Slice[Dyn[Hash]]) -> usize { // error: interface.not-dyn-usable
    return values.len()
} count_views
```

## Derives

A **derive** is a compiler-written conformance listed in the `derives` clause of a struct or enum ([structs](04-types.md#structs), [enums](04-types.md#enums)).

```ebnf
derives_clause = "derives" IDENT ( "," IDENT )* ;
```

- The entries are exactly the core names `Copy`, `Eq`, `Ord`, `Hash`, `Default`, `Render`, `Encode` and `Generate`, and the promises `Send`, `Inert` and `AnyBits`. Any other entry is an error (`derive.unknown`), and an entry listed twice is an error (`derive.duplicate`). Entries are order-free; `nan fmt` writes them in [canonical order](35-format.md#canonical-order).
- `Copy` is defined in [Copy](05-properties.md#copy), the promises in [Promises](05-properties.md#promises), `Render` in [derived Render](13-text-encode.md#derived-render), `Encode` in [Encode](13-text-encode.md#encode), and `Generate` in [generation](22-testing.md#generation).
- A derive of `Eq`, `Ord`, `Hash` or `Default` is an impl line in the type's module whose methods the compiler writes as members of the type, with the visibility that [conformance](#conformance) gives; [`nan expand`](33-toolchain.md#commands) prints them. They meet their interface's limits.
- A derive of any interface other than `Copy` and the promises, `I` on a type `S`, is checked at the definition: each constituent type of `S` implements `I`, counting each type parameter of `S` as implementing it. A constituent type that does not is an error (`derive.constituent-missing`), reported on the type's first line. A **derive parameter** is a type parameter of `S` that the check needs: counting it alone as not implementing `I` leaves some constituent type not implementing `I`. The derived impl applies to an instantiation exactly when each derive parameter's argument implements `I`.

The derived methods:

- `Eq`: two struct values are equal when every pair of fields is; two enum values when they are the same variant and every pair of payload values is equal.
- `Ord`: structs compare fields in declaration order, the first pair that is not `Equal` deciding; enum values of different variants order by the variants' declaration positions, earlier being less, and of one variant compare payload values as fields. For a type deriving `Ord`, the declaration order of its fields and variants is part of its interface.
- `Hash`: a struct hashes each field in byte order of field names. An enum value writes its variant's name, as its byte length in 8 bytes little-endian followed by its UTF-8 bytes, then hashes its payload values: a single unlabelled payload, or labelled ones in byte order of labels.
- `Default`: defined for structs only ([derived Default](04-types.md#derived-default)); on an enum it is an error (`derive.enum-default`).

```nan
pub closed enum Priority derives Copy, Eq, Ord {
    Low
    Normal
    Urgent
} Priority

pub fn is_pressing(priority: Priority) -> bool
    example is_pressing(Priority.Urgent)
    example not is_pressing(Priority.Low)
{
    return priority > Priority.Normal
} is_pressing
```

```nan
pub closed struct Reading derives Eq { // error: derive.constituent-missing
    pub level: f64
} Reading
```

## Diagnostics

| Code | Class | Rule |
| --- | --- | --- |
| `derive.constituent-missing` | error | [Derives](#derives), [Opacity](15-untrusted.md#opacity) |
| `derive.duplicate` | error | [Derives](#derives) |
| `derive.enum-default` | error | [Derives](#derives) |
| `derive.unknown` | error | [Derives](#derives) |
| `generic.argument-kind` | error | [Instantiation](#instantiation) |
| `generic.arity` | error | [Instantiation](#instantiation) |
| `generic.const-type` | error | [Generic parameters](#generic-parameters) |
| `generic.parameter-unused` | error | [Generic parameters](#generic-parameters) |
| `generic.uninferred` | error | [Inference](#inference) |
| `generic.unsatisfied-bound` | error | [Conformance](#conformance), [Instantiation](#instantiation), [Failure bounds](11-errors-domains.md#failure-bounds), [Templates](13-text-encode.md#templates), [Opacity](15-untrusted.md#opacity) |
| `generic.where-misplaced` | error | [Generic parameters](#generic-parameters) |
| `generic.where-unmet` | error | [Instantiation](#instantiation) |
| `interface.ambiguous-method` | error | [Method resolution](#method-resolution) |
| `interface.duplicate-impl` | error | [Coherence](#coherence) |
| `interface.dyn-view-mode` | error | [Dynamic dispatch](#dynamic-dispatch) |
| `interface.head-mismatch` | error | [Conformance](#conformance), [Projection results](07-ownership.md#projection-results), [Components](26-components.md#components) |
| `interface.impl-misplaced` | error | [Impl lines](#impl-lines) |
| `interface.impl-parameters` | error | [Impl lines](#impl-lines) |
| `interface.impl-target` | error | [Impl lines](#impl-lines) |
| `interface.impl-visibility` | error | [Conformance](#conformance) |
| `interface.method-clause` | error | [Interfaces](#interfaces) |
| `interface.method-collision` | error | [Conformance](#conformance) |
| `interface.missing-method` | error | [Conformance](#conformance) |
| `interface.not-dyn-usable` | error | [Dynamic dispatch](#dynamic-dispatch) |
| `interface.not-implemented` | error | [Method resolution](#method-resolution), [Operator interfaces](#operator-interfaces), [Dynamic dispatch](#dynamic-dispatch), [Opacity](15-untrusted.md#opacity) |
| `interface.not-interface` | error | [Impl lines](#impl-lines), [Generic parameters](#generic-parameters), [Dynamic dispatch](#dynamic-dispatch) |
| `interface.unqualified-call` | error | [Method resolution](#method-resolution) |
