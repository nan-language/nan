# Contracts

This chapter defines the clauses of a contract (requires, ensures with `result` and `old`, examples, decreases and loop invariants), contract terms with their quantifiers, well-definedness and opacity, vacuous contracts, `expect` and `assume`, obligations and facts, statuses, run-time checks and their elimination, checking entries, the contracts of interface methods and their implementing functions, and the `todo` forms with holes, missing specifications and disputed clauses.

## Clauses

A **contract** is the requires, ensures, example and decreases clauses of a function, together with the [predicates](04-types.md#predicates) of types and the loop invariants of its body. Clauses sit in the [clause region](02-items.md#clause-region) of a function with a body block or of an [interface method](08-generics-interfaces.md#interfaces).

```ebnf
requires_clause = "requires" expr NL ;
ensures_clause = "ensures" expr NL ;
result_expr = 'result' ;
old_expr = 'old' "(" expr ")" ;
example_clause = 'example' expr NL ;
decreases_clause = "decreases" expr NL ;
```

- A function has any number of requires, ensures and example clauses and at most one decreases clause. Clauses of one kind are conjoined, and each requires and ensures clause gives [obligations](#obligations) of its own, each with its own [status](#statuses).
- The expressions of requires and ensures clauses are [contract terms](#contract-terms) of type `bool`, and that of a decreases clause a contract term of an unsigned integer scalar type (`type.mismatch`). Besides the items visible in its module, a requires, ensures or decreases clause names the function's parameters, `self` included, and its generic parameters; it names no local binding.
- A **direct call** is a call of a function item named by its path, or found by [method resolution](08-generics-interfaces.md#method-resolution) or a qualified interface call on a type other than a generic parameter, `Self`, `Dyn[I]` or `mem::Box[Dyn[I]]`. A **call through a bound** calls an interface method on a generic parameter or `Self`, and a **call through `Dyn`** is a call that [dynamic dispatch](08-generics-interfaces.md#dynamic-dispatch) dispatches: a method call on a `Dyn[I]` or `mem::Box[Dyn[I]]` value, or a qualified call whose `Self` is one of them. Every other call goes through a function value or a registry field.

### Requires

`requires P` states a condition that the arguments of every call meet.

- At each direct call, P with each parameter replaced by its argument, the receiver replacing `self`, is an obligation at the call, after the arguments are evaluated and before the body runs.
- A call through a bound or `Dyn` meets the interface method's requires instead ([Interface contracts](#interface-contracts)). A function reached in any other way meets P in its [checking entry](#checking-entries).
- At entry to the body, P is a [fact](#obligations). For a `mut` parameter, P describes the value at entry.

```nan
pub fn midpoint(low: u32, high: u32) -> u32
    requires low <= high
    ensures low <= result and result <= high
{
    return low + (high - low) / 2
} midpoint

pub fn middle_of_ten() -> u32 {
    return midpoint(0, high: 10)
} middle_of_ten
```

### Ensures

`ensures Q` states what holds when the function returns.

- Within an ensures clause, `result` is the returned value and `old(e)` is the value of `e` at entry, after the requires are met, whatever bindings or items share these names. A `path_expr` spelt `result` is a `result_expr` there, and a call with one unlabelled, unmarked argument of a `path_expr` spelt `old` is an `old_expr`; elsewhere `result` and `old` are ordinary identifiers.
- The `e` of `old(e)` has a Copy type and contains no `result` and no other `old` (`contract.old-form`).
- A parameter in Q denotes its value at return: the final value of a `mut` parameter and the unchanged value of a read parameter. A `take` parameter appears only inside `old(e)` (`contract.taken-parameter`).
- Q is an obligation at each return of the function's body outside its closures: each `return` statement, and the end of the body of a function whose result is `Unit`. It is met after the function's [deferred calls](17-suspension-io.md#defer) run.
- A call that fails meets no ensures; on failure the callee guarantees only the predicates of the values it received by `mut` or `take` ([predicate obligations](04-types.md#obligations)).
- After a direct call returns, Q is a fact with each parameter replaced by its argument, `result` by the call's value and each `old(e)` by `e` over the arguments as they were before the call; a `mut` argument denotes its value after the call.

```nan
pub closed struct Counter {
    pub hits: u32
} Counter

pub fn Counter.bump(mut self)
    requires self.hits < 1_000
    ensures self.hits == old(self.hits) + 1
{
    self.hits += 1
} Counter.bump

pub closed struct Reading {
    pub value: u32
} Reading

pub fn Reading.into_value(take self) -> u32
    ensures result == old(self.value)
{
    return self.value
} Reading.into_value
```

```nan
pub closed struct Reading {
    pub value: u32
} Reading

pub fn Reading.into_value(take self) -> u32
    ensures result == self.value // error: contract.taken-parameter
{
    return self.value
} Reading.into_value
```

```nan
pub fn next_level(level: u32) -> u32
    requires level < 100
    ensures old(result) > level // error: contract.old-form
{
    return level + 1
} next_level
```

### Examples

An **example** `example E` states a `bool` expression that holds and shows how the function is used. It is part of the function's interface ([hashes](29-identity-versioning.md#hashes)).

- E has type `bool` (`type.mismatch`). It is checked as the returned expression of a private function of the item's module that has no parameters, no `fails` and no `uses wait`: it names no parameter of the function, and every generic item it names is fully instantiated.
- `try`, `fail` or `await` in E is an error (`example.form`); a failing call in E is reified with `attempt`.
- E may name items of `testing` and sim constructors ([simulated capabilities](22-testing.md#simulated-capabilities)).
- `nan check` runs every example of the items it checks in an [example run](21-interpreter.md#example-runs). A run that yields `false` is an error (`example.failed`), and one that traps is an error (`example.trapped`); the interpreter reports the other ends of the run.
- An example is never a fact and never removes a run-time check.

```nan
pub closed enum DigitError derives Eq, Render {
    NotDigit(u8)
} DigitError

pub fn parse_digit(byte: u8) -> u8 fails DigitError
    example attempt parse_digit(b'7') == Ok(7)
    example attempt parse_digit(b'x') == Err(DigitError.NotDigit(b'x'))
{
    if byte < b'0' or byte > b'9' {
        fail DigitError.NotDigit(byte)
    }
    return byte - b'0'
} parse_digit
```

```nan
pub fn double(value: u32) -> u32
    example double(4) == 9 // error: example.failed
{
    return value * 2
} double
```

```nan
pub closed enum DigitError derives Eq, Render {
    NotDigit(u8)
} DigitError

pub fn parse_digit(byte: u8) -> u8 fails DigitError
    example try parse_digit(b'7') == 7 // error: example.form
{
    if byte < b'0' or byte > b'9' {
        fail DigitError.NotDigit(byte)
    }
    return byte - b'0'
} parse_digit
```

### Decreases

`decreases M` bounds recursion within a cycle, as [propagation](10-effects-digests.md#propagation) defines cycles and their termination.

- At each direct call from a function of a cycle to a function of the same cycle, both with a decreases clause, the callee's M with each parameter replaced by its argument is less than the caller's M at entry, both compared as integers. This obligation must be proved; one the prover does not prove is an error (`contract.decreases-unproven`), whose because-chain gives the prover's [outcome](20-prover.md#outcomes), unknown or refuted with its counterexample.
- A decreases clause on a function in no cycle, and a second decreases clause, are errors (`contract.decreases-unneeded`); the exact fix deletes the clause.

```nan
pub fn digit_count(value: u64) -> u32
    decreases value
{
    if value < 10 {
        return 1
    }
    return 1 + digit_count(value / 10)
} digit_count
```

```nan
pub fn halve(value: u64) -> u64
    decreases value // error: contract.decreases-unneeded
{
    return value / 2
} halve
```

### Loop invariants

A **loop invariant** is an `invariant P` line opening the body of a `while`, `for` or `loop` statement ([statements](06-statements-expressions.md#statements)).

```ebnf
loop_invariant = "invariant" expr NL ;
```

- P is a `bool` contract term. It names the bindings in scope at the start of the body, the loop element included.
- Each time control reaches the start of the body, each invariant line in order is an obligation and then a fact. The [prover](20-prover.md#weakest-preconditions) proves it from the facts of the paths reaching that point, the loop's back edges removed; the loop's header gains no premise from it.
- An invariant kept as a [run-time check](#run-time-checks) traps when false (`trap.expect`).

```nan
pub fn count_below(values: Slice[u8], limit: u8) -> usize
    ensures result <= values.len()
{
    var count: usize = 0
    for i in 0..<values.len() {
        invariant count <= i
        if values[i] < limit {
            count += 1
        }
    }
    return count
} count_below
```

```nan
pub fn sum_small(values: Slice[u8]) -> u32 {
    var total: u32 = 0
    for value in values {
        invariant total < 1_000 // traps: trap.expect
        total += u32.widen(value)
    }
    return total
} sum_small

test five_large_bytes for sum_small {
    expect sum_small(b"\xFF\xFF\xFF\xFF\xFF") > 0
} five_large_bytes
```

## Contract terms

A **contract term** is the expression of a requires, ensures, decreases, loop invariant or `assume` line, of a [predicate](04-types.md#predicates), or of a `where` condition on a [const parameter](08-generics-interfaces.md#generic-parameters). Contract terms are total: evaluating one never traps, other than by [stack overflow](17-suspension-io.md#stack-overflow), and never waits, runs without end or acts on anything.

- A call in a contract term, an operator on a non-scalar type included, reaches only a callee whose [digest](10-effects-digests.md#digests), or [declared digest](10-effects-digests.md#indirect-calls) for a call through a bound, through `Dyn` or through a function value, is [pure](10-effects-digests.md#purity) with terminates true and may-trap false. No obligation of such a callee, its ensures included, keeps a run-time check.
- A contract term contains no `try`, `attempt`, `fail`, `await`, `mut` or `take` marker, closure, trusted code, `todo`, `dbg` or `await when`.
- A contract term breaking these rules is an error (`contract.ill-defined`).

```nan
pub fn all_below(values: Slice[u8], limit: u8) -> bool
    ensures result == (all i in 0..<values.len(): values[i] < limit)
{
    for i in 0..<values.len() {
        invariant all j in 0..<i: values[j] < limit
        if values[i] >= limit {
            return false
        }
    }
    return true
} all_below
```

```nan
pub fn scaled(value: u32) -> u32 {
    return value * 3
} scaled

pub fn grow(value: u32) -> u32
    requires scaled(value) < 1_000 // error: contract.ill-defined
{
    return scaled(value)
} grow
```

### Quantifiers

A quantifier states a condition over every value of a range, or over some value of it.

```ebnf
quantifier_expr = ( 'all' | 'any' ) IDENT "in" expr ":" expr ;
```

- The expression after `in` is a [range expression](06-statements-expressions.md#ranges), `a..<b` or `a..=b`, over an integer scalar type; any other domain is an error (`contract.ill-defined`). The identifier is bound in the body, with the range's element type, to each value of the range.
- The body has type `bool` (`type.mismatch`). `all i in r: P` is true when P is true for every value of r, so for an empty range. `any i in r: P` is true when P is true for at least one value of r.
- Evaluation takes the values in increasing order and stops at the first value that decides the result.
- The body after `:` is a whole `expr`, so it extends as far as an expression continues. A quantifier that is the operand of an operator, `not` included, is written in parentheses (`expr.parentheses`).
- A quantifier appears only in contract terms and `expect` conditions; anywhere else it is an error (`contract.quantifier-outside`).

```nan
pub fn has_zero(values: Slice[u8]) -> bool {
    return any i in 0..<values.len(): values[i] == 0 // error: contract.quantifier-outside
} has_zero
```

```nan
pub fn all_below(values: Slice[u8], limit: u8) -> bool
    ensures result == all i in 0..<values.len(): values[i] < limit // error: expr.parentheses
{
    for i in 0..<values.len() {
        if values[i] >= limit {
            return false
        }
    }
    return true
} all_below
```

### Well-definedness

Each partial operation in a contract term carries a **well-definedness obligation**, the condition under which it does not trap: an index, an arithmetic operation, shift or named conversion that may trap ([Numbers](09-numbers.md)), a construction of a type with predicates, and a call to a function with requires.

- The obligation is discharged under the conditions guarding the operation: the left operand of `and`, the negated left operand of `or`, the conditions under which the enclosing `if` branch or `match` arm is taken, and, for a quantifier's bound name, its range.
- Its other premises depend on the term's position:
    - a requires clause: the parameters' types, the predicates that are facts at entry and the requires clauses written before it;
    - a decreases clause, and the `e` of an `old(e)`: those of a requires clause and every requires clause;
    - the rest of an ensures clause: those of a requires clause and every requires clause, over the values at entry, the result's type, the predicates that [predicate obligations](04-types.md#obligations) make facts at each exit of the function, and the ensures clauses written before it;
    - a loop invariant or an `assume`: the facts at its position;
    - a predicate or a `where` condition: the types of the fields, the consts and the `where` conditions of const parameters.
- A well-definedness obligation must be proved. One the prover does not prove is an error, reported with its outcome, `proof.unknown` or `proof.refuted`.

### Opacity

A subterm of a contract term is **opaque** to code that cannot see a field, item or member it names ([visibility](03-packages-modules.md#visibility)).

- The prover represents each opaque subterm as a fresh symbol, one per distinct subterm within the facts of one call and within one requires site ([callee knowledge](20-prover.md#callee-knowledge)), so it knows the subterm only through its type's range and the facts in which that symbol stands.
- A call to a [transparent function](02-items.md#transparent-functions) that the code sees but does not unfold is not opaque; callee knowledge gives its term.
- Opacity changes what the prover knows, never what a term means: a run-time check evaluates the whole term.

In the next example the root module cannot unfold `Meter.value`, which names the private field `count`, so it knows `m.value()` only as the application of `Meter.value` to `m`. The ensures of `Meter.tick` gives a fact over that same application, which proves that the subtraction in `headroom` cannot overflow.

```nan
// file: example/package.nan
//! Bounded counters.

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

pub fn Meter.tick(mut self)
    ensures self.value() <= LIMIT
{
    if self.count < LIMIT {
        self.count += 1
    }
} Meter.tick

// file: example/example.nan
//! Headroom left after a tick.

pub fn headroom(mut m: meter::Meter) -> u32 limits notrap {
    mut m.tick()
    return meter::LIMIT - m.value()
} headroom
```

### Vacuous contracts

A contract that no input satisfies is an error (`contract.vacuous`), reported when the prover proves, by [entailment](20-prover.md#entailment), that the conjunction of these clauses is `false`:

- the requires clauses of a function, under the parameters' types and the predicates that are facts at entry, reported on the last requires clause;
- the ensures clauses of a function, under its requires clauses over the values at entry, the parameters' and result's types and the predicates that are facts at entry and at each exit, reported on the last ensures clause;
- the predicates of a struct, under the types of its fields and the consts, reported on the struct's first line.

```nan
pub fn narrow(level: u8) -> u8
    requires level > 200
    requires level < 100 // error: contract.vacuous
{
    return level
} narrow
```

## Expect

`expect P` states that P holds at that point of a body.

```ebnf
expect_stmt = "expect" expr NL ;
```

- P has type `bool` (`type.mismatch`). A P that is not [pure](10-effects-digests.md#purity) or contains an `await`, a `mut` marker or a `take` marker is an error (`contract.impure-condition`). Unlike a contract term, P may contain operations that trap, whose obligations are ordinary.
- P is an obligation at the statement. Executing the statement evaluates P; when the expect's status is checked, it then traps if P is false (`trap.expect`), and otherwise it does nothing more.
- After the statement, P is a fact.
- When P is a comparison, the [trap report](11-errors-domains.md#trap-reports) of a false expect gives both sides on its operand line.

```nan
pub fn checksum(data: Slice[u8]) -> u8 {
    var total: u8 = 0
    for byte in data {
        total = total.wrapping_add(byte)
    }
    return total
} checksum

test checksum_of_pair for checksum {
    expect checksum(b"\x01\x02") == 3
} checksum_of_pair
```

```nan
pub fn checksum(data: Slice[u8]) -> u8 {
    var total: u8 = 0
    for byte in data {
        total = total.wrapping_add(byte)
    }
    return total
} checksum

test checksum_of_pair for checksum {
    expect checksum(b"\x01\x02") == 4 // traps: trap.expect
} checksum_of_pair
```

```nan
pub closed struct Ticker {
    pub count: u32
} Ticker

pub fn Ticker.advance(mut self) -> u32 {
    self.count = self.count.saturating_add(1)
    return self.count
} Ticker.advance

test advance_counts_up for Ticker.advance {
    var ticker = Ticker(count: 0)
    expect mut ticker.advance() == 1 // error: contract.impure-condition
} advance_counts_up
```

## Assume

`assume P because "why"` makes P a fact without proof or check.

```ebnf
assume_stmt = "assume" expr 'because' TEXT NL ;
```

- An `assume` is legal only inside [trusted code](23-low-level.md#trusted-code); anywhere else it is an error (`contract.assume-untrusted`), whose exact fix replaces it with `expect P`.
- P is a `bool` contract term. A reason that is empty or holds only spaces is an error (`contract.empty-reason`).
- Compiled code runs no code for an `assume`; [interpreter runs](21-interpreter.md#ptr-checks) evaluate its condition. After it, P is a fact.
- Each `assume` is a [trust escape](31-trust.md#trust-escapes) and is listed in the [ledger](31-trust.md#ledger).

```nan
// file: board/package.nan
//! Firmware table access.

package board {
    language 1
    targets any
    allow trusted in board
} board

// file: board/board.nan
//! Reads entries of a firmware table of fixed length.

pub const ENTRIES: usize = 16

pub fn entry(table: Slice[u32], index: usize) -> u32
    requires index < ENTRIES
{
    trusted "the firmware hands over the whole table" {
        assume table.len() == ENTRIES because "the firmware specification fixes the table at ENTRIES words"
    }
    return table[index]
} entry
```

```nan
// file: board/package.nan
//! Firmware table access.

package board {
    language 1
    targets any
    allow trusted in board
} board

// file: board/board.nan
//! Reads entries of a firmware table.

pub fn entry(table: Slice[u32], index: usize) -> u32 {
    assume index < table.len() because "callers pass valid indexes" // error: contract.assume-untrusted
    return table[index]
} entry
```

## Obligations

An **obligation** is a condition that must hold at a program point. Obligations are elaborated as weakest preconditions over [NIR](19-nir.md#nir) and discharged by the [prover](20-prover.md#queries).

| Obligation | Program point | Trap kind | Rule |
| --- | --- | --- | --- |
| requires | each direct call; the checking entry | `trap.requires` | [Requires](#requires) |
| interface requires | each call through a bound or `Dyn` | `trap.requires` | [Interface contracts](#interface-contracts) |
| trusted requirement | each call to a trusted primitive | `trap.requires` | [Trusted primitives](23-low-level.md#trusted-primitives) |
| ensures | each return | `trap.ensures` | [Ensures](#ensures) |
| expect | the statement | `trap.expect` | [Expect](#expect) |
| loop invariant | each start of the loop body | `trap.expect` | [Loop invariants](#loop-invariants) |
| predicate | construction and the module boundary | `trap.predicate` | [Predicates](04-types.md#obligations) |
| index bound | each call of `at` or `at_mut` of an array, slice, list, bytes or bounded list | `trap.bounds` | [Indexing](07-ownership.md#indexing) |
| arithmetic, shift, division, conversion | each operation | the operation's | [Numbers](09-numbers.md) |
| layout field | each construction of a layout value, and each store into a ranged `bit` or `rest` field | `trap.conversion` | [Layout values](23-low-level.md#layout-values) |
| well-definedness | each partial operation of a contract term | none | [Well-definedness](#well-definedness) |
| decreases | each direct call within a cycle | none | [Decreases](#decreases) |

- An obligation with a trap kind has one of the four [statuses](#statuses). One without a trap kind must be proved, as its rule states.
- Where [outcomes](20-prover.md#outcomes) demands proof (notrap demanded by a function's own limits, an implemented interface method, an interrupt fn's kind or a fitted function type, transparent bodies, and the well-definedness of contract terms), every obligation must be proved.

A **fact** is a condition the prover may use as a premise at a program point. This chapter's facts are: each requires clause at entry to the body; after a call returns, the ensures that [Ensures](#ensures) and [Interface contracts](#interface-contracts) give it; the condition of every obligation that is not open, after its program point; and the condition of an `assume` after its statement. Branch conditions are facts through [weakest preconditions](20-prover.md#weakest-preconditions), and the owning chapters state the others, such as [predicates](04-types.md#obligations) and the element range of a `for` over a [range](09-numbers.md#ranges). Disputed clauses, examples and tests are never facts.

In the next example the bounds obligation of `data[index]` is proven from the branch condition, so no check remains.

```nan
pub fn byte_at(data: Slice[u8], index: usize) -> u8 {
    if index < data.len() {
        return data[index]
    }
    return 0
} byte_at
```

## Statuses

A **status** tells how an obligation holds. The statuses are exactly:

- **proven**: the prover's [outcome](20-prover.md#outcomes) is proved, and the obligation is not assumed; no code runs.
- **assumed**: the outcome is proved, and one of the query's relevant premises ([relevance](20-prover.md#relevance)) is the operand of an `assume` or the condition of an obligation site whose status is assumed; no code runs.
- **checked**: the outcome is refuted or unknown, and a run-time check enforces the obligation.
- **open**: the obligation belongs to a [disputed clause](#disputed-clauses); it is never checked and never a fact.

A status is never written in source. The status of an ensures clause is the first of open, assumed, checked and proven among the statuses of its obligations; `nan show <path> --api` prints it after the clause, and `nan explain <item>` prints the status of every obligation of the item ([commands](33-toolchain.md#commands)).

In the next example the expect is checked, and the overflow and bounds obligations of `values[values.len() - 1]` are proven from the fact it leaves.

```nan
pub fn last(values: Slice[u32]) -> u32 {
    expect values.len() > 0
    return values[values.len() - 1]
} last
```

## Run-time checks

A **run-time check** is code that evaluates an obligation's condition at its program point and traps with the obligation's trap kind when the condition is false. An unproven ensures is checked by a **return check** at each return.

- An obligation keeps its check exactly when its status is checked. Only a proof whose premises are facts removes a check; tests and examples never remove one.
- Checks run in every build of every target; no option or target removes one.
- At a call, the arguments are evaluated, then the predicate checks of the call run, then the requires checks in clause order. At a return, the deferred calls run, then the predicate checks of the exit, then the return checks in clause order, before control reaches the caller. Expect and invariant checks run where they are written.
- Evaluating a contract term traps only by stack overflow, since contract terms are total. A return check reads each `old(e)` as the value `e` had at entry; the implementation saves that value at entry when a return check needs it.
- In the [trap report](11-errors-domains.md#trap-reports) of a failed requires, ensures or loop invariant check, the trapping operation is the clause or invariant line in the item that declares it, or the call when that item belongs to std; for a requires or ensures check, the first caller line is the call the check belongs to. For a failed expect, the trapping operation is the statement.

```nan
pub fn midpoint(low: u32, high: u32) -> u32
    requires low <= high // traps: trap.requires
{
    return low + (high - low) / 2
} midpoint

test inverted_bounds for midpoint {
    let middle = midpoint(9, high: 3)
    expect middle == 6
} inverted_bounds
```

```nan
pub fn clamp_percent(value: u32) -> u8
    ensures result <= 100 // traps: trap.ensures
{
    if value > 200 {
        return 100
    }
    return u8.saturate(value)
} clamp_percent

test large_values_clamp for clamp_percent {
    expect clamp_percent(150) == 100
} large_values_clamp
```

## Checking entries

A **checking entry** is the entry of a function that meets its requires before running its body, for reaches where no caller meets them.

- A function with a requires clause has a checking entry. It is used for every reach other than a direct call or a call through a bound or `Dyn`: a use as a function value at each site of the [fit rule](10-effects-digests.md#fit), a [contribution](25-registries.md#contributions) to a registry, the [entry function](27-program-model.md#entry), and a [machine seam](23-low-level.md#machine-seams).
- The checking entry meets each requires in clause order, with the parameters' types and the predicates that are facts at entry as premises. A requires it does not prove is checked there and traps when false (`trap.requires`).
- Where the fit rule judges a function used as a value, its may-trap fact is true when its checking entry keeps a check.
- A direct call never passes through the checking entry.

```nan
pub closed struct Rule {
    pub apply: fn(divisor: u32) -> u32
} Rule

pub fn per_hundred(divisor: u32) -> u32
    requires divisor > 0 // traps: trap.requires
{
    return 100 / divisor
} per_hundred

pub fn per_hundred_rule() -> Rule = Rule(apply: per_hundred)

test zero_reaches_checking_entry for per_hundred {
    let rule = per_hundred_rule()
    let share = (rule.apply)(0)
    expect share == 0
} zero_reaches_checking_entry
```

## Interface contracts

The contract of an interface method binds every implementing function. In it, `Self` and `self` denote the implementing type and value.

- At a call through a generic [bound](08-generics-interfaces.md#generic-parameters) or `Dyn`, the interface method's requires, with `Self` replaced by the type, is an obligation at the call; callers check it, never the implementing function. Its ensures is a fact after the call returns.
- An [implementing function](08-generics-interfaces.md#conformance) `T.m` with requires clauses that are not disputed has those requires; any other has the interface method's, with `Self` replaced by `T`.
- Each requires of `T.m` is entailed ([entailment](20-prover.md#entailment)) by the interface method's requires, with `Self` replaced by `T`, together with the parameters' types and the predicates that are facts at entry. A requires that is not entailed is an error (`contract.impl-requires`). An implementing function thus weakens requires and never strengthens them.
- The ensures of `T.m` are its own ensures clauses and, for each ensures Q of the interface method, Q itself when the interface method has no requires that are not disputed, and otherwise the clause `not old(R) or Q`, R being the conjunction of those requires; `Self` is replaced by `T` throughout. An implementing function thus strengthens ensures wherever the interface's requires hold and never weakens them.
- A direct call of `T.m` has the requires and ensures of `T.m`. A default body has the interface method's contract.

```nan
pub interface Slots {
    fn capacity(self) -> u32 limits pure, notrap, terminates
    fn slot_offset(self, slot: u32) -> u32
        requires slot < self.capacity()
        ensures result < 4_096
} Slots

pub closed struct Page {
    pub slots: u32 where slots <= 64
} Page

impl Page: Slots

pub fn Page.capacity(self) -> u32 = self.slots

pub fn Page.slot_offset(self, slot: u32) -> u32
    requires slot < 64
    ensures result <= 4_032
{
    return slot * 64
} Page.slot_offset

pub fn first_offset[S: Slots](table: S) -> u32
    requires table.capacity() > 0
{
    return table.slot_offset(0)
} first_offset
```

```nan
pub interface Slots {
    fn capacity(self) -> u32 limits pure, notrap, terminates
    fn slot_offset(self, slot: u32) -> u32
        requires slot < self.capacity()
        ensures result < 4_096
} Slots

pub closed struct Page {
    pub slots: u32 where slots <= 64
} Page

impl Page: Slots

pub fn Page.capacity(self) -> u32 = self.slots

pub fn Page.slot_offset(self, slot: u32) -> u32
    requires slot < 32 // error: contract.impl-requires
{
    return slot * 64
} Page.slot_offset
```

## Todo

`todo` marks what is unfinished, with a text stating the intent or the open question. A text that is empty or holds only spaces is an error (`todo.empty-reason`). Each form of `todo` is [incompleteness](31-trust.md#incompleteness) with the code its section names.

```ebnf
todo_expr = "todo" TEXT ;
todo_clause = "todo" TEXT ( NL | requires_clause | ensures_clause | example_clause ) ;
```

### Holes

A **hole** is a `todo` expression, or a part missing from a prefix that `nan check --prefix` closes ([prefix viability](41-grammar.md#prefix-viability)), typed by what is expected there.

- A `todo` expression has the expected type of its position, and type `Unit` as an expression statement. One without an expected type elsewhere is an error (`todo.untyped`).
- A `todo` expression with a type is incompleteness (`todo.hole`). Reaching it traps (`trap.todo`).
- The **producers** of a hole are the bindings in scope at the hole whose type is its expected type and the functions its module may name whose result type is its expected type. `nan holes` lists each hole with its expected type and producers ([commands](33-toolchain.md#commands)).

```nan
pub fn retry_delay(retries: u32) -> u32 {
    if retries == 0 {
        return 0
    }
    return todo "choose the backoff curve" // incomplete: todo.hole
} retry_delay
```

```nan
pub fn retry_delay(retries: u32) -> u32 {
    let delay = todo "choose the backoff curve" // error: todo.untyped
    return delay
} retry_delay
```

### Missing specifications

A `todo_clause` ending at its text, `todo "question"`, marks the function's specification as missing. It is incompleteness (`todo.missing-spec`) and gives no obligation.

```nan
pub fn retry_delay(retries: u32) -> u32
    todo "is the delay capped?" // incomplete: todo.missing-spec
{
    return retries.saturating_mul(100)
} retry_delay
```

### Disputed clauses

A requires, ensures or example clause written after `todo TEXT` on its line is **disputed**, and so is a test whose item starts with `todo TEXT` ([test items](22-testing.md#test-items)).

- A disputed clause is a clause of the kind it marks. It obeys the rules of form of that kind (its type, the rules of contract terms, `result` and `old`, and the form of examples) and has no well-definedness obligations.
- A disputed requires or ensures stays in the item's text and interface, and its obligations are open. A disputed clause takes no part in vacuity, refinement or checking entries, and a disputed example does not run. Every other rule of this chapter that gives a clause an effect applies only to clauses that are not disputed.
- Each disputed clause or test is incompleteness (`todo.disputed`).

```nan
pub fn retry_delay(retries: u32) -> u32
    todo "the cap moves to the caller" ensures result <= 10_000 // incomplete: todo.disputed
{
    return retries.saturating_mul(100)
} retry_delay
```

## Diagnostics

| Code | Class | Rule |
| --- | --- | --- |
| `contract.assume-untrusted` | error | [Assume](#assume) |
| `contract.decreases-unneeded` | error | [Decreases](#decreases) |
| `contract.decreases-unproven` | error | [Decreases](#decreases) |
| `contract.empty-reason` | error | [Assume](#assume) |
| `contract.ill-defined` | error | [Contract terms](#contract-terms), [Quantifiers](#quantifiers) |
| `contract.impl-requires` | error | [Interface contracts](#interface-contracts) |
| `contract.impure-condition` | error | [Expect](#expect) |
| `contract.old-form` | error | [Ensures](#ensures) |
| `contract.quantifier-outside` | error | [Quantifiers](#quantifiers) |
| `contract.taken-parameter` | error | [Ensures](#ensures) |
| `contract.vacuous` | error | [Vacuous contracts](#vacuous-contracts) |
| `example.failed` | error | [Examples](#examples) |
| `example.form` | error | [Examples](#examples) |
| `example.trapped` | error | [Examples](#examples) |
| `todo.disputed` | incompleteness | [Disputed clauses](#disputed-clauses) |
| `todo.empty-reason` | error | [Todo](#todo) |
| `todo.hole` | incompleteness | [Holes](#holes) |
| `todo.missing-spec` | incompleteness | [Missing specifications](#missing-specifications) |
| `todo.untyped` | error | [Holes](#holes) |
