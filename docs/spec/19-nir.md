# NIR

This chapter defines NIR with its units, functions, types, places, instructions, obligations and spans; elaboration; the symbolic fields of foreign open types; the text notation; and the canonical encoding.

## NIR

**NIR** is the typed static single assignment form of checked source. It keeps modes, the linearity and other [structural properties](05-properties.md#structural-properties) of types, predicates and contracts as functions, [digests](10-effects-digests.md#digests) and spans. The [prover](20-prover.md#procedure), the [interpreter](21-interpreter.md#interpreter), [lowering](30-artefacts.md#lowering) and [artefacts](30-artefacts.md#artefacts) read NIR, never source, and executing NIR behaves exactly as its source does.

NIR states no layout and keeps generic parameters abstract; lowering fixes both per final closure.

### Units

A **unit** is the NIR of one item other than a `package` or `retired` item, or of one [target variant](03-packages-modules.md#target-variants) of it. It holds, in order:

1. the item's [canonical path](29-identity-versioning.md#canonical-paths);
2. its [item kind](02-items.md#item-kinds);
3. the targets it is checked for;
4. its visibility;
5. its head: modifiers, generic parameters with their bounds and, for a function, its parameters with modes and NIR types, result, failure type, `uses wait`, limits, convention, and whether consumers unfold its [transparent](02-items.md#transparent-functions) body;
6. its declaration: fields, variants and payloads with visibility and types; a layout's bits and bytes; registers; interface method heads; an impl line's type and interface; a registry's entry type, key and `unique` fields; a contribution's registry and entry name; a const's type and value; a test's target;
7. the structural properties of a declared type and the Dyn-usability of an interface;
8. its functions, by role in the role table's order, then by index;
9. per target, the digest of each `body`, `closure` and `default` function;
10. the premise of each trusted block or expression, in source order;
11. the [status](18-contracts.md#statuses) of each obligation site, in site order;
12. its spans.

A unit names other items only by canonical path. A derive elaborates as the items `nan expand` prints, each a unit. Its generic parameters are `Self` in an interface, those of the type or interface the item is a member of, then its own.

### Functions

A unit holds **NIR functions**, each identified by a role and an index from 0:

| Role | Function | Parameters | Index order |
| --- | --- | --- | --- |
| `body` | the item's body, initialiser, assert condition or contribution value | the item's | none |
| `entry` | the [checking entry](18-contracts.md#checking-entries) of a function with requires | the item's | none |
| `closure` | a closure | its captures, then its own | source order |
| `requires` | a requires clause | the item's | clause order |
| `ensures` | an ensures clause | the item's but `take` ones, `result`, then its `old` values | clause order |
| `old` | an `old(e)` term, returning e | the item's | source order |
| `example` | an example clause | none | clause order |
| `decreases` | the decreases measure | the item's | none |
| `invariant` | a loop invariant line | the bindings it names, by first mention | source order |
| `assume` | the condition of an `assume` statement | the bindings it names, by first mention | source order |
| `predicate` | a field `where` or struct `invariant` line | the struct value | declaration order |
| `where` | a const parameter's condition | the const parameters | parameter order |
| `default` | an interface method's default body | the method's | declaration order |

- An interface's unit numbers each role across its methods in declaration order.
- `requires`, `ensures`, `example`, `invariant`, `assume`, `predicate` and `where` functions return `bool`. Parameters of roles other than `body`, `entry`, `closure` and `default` are read parameters.
- An `entry` function makes the requires checks a direct call makes, then calls `body`, returning its result or failure.
- A closure's captures are its capture list, then its unlisted read captures in order of first use.
- A function has parameters, each with a mode and a NIR type, and blocks, the first being its entry. A **block** has typed parameters, instructions it runs in order, and one terminator it runs last.
- A **value** is a parameter of a function or block, or an instruction's result. Each is defined once, and its definition dominates every use.

### Types and references

NIR types are the types of source plus `ref T` and `ref mut T`, designating a place of type T, read-only or writable. Here a type parameter is never a view, and is Copy exactly when bounded by `Copy`.

- A `take` parameter has type T, a `mut` one `ref mut T`, and a read one T when T is Copy or a view and `ref T` otherwise.
- A [projection](07-ownership.md#projections) declared `-> R` returns R when R is a view, `Option` or `Result` with this rule applied to each payload when R is one, and otherwise `ref mut R` when its result is [writable](07-ownership.md#projection-results) and `ref R` when not.

### Places

A **NIR place** is a value, its root, and steps naming a field (`.seq`), the unlabelled payload of a variant (`.Some`) or a labelled payload field (`.Data.len`); [naming conventions](03-packages-modules.md#naming-conventions) tell fields from variants. A step reaching a reference continues at the place it designates, so a place's type is never a reference type. A root is a slot, a reference or an owned value.

### Instructions

The instructions, with their results and behaviour, are:

- `slot N: T`: a new uninitialised place of type T, for the binding N if any.
- `load P`: a copy of the Copy value at P.
- `move P`: the value at P, which becomes moved.
- `ref P`, `ref mut P`: a reference to P.
- `init P, o`: stores o into P, which holds no value; into a ranged `bit` or `rest` field of a layout value, obligation `conversion`.
- `assign P, o`: releases what P still holds, then stores o; into a ranged `bit` or `rest` field of a layout value, obligation `conversion`.
- `release P`: releases what P still holds ([end of scope](07-ownership.md#ownership)).
- `fnref F[A]`: F instantiated with A as a value, called through F's `entry` function if any.
- `constant C`: the value of the const item, contribution or const parameter C.
- `construct T(f: o, ...)`: a struct value, fields in declaration order; for a layout, obligation `conversion` on each ranged `bit` field and each `rest` field ([layout values](23-low-level.md#layout-values)).
- `variant E.V`, `variant E.V(...)`: an enum value.
- `is P, E.V`: whether the enum value at P is variant V.
- `closure K(o, ...)`: a value of the unit's `closure K` holding the capture operands.
- `convert o, F`: o converted to F by [failure conversion](11-errors-domains.md#failure-conversion); `F.V` names the from variant chosen.
- `neg`, `add`, `sub`, `mul`: [overflow](09-numbers.md#overflow) arithmetic on integers, IEEE on floats; obligation `overflow` on integers.
- `div`, `rem`: [division](09-numbers.md#division); obligation `division-by-zero` on integers.
- `shl`, `shr`: [shifts](09-numbers.md#shifts); obligations `shift`, and `overflow` for signed `shl`.
- `band`, `bor`, `bxor`, `inv`: bitwise and, or, exclusive or, inversion.
- `eq`, `ne`, `lt`, `le`, `gt`, `ge`: comparison of two scalars of one type, IEEE on floats; `eq` and `ne` also compare two `Str` or two `Slice[u8]` by length and bytes.
- `not`: boolean negation.
- `call F[A](o, ...)`: a direct call, `call await` when marked `await`, with the obligation a built-in callee's rule states. F is a function item, a NIR function, or an interface method called through a bound, whose type parameter is then the first of A.
- `call v(o, ...)`: an [indirect call](10-effects-digests.md#indirect-calls) through the function value v.
- `call dyn P I.m(o, ...)`: method m of interface I on the `Dyn[I]` at P.
- `check K, o`: an obligation of kind K that o is `true`.
- `assume o`: nothing; o is a [fact](18-contracts.md#assume).
- `all`, `any`: `all lo ..< hi R` runs region R for each integer of the range upward until R yields `false`, and is `true` when none does; `any` stops at `true`; `..=` includes hi.
- `when`: `when (o, ...) R` acquires the cowns whose handles its `read` and `mut` operands name, trapping as `await when` does ([cowns](16-concurrency.md#cowns)), then yields R's value, R receiving a `ref` or `ref mut` to each cown's value; `when attempt` yields `Err(task::Poisoned)` instead of `trap.poisoned`, and `Ok` of R's value.
- `dbg o`: o, once a test run has written the line of its `dbg` ([debug output](22-testing.md#debug-output)).

`init`, `assign`, `release`, `check` and `assume` have no result, nor does a call or `when` of type `Unit`. A call of a callee whose head states `fails` is always a terminator.

An **operand** is a value, a literal, or a place with a mode: `read P`, `mut P` or `take P`, which moves P when the call starts. A Copy value, and a view passed by read, is a value operand.

A **region** is a list of blocks run from the first, whose parameters receive the integer or the references, until a `yield` ends it. A `when` region also ends at a `jump` out of it, made by an exit leaving the body; only `yield` continues after the instruction. Either end releases the cowns.

An instruction elaborated from trusted code carries its premise's index, written `trusted K` before the operation.

### Terminators

The terminators are:

- `jump ^B(o, ...)`: continues at B with the operands as its parameters.
- `branch o, ^T, ^F`: continues at T when o is `true`, else at F.
- `call ... ok ^B fail ^C`: a call that may fail, continuing at B with the result or at C with the failure as the block's one parameter; it is their only predecessor.
- `return o`: returns o, or `Unit` when o is absent.
- `fail o`: fails with o, of the function's failure type.
- `todo "intent"`: traps (`trap.todo`).
- `unreachable`: follows a call returning `Never`; no execution reaches it.
- `yield o`: ends a region with o.

### Obligations

An **obligation site** is a `check`, an instruction whose entry names an obligation, or a call of a built-in operation whose rule states one. Sites are ordered by function, block and position. The kinds of `check` are `requires`, `ensures`, `predicate`, `expect`, `invariant` and `decreases`.

- The [prover](20-prover.md#queries) decides each site, and the unit records its status. Lowering emits a [run-time check](18-contracts.md#run-time-checks) for exactly the sites with status checked.
- A kept check of kind K traps with `trap.K`, and of kind `invariant` with `trap.expect`; a `decreases` check is never kept ([decreases](18-contracts.md#decreases)). A kept partial instruction or built-in call traps with the kind its rule states.
- The sites in functions of roles other than `body`, `entry`, `closure`, `example` and `default` are the well-definedness obligations of [contract terms](18-contracts.md#contract-terms), never run-time checks.

### Well-formedness

Elaboration produces only well-formed NIR:

- Every operand has the type its position requires, and each place operand of a call the mode of its parameter.
- A value whose type is not Copy, a view, a reference type, or an `Option` or `Result` of these, is consumed exactly once on every path, after its other uses, by `init`, `assign`, `release`, `convert`, `return`, `fail`, `yield`, or as a `take`, construction, variant or block operand.
- On every path to a function exit, each slot holding a value neither Copy nor a view is moved or released; a path through a `branch` on a guard slot ([scope exits](#scope-exits)) takes the target that the value last stored there selects.
- `move`, `init` and `release` apply only to places rooted at a slot or an owned value; `assign` and `mut` operands also to places a `ref mut` designates.
- No `release`, and no `assign` to a place holding a value, applies to a place whose type may be [linear](05-properties.md#linear-types).

Elaboration producing NIR that is not well formed is an internal error ([diagnostic classes](34-diagnostics.md#classes)).

### Spans

A **span** is the source range a part of a unit comes from: its file path prefixed by its package's name, its first line, the byte offset in that line of its first character, its last line and the byte offset in that line after its last character. Every function, function parameter, instruction and terminator has one; those of function parameters and slots carry their names.

- A function, and its entry code, has its item's, clause's, line's or closure's span; other code that of the construct it elaborates, and exit code that of the statement making the exit or of the last line of the block it ends.
- A copied deferred call has its `defer`'s span, the code of a forwarded use, the functions of its rewrite's closures included, the use's, derived code its `derives` entry's, and a requires, ensures or invariant check, with the call computing its condition, its clause's or line's, or the call's for a std callee.
- Trap reports and diagnostics use a span's first line.

## Elaboration

**Elaboration** is the translation of an accepted item's checked source into its unit, except parts 9 and 11, which the checker adds. It depends only on the item's [closure set](00-charter.md#guarantees) and targets and must produce exactly the NIR this section defines, so every implementation gives an item one unit.

### Order and numbering

- Instructions follow the [evaluation order](06-statements-expressions.md#evaluation-order).
- Unreachable blocks are dropped. Blocks are numbered in depth-first preorder from the entry, successors in their terminator's order, a region's blocks when the walk reaches its instruction. Values are numbered along the walk: function parameters, then each block's parameters and results.
- Blocks have parameters only at the join of a value-yielding `if` or `match`, `and` or `or`, at an arm block taking bindings from several alternatives, as targets of a failing call, and first in a region.

### Bindings

- A `let` binding of a Copy type, a part or a wrapped part ([projection results](07-ownership.md#projection-results)), a read or `mut` parameter, and a name a pattern binds by `load` or reference are values.
- Every other binding, and each `take` parameter, is a slot, created before its initialiser's code and stored by `init`; entry stores each `take` parameter into its slot.
- A non-Copy temporary used without being consumed lives in a slot of the scope its statement runs in, released at its statement's end, temporaries in reverse order of creation; one created in a branch is released at the branch's end, and those of a `while` condition, a guard or the right operand of `and` or `or` once its value is computed.
- A place used as a value becomes `load` when Copy and `move` otherwise. An assignment is `init` where its place holds no value and `assign` where it holds one, under a `branch` on the guard deciding which.

### Scope exits

At an exit, after the returned value or raised failure, elaboration emits the [deferred calls](17-suspension-io.md#registration-and-exits) pending on that path, innermost scope first and in reverse order of registration, then a `release` of each slot of the scopes left holding a value neither Copy nor a view, innermost scope first and in reverse order of definition. An exit leaving a `when` body does so for the scopes inside the body before leaving the region.

- A deferred call pending on only some incoming edges of a join is **guarded**: a `bool` slot, initialised to `true` where the call is registered and set to `false` by each `undefer` cancelling it, decides at each later exit whether the call runs.
- A slot holding a value on only some incoming edges of a join is released on those edges, except where a guarded call naming it is pending; after the join it holds a value exactly where that guard holds `true`.

A `return`, the end of a `Unit` body and a `fail` then emit:

1. the [predicate checks](#predicate-checks) that predicate obligations place at the exit;
2. for a return, a call of the `ensures` function of each ensures clause the function has ([interface contracts](18-contracts.md#interface-contracts)), with the parameters, the result and its old values, and `check ensures`;
3. the release of `take` parameter slots, in reverse order;
4. the terminator.

The entry block starts with the `take` parameter slots, then a call of each `old` function of those ensures clauses, in index order, then of the `decreases` function, if any.

### Predicate checks

At each point where [predicate obligations](04-types.md#obligations) place one, elaboration emits, for each value covered, a call of the `predicate` function of each predicate of its type, in declaration order, and `check predicate`. Values are covered depth-first: a value before its fields in declaration order, a payload behind a `branch` on its variant's `is`, and an `Array`'s elements in index order, in a loop. A hold's end is each point, on each path, after the last use of the part and of the values derived from it ([holds](07-ownership.md#holds)), before the next instruction; the check at the end of an `await when` body is placed on each exit of its region.

### Constructs

| Source | NIR |
| --- | --- |
| `P = e`, `P op= e` | P's operands, e, the `at_mut` call for an indexed P, for `op=` the operator on P and e's value, then `init` or `assign` |
| operator | its instruction on scalars, else the calls of its [operator interface](08-generics-interfaces.md#operator-interfaces) meaning, testing `core::Ordering` values by `is` |
| `a and b`, `a or b` | `branch` on a: for `and` to a block computing b, else to the join with `false`; for `or` to the join with `true`, else to that block, which jumps to the join with b |
| `if`, `match` | a `branch` per test, a block per branch, a join where execution continues; a false `if` without `else` branches to the join |
| `while c`, `loop` | a header, for `while` computing c and branching to body or exit; the body jumps back |
| `for p in e` | a slot for the cursor [iteration](07-ownership.md#iteration) obtains; a header calling `next` by `mut` and branching on `is Some`; a body binding p from the payload, jumping back |
| loop invariants | at the body's start, after a `for` binds its element, a call of each `invariant` function and `check invariant` |
| `break`, `continue` | the exits of the scopes left, then `jump` to the loop's exit or header |
| a call | callee and arguments; the call's predicate checks; for `at` or `at_mut` of an `Array`, slice, list, bytes or bounded list, a `bounds` site whose condition is the index being below the indexed value's `len()`; per requires clause that a [direct](18-contracts.md#clauses) callee has ([interface contracts](18-contracts.md#interface-contracts)), or its interface method for a call through a bound or `Dyn`, a call of its function with the arguments by read and `check requires`; within a cycle, the callee's `decreases` call, `lt` against the entry measure, each measure whose type is not `u64` widened by `u64.widen` when their types differ, and `check decreases`; the `call` |
| `try` | a failing `call` whose `fail` target converts when the types differ and exits as `fail` does |
| `attempt` | a failing `call` whose targets build `Ok` and `Err` and join |
| `fail e` | e, `convert` when its type is not the failure type, the exit |
| `x[i]` | the `at` or `at_mut` call with its `bounds` site |
| construction | the values in written order, `construct`, the predicate checks |
| closure | `closure` with one operand per capture, by its mode |
| `defer c`, `undefer P` | nothing in place; c is emitted at each later exit until `undefer` |
| `discard e` | e, then `release` unless Copy or a view |
| `expect P` | P's code, then `check expect` |
| `assume P because "why"` | a call of its `assume` function, then `assume` of the result |
| `todo "intent"` | the `todo` terminator |
| `dbg(e)` | e as a `take` operand, then `dbg` |
| `await when`, quantifiers | `when`, `all`, `any` |

- A Copy scrutinee that is not a place, and a part or wrapped part, is the root of its parts. Arms are tested in source order, an `else` arm last, the final arm without a test. A pattern is tested depth-first, left to right: a variant by `is` then its payload patterns, a literal or constant by `eq`, a range by `ge` then `le`, or `lt` for `..<`, `or` alternatives in order; bindings and `_` test nothing. A const in a pattern is compared through its `constant`.
- After its tests, a pattern binds its names in written order: a Copy part by `load`, a non-Copy part of an owned scrutinee by `move` into a slot, any other part by `ref`, or `ref mut` in the `mut` form of `for`; the `take` form of `for` binds into slots. An arm with several alternatives creates its slots before the match's first test, and each alternative passes its other bindings to the arm's block as parameters, in the first alternative's order.
- A `match first` guard follows its pattern's tests, with each name the pattern moves bound by `ref`; the moves follow a `true` guard. When an arm's alternatives bind names, each alternative tests the guard before passing its bindings.
- `let P = e else { B }` elaborates as a match with one arm and B.
- A use of a `preserves` tombstone elaborates as its rewrite ([tombstones](29-identity-versioning.md#tombstones)).

```nan
pub fn clamp_percent(value: u32) -> u8
    ensures result <= 100
{
    if value > 100 {
        return 100
    }
    return u8.fit(value)
} clamp_percent
```

`example::clamp_percent` elaborates to this unit; the call of `u8.fit` is a `conversion` site:

```text
unit example::clamp_percent

fn body (read %0: u32) -> u8
^0:
    %1 = gt %0, 100u32
    branch %1, ^1, ^2
^1:
    %2 = call example::clamp_percent ensures 0(%0, 100u8)
    check ensures, %2
    return 100u8
^2:
    %3 = call u8.fit(%0)
    %4 = call example::clamp_percent ensures 0(%0, %3)
    check ensures, %4
    return %3

fn ensures 0 (read %0: u32, read %1: u8) -> bool
^0:
    %2 = le %1, 100u8
    return %2
```

```nan
pub fn total(values: Slice[u32]) -> u64 {
    var sum: u64 = 0
    for value in values {
        sum += u64.widen(value)
    }
    return sum
} total
```

`example::total` elaborates to this unit:

```text
unit example::total

fn body (read %0: Slice[u32]) -> u64
^0:
    %1 = slot sum: u64
    init %1, 0u64
    %2 = slot core::SliceCursor[u32]
    %3 = call Slice.cursor[u32](%0)
    init %2, %3
    jump ^1
^1:
    %4 = call core::SliceCursor.next[u32](mut %2)
    %5 = is %4, Option.Some
    branch %5, ^2, ^3
^2:
    %6 = load %4.Some
    %7 = call u64.widen(%6)
    %8 = load %1
    %9 = add %8, %7
    assign %1, %9
    jump ^1
^3:
    %10 = load %1
    return %10
```

## Symbolic fields

A **foreign open type** of a unit is an [open type](04-types.md#closed-and-open-types) of another package. A unit depends on one only through its path, type arguments, `pub` fields, variants, [promises](05-properties.md#promises) and functions:

- Its fields are **symbolic**: steps name only `pub` fields, and its field set, size and alignment appear in no other package's unit.
- No other package's unit holds a `construct` of it. `variant` and `is` name an open enum's variants, and its matches end in the `else` arm's block, which receives every variant no arm names, including variants added later.
- Well-formedness reads its structural properties from its promises only.

Lowering resolves symbolic fields with the layouts of the final closure, so a release adding a field or variant to an open type leaves its consumers' units unchanged.

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

pub fn Total.empty() -> Total = Total(sum: 0, samples: 0)

// file: report/package.nan
//! Reports meter totals.

package report {
    language 1
    targets any
    dep meter "../meter"
} report

// file: report/report.nan
//! Reads a fresh total.

pub fn fresh_sum() -> u64 {
    let total = meter::Total.empty()
    return total.sum
} fresh_sum
```

`report::fresh_sum` elaborates to this unit, whose `.sum` step is symbolic:

```text
unit report::fresh_sum

fn body () -> u64
^0:
    %0 = slot total: meter::Total
    %1 = call meter::Total.empty()
    init %0, %1
    %2 = load %0.sum
    release %0
    return %2
```

## Text notation

The **NIR text notation** writes units in this specification. It is lexed as source is ([Lexical structure](01-lexical.md)) and follows this grammar, written in the [grammar notation](00-charter.md#grammar-notation), whose single-quoted words match an `IDENT` by spelling; `path`, `type_expr`, `generic_args` and `limits_clause` are the source rules of those names.

```text
nir_unit = COL0 'unit' path NL nir_function+ ;
nir_function = COL0 "fn" role 'task'? nir_params ( "->" nir_type )? ( "fails" nir_type )? ( "uses" 'wait' )? limits_clause? NL nir_block+ ;
role = 'body' | 'entry' | "decreases" | role_word INT ;
role_word = 'closure' | "requires" | "ensures" | 'old' | 'example' | "invariant" | "assume" | 'predicate' | "where" | 'default' ;
nir_params = "(" ( nir_param ( "," nir_param )* )? ")" ;
nir_param = ( 'read' | "mut" | "take" ) value ":" nir_type ;
nir_type = ( 'ref' "mut"? )? type_expr ;
nir_block = COL0? label block_params? ":" ( instruction NL )* terminator NL ;
block_params = "(" value ":" nir_type ( "," value ":" nir_type )* ")" ;
instruction = ( value "=" )? ( "trusted" INT )? operation ;
operation = 'slot' ( IDENT ":" )? nir_type
    | ( 'load' | 'move' | 'ref' | 'ref' "mut" | 'release' ) place
    | ( 'init' | 'assign' ) place "," operand
    | 'fnref' fn_ref
    | 'constant' path
    | 'construct' nir_type "(" labelled ( "," labelled )* ")"
    | 'variant' nir_type "." IDENT ( "(" ( operand | labelled ( "," labelled )* ) ")" )?
    | 'is' place "," nir_type "." IDENT
    | 'closure' INT "(" operands? ")"
    | 'convert' operand "," nir_type ( "." IDENT )?
    | ( 'neg' | 'inv' | "not" ) operand
    | binary_word operand "," operand
    | call
    | 'check' check_kind "," operand
    | "assume" operand
    | ( 'all' | 'any' ) operand ( "..<" | "..=" ) operand region
    | "when" "attempt"? "(" operands ")" region
    | "dbg" operand ;
binary_word = 'add' | 'sub' | 'mul' | 'div' | 'rem' | 'shl' | 'shr' | 'band' | 'bor' | 'bxor' | 'eq' | 'ne' | 'lt' | 'le' | 'gt' | 'ge' ;
check_kind = "requires" | "ensures" | 'predicate' | "expect" | "invariant" | "decreases" ;
call = 'call' "await"? callee "(" operands? ")" ;
callee = fn_ref | value | 'dyn' place path "." IDENT ;
fn_ref = path ( "." IDENT )? role? generic_args? ;
terminator = 'jump' target
    | 'branch' operand "," target "," target
    | call 'ok' target "fail" target
    | "return" operand?
    | "fail" operand
    | "todo" TEXT
    | 'unreachable'
    | 'yield' operand ;
target = label ( "(" operands ")" )? ;
region = "{" nir_block+ "}" ;
operands = operand ( "," operand )* ;
operand = value | literal | ( 'read' | "mut" | "take" ) place ;
labelled = IDENT ":" operand ;
place = value ( "." IDENT )* ;
value = "%" INT ;
label = "^" INT ;
literal = INT | FLOAT | TEXT | BYTES | CHAR | BYTE | "true" | "false" | 'Unit' ;
```

- `task` and a `limits_clause`, written as the source writes them ([limits](10-effects-digests.md#limits)), appear only on a `closure` function: `task` for a task closure.
- `unit` and `fn` lines and block labels start at column 0, instructions and terminators are indented 4 spaces, and a region's lines 4 more than its instruction. A blank line precedes each `fn` line. A block label's final `:` continues its logical line onto the first instruction.
- Paths are written in alias form, number literals with their literal suffix, and a call's type arguments in full. Result types are not written, and a slot shows its binding's name from its span.

## Encoding

The **canonical NIR encoding** gives each unit one byte sequence. A **NIR stream** is the bytes `4E 49 52 00`, the NIR version as a uvar, then a list of units sorted by canonical path and then by first target name. The **NIR version** is an integer; language 1 uses version 1, which this chapter defines.

Its forms are:

- A uvar is an unsigned integer in LEB128 without redundant trailing groups. An svar writes n as the uvar 2n when n is at least 0 and −2n − 1 otherwise.
- Bytes are a uvar length and the bytes; a text is the bytes of its UTF-8; a list is a uvar count and its elements; an optional part is a uvar 0, or 1 and the part.
- A record is a list of fields, each a uvar key and the bytes of its content, keys strictly increasing; an absent field is omitted. A pair is its first part followed by its second.
- A choice among alternatives this specification enumerates, such as an item kind, mode or status, is a uvar, its position from 0.

Its parts are:

- A unit is a record whose keys 1 to 12 are the parts of [Units](#units) in order, its targets sorted by name. Parts 5 to 7 are records keyed from 1 by their elements in the order Units lists them, elements of one kind forming a list in declaration order and structural properties being booleans in their [table](05-properties.md#structural-properties)'s order. Each element is a record of its parts in the order its source grammar writes them: names as texts, types and literals as here, and words by their position among their rule's alternatives. A limit is a record of 1 its position among the alternatives of `limit` and 2 the N of `stack N`, or a class encoded as a digest's key 11.
- A function is a record whose keys 1 to 7 are its role's position in the role table, its index (0 when its role has none), its parameters as pairs of a mode and a type, its result type, failure type, wait flag and blocks. A `closure` function also has key 8: a record of 1 its kind, a uvar 0 for a closure and 1 for a [task closure](16-concurrency.md#task-closures), and 2 its limits, encoded as the limits of part 5. A block is its parameter types, its instructions and its terminator.
- An instruction is its opcode, a uvar 0, or 1 plus its premise index when trusted, then its parts in notation order after the operation word, without slot names or labels, labelled operands in declaration order; a word the notation may omit or vary (`await`, `mut` after `ref`, `attempt`, `..=` for `..<`) is a uvar 0 or 1. Opcodes number the names of [Instructions](#instructions) from 0 in list order, each distinct name of an entry in turn and each `call` entry once, then the entries of [Terminators](#terminators), whose failing call holds its call as an instruction, then the query forms of the [proof cache](20-prover.md#proof-cache), in this order: `ite` (condition and two values), quotient and remainder (dividend and divisor), tag (its term), then fresh, skolem and bound symbols (their numbers).
- An operand is a uvar 0 and a value's number, 1 and a literal, or 2, a mode and a place, which is its root's number and its steps as texts. `constant` holds a uvar 0 and a canonical path, or 1 and a const parameter's position. A function reference is a canonical path, a role and index (`body` and 0 for a function item, built-in or interface method), and type arguments.
- A type is a uvar constructor and its content: 0 a [scalar](04-types.md#scalars), its position in the scalar table; 1 `Unit`; 2 `Never`; 3 a named type, its canonical path and arguments; 4 a generic parameter, its position in the unit's; 5 a const argument, its literal; 6 a function type, a uvar 0, 1 or 2 for plain, `interrupt fn` or `task fn`, then its head as a function's part 5; 7 `ref` and 8 `ref mut`, with their type.
- A literal is its type and its value: an integer as an svar, or a uvar for unsigned types; a float as its bits in 4 or 8 bytes, least significant first; a `bool` or `char` as a uvar; text and bytes as bytes; `Unit` as nothing. A const's value follows its type: a scalar or `Unit` as a literal's value, a struct as its fields in declaration order, an enum value as its variant's position and payload, an `Array` or `Slice` as the list of its elements, a `Str` as bytes, and a function value as its function reference.
- Part 9 holds, for each function it covers in order, a list of pairs of a target name and a digest, sorted by name. A digest is a record keyed from 1 by the rows of the [digest](10-effects-digests.md#digests) table; booleans are uvars, and a set is a uvar 0 and its sorted paths, or 1 for the unbounded set. Key 11, the class, is omitted when it is `traceless`, and is otherwise its position from 0 in the [class table](10-effects-digests.md#reversibility), followed for an irreversible class by the list of its kinds' positions in kind order.
- The spans of a unit are, for each function in order, its span, the spans of its parameters, then those of its instructions and terminators in block order, each a text and four uvars followed, for a parameter or slot, by its name as a text.

The literal `300u16` encodes as these bytes:

```text
00 01 AC 02
```

An implementation must write exactly the canonical encoding and decode each version it implements under that version's rules. A stream that does not decode, holds a record key this chapter does not define, or is not the canonical encoding of what it decodes to is an error (`nir.malformed`). A stream of a version the toolchain does not implement is an error (`nir.version-unsupported`). Both are reported on the `dep` directive through which the checked package reaches the package whose artefact holds the stream.

## Diagnostics

| Code | Class | Rule |
| --- | --- | --- |
| `nir.malformed` | error | [Encoding](#encoding) |
| `nir.version-unsupported` | error | [Encoding](#encoding) |
