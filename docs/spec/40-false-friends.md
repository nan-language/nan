# False friends

A **false friend** is a NaN form spelt like a form of C, Rust, Go, Python or TypeScript but meaning something else, or a mainstream form that NaN rejects. This chapter lists the false friends shared by several of these languages, then those of each language. Each row gives the mainstream form, what NaN does with that spelling or writes for that intent, and the section owning the rule, which alone defines it. Exact fixes for `syntax.foreign` come from the [idiom table](34-diagnostics.md#idiom-table).

## Common forms

| Form | In NaN | Rule |
| --- | --- | --- |
| `&&`, `\|\|`, `!` | `and`, `or`, `not`; the foreign spellings are `syntax.foreign` | [Logical operators](06-statements-expressions.md#logical-operators) |
| `;` ending a statement | A statement ends with its logical line; `;` is `syntax.foreign` | [Statements](06-statements-expressions.md#statements) |
| `/* ... */` | Only line comments exist | [Comments](01-lexical.md#comments) |
| `if count`, `while node` | A condition is a `bool`; no value converts to one (`type.mismatch`) | [Conditionals and loops](06-statements-expressions.md#conditionals-and-loops) |
| `c ? a : b`, `a if c else b` | A value-yielding `if` with an `else`; `?` is `syntax.foreign` | [Value-yielding if and match](06-statements-expressions.md#value-yielding-if-and-match) |
| `x++`, `x--`, `if (x = 0)` | `x += 1`, `x -= 1` (`syntax.foreign`); an assignment is a statement | [Assignment](06-statements-expressions.md#assignment) |
| `null`, `nil`, `None`, `undefined` | Absence is `None` of `Option[T]`; no null literal exists | [Option and Result](04-types.md#option-and-result) |
| Tuples, multiple results, `()`, `void` | A named struct; `Unit`, which a head never writes as its result (`item.unit-result`) | [Structs](04-types.md#structs), [Heads](02-items.md#heads) |
| `throw`, `raise`, error codes | `fail e`, where `e` converts to the type of the head's `fails` clause (`error.unconvertible`) | [Fail](11-errors-domains.md#fail) |
| `try { ... } catch`, `except` | `try f(x)` passes a call's failure to the caller, `attempt f(x)` yields a `Result`, and a call that may fail with neither is `error.unhandled` | [Unhandled failures](11-errors-domains.md#unhandled-failures) |
| `finally`, destructors, `with` | `defer` registers a call for the exit of its block; end of scope runs no user code | [Defer](17-suspension-io.md#defer), [Ownership](07-ownership.md#ownership) |
| Catching a panic, `recover` | A trap abandons the innermost domain unobserved; only `sys::Domain.run` turns it into a value, in the caller's domain | [Traps](11-errors-domains.md#traps) |
| `assert` | `expect P`, checked at run time unless proven; `const assert` is a check-time item | [Expect](18-contracts.md#expect), [Constants](02-items.md#constants) |
| Macros, `#define`, `println!` | No macros exist; `x!(...)` is `syntax.foreign` | [Idiom table](34-diagnostics.md#idiom-table) |
| Wrapping or unbounded integers | `+`, `-` and `*` trap on overflow in every build; `wrapping_` and its peers name the alternatives | [Overflow](09-numbers.md#overflow) |
| Implicit conversions, casts | No conversion is implicit (`type.mismatch`); number types change only through named conversions such as `T.fit(x)` | [Conversions](09-numbers.md#conversions) |
| A literal defaulting to `int` or `number` | A literal takes its suffix's type or its expected type, else `type.literal-untyped` | [Expected types and literals](04-types.md#expected-types-and-literals) |
| `let x: f64 = 1` | An integer literal never takes a float type (`type.mismatch`); write `1.0` | [Expected types and literals](04-types.md#expected-types-and-literals) |
| Signed `/` and `%` | `num.signed-division`; `div_trunc` and `rem_trunc` round as C does, `div_floor` and `mod_floor` as Python does | [Division](09-numbers.md#division) |
| Cases tried top to bottom | `match` arms are disjoint and unordered; only `match first` tests in order and takes guards | [Match first](06-statements-expressions.md#match-first) |
| `import`, `use`, `#include`, `a.b.Item` | No import form exists: each use writes its path `alias::item`; `.` after a module is `name.dot-for-path` | [Paths](03-packages-modules.md#paths) |
| Global mutable state, `print`, `argv` | Capabilities arrive as values, on the host as fields of `host::World`; arguments and environment are `Untrusted` | [Authority](14-authority.md#authority), [Root grants](14-authority.md#root-grants) |
| Classes, inheritance, embedding | Structs, flat `Type.name` methods, interfaces conformed to by impl lines | [Methods](02-items.md#methods), [Coherence](08-generics-interfaces.md#coherence) |
| Structural typing, implicit interface satisfaction | Nominal types; a type implements an interface only through an impl line or a derive | [Coherence](08-generics-interfaces.md#coherence) |
| Overloading, default parameters | One function per name (`item.duplicate`); every argument is written | [Order and names](02-items.md#order-and-names) |
| `f(a, 3)`, `Point(1, 2)`, `{x: 1}` | `f(a, count: 3)` and `Point(x: 1, y: 2)`: `f(a, 3)` is `expr.argument-label` and `Point(1, 2)` is `type.unlabelled-field` | [Arguments and labels](06-statements-expressions.md#arguments-and-labels), [Construction](04-types.md#construction) |
| `<T>` | `[T]`, with bounds `T: Eq and Hash` | [Generic parameters](08-generics-interfaces.md#generic-parameters) |
| `&x`, `&mut x`, `*p`, `p->f` | A read argument is unmarked, `mut x` marks every `mut` argument and `take x` a non-Copy `take` argument; `p.f` reaches a field; the foreign spellings are `syntax.foreign` | [Marking](07-ownership.md#marking) |
| Text length, `s[i]`, `s.len()` | `byte_len`, `char_count`; `s[i]` on text is `text.integer-index` and `s.len()` is `text.bare-len` | [Lengths and slicing](13-text-encode.md#lengths-and-slicing) |
| `async` functions, futures | `uses wait` in the head and `await` before every call that may wait; no value stands for a pending call | [Waiting](10-effects-digests.md#waiting) |
| Closures stored freely, capturing by reference | Unlisted captures are reads; a closure is only passed as an argument, while a named function is a value | [Closures](07-ownership.md#closures) |
| Map iteration order | `mem::Map` iterates in ascending key order, `mem::HashMap` in insertion order | [Maps and sets](12-memory.md#maps-and-sets) |

```nan
pub closed struct Span derives Copy, Eq {
    pub start: u32
    pub length: u32
} Span

pub fn Span.end(self) -> Option[u32] {
    return self.start.checked_add(self.length)
} Span.end

pub fn wider(left: Span, right: Span) -> Span {
    let chosen = if left.length >= right.length {
        left
    } else {
        right
    }
    return chosen
} wider

pub fn unit_span(start: u32) -> Span {
    return Span(start: start, length: 1)
} unit_span
```

## C

| Form | In NaN | Rule |
| --- | --- | --- |
| `char`, `'a'` as small integers | `char` holds a Unicode scalar value; a byte is a `u8`, written `b'a'` | [Scalars](04-types.md#scalars), [Text literals](01-lexical.md#text-literals) |
| NUL-terminated strings | `Str` is a UTF-8 view carrying its length; `b"..."` adds no NUL | [Text types](13-text-encode.md#text-types) |
| `0755` | No octal form exists (`lex.malformed-number`) | [Number literals](01-lexical.md#number-literals) |
| `int`, `long` | Fixed-width scalars; `usize` and `isize` are 64-bit and distinct from `u64` and `i64` | [Scalars](04-types.md#scalars) |
| Signed `>>`, shifts by the width | `num.signed-shift-right`, written `shr_arith`; a shift by the width or more traps | [Shifts](09-numbers.md#shifts) |
| `break` leaving a `switch` | Arms never fall through; `break` in an arm leaves the enclosing loop | [Conditionals and loops](06-statements-expressions.md#conditionals-and-loops) |
| `const int` | A `let` binding; a `const` item is computed at check time | [Constants](02-items.md#constants) |
| Integer enums, `union` | A variant has no integer value; a `layout` with `tags` fixes bits | [Layouts](23-low-level.md#layouts) |
| Raw pointers, pointer arithmetic | `hw::Ptr` is read, written and offset only in trusted code (`trusted.primitive-outside`) | [Trusted primitives](23-low-level.md#trusted-primitives) |
| `volatile` | `registers` items reached through `hw::Mmio` or `hw::Ports` | [Registers](23-low-level.md#registers) |
| `malloc`, `free` | Allocator values and stores; release at end of scope | [Allocators](12-memory.md#allocators) |
| `#if` | Per-target code lives in target variants | [Target variants](03-packages-modules.md#target-variants) |
| `goto` | `loop as label` with `break label` and `continue label` | [Conditionals and loops](06-statements-expressions.md#conditionals-and-loops) |

```nan
pub fn count_until_nul(bytes: Slice[u8]) -> usize
    example count_until_nul(b"ab\x00cd") == 2
{
    var count: usize = 0
    for byte in bytes {
        match byte {
            0 => break
            else => {
                count += 1
            }
        }
    }
    return count
} count_until_nul

pub fn is_newline(byte: u8) -> bool {
    return byte == b'\n'
} is_newline
```

```nan
pub fn is_newline(byte: u8) -> bool {
    return byte == '\n' // error: type.mismatch
} is_newline
```

## Rust

| Form | In NaN | Rule |
| --- | --- | --- |
| `let mut x` | `var x` (`syntax.foreign`); a `let` binding is never assigned or accessed by `mut` (`mode.immutable-binding`) | [Modes](07-ownership.md#modes) |
| `f(x)?` | `try f(x)` (`syntax.foreign`); failures convert only through `from` variants | [Failure conversion](11-errors-domains.md#failure-conversion) |
| `-> Result<T, E>`, `Ok(v)` | `-> T fails E`, `return v`, `fail e`; `attempt` yields the `Result` | [Failures](11-errors-domains.md#failures) |
| `if let Some(x) = e` | `let Some(x) = e else { ... }`, or `match` | [Let else](06-statements-expressions.md#let-else) |
| `Enum::Variant`, `Type::new()` | `Enum.Variant`, `Type.empty()`: `::` before a member is `name.colon-for-member`, and std names a function making a value by its verb | [Paths](03-packages-modules.md#paths), [Controlled vocabulary](28-stdlib.md#controlled-vocabulary) |
| `impl T { ... }`, `impl Trait for T` | Flat items `fn T.f(...)`; the line `impl T: I` | [Impl lines](08-generics-interfaces.md#impl-lines) |
| `Self` in a method | `Self` exists only inside interface items; a method names its type | [Interfaces](08-generics-interfaces.md#interfaces) |
| `impl Add<A> for T` beside `impl Add<B> for T` | A type has one impl line per interface, whatever its arguments (`interface.duplicate-impl`) | [Coherence](08-generics-interfaces.md#coherence) |
| `_ =>` after other arms | Outside `match first`, `_` overlaps them (`expr.overlapping-arms`, whose exact fix writes `else`); the catch-all arm is `else =>` | [Disjointness and exhaustiveness](06-statements-expressions.md#disjointness-and-exhaustiveness) |
| A tail expression as the result | `return e` (`stmt.missing-return`) | [Returns](06-statements-expressions.md#returns) |
| `let x = x + 1` | `stmt.shadowing` | [Bindings](06-statements-expressions.md#bindings) |
| `x as u8` | A named conversion (`syntax.foreign`) | [Conversions](09-numbers.md#conversions) |
| `0..n` | `0..<n` (`syntax.foreign`); `0..=n` keeps its meaning | [Ranges](06-statements-expressions.md#ranges) |
| `\|x\| x + 1`, `move \|\|` | `fn(x: u32) => x + 1`, `fn[take conn]() { ... }` (`syntax.foreign`); `\|` is bitwise or | [Closures](07-ownership.md#closures) |
| `let _ = e`, `#[must_use]` | Every non-`Unit` result is used or `discard`ed (`stmt.unused-result`); `let _ = e` is `stmt.wildcard-binding` | [Expression statements](06-statements-expressions.md#expression-statements) |
| `unsafe` | `trusted "premise" { ... }` in a module with `allow trusted` | [Trusted code](23-low-level.md#trusted-code) |
| `Rc`, `Arc`, `RefCell`, `Mutex` | `mem::Shared`, `task::Cown`; only the listed std types mutate through a read access | [Interior mutability](12-memory.md#interior-mutability) |
| `Vec<T>`, `String`, `&str` | `mem::List[T]`, `text::String`, `Str` | [Collections](12-memory.md#collections), [Text types](13-text-encode.md#text-types) |
| `f(x).await` | `await f(x)` (`syntax.foreign`) | [Awaited calls](10-effects-digests.md#awaited-calls) |
| Trait methods callable once imported | An interface method declared outside `core` and the receiver type's package is called qualified, `geo::Measure.size(x)` (`interface.unqualified-call`) | [Method resolution](08-generics-interfaces.md#method-resolution) |

```nan
pub fn is_vowel(letter: u8) -> bool {
    return match letter {
        b'a' or b'e' or b'i' or b'o' or b'u' => true
        else => false
    }
} is_vowel

pub fn sum_all(values: Slice[u32]) -> u64 {
    var sum: u64 = 0
    for value in values {
        sum += u64.widen(value)
    }
    return sum
} sum_all
```

```nan
pub fn is_vowel(letter: u8) -> bool {
    return match letter {
        b'a' or b'e' or b'i' or b'o' or b'u' => true
        _ => false // error: expr.overlapping-arms
    }
} is_vowel
```

```nan
pub fn sum_all(values: Slice[u32]) -> u64 {
    let mut sum: u64 = 0 // error: syntax.foreign
    for value in values {
        sum += u64.widen(value)
    }
    return sum
} sum_all
```

```nan
pub fn doubled(value: u32) -> u32 {
    value.saturating_mul(2)
} doubled // error: stmt.missing-return
```

## Go

| Form | In NaN | Rule |
| --- | --- | --- |
| `x := e` | `let x = e` or `var x = e` (`syntax.foreign`) | [Bindings](06-statements-expressions.md#bindings) |
| `var x int` holding a zero value | Every binding is initialised at its definition | [Bindings](06-statements-expressions.md#bindings) |
| `(T, error)`, `if err != nil` | `-> T fails E` and `try f(x)` | [Failures](11-errors-domains.md#failures) |
| `defer` | Runs at the exit of its innermost block, evaluates its arguments when it runs, never runs on a trap, and never fails or waits | [Defer](17-suspension-io.md#defer) |
| `go f()` | `s.spawn(c)` inside `task::scope`, which returns only after every spawned task ends | [Task scopes](16-concurrency.md#task-scopes) |
| `make(chan T)` | `task::Channel[T].bounded(capacity, alloc)`; every channel's capacity is above 0 | [Channels](16-concurrency.md#channels) |
| `sync.Mutex` | `task::Cown[T]`, reached only in `await when` bodies, which never wait | [Cowns](16-concurrency.md#cowns) |
| `[T any]` | `[T]` admits droppable types; `[T: Any]` also admits linear ones | [Generic parameters](05-properties.md#generic-parameters) |
| Capitalised names exported | `pub`; case follows the kind of name (`name.case`) | [Naming conventions](03-packages-modules.md#naming-conventions) |
| `for i := 0; i < n; i++` | `for i in 0..<n`; `while` and `loop` are separate forms | [Conditionals and loops](06-statements-expressions.md#conditionals-and-loops) |
| `for i, v := range xs` | `for v in xs` yields elements only | [Iteration](07-ownership.md#iteration) |
| `len(s)`, strings of any bytes | Text is always well-formed UTF-8; `s.byte_len()`, and `s.as_bytes()` for the bytes | [Text types](13-text-encode.md#text-types) |
| `append`, `make`, `new` | Growable containers store their allocator; `push` traps on exhaustion, `try_push` fails | [Growth](12-memory.md#growth) |
| `func (s *T) Reset()` | `fn T.reset(mut self)` | [Methods](02-items.md#methods) |
| `context.Context` deadlines | `task::within`; cancellation is the failure `task::Cancelled` | [Deadlines and races](16-concurrency.md#deadlines-and-races) |

```nan
pub fn send_batches(batches: u32, log: sys::Log) {
    var sent: u32 = 0
    defer log.info("batches sent", value: sent)
    for batch in 0..<batches {
        defer log.info("batch done", value: batch)
        sent += 1
    }
} send_batches
```

```nan
pub fn doubled_count(count: u32) -> u32 {
    total := count * 2 // error: syntax.foreign
    return total
} doubled_count
```

## Python

| Form | In NaN | Rule |
| --- | --- | --- |
| Blocks by indentation | Braces delimit blocks; indentation means nothing beyond column 0 | [Indentation and whitespace](01-lexical.md#indentation-and-whitespace) |
| `#` comments, docstrings | `//`; `///` before the documented item; `#` is `lex.unexpected-character` | [Comments](01-lexical.md#comments) |
| `a // b` | `//` starts a comment; floor division is `/` on unsigned integers and `div_floor` on signed ones | [Division](09-numbers.md#division) |
| `x = 1` creating a name | `let` or `var`, whose type is fixed at the definition | [Bindings](06-statements-expressions.md#bindings) |
| `True`, `False` | `true`, `false` | [Scalars](04-types.md#scalars) |
| `a or b` yielding an operand | `and` and `or` take and yield `bool` | [Logical operators](06-statements-expressions.md#logical-operators) |
| `a < b < c` | `expr.chained-comparison` | [Operators](06-statements-expressions.md#operators) |
| `range(n)`, `x in xs` | `0..<n`; membership is a method such as `contains` | [Ranges](06-statements-expressions.md#ranges) |
| `xs[-1]` | An index is a `usize`, so `-1` is `type.literal-range` | [Indexing](07-ownership.md#indexing) |
| `elif` | `else if` | [Conditionals and loops](06-statements-expressions.md#conditionals-and-loops) |
| `lambda x: e` | `fn(x: u32) => e` | [Closures](07-ownership.md#closures) |
| `f(x, count=3)` | `f(x, count: 3)` | [Arguments and labels](06-statements-expressions.md#arguments-and-labels) |
| `nonlocal` | A capture list, `fn[mut total](...)` | [Captures](07-ownership.md#captures) |
| `f"{x}"` | `text::fmt("{x}", x)` | [Templates](13-text-encode.md#templates) |

```nan
pub fn halved(total: u32) -> u32
    example halved(10) == 5
{
    return total / 2
} halved

pub fn floor_half(total: u32) -> u32
    example floor_half(10) == 10
{
    return total // 2
} floor_half
```

```nan
pub fn last_reading(readings: Slice[u32]) -> u32 {
    return readings[-1] // error: type.literal-range
} last_reading
```

## TypeScript

| Form | In NaN | Rule |
| --- | --- | --- |
| `let` reassignable, `const` binding | `var` and `let`; `const` declares a check-time item | [Bindings](06-statements-expressions.md#bindings), [Constants](02-items.md#constants) |
| `===`, coercing `==` | `==` (`syntax.foreign` for `===`) compares two operands of one type through `Eq`, or by IEEE 754 for floats | [Equality, ordering, hashing and defaults](08-generics-interfaces.md#equality-ordering-hashing-and-defaults), [Float comparison](08-generics-interfaces.md#float-comparison) |
| `x?.y`, `x ?? d`, `x!` | `match` or `let ... else` on an `Option`; `?` and `!` are `syntax.foreign` | [Let else](06-statements-expressions.md#let-else) |
| `number` | Integer and float scalars; unsigned `/` rounds toward zero | [Division](09-numbers.md#division) |
| Type aliases, unions `A \| B`, string enums | An enum, whose variants have no integer or text value; no alias form exists; `\|` is bitwise or | [Enums](04-types.md#enums) |
| `(x) => e` | `fn(x: u32) => e` | [Closures](07-ownership.md#closures) |
| `(x) => { return e }` | `fn(x: u32) => e` or `fn(x: u32) -> u32 { return e }`; `=> return e` is `expr.misplaced-jump` | [Returns](06-statements-expressions.md#returns) |
| `for (k in o)` | `for x in xs` yields elements, like `for...of` | [Iteration](07-ownership.md#iteration) |
| `s.length` | `byte_len` or `char_count`, never UTF-16 units | [Lengths and slicing](13-text-encode.md#lengths-and-slicing) |
| `` `${x}` `` | `text::fmt("{x}", x)`; `` ` `` is `lex.unexpected-character` | [Templates](13-text-encode.md#templates) |
| Unawaited calls, `Promise.all` | A call that may wait is awaited (`effect.await-missing`); concurrent work runs in a task scope | [Awaited calls](10-effects-digests.md#awaited-calls) |
| `any` | No type escapes checking; `Dyn[I]` holds a value of any type implementing `I` | [Dynamic dispatch](08-generics-interfaces.md#dynamic-dispatch) |

```nan
pub fn total_length(parts: Slice[u32]) -> u64 {
    var total: u64 = 0
    for part in parts {
        total += u64.widen(part)
    }
    return total
} total_length
```

```nan
pub fn total_length(parts: Slice[u32]) -> u64 {
    let total: u64 = 0
    for part in parts {
        total += u64.widen(part) // error: mode.immutable-binding
    }
    return total
} total_length
```
