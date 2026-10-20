# Statements and expressions

This chapter defines statements, blocks and divergence, bindings, assignment, expression statements and `discard`, control flow and returns, value-yielding `if` and `match`, `let ... else`, match arms, operators and their precedence, range expressions and `core::Range`, calls with their arguments and markers, field access and method calls, the evaluation order, and the thresholds on nesting depth and body length.

## Statements

A **statement** is the unit of a block: a binding, an assignment, a control-flow construct or an expression. Every statement ends with the `NL` of a logical line, and that `NL` ends no other statement; nothing else separates statements. A statement holding blocks also spans the lines of its blocks.

A **block** is a braced sequence of statements. Because `{` continues its logical line ([lines](01-lexical.md#lines)), the first statement of a block shares the logical line of the `{` that opens it, and the `}` closing a block that holds statements follows the `NL` of its last statement.

```ebnf
block = "{" stmt* "}" ;
loop_block = "{" loop_invariant* stmt* "}" ;
stmt = let_stmt | var_stmt | discard_stmt | while_stmt | for_stmt | loop_stmt
    | defer_stmt | undefer_stmt | expect_stmt | assume_stmt | expr_stmt ;
```

- The bodies of `while`, `for` and `loop` are loop blocks, which may open with [loop invariants](18-contracts.md#loop-invariants). Every other block is a `block`.
- `defer_stmt` and `undefer_stmt` are defined by [deferred calls](17-suspension-io.md#defer), `expect_stmt` by [expect](18-contracts.md#expect) and `assume_stmt` by [assume](18-contracts.md#assume).

### Divergence

A statement **diverges** when control never reaches its end. Exactly these statements diverge:

- an expression statement whose expression has type `Never` ([Unit and Never](04-types.md#unit-and-never));
- a `let` or `var` statement whose initialiser, or an assignment whose right side, has type `Never`;
- an expression statement whose expression is an `if` in [statement position](#expression-statements) that ends in an `else` block and whose blocks, those of `else if` included, all diverge;
- an expression statement whose expression is a `match` in statement position whose arm bodies all diverge, an expression body diverging when its type is `Never`;
- a `loop` that no `break` leaves: no `break` in its loop block outside closure bodies names its label or, unlabelled, has it as its innermost enclosing loop.

A block diverges when its last statement diverges. A statement after a diverging statement of the same block is an error (`stmt.unreachable`). `while` and `for` never diverge, whatever their condition.

```nan
pub fn sign(value: i32) -> i32 {
    if value < 0 {
        return -1
    }
    if value > 0 {
        return 1
    }
    return 0
} sign
```

```nan
pub fn sign(value: i32) -> i32 {
    if value < 0 {
        return -1
    }
    return 1
    return 0 // error: stmt.unreachable
} sign
```

## Bindings

A **binding** is a local name introduced by `let`, which is never reassigned, or by `var`, which may be.

```ebnf
let_stmt = "let" pattern ( ":" type_expr )? "=" expr ( "else" block )? NL ;
var_stmt = "var" IDENT ( ":" type_expr )? "=" expr NL ;
```

- `let P = e` matches the value of `e` against the [pattern](04-types.md#patterns) `P` and binds the names `P` introduces; `var x = e` binds one name. Every binding is initialised at its definition.
- The names a pattern introduces in a `let` statement or a `match` arm are **pattern bindings**, and each is a `let` binding. [Modes](07-ownership.md#modes) fixes which bindings and parameters may be assigned, accessed by `mut` or moved.
- A type after `:` is the binding's type and the [expected type](04-types.md#expected-types-and-literals) of the initialiser. Without one, the binding's type is the type of the initialiser, which must be fixed without an expected type: an unsuffixed number literal there is `type.literal-untyped`, and an unfixed generic argument is `generic.uninferred` ([inference](08-generics-interfaces.md#inference)). Either way the type is fixed at the definition, and no later statement changes or completes it.
- A refutable pattern takes an `else` block ([Let else](#let-else)).
- A `let` statement whose pattern is `_` is an error (`stmt.wildcard-binding`); its exact fix replaces `let _ = e`, with any type after `_`, by `discard e`, or by `e` alone when `e` has type `Unit` or `Never`.
- A binding is in scope from the statement after its own to the end of its block. The names of a `for` pattern are in scope in its loop block, those of an arm's patterns in its guard and body, and the parameters of a function or closure in its whole body.

No name is shadowed. A binding, a parameter (closure parameters included) or a [quantifier](18-contracts.md#quantifiers) variable spelt like another binding, parameter or quantifier variable whose scope overlaps its own, like a name of its module's values namespace ([naming conventions](03-packages-modules.md#naming-conventions)), or like a [core vocabulary](03-packages-modules.md#core-vocabulary) name, is an error (`stmt.shadowing`), reported on the later of two locals and otherwise on the local. A loop label spelt like the label of an enclosing loop is also an error (`stmt.shadowing`).

```nan
pub fn mean(samples: Slice[u32]) -> Option[u32] {
    if samples.len() == 0 {
        return None
    }
    var sum: u64 = 0
    var count: u64 = 0
    for sample in samples {
        sum += u64.widen(sample)
        count += 1
    }
    let average = sum / count
    return Some(u32.fit(average))
} mean
```

```nan
pub fn scaled(value: u32, factor: u32) -> u32 {
    let value = value * factor // error: stmt.shadowing
    return value
} scaled
```

```nan
pub fn byte_count(values: Slice[u8]) -> usize {
    let _ = values.len() // error: stmt.wildcard-binding
    return values.len()
} byte_count
```

## Assignment

```ebnf
expr_stmt = expr ( assign_operator expr )? NL ;
assign_operator = "=" | "+=" | "-=" | "*=" | "/=" | "%=" | "&=" | "|=" | "^=" | "<<=" | ">>=" ;
```

- In `p = e` and `p op= e`, `p` is a [place](07-ownership.md#places); any other left side, such as a call or a literal, is an error (`stmt.assign-target`). Assigning a place takes no marker.
- `p = e` stores the value of `e`, whose expected type is the type of `p`; the value `p` held before ends as at [end of scope](07-ownership.md#ownership).
- `p op= e`, for each operator `op` of `+`, `-`, `*`, `/`, `%`, `&`, `|`, `^`, `<<` and `>>`, stores the value of `p op e`, computed by the rules of `op`; a `p op e` whose type differs from the type of `p` is an error (`type.mismatch`). The operands of `p` are evaluated once ([Evaluation order](#evaluation-order)).
- An assignment is a statement and never an expression.

```nan
pub closed struct Tally {
    pub hits: u32
    pub misses: u32
} Tally

pub fn Tally.record(mut self, hit: bool) {
    if hit {
        self.hits += 1
    } else {
        self.misses += 1
    }
} Tally.record

pub fn reset_first(mut counts: SliceMut[u32]) {
    if counts.len() > 0 {
        counts[0] = 0
    }
} reset_first
```

```nan
pub fn reset(mut counts: SliceMut[u32]) {
    counts.len() = 0 // error: stmt.assign-target
} reset
```

## Expression statements

An **expression statement** is an `expr_stmt` without an assignment operator. An expression is in **statement position** when it is the whole expression of an expression statement other than the value statement of a branch ([Value-yielding if and match](#value-yielding-if-and-match)), or the expression body of an arm of a `match` in statement position.

```ebnf
discard_stmt = "discard" expr NL ;
```

- An expression in statement position has type `Unit` or `Never`. A value of any other type is used, as an operand, initialiser, argument, assigned or returned value, or dropped with `discard`; an expression in statement position of another type is an error (`stmt.unused-result`), whose exact fix inserts `discard` before it.
- `discard e` evaluates `e` and ends its value at once, as at [end of scope](07-ownership.md#ownership): a place owning its value is moved, and a part or any other place not owning its value is read. `discard` before an expression of type `Unit` or `Never` is an error (`stmt.discard-unit`), whose exact fix deletes `discard`. Discarding a linear value is an error (`own.linear-dropped`, [linear consumption](07-ownership.md#linear-consumption)).
- An `if` or `match` in statement position yields no value: its blocks hold statements, its arms' expression bodies are in statement position, and its type is `Never` when it diverges and `Unit` otherwise.

```nan
pub closed struct Counter {
    pub count: u32
} Counter

pub fn Counter.advance(mut self) -> u32 {
    self.count += 1
    return self.count
} Counter.advance

pub fn advance_twice(mut counter: Counter) -> u32 {
    discard mut counter.advance()
    return mut counter.advance()
} advance_twice
```

```nan
pub closed struct Counter {
    pub count: u32
} Counter

pub fn Counter.advance(mut self) -> u32 {
    self.count += 1
    return self.count
} Counter.advance

pub fn advance_twice(mut counter: Counter) -> u32 {
    mut counter.advance() // error: stmt.unused-result
    return mut counter.advance()
} advance_twice
```

```nan
pub fn reopened(alloc: mem::Alloc) -> usize {
    let bytes = mem::Bytes.empty(alloc)
    discard bytes
    return bytes.len() // error: own.use-after-move
} reopened
```

## Control flow

### Conditionals and loops

```ebnf
if_expr = "if" expr block ( "else" ( if_expr | block ) )? ;
while_stmt = "while" expr loop_block NL ;
for_stmt = "for" pattern "in" expr loop_block NL ;
loop_stmt = "loop" ( 'as' IDENT )? loop_block NL ;
```

- The condition of `if` and `while` has the expected type `bool`. No other type converts to `bool`, so an integer, `Option` or any other value as a condition is an error (`type.mismatch`).
- `if c { A } else { B }` runs `A` when `c` is true and `B` otherwise; `else if` chains a further condition. An `else` directly after the last block of an `if` belongs to that `if`.
- `while c { B }` evaluates `c` before each iteration and runs `B` while it is true.
- `loop { B }` repeats `B` until a `break` or `return` leaves it.
- `for P in e { B }` runs `B` once per element `e` yields, matching each element against the irrefutable pattern `P` ([refutability](04-types.md#refutability)). [Iteration](07-ownership.md#iteration) states what `e` may be and how elements are reached.
- `loop as name { B }` gives the loop the label `name`; only `loop` takes a label. A label that no `break` or `continue` names is an error (`stmt.unused-label`), whose exact fix deletes `as name`.

```ebnf
jump_expr = "return" expr? | "break" IDENT? | "continue" IDENT? ;
```

- `break` leaves the innermost enclosing loop, and `continue` starts its next iteration: a `while` re-evaluates its condition and a `for` takes its next element. `break name` and `continue name` act on the loop labelled `name`.
- `break` or `continue` outside every loop of its own function or closure body is an error (`stmt.no-loop`); a label naming no enclosing loop is an error (`stmt.unknown-label`). Neither crosses a closure body.

```nan
pub fn first_blank_row(cells: Slice[u8], width: usize) -> Option[usize] {
    var row: usize = 0
    loop as rows {
        let start = row * width
        if start >= cells.len() {
            return None
        }
        for offset in 0..<width {
            let index = start + offset
            if index < cells.len() and cells[index] != b' ' {
                row += 1
                continue rows
            }
        }
        return Some(row)
    }
} first_blank_row
```

```nan
pub fn leading_spaces(line: Slice[u8]) -> usize {
    var count: usize = 0
    for byte in line {
        if byte != b' ' {
            break outer // error: stmt.unknown-label
        }
        count += 1
    }
    return count
} leading_spaces
```

```nan
pub fn has_items(count: usize) -> bool {
    if count { // error: type.mismatch
        return true
    }
    return false
} has_items
```

### Returns

- `return e` ends the innermost function or closure body with the value of `e`, whose expected type is the body's result type. A bare `return` ends a body whose result type is `Unit`.
- In a body whose result type is `Unit`, `return` followed by an expression is an error (`stmt.return-value`). Its exact fix deletes the expression when it is the value `Unit`, deletes `return` when the expression has type `Never`, and otherwise moves the expression to a statement of its own before a bare `return`, prefixed with `discard` when its type is not `Unit`.
- A block never yields its last expression. A function or closure block body whose result type is not `Unit` must diverge; one that does not is an error (`stmt.missing-return`), reported on its closing line. When inserting `return` before its last statement passes the [in-memory re-check](34-diagnostics.md#exact-fixes), that insertion is the exact fix, and that statement's `stmt.unused-result` errors are not reported.
- A body whose result type is `Unit` also ends by reaching its closing `}`.
- `return`, `break`, `continue` and `fail` ([fail](11-errors-domains.md#fail)) expressions have type `Never`. Each stands only as the whole expression of an expression statement or as the expression body of a match arm; anywhere else, such as an initialiser, an argument or a closure body, it is an error (`expr.misplaced-jump`).

```nan
pub closed struct Totals {
    pub sum: u64
} Totals

pub fn Totals.add_until_zero(mut self, values: Slice[u8]) {
    for value in values {
        if value == 0 {
            return
        }
        self.sum += u64.widen(value)
    }
} Totals.add_until_zero

pub fn digit_value(byte: u8) -> u8 {
    let digit = match byte {
        b'0'..=b'9' => byte - b'0'
        else => return 0
    }
    return digit
} digit_value
```

```nan
pub fn sign(value: i32) -> i32 {
    if value < 0 {
        return -1
    }
    if value > 0 {
        return 1
    }
} sign // error: stmt.missing-return
```

```nan
pub fn count_where(values: Slice[u32], keep: fn(x: u32) -> bool) -> usize {
    var count: usize = 0
    for value in values {
        if keep(value) {
            count += 1
        }
    }
    return count
} count_where

pub fn count_above(values: Slice[u32], floor: u32) -> usize {
    return count_where(values, keep: fn(x: u32) => return x > floor) // error: expr.misplaced-jump
} count_above
```

## Value-yielding if and match

Every `if` or `match` that is not in [statement position](#expression-statements) is **value-yielding**.

- A branch block of a value-yielding `if` either diverges or holds exactly one statement, an expression statement whose expression is the branch's value: its **value statement**. The body of each arm of a value-yielding `match` is an expression, or a block that diverges. Any other branch is an error (`expr.branch-form`), reported on its first statement, or on its `{` when it holds none.
- A value-yielding `if` has an `else`; one without is an error (`expr.missing-else`).
- The expected type of each branch is the expected type of the `if` or `match`. Without one, the first branch in source order whose type is not `Never` fixes the type of the `if` or `match` and the expected type of the branches after it (`type.mismatch`). When every branch has type `Never`, so does the `if` or `match`.
- Evaluation runs the condition or scrutinee, then exactly one branch, whose value is the result.

```nan
pub fn larger(left: u32, right: u32) -> u32 {
    let bigger = if left >= right {
        left
    } else {
        right
    }
    return bigger
} larger

pub closed enum Level {
    Low
    Mid
    High
} Level

pub fn level_for(reading: u16) -> Level {
    return match reading {
        0..<100 => Level.Low
        100..<1_000 => Level.Mid
        else => Level.High
    }
} level_for
```

```nan
pub fn larger(left: u32, right: u32) -> u32 {
    let bigger = if left >= right {
        let chosen = left // error: expr.branch-form
        chosen
    } else {
        right
    }
    return bigger
} larger
```

```nan
pub fn capped(value: u32) -> u32 {
    let limited: u32 = if value > 100 { // error: expr.missing-else
        100
    }
    return limited
} capped
```

## Let else

`let P = e else { B }` binds a refutable pattern.

- The value of `e` is matched against `P`. On a match, the statement binds as `let` does. Otherwise `B` runs, and the names of `P` are not in scope in `B`.
- `B` must diverge; a block that does not is an error (`stmt.else-not-diverging`), reported on the `let` line.
- `P` is refutable; an `else` after an irrefutable pattern is an error (`stmt.irrefutable-else`), whose exact fix deletes the `else` block.

```nan
pub fn doubled_or_zero(reading: Option[u32]) -> u32 {
    let Some(value) = reading else {
        return 0
    }
    return value.saturating_mul(2)
} doubled_or_zero
```

```nan
pub fn doubled_or_zero(reading: Option[u32]) -> u32 {
    let Some(value) = reading else { // error: stmt.else-not-diverging
    }
    return value.saturating_mul(2)
} doubled_or_zero
```

## Match

```ebnf
match_expr = "match" "first"? expr "{" match_arm+ "}" ;
match_arm = ( pattern ( "or" pattern )* ( "if" expr )? | "else" ) "=>" arm_body NL ;
arm_body = block | expr ;
```

`match` evaluates its scrutinee once and runs the body of the arm whose pattern the value matches; each pattern's type is the scrutinee's type ([patterns](04-types.md#patterns)).

### Match arms

- An arm `p1 or p2 => b` matches when any of its alternatives matches. All alternatives of an arm bind the same names with the same types; an arm whose alternatives differ in the names or types they bind is an error (`expr.or-bindings`).
- An `else` arm matches every value no other arm matches. A match has at most one; a second is an error (`expr.overlapping-arms`).
- A float literal as a pattern is an error (`expr.float-pattern`).
- In a `match` whose scrutinee is owned ([pattern bindings](04-types.md#pattern-bindings)), an `else` arm or a `_` pattern matching a value or part that holds a linear value is an error (`own.linear-dropped`, [linear consumption](07-ownership.md#linear-consumption)).

```nan
pub closed enum Command {
    Start
    Stop
    Pause
    Step(u32)
} Command

pub fn Command.is_motion(self) -> bool {
    return match self {
        Command.Start or Command.Step(_) => true
        Command.Stop or Command.Pause => false
    }
} Command.is_motion
```

```nan
pub fn is_unit(ratio: f64) -> bool {
    return match ratio {
        1.0 => true // error: expr.float-pattern
        else => false
    }
} is_unit
```

### Disjointness and exhaustiveness

- In a `match` without `first`, the alternatives of all arms are pairwise disjoint: no value matches two of them, within one arm or across arms. Two overlapping alternatives are an error (`expr.overlapping-arms`), reported on the later of their arms in source order. When that arm's only pattern is `_` and the match has no `else` arm, the exact fix replaces `_` with `else`. The order of arms carries no meaning.
- Every match is exhaustive: each value of the scrutinee's type matches an arm. A match that is not is an error (`expr.non-exhaustive`), reported on the `match` line. When every value no arm matches belongs to a variant that no arm names, the exact fix adds after the last arm one arm `P => todo "unhandled"` per such variant, `P` being the variant's pattern with `_` for each payload value; otherwise it adds one arm `else => todo "unhandled"`.
- In a `match` without `first`, an `else` arm when the other arms already match every value is an error (`expr.unreachable-arm`), whose exact fix deletes the arm. Outside an open enum's package, the enum has values matching none of its declared variants ([closed and open types](04-types.md#closed-and-open-types)), so an `else` arm there always matches a value.
- The checker decides disjointness and exhaustiveness exactly over the values of the type: integers and `char` as sets of intervals, `bool` and `Unit` by value, enums by variant and payload, structs field by field, text and byte strings by value, which no finite set of literals covers, and a constant pattern by its const's value. A wildcard or binding matches every value, and only a wildcard, a binding or `else` matches the values of any other type.

```nan
const SPLIT: u8 = 2 * 64

pub fn half(level: u8) -> u8 {
    return match level {
        0..=SPLIT => 0
        SPLIT..=255 => 1 // error: expr.overlapping-arms
    }
} half
```

```nan
pub fn bucket(size: u32) -> u8 {
    return match size {
        0..<512 => 0
        256..<4_096 => 1 // error: expr.overlapping-arms
        else => 2
    }
} bucket
```

```nan
pub closed enum Light {
    Red
    Amber
    Green
} Light

pub fn may_cross(light: Light) -> bool {
    return match light { // error: expr.non-exhaustive
        Light.Green => true
        Light.Amber => false
    }
} may_cross
```

### Match first

- `match first` tests its arms in source order and runs the first whose pattern matches and whose guard, if any, is true. Its arms may overlap.
- A guard `if g` is a `bool` expression evaluated after its arm's pattern matches, with the pattern's names in scope. A guard in a `match` without `first` is an error (`expr.guard-outside-first`), whose exact fix inserts `first`.
- An arm that no value reaches, because unguarded earlier arms match all its values or because it follows the `else` arm, is an error (`expr.unreachable-arm`), whose exact fix deletes the arm.
- Only unguarded arms count toward exhaustiveness.

```nan
pub closed enum Reading {
    Value(u16)
    Missing
} Reading

pub fn severity(reading: Reading) -> u8 {
    return match first reading {
        Reading.Value(level) if level >= 900 => 2
        Reading.Value(level) if level >= 500 => 1
        else => 0
    }
} severity
```

```nan
pub closed enum Reading {
    Value(u16)
    Missing
} Reading

pub fn severity(reading: Reading) -> u8 {
    return match reading {
        Reading.Value(level) if level >= 500 => 1 // error: expr.guard-outside-first
        else => 0
    }
} severity
```

## Operators

The operators are the prefix operators `-` and `~`, the binary operators of the table, and `not`. Postfix forms (member access, calls and indexing) bind tightest, then prefix operators and [markers](#markers), then the binary operators. Each binary operator combines without parentheses only as the table states.

| Operators | Kind | Combines without parentheses with |
| --- | --- | --- |
| `*` `/` `%` | multiplicative | multiplicative and additive operators, binding tighter than additive; comparisons, `and` and `or` around it |
| `+` `-` | additive | multiplicative and additive operators; comparisons, `and` and `or` around it |
| `==` `!=` `<` `<=` `>` `>=` | comparison | arithmetic operands; `and` and `or` around it |
| `and` | conjunction | further `and`; arithmetic, comparison and `not` operands |
| `or` | disjunction | further `or`; arithmetic, comparison and `not` operands |
| `&` `\|` `^` | bitwise | further uses of the same operator |
| `<<` `>>` | shift | nothing |
| `..<` `..=` | range | nothing |

```ebnf
expr = jump_expr | fail_expr | quantifier_expr | operator_expr ;
operator_expr = negation logic_tail?
    | prefix_expr operator_tail? ;
negation = "not" operand ( value_operator {!expr.parentheses} )? ;
operator_tail = relation_tail logic_tail?
    | logic_tail
    | bitwise_tail
    | shift_operator operand ( binary_operator {!expr.parentheses} )?
    | range_operator operand ( binary_operator {!expr.parentheses} )? ;
relation_tail = arithmetic_tail ( compare_tail | mixed_operator )?
    | compare_tail ;
arithmetic_tail = ( multiplicative_operator operand )+ ( additive_operator term )*
    | ( additive_operator term )+ ;
term = operand ( multiplicative_operator operand )* ;
compare_tail = compare_operator comparand ( compare_operator {!expr.chained-comparison} | mixed_operator )? ;
comparand = operand arithmetic_tail? ;
logic_tail = ( "and" logic_operand )+ ( "or" {!expr.parentheses} )?
    | ( "or" logic_operand )+ ( "and" {!expr.parentheses} )? ;
logic_operand = negation | prefix_expr ( relation_tail | mixed_operator )? ;
bitwise_tail = ( "&" operand )+ ( ( "|" | "^" | other_operator ) {!expr.parentheses} )?
    | ( "|" operand )+ ( ( "&" | "^" | other_operator ) {!expr.parentheses} )?
    | ( "^" operand )+ ( ( "&" | "|" | other_operator ) {!expr.parentheses} )? ;
mixed_operator = ( bitwise_operator | shift_operator | range_operator ) {!expr.parentheses} ;
operand = prefix_expr | "not" {!expr.parentheses} ;
prefix_expr = ( "-" | "~" ) prefix_expr | marked_expr | postfix_expr ;
value_operator = compare_operator | multiplicative_operator | additive_operator | bitwise_operator | shift_operator | range_operator ;
binary_operator = value_operator | "and" | "or" ;
other_operator = compare_operator | multiplicative_operator | additive_operator | shift_operator | range_operator | "and" | "or" ;
compare_operator = "==" | "!=" | "<" | "<=" | ">" | ">=" ;
multiplicative_operator = "*" | "/" | "%" ;
additive_operator = "+" | "-" ;
bitwise_operator = "&" | "|" | "^" ;
shift_operator = "<<" | ">>" ;
range_operator = "..<" | "..=" ;
```

- `fail_expr` is defined by [failures](11-errors-domains.md#failures). `return`, `break`, `continue` and `fail` form only a whole `expr`, never an operand, and parentheses never enclose them; [Returns](#returns) restricts where that `expr` stands.
- Multiplicative and additive operators associate to the left: `a - b - c` is `(a - b) - c`.
- A comparison takes two arithmetic operands and never chains. `a < b < c` is an error (`expr.chained-comparison`); when `b` is a binding, a parameter or a field path from one, its exact fix writes `a < b and b < c`.
- `not` applies to one prefix expression and is itself an operand only of `and` or `or`: `not a == b` and `a == not b` are errors (`expr.parentheses`).
- Every combination the table does not list is an error (`expr.parentheses`). Each of the two operators it combines gives a candidate fix parenthesising that operator with its operands, and the exact fix is offered only when exactly one candidate passes the [in-memory re-check](34-diagnostics.md#exact-fixes). Parentheses make any other expression an operand.
- Arithmetic, bitwise and comparison operators on numbers are defined in [Numbers](09-numbers.md), and on other types through the [operator interfaces](08-generics-interfaces.md#operator-interfaces), `Eq` and `Ord`. `&&`, `||`, `!`, `?` and `--` are foreign tokens whose exact fixes come from the [idiom table](34-diagnostics.md#idiom-table).

```nan
pub fn in_band(value: u32, low: u32, high: u32, strict: bool) -> bool {
    return (strict and low < value and value < high) or (not strict and low <= value and value <= high)
} in_band

pub fn low_nibble_set(flags: u8) -> bool {
    return (flags & 0x0F) != 0
} low_nibble_set
```

```nan
pub fn admits(age: u8, member: bool, guest: bool) -> bool {
    return age >= 18 and member or guest // error: expr.parentheses
} admits
```

```nan
pub fn low_nibble_set(flags: u8) -> bool {
    return flags & 0x0F != 0 // error: expr.parentheses
} low_nibble_set
```

```nan
pub fn in_order(low: u32, middle: u32, high: u32) -> bool {
    return low < middle < high // error: expr.chained-comparison
} in_order
```

### Logical operators

- `and`, `or` and `not` take `bool` operands, each with expected type `bool` ([expected types](04-types.md#expected-types-and-literals)), and yield `bool`.
- `a and b` evaluates `b` only when `a` is true, and is true when both are. `a or b` evaluates `b` only when `a` is false, and is true when either is. `not a` is true when `a` is false.
- Every [obligation](18-contracts.md#obligations) of `b` is discharged with `a` as a premise in `a and b`, and with `not a` in `a or b`, in code and in contract terms alike.

```nan
pub fn starts_with_zero(values: Slice[u8]) -> bool {
    return values.len() > 0 and values[0] == 0
} starts_with_zero

test empty_is_not_zero for starts_with_zero {
    expect not starts_with_zero(b"")
} empty_is_not_zero
```

## Ranges

A range expression `a..<b` or `a..=b` builds a `core::Range`.

```nan-api
// module: core

pub closed struct Range[T: Copy and Ord] derives Copy, Eq {
    pub low: T
    pub high: T
    pub inclusive: bool
} Range

pub fn Range.contains(self, value: T) -> bool limits pure, notrap, terminates
    ensures result == (self.low <= value and (value < self.high or (self.inclusive and value <= self.high)))

pub fn Range.is_empty(self) -> bool limits pure, notrap, terminates
    ensures result == (self.high < self.low or (not self.inclusive and self.high <= self.low))
```

- `a..<b` is `core::Range(low: a, high: b, inclusive: false)` and `a..=b` is `core::Range(low: a, high: b, inclusive: true)`.
- Both bounds have one type, an integer scalar or `char`; any other type is an error (`expr.range-type`). Their expected types are fixed in [expected types](04-types.md#expected-types-and-literals).
- A range whose `low` exceeds its `high`, or an exclusive range whose bounds are equal, is empty; neither is an error.
- [Iteration](07-ownership.md#iteration) over a range yields its values in increasing order and never steps past its bound ([ranges](09-numbers.md#ranges)). Range patterns are defined in [patterns](04-types.md#patterns).

```nan
pub fn sum_below(limit: u32) -> u64 {
    var total: u64 = 0
    for value in 0..<limit {
        total += u64.widen(value)
    }
    return total
} sum_below

pub fn is_digit_code(code: u32) -> bool {
    let digits = 48u32..=57
    return digits.contains(code)
} is_digit_code

pub fn sum_window(start: u32, width: u32) -> u64 {
    var total: u64 = 0
    for value in start..<(start + width) {
        total += u64.widen(value)
    }
    return total
} sum_window
```

```nan
pub fn in_unit(ratio: f64) -> bool {
    let unit = 0.0f64..<1.0 // error: expr.range-type
    return unit.contains(ratio)
} in_unit
```

## Calls

A call applies a function to arguments. An **argument** is an expression supplied for a parameter. The call forms are:

- `f(...)`, where the path `f` names a function item, or `f` is a binding or parameter of [function type](10-effects-digests.md#function-types).
- `T.f(...)`, where the member path `T.f` names an associated function, a method or an interface method ([method resolution](08-generics-interfaces.md#method-resolution)); a method called this way takes its `self` argument as the first parenthesised argument.
- `x.f(...)`, a **method call**, where `x` is a value: the **receiver**, the call's first argument, passed for the method's `self` parameter.
- `(e)(...)`, which calls the function value of the expression `e`.

A path or member path naming a struct or a variant followed by an argument list is a [construction](04-types.md#construction) instead.

```ebnf
arg_list = arg ( "," arg_list? )? ;
arg = expr ( ":" expr )? ;
```

### Arguments and labels

- The arguments correspond to the parameters in order: the receiver to `self`, then the parenthesised arguments to the remaining parameters. A call with more or fewer arguments than parameters is an error (`expr.argument-count`), and its labels are then not checked.
- An **argument label** is the parameter's name written before an argument and its markers: `label: e`, as in `buf: mut reply`. The argument for `self` carries no label. The argument for the first parameter other than `self` is the **leading argument** and may carry its label. Every other argument carries its label unless punned.
- **Punning** omits the label of an argument after the leading one that is a binding or parameter, alone or after a mode, spelt like its parameter: `buf.copy_prefix(count, alloc)`. `nan fmt` writes `name: name` in the punned form.
- A label that is not a single identifier, a label other than its parameter's name, a label on the argument for `self`, and a missing label on an argument after the leading one that is not punned are errors (`expr.argument-label`), reported once per call, at the first such argument. When each argument after the leading one is labelled or is a binding or parameter alone or after a mode, and these labels and spellings name each parameter after the leading one exactly once, the exact fix reorders those arguments into parameter order. Otherwise the exact fix deletes the label of the first such argument when it is the argument for `self`, and writes its parameter's name as its label in every other case.
- A call through a function value takes its labels from the parameter names of its function type.

```nan
pub fn clamp_to(value: u32, low: u32, high: u32) -> u32 {
    if value < low {
        return low
    }
    if value > high {
        return high
    }
    return value
} clamp_to

pub fn clamp_reading(reading: u32, limit: u32) -> u32 {
    let low: u32 = 10
    return clamp_to(reading, low, high: limit)
} clamp_reading
```

```nan
pub fn clamp_to(value: u32, low: u32, high: u32) -> u32 {
    if value < low {
        return low
    }
    if value > high {
        return high
    }
    return value
} clamp_to

pub fn clamp_reading(reading: u32, limit: u32) -> u32 {
    let low: u32 = 10
    return clamp_to(reading, high: limit, low) // error: expr.argument-label
} clamp_reading
```

```nan
pub closed struct Gauge {
    pub level: u32
} Gauge

pub fn Gauge.read(self) -> u32 {
    return self.level
} Gauge.read

pub fn level_of(gauge: Gauge) -> u32 {
    return Gauge.read(value: gauge) // error: expr.argument-label
} level_of
```

### Markers

A **marker** is a word written at a call site: `try` or `attempt` ([failures](11-errors-domains.md#failures)), `await` ([waiting](10-effects-digests.md#waiting)), and the [modes](07-ownership.md#modes) `mut` and `take`.

```ebnf
call_expr = marked_expr | postfix_expr ;
marked_expr = failure_marker after_failure_marker
    | "await" after_await
    | mode after_mode ;
failure_marker = "try" | "attempt" ;
after_failure_marker = "await" after_await | mode after_mode | postfix_expr | failure_marker {!expr.marker-order} ;
after_await = when_expr | mode after_mode | postfix_expr | ( failure_marker | "await" ) {!expr.marker-order} ;
after_mode = postfix_expr | marker {!expr.marker-order} ;
marker = failure_marker | "await" | mode ;
```

- Markers come in the order `try` or `attempt`, then `await`, then a mode, then the expression they mark, each at most once. Any other order or repetition is an error (`expr.marker-order`), whose exact fix writes the canonical order.
- `try`, `attempt` and `await` apply to the call that ends the postfix expression after the markers: `try await mut conn.read(mut buf.view_mut())` applies all three to `conn.read(...)`. `await` also precedes `when` ([cowns](16-concurrency.md#cowns)). Before a postfix expression that is not a call, a construction included, they are an error (`expr.marker-target`).
- Before a method call, a mode marks the receiver: `mut out.push(value)` passes `out` by `mut`. When the receiver is itself a call of a [projection](07-ownership.md#projections), the mode marks that call's receiver in turn.
- Before any other expression forming a whole argument, a whole construction value or the iterated operand of `for`, a mode marks that expression.
- A mode in any other position, such as before an initialiser or a returned value, is an error (`expr.marker-target`). [Marking](07-ownership.md#marking) states which arguments and receivers are marked and requires each marked one to be a place.

```nan
pub closed enum PingError {
    Net(sys::net::NetError) from
    Cancelled(task::Cancelled) from
} PingError

pub fn has_data(mut conn: sys::net::Conn, alloc: mem::Alloc) -> bool fails PingError uses wait {
    var buf = Untrusted.of(mem::Bytes.zeroed(64, alloc))
    let count = try await mut conn.read(mut buf.view_mut())
    return count > 0
} has_data
```

```nan
pub closed enum PingError {
    Net(sys::net::NetError) from
    Cancelled(task::Cancelled) from
} PingError

pub fn has_data(mut conn: sys::net::Conn, alloc: mem::Alloc) -> bool fails PingError uses wait {
    var buf = Untrusted.of(mem::Bytes.zeroed(64, alloc))
    let count = await try mut conn.read(mut buf.view_mut()) // error: expr.marker-order
    return count > 0
} has_data
```

```nan
pub fn keep(take items: mem::List[u8]) -> mem::List[u8] {
    let kept = take items // error: expr.marker-target
    return kept
} keep
```

## Field access and method calls

```ebnf
postfix_expr = primary_expr postfix_suffix* ;
postfix_suffix = "." IDENT | "(" arg_list? ")" | "[" expr "]" ;
primary_expr = literal | path_expr | "self" | "(" ( operator_expr | quantifier_expr ) ")" | if_expr | match_expr
    | closure_expr | trusted_expr | todo_expr | dbg_expr | quantifier_expr {!expr.parentheses} ;
path_expr = path generic_args? ;
literal = INT | FLOAT | TEXT | BYTES | CHAR | BYTE | TEXT_LINE+ | "true" | "false" ;
```

- `x.name` reaches a [member](03-packages-modules.md#paths). When `x` is a value, `name` is a field, read as a value or used as a place. When `x` names a type, `T.name` is an associated function, a method as a function value, or a variant; when it names an interface, `I.name` is a method of that interface.
- `x.name(...)` with `x` a value is a method call, found by [method resolution](08-generics-interfaces.md#method-resolution). A field of function type is called as `(x.name)(...)`; `x.name(...)` where `name` is a field and method resolution finds no method of that name is an error (`expr.field-call`), whose exact fix adds the parentheses.
- A method of a value written without an argument list is an error (`expr.method-value`); `T.name` names the method as a [function value](07-ownership.md#function-values).
- `x[i]` indexes `x` through its projections `at` and `at_mut` ([indexing](07-ownership.md#indexing)).
- After a path whose last identifier starts with an uppercase letter and is one letter long or holds a lowercase letter, which is the PascalCase shape of [naming conventions](03-packages-modules.md#naming-conventions), `[` opens `generic_args`: `Pair[u8].same(0)`. Everywhere else `[` opens an index. Type arguments are written only on types; those of a generic function are [inferred](08-generics-interfaces.md#inference).
- `closure_expr` is defined by [closures](07-ownership.md#closures), `trusted_expr` by [trusted code](23-low-level.md#trusted-code), `todo_expr` by [todo](18-contracts.md#todo), `dbg_expr` by [debug output](22-testing.md#debug-output), `quantifier_expr` by [quantifiers](18-contracts.md#quantifiers) and `when_expr` by [cowns](16-concurrency.md#cowns). The words `all` and `any` start a `quantifier_expr` when an identifier follows them, and a path otherwise. A quantifier is a whole `expr` or stands between parentheses; as an operand it is an error (`expr.parentheses`). Literal types are fixed in [expected types](04-types.md#expected-types-and-literals).
- A text run ends its logical line ([text literals](01-lexical.md#text-literals)), so it is the last token of the statement, clause, member line or single-line item holding it; where the grammar requires a token after it, the `NL` that follows the run is a [syntax failure](34-diagnostics.md#syntax-failures).

```nan
pub closed struct Pair[T: Copy] derives Copy {
    pub left: T
    pub right: T
} Pair

pub fn Pair.same(value: T) -> Pair[T] = Pair(left: value, right: value)

pub fn zero_pair() -> Pair[u8] {
    return Pair[u8].same(0)
} zero_pair

pub fn apply_twice(value: u32, step: fn(x: u32) -> u32) -> u32 {
    return step(step(value))
} apply_twice

pub fn double(x: u32) -> u32 {
    return x.saturating_mul(2)
} double

pub fn quadruple(value: u32) -> u32 {
    return apply_twice(value, step: double)
} quadruple

pub closed struct Handler {
    pub run: fn(arg: u32) -> u32
} Handler

pub fn Handler.apply(self, arg: u32) -> u32 {
    return (self.run)(arg)
} Handler.apply
```

```nan
pub closed struct Handler {
    pub run: fn(arg: u32) -> u32
} Handler

pub fn Handler.apply(self, arg: u32) -> u32 {
    return self.run(arg) // error: expr.field-call
} Handler.apply
```

## Evaluation order

The **evaluation order** is normative, and the prover and the interpreter follow it:

1. The statements of a block run in source order.
2. The operands of an expression are evaluated left to right, each completely, before its operator applies: both operands of a binary operator, the object then the index of `x[i]`, the object of a field access, the bounds of a range, and the values of a construction in the order written.
3. In `a and b` and `a or b`, `b` is evaluated only as [Logical operators](#logical-operators) states.
4. A call evaluates its callee or receiver, then its arguments left to right, then runs the function. `await` suspends the task while the call runs; `try` and `attempt` act on the call's outcome once it completes.
5. An assignment evaluates the operands of its place (the object and index of each index step, the object of a field path), then its right side, then reaches the place through `at_mut` when it is indexed; a compound assignment then reads the place and applies its operator; the value is then stored.
6. `if` evaluates its condition, then one branch. `match` evaluates its scrutinee, then tests its arms; `match first` tests them in source order, evaluating each guard after its pattern matches. Matching runs no user code, so the testing order of other matches is not observable.
7. `let P = e else { B }` evaluates `e`, matches `P`, then binds or runs `B`.
8. A closure expression yields its value without running its body.
9. At an exit, the pending [deferred calls](17-suspension-io.md#defer) of every block being left run, innermost block first, the calls of each block in reverse order of registration ([registration and exits](17-suspension-io.md#registration-and-exits)).
10. `fail e` and `return e` evaluate `e`, then end the body. A failure propagated by `try` ends evaluation at the call that failed, and a [trap](11-errors-domains.md#traps) ends it at once.

```nan
pub fn fold_first(mut values: SliceMut[u32]) {
    if values.len() >= 2 {
        values[0] += values[1]
    }
} fold_first
```

## Thresholds

A **threshold** limits the nesting depth (4) or the length (80 lines) of a function's block body, the default body of an interface method included. Excess is incompleteness unless a waiver accepts it. Test bodies have no thresholds.

### Depth

- A **nesting root** is a function's block body, or the block body of a closure passed as the last argument of a call.
- A **level** is a block, a loop block or the braced body of [trusted code](23-low-level.md#trusted-code), other than a nesting root, or the expression body of a match arm other than a one-call arm. A **one-call arm** is an arm whose body is one call, with its markers.
- The depth of a level is 1 plus the number of levels enclosing it within its nesting root. A level of depth 5 or more is incompleteness (`threshold.depth`), reported once per item, on the line where the first such level in source order begins.

```nan
pub fn has_repeat(values: Slice[u8], skip: u8) -> bool
    waive depth "a pairwise scan reads best as one loop nest"
{
    for left in 0..<values.len() {
        if values[left] != skip {
            for right in 0..<values.len() {
                if left != right {
                    if values[left] == values[right] {
                        return true
                    }
                }
            }
        }
    }
    return false
} has_repeat
```

```nan
pub fn has_repeat(values: Slice[u8], skip: u8) -> bool {
    for left in 0..<values.len() {
        if values[left] != skip {
            for right in 0..<values.len() {
                if left != right {
                    if values[left] == values[right] { // incomplete: threshold.depth
                        return true
                    }
                }
            }
        }
    }
    return false
} has_repeat
```

### Length

A body's lines are the physical lines strictly between the line holding its opening `{` and the line holding its closing `}`, blank and comment lines included; the lines of closures within it count. A body of more than 80 lines is incompleteness (`threshold.length`), reported on its closing line.

The body of `banner_size` has 81 lines: the `let` line, a text run of 79 lines and the `return` line.

```nan
pub fn banner_size() -> usize {
    let banner: Str =
        \\-
        \\-
        \\-
        \\-
        \\-
        \\-
        \\-
        \\-
        \\-
        \\-
        \\-
        \\-
        \\-
        \\-
        \\-
        \\-
        \\-
        \\-
        \\-
        \\-
        \\-
        \\-
        \\-
        \\-
        \\-
        \\-
        \\-
        \\-
        \\-
        \\-
        \\-
        \\-
        \\-
        \\-
        \\-
        \\-
        \\-
        \\-
        \\-
        \\-
        \\-
        \\-
        \\-
        \\-
        \\-
        \\-
        \\-
        \\-
        \\-
        \\-
        \\-
        \\-
        \\-
        \\-
        \\-
        \\-
        \\-
        \\-
        \\-
        \\-
        \\-
        \\-
        \\-
        \\-
        \\-
        \\-
        \\-
        \\-
        \\-
        \\-
        \\-
        \\-
        \\-
        \\-
        \\-
        \\-
        \\-
        \\-
        \\-
    return banner.byte_len()
} banner_size // incomplete: threshold.length
```

### Waivers

A **waiver** is a clause in the [clause region](02-items.md#clause-region) of a function or interface method accepting its excess of one threshold.

```ebnf
waive_clause = "waive" ( 'depth' | 'length' ) TEXT NL ;
```

- `waive depth "why"` accepts every `threshold.depth` excess of its body, and `waive length "why"` its `threshold.length` excess. The text states the reason; a reason that is empty or holds only spaces is an error (`threshold.empty-reason`).
- A waiver for a threshold the body does not exceed, or a second waiver of one kind, is an error (`threshold.unused-waiver`), whose exact fix deletes it.
- The [ledger](31-trust.md#ledger) lists every waiver.

```nan
pub fn double(value: u32) -> u32
    waive length "kept whole for review" // error: threshold.unused-waiver
{
    return value * 2
} double
```

## Diagnostics

| Code | Class | Rule |
| --- | --- | --- |
| `expr.argument-count` | error | [Arguments and labels](#arguments-and-labels) |
| `expr.argument-label` | error | [Arguments and labels](#arguments-and-labels), [Templates](13-text-encode.md#templates) |
| `expr.branch-form` | error | [Value-yielding if and match](#value-yielding-if-and-match) |
| `expr.chained-comparison` | error | [Operators](#operators) |
| `expr.field-call` | error | [Field access and method calls](#field-access-and-method-calls) |
| `expr.float-pattern` | error | [Match arms](#match-arms), [Patterns](04-types.md#patterns) |
| `expr.guard-outside-first` | error | [Match first](#match-first) |
| `expr.marker-order` | error | [Markers](#markers) |
| `expr.marker-target` | error | [Markers](#markers) |
| `expr.method-value` | error | [Field access and method calls](#field-access-and-method-calls) |
| `expr.misplaced-jump` | error | [Returns](#returns) |
| `expr.missing-else` | error | [Value-yielding if and match](#value-yielding-if-and-match) |
| `expr.non-exhaustive` | error | [Disjointness and exhaustiveness](#disjointness-and-exhaustiveness) |
| `expr.or-bindings` | error | [Match arms](#match-arms) |
| `expr.overlapping-arms` | error | [Disjointness and exhaustiveness](#disjointness-and-exhaustiveness), [Match arms](#match-arms) |
| `expr.parentheses` | error | [Operators](#operators), [Field access and method calls](#field-access-and-method-calls), [Quantifiers](18-contracts.md#quantifiers) |
| `expr.range-type` | error | [Ranges](#ranges) |
| `expr.unreachable-arm` | error | [Disjointness and exhaustiveness](#disjointness-and-exhaustiveness), [Match first](#match-first) |
| `stmt.assign-target` | error | [Assignment](#assignment) |
| `stmt.discard-unit` | error | [Expression statements](#expression-statements) |
| `stmt.else-not-diverging` | error | [Let else](#let-else) |
| `stmt.irrefutable-else` | error | [Let else](#let-else) |
| `stmt.missing-return` | error | [Returns](#returns) |
| `stmt.no-loop` | error | [Conditionals and loops](#conditionals-and-loops) |
| `stmt.return-value` | error | [Returns](#returns) |
| `stmt.shadowing` | error | [Bindings](#bindings) |
| `stmt.unknown-label` | error | [Conditionals and loops](#conditionals-and-loops) |
| `stmt.unreachable` | error | [Divergence](#divergence) |
| `stmt.unused-label` | error | [Conditionals and loops](#conditionals-and-loops) |
| `stmt.unused-result` | error | [Expression statements](#expression-statements) |
| `stmt.wildcard-binding` | error | [Bindings](#bindings) |
| `threshold.depth` | incompleteness | [Depth](#depth) |
| `threshold.empty-reason` | error | [Waivers](#waivers) |
| `threshold.length` | incompleteness | [Length](#length) |
| `threshold.unused-waiver` | error | [Waivers](#waivers) |
