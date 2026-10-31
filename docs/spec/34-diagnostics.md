# Diagnostics

This chapter defines the diagnostic record and its because-chain, the diagnostic classes with internal errors, diagnostic codes with their families, root-cause reporting, exact fixes and their in-memory re-check, the syntax failures of the `syntax` family, the idiom table, and the catalogue of every code.

## Diagnostic record

A **diagnostic** is the checker's report of one root cause ([root-cause reporting](#root-cause-reporting)). Its record holds:

- the code ([codes](#codes)), which fixes its [class](#classes);
- the location: the reported line, written as [commands](33-toolchain.md#commands) write locations; a diagnostic on a file or directory as a whole, or on one that is missing, has its path alone, written as its owning rule states; a diagnostic on no file has no location;
- the item: the item holding the reported line, written as [commands](33-toolchain.md#commands) write `<item>`, absent when the location has no line or no item with a canonical path holds the line;
- the targets: the declared [targets](27-program-model.md#targets) under which the diagnostic arises, present only when they are some but not all of its package's targets;
- the quote: the reported line exactly as written, without its line end, absent when the location has no line;
- expected and found;
- the because-chain;
- the fixes: the [exact fixes](#exact-fixes) offered, in the order the owning rule gives them.

The **reported construct** is the token, expression, pattern, type, clause, head, item, directive or file whose text breaks the rule. A diagnostic is reported on the line its rule names, and otherwise on the line of the first token of the reported construct.

Expected and found are each one line of text: what the rule requires at the reported construct and what the source holds there. They are present when the owning rule states them. When it states neither and the diagnostic offers a fix that changes the text of the reported construct, found is that text as written and expected is that text after the first offered fix, absent when empty. Otherwise both are absent. A text spanning several lines is written as its lines without comments and indentation, joined by single spaces.

### Because-chain

The **because-chain** is the ordered list of reasons that lead from the root cause to the reported line. Each reason holds a role from this table, a subject, and, when it lies in source, a location, an item and the quote of its line (the item written as for the record).

| Role | The reason |
| --- | --- |
| `declared` | Its line declares the subject, a head, type, clause, directive or name the reported construct is checked against. |
| `bound` | Its line binds or creates the subject: a binding, view, capture, parameter or temporary. |
| `frozen` | Its line starts a [hold](07-ownership.md#holds) freezing the subject. |
| `held` | Its line starts a hold holding the subject. |
| `moved` | Its line moves the subject. |
| `used` | Its line uses the subject, keeping a hold or a value live. |
| `accessed` | Its line accesses the subject, which is written with its mode (`mut inbox.frames`). |
| `called` | Its line calls the subject, through which a fact reaches the reported line. |
| `raised` | Its line is the site of a fact: a trap site, an allocation, a wait, an authority use or a failure; the subject is the operation. |
| `diagnostic` | Its line is the reported line of another diagnostic, whose code is the subject. |
| `command` | It has no line; the subject is the command that makes the change the rule requires. |
| `note` | It has no line; the subject is a value the owning rule names, such as a path, hash, release, advisory number or prover outcome. |

- The subject is NaN text (a name, place, path, type or operation), a code, a command line or a value, as its role states.
- The owning rule of a code states the reasons of its diagnostics; a rule stating none gives an empty chain.
- Reasons appear in the order the owning rule gives, and otherwise in order of path, then line, reasons without a line last.

### Record layout

A diagnostic prints as a [record](33-toolchain.md#records) whose lines are, in order:

1. the header: the class word (`error`, `incomplete` or `internal`), the code, the location and the item, separated by single spaces, `-` standing for an absent location or item;
2. the quote;
3. `targets: ` and the targets, separated by `, `;
4. the line `expected: ` and the expected text, then the line `found: ` and the found text;
5. for each reason, `because`, its role, its location, its item and its subject, separated by single spaces, `-` standing for an absent location or item, followed by the quote of its line;
6. for each edit of each fix, `fix`, the fix's number counting the offered fixes from 1, and the edit's path, separated by single spaces, followed by each line of the edit's old text prefixed `- ` and each line of its new text prefixed `+ `, an empty line printing as `-` or `+` alone;
7. for an internal error, `reproducer ` and the reproducer's path.

Absent parts print nothing. A quote is the single line after the header or `because` line it belongs to; every other continuation line starts with `targets: `, `expected: `, `found: `, `because `, `fix `, `reproducer `, `-` or `+`. A quote or edit line copied from source prints in an [envelope](33-toolchain.md#envelopes) where that section requires one.

```nan
pub closed linear struct Lease {
    pub slot: u32
} Lease

pub fn lease_slot(slot: u32) -> u32 {
    let lease = Lease(slot: slot)
    return slot // error: own.linear-dropped
} lease_slot
```

[Linear consumption](07-ownership.md#linear-consumption) gives `own.linear-dropped` one `bound` reason, the line binding the dropped value:

```text
$ nan check example
failed check example: errors 1, incomplete 0
error own.linear-dropped example/example.nan:8 example::lease_slot
        return slot // error: own.linear-dropped
    because bound example/example.nan:7 example::lease_slot lease
        let lease = Lease(slot: slot)
failed check example: errors 1, incomplete 0
```

## Classes

A **diagnostic class** is one of error, incompleteness and internal error. Each code has exactly one class, which its owning chapter's Diagnostics table states.

- An **error** is the class of a diagnostic that rejects the source: a command reporting one does not succeed ([exit classes](33-toolchain.md#exit-classes)).
- [Incompleteness](31-trust.md#incompleteness) is the class that `nan check` and `nan test` accept and the [release gate](31-trust.md#release-gate) rejects.
- An internal error is the class of a diagnostic reporting a fault of the implementation ([internal errors](#internal-errors)).

No option, setting or comment changes a diagnostic's class or suppresses it; only constructs that a rule names, such as [waivers](06-statements-expressions.md#waivers), accept what a diagnostic would otherwise report.

```nan
pub fn checksum(data: Slice[u8]) -> u8 {
    return todo "sum the bytes modulo 256" // incomplete: todo.hole
} checksum
```

```text
$ nan check example
ok check example: errors 0, incomplete 1
incomplete todo.hole example/example.nan:3 example::checksum
        return todo "sum the bytes modulo 256" // incomplete: todo.hole
ok check example: errors 0, incomplete 1
```

### Internal errors

An **internal error** is a diagnostic reporting that the implementation failed, as [Toolchain](33-toolchain.md#internal-errors) defines; it is no verdict on the source, which the command neither accepts nor rejects.

- An implementation finding that one of its own invariants does not hold, including elaboration producing NIR that is not [well formed](19-nir.md#well-formedness) and a trap in its own code, reports `internal.invariant`.
- An implementation that cannot obtain the memory or stack a command needs reports `internal.resource-exhausted`.
- The location and item of an internal error are those of the line being checked when the fault occurred, or absent.
- A conforming implementation reports no internal error on a conformance test.

## Codes

A **diagnostic code** is the stable name `family.condition` of the rule a diagnostic reports. Its **family** is the segment before the dot, of lowercase ASCII letters. The condition is one or more words of lowercase ASCII letters and digits joined by single hyphens, naming the defect as found, never its fix.

- Each family has exactly one owning chapter, which defines each of its codes with its class and meaning and lists them in its Diagnostics table. A rule of another chapter may report a code only for a case of the meaning its owner fixes. The owner's table links, for each code, every section in any chapter that states a rule reporting it, its own sections first, then the others in chapter order.
- A [language version](37-evolution.md#contents-of-a-version) fixes its codes, each code's class and the rules reporting it, and a package is checked with those of the language version its manifest names. A code keeps its rule in every language version holding it and never names another rule.
- [Trap kinds](11-errors-domains.md#traps) are named like codes, in the family `trap`; they appear only in trap reports and are never diagnostics.
- The [test-run codes](22-testing.md#test-runs) appear only as the cause of a failing test's record ([checking commands](33-toolchain.md#checking-commands)) and are never diagnostics.

| Chapter | Families |
| --- | --- |
| [Lexical structure](01-lexical.md) | `lex` |
| [Items](02-items.md) | `item` |
| [Packages, modules and names](03-packages-modules.md) | `manifest`, `module`, `name` |
| [Data types](04-types.md) | `pattern`, `predicate`, `type` |
| [Structural properties](05-properties.md) | `prop` |
| [Statements and expressions](06-statements-expressions.md) | `expr`, `stmt`, `threshold` |
| [Ownership and access](07-ownership.md) | `closure`, `mode`, `own` |
| [Generics and interfaces](08-generics-interfaces.md) | `derive`, `generic`, `interface` |
| [Numbers](09-numbers.md) | `num` |
| [Effects, digests and limits](10-effects-digests.md) | `effect`, `limit` |
| [Errors, traps and domains](11-errors-domains.md) | `error`, `trap` |
| [Memory and allocation](12-memory.md) | `mem` |
| [Text, rendering and Encode](13-text-encode.md) | `encode`, `text` |
| [Capabilities and authority](14-authority.md) | `authority` |
| [Untrusted and secret data](15-untrusted.md) | `secret`, `untrusted` |
| [Structured concurrency](16-concurrency.md) | `task` |
| [Suspension, I/O and cleanup](17-suspension-io.md) | `defer`, `suspend` |
| [Contracts](18-contracts.md) | `contract`, `example`, `todo` |
| [NIR](19-nir.md) | `nir` |
| [The normative prover](20-prover.md) | `proof` |
| [The reference interpreter](21-interpreter.md) | `const` |
| [Testing and evidence](22-testing.md) | `test` |
| [Low-level programming](23-low-level.md) | `hw`, `layout`, `register`, `trusted` |
| [Kernel execution model](24-kernel.md) | `kernel` |
| [Registries](25-registries.md) | `registry` |
| [Components](26-components.md) | `component` |
| [Targets and program model](27-program-model.md) | `target` |
| [Identity and versioning](29-identity-versioning.md) | `dep`, `retired`, `version` |
| [Artefacts](30-artefacts.md) | `artefact` |
| [Trust and the release gate](31-trust.md) | `api`, `key`, `seal`, `trust`, `unused` |
| [Documentation](32-docs.md) | `doc` |
| [Toolchain](33-toolchain.md) | `edit`, `merge`, `tool` |
| [Diagnostics](34-diagnostics.md) | `internal`, `syntax` |
| [Canonical format and machine files](35-format.md) | `format` |
| [Builds](36-builds.md) | `build` |
| [Evolution and governance](37-evolution.md) | `language` |

## Root-cause reporting

The checker reports each defect once, at its root cause, and never a diagnostic that only follows from another.

1. The reported construct of an error is **failed**. A check depending on a failed construct reports nothing: a failed expression or pattern satisfies every type, property, mode and limit its position requires; every use of a failed name, head, type or item is accepted; and every fact computed from a failed construct, such as a type, a structural property or a digest, satisfies every rule checking it. A fact that a construct lacks something is computed from every construct it holds: a body holding a failed construct may fail and may wait, a failed `break` or `continue` names every enclosing loop, and a failed `break` leaves each; a function reaching such a body may belong to a cycle; a failed type mentions every generic parameter; and an item holding a failed construct, or a module holding an item abandoned at a syntax failure, may name every item, read every field and perform every authority. A failed `targets` directive declares unknown targets: no rule judging the targets a package declares reports it, in its package or in a package depending on it, and no field, dependency or grant of its package is unused.
2. When several rules report one construct, only the first in this order reports it: the lexical rules; the rules reporting [syntax failures](02-items.md#resynchronisation), whichever chapter owns their codes; the other rules of each chapter, in chapter order; and the rules applying only to a [final build](27-program-model.md#final-builds). Among the rules of one chapter, the rule stated first reports. A rule that states it is reported in place of, or before, the rule of another chapter takes that position: [`text.integer-index` and `text.bare-len`](13-text-encode.md#lengths-and-slicing), [`secret.public-use`](15-untrusted.md#secret-values), the checks of [deferred calls](17-suspension-io.md#defer), and [`merge.unresolved`](33-toolchain.md#conflicts), which replaces every other diagnostic, lexical ones included, on the lines of a conflict block.
3. A rule that a declaration breaks is reported at the declaration, never at its uses.
4. A line holding a lexical diagnostic, and an item abandoned at a syntax failure, report only what [line-local lexing](01-lexical.md#line-local-lexing) and resynchronisation allow.
5. Errors never suppress incompleteness: every construct that rule 1 does not exempt is checked for it.
6. Diagnostics arising under several targets with the same code, location, item and parts other than the targets are one diagnostic, whose targets part lists those targets when they are not all the package's targets.

In the next example the unknown type `u46` is the root cause; the assignment that adds `amount` to a `u64` field reports nothing, since `amount` has a failed type.

```nan
pub closed struct Meter {
    pub total: u64
} Meter

pub fn Meter.add(mut self, amount: u46) { // error: name.unknown
    self.total += amount
} Meter.add
```

In the next example the misspelt call is the root cause; the `fails` clause of `first_digit` is not unneeded, since a body holding the failed call may fail.

```nan
pub closed enum ParseError derives Render {
    Empty
} ParseError

pub fn parse_digit(code: u8) -> u8 fails ParseError {
    if code == 0 {
        fail ParseError.Empty
    }
    return code
} parse_digit

pub fn first_digit(code: u8) -> u8 fails ParseError {
    return try parse_digt(code) // error: name.unknown
} first_digit
```

## Exact fixes

An **exact fix** is a rewrite of source text that removes the root cause of a diagnostic, verified before it is offered. It replaces one or more **changed texts**, each a run of characters of one source file, empty for an insertion, with a **replacement** written in [canonical format](35-format.md#canonical-format). It is shown as one or more edits, each holding a path, an old text and a new text.

- The owning rule of a code names the fixes of its diagnostics. Each named fix that passes the in-memory re-check is offered. A rule may instead name alternative rewrites in order, as the [idiom table](#idiom-table) does; the first that passes the re-check is the fix, and the others are not offered.
- The edits of one fix lie in source files inside the workspace. A fix never creates, deletes or renames a file.
- An edit's old text is a run of whole physical lines of one file, each with its line end. The runs of a file start as the maximal runs of consecutive lines holding part of a changed text, an empty changed text being held by the line holding the character after it, or by the last line at the end of the file. A run is extended by the line before it, or, at the file's first line, by the line after it, until its text occurs exactly once in the file, counting overlapping occurrences; runs sharing a line merge.
- An edit's new text is its lines after the fix, each with its line end; an empty new text deletes the lines.

The **in-memory re-check** applies a fix to a copy of the workspace in memory and checks the copy over the same scope with the same rules as the command reporting the diagnostic. A position outside the changed texts corresponds to the position of the same character after the fix. A construct overlaps a replacement when it starts before the replacement's end and ends after its start. The fix passes the re-check when:

1. no diagnostic of the fixed diagnostic's code has a reported construct overlapping a replacement;
2. every error the re-check reports has a reported construct that either starts at the position corresponding to the start of the reported construct of an error of the same code in the unfixed workspace, or overlaps no replacement and went unchecked in the unfixed workspace because of the fixed diagnostic: it follows the fixed diagnostic's failing token in the item that failure made the parser abandon, or its check depended on the construct the fixed diagnostic failed ([root-cause reporting](#root-cause-reporting)).

A diagnostic whose fixes all fail the re-check is reported without fixes. The fixes offered depend only on the workspace, the scope and the command; `nan fix` applies them as [checking commands](33-toolchain.md#checking-commands) states.

```nan
pub fn leading(values: Slice[u32]) -> Option[u32] {
    if values.len() == 0 {
        return None
    }
    return Some(values[0])
} leading

pub fn leading_doubled(values: Slice[u32]) -> Option[u32] {
    let value = leading(values)? // error: syntax.foreign
    return Some(value.saturating_mul(2))
} leading_doubled
```

The rewrite `try leading(values)` fails the re-check, since `leading` cannot fail (`error.marker-unneeded`), so the diagnostic has no fix:

```text
$ nan check example
failed check example: errors 1, incomplete 0
error syntax.foreign example/example.nan:10 example::leading_doubled
        let value = leading(values)? // error: syntax.foreign
    expected: try leading(values)
    found: leading(values)?
failed check example: errors 1, incomplete 0
```

## Syntax failures

A [syntax failure](02-items.md#resynchronisation) that no other rule names is reported by the first of these rules that applies, on the line of its failing token; an `NL` lies on the last physical line of its logical line, a `COL0` on the line of the token it precedes, and `EOF` on the last line of the file.

- A failure that an entry of the [idiom table](#idiom-table) matches, and any other failure at a `FOREIGN` token ([punctuation and operators](01-lexical.md#punctuation-and-operators)), are errors (`syntax.foreign`).
- A failure at `NL` or `EOF` while a `(` or `[` opened on the same logical line is unclosed is an error (`syntax.unclosed-delimiter`). Expected is the missing closers, innermost first, separated by single spaces, and the exact fix inserts them in that order, without spaces, after the last token of the logical line.
- A `)`, `]` or `}` that is not at column 0 and whose innermost unclosed delimiter on its logical line is a `(` or `[` it does not close is an error (`syntax.mismatched-delimiter`). Expected is the closer of that delimiter, and the exact fix replaces the failing token with it.
- Any other failure is an error (`syntax.unexpected-token`), without a fix. Expected is every token that the grammar, its rejected alternatives aside, accepts at the failure, separated by `, ` in byte order of their written forms.

In these diagnostics found is the failing token, and expected and found are written in [grammar notation](00-charter.md#grammar-notation): a hard keyword, `PUNCT` or `FOREIGN` token as its double-quoted spelling, a context word accepted by its spelling as its single-quoted spelling, and any other token by its name, such as `IDENT`, `INT`, `NL`, `COL0` or `EOF`; the idiom table states expected and found for the failures its entries match.

```nan
pub fn larger(left: u32, right: u32) -> u32 {
    if left > right {
        return left
    }
    return right
} larger

pub fn total(values: Slice[u32]) -> u64 {
    var sum: u64 = 0
    for value in values {
        sum += u64.widen(value)
    }
    return sum
} total
```

```nan
pub fn larger(left: u32 right: u32) -> u32 { // error: syntax.unexpected-token
    if left > right {
        return left
    }
    return right
} larger
```

```nan
pub fn total(values: Slice[u32]) -> u64 {
    var sum: u64 = 0
    for value in values {
        sum += u64.widen(value // error: syntax.unclosed-delimiter
    }
    return sum
} total
```

```nan
pub fn total(values: Slice[u32]) -> u64 {
    var sum: u64 = 0
    for value in values {
        sum += u64.widen(value] // error: syntax.mismatched-delimiter
    }
    return sum
} total
```

## Idiom table

The **idiom table** lists foreign spellings, each with the position of its foreign anchor and its rewrites. At a syntax failure that no other rule names, its entries are tried in table order, and the first that matches makes the failure `syntax.foreign`. An entry matches when its foreign form matches the tokens around the failure with its foreign anchor at the failing token, and that anchor stands where the entry's position states.

- The **foreign anchor** of a foreign form is its first token that the grammar does not accept at its position.
- In foreign forms and rewrites, `a`, `b` and `e` stand for prefix expressions ([operators](06-statements-expressions.md#operators)), or for expressions where they stand between the parentheses of a form; `c` for a postfix expression ending in an argument list, with the [markers](06-statements-expressions.md#markers) written before it; `p` for a place; `T` for a type; `x` for an identifier; and `t` for a text literal. A placeholder that starts a form stands for the longest text of its kind ending where the form's next token starts, and any other placeholder for the longest text of its kind starting where it stands.
- The **matched text** runs from the first token of the form to its last; a rewrite replaces exactly the matched text, unless the table states another extent.
- Rewrites separated by "else" are alternatives in order: the fix is the first that passes the [in-memory re-check](#exact-fixes), and none is offered when none passes. A marker that a rewrite adds to `c` is written where the marker order places it.
- Found is the matched text. Expected is the alternative that is the fix, or the first when none is; the entries of `;` and `-> ()`, whose rewrites the table states in words, have no expected part.
- An entry whose rewrite is "none", and a `FOREIGN` token that no entry matches, give expected and found as `syntax.unexpected-token` does, and no fix.

| Foreign form | Position | Rewrite |
| --- | --- | --- |
| `a && b` | after an operand | `a and b` |
| `a \|\| b` | after an operand | `a or b` |
| `\|\| e` | where an operand is expected | `fn() => e` |
| `\|x\| e` | where an operand is expected | none |
| `assert!(e)` | the keyword `assert` starting a statement | `expect e` |
| `assert_eq!(a, b)` | after the identifier starting a statement | `expect a == b` |
| `assert_ne!(a, b)` | after the identifier starting a statement | `expect a != b` |
| `todo!(t)` | anywhere | `todo t` |
| `unimplemented!(t)` | anywhere | `todo t` |
| `x!` | after an identifier | none |
| `!e` | where an operand is expected | `not e` else `(not e)` else `~e` |
| `a === b` | the `=` directly after `==` | `a == b` |
| `a !== b` | the `=` directly after `!=` | `a != b` |
| `c?` | after an operand, with no operand after it | `try c` |
| `?` | anywhere | none |
| `;` | ending its logical line | the `;` and the spaces before it deleted |
| `;` | followed by a token on its logical line | the `;` and the spaces after it replaced by a line break and the indentation of its line |
| `a..b` | after an operand | `a..<b` |
| `..` | anywhere | none |
| `let mut x` | after `let` | `var x` |
| `x := e` | after the identifier starting a statement | `let x = e` else `var x = e` |
| `e as T` | after an operand | `T.widen(e)` else `T.truncate(e)` else `T.saturate(e)` else `T.nearest(e)` else `e` |
| `&mut e` | where an operand is expected | `mut e` |
| `&e` | where an operand is expected | `e` |
| `&mut self` | where a parameter is expected | `mut self` |
| `&self` | where a parameter is expected | `self` |
| `x: &mut T` | where a parameter's type is expected | `mut x: T` |
| `x: &T` | where a parameter's type is expected | `x: T` |
| `*e` | where an operand is expected | none |
| `c.await` | after `.` | `await c` |
| `p++` | the second `+` directly after the first | `p += 1` |
| `p--` | at `--` after an operand | `p -= 1` |
| `p->x` | after an operand | `p.x` |
| `-> ()` | where a function's result type is expected | the matched text and the space before it deleted |

A language version fixes its idiom table; that of language 1 is exactly this table.

```nan
pub closed enum ParseError {
    Empty
} ParseError

pub fn first_byte(input: Slice[u8]) -> u8 fails ParseError {
    if input.len() == 0 {
        fail ParseError.Empty
    }
    return input[0]
} first_byte

pub fn first_word(input: Slice[u8]) -> u32 fails ParseError {
    let byte = try first_byte(input)
    return u32.widen(byte)
} first_word

pub fn low_byte(word: u32) -> u8 {
    return u8.truncate(word)
} low_byte
```

```nan
pub closed enum ParseError {
    Empty
} ParseError

pub fn first_byte(input: Slice[u8]) -> u8 fails ParseError {
    if input.len() == 0 {
        fail ParseError.Empty
    }
    return input[0]
} first_byte

pub fn first_word(input: Slice[u8]) -> u32 fails ParseError {
    let byte = first_byte(input)? // error: syntax.foreign
    return u32.widen(byte)
} first_word
```

```text
$ nan check example
failed check example: errors 1, incomplete 0
error syntax.foreign example/example.nan:14 example::first_word
        let byte = first_byte(input)? // error: syntax.foreign
    expected: try first_byte(input)
    found: first_byte(input)?
    fix 1 example/example.nan
    -     let byte = first_byte(input)? // error: syntax.foreign
    +     let byte = try first_byte(input) // error: syntax.foreign
failed check example: errors 1, incomplete 0
```

```nan
pub fn low_byte(word: u32) -> u8 {
    return word as u8 // error: syntax.foreign
} low_byte
```

In the last example `u8.widen(word)` fails the re-check (`num.widen-lossy`), so the fix and the expected text are `u8.truncate(word)`.

## Code catalogue

The catalogue lists every diagnostic code by family, in chapter order, linked to the first section its owner's table gives, with the class its owning table states. The `mem` family has no codes, and the `trap` family names trap kinds.

| Family | Class | Codes |
| --- | --- | --- |
| `lex` | error | [`lex.carriage-return`](01-lexical.md#source-files), [`lex.char-literal-size`](01-lexical.md#text-literals), [`lex.escape-out-of-range`](01-lexical.md#text-literals), [`lex.indentation`](01-lexical.md#indentation-and-whitespace), [`lex.invalid-utf8`](01-lexical.md#source-files), [`lex.literal-suffix`](01-lexical.md#number-literals), [`lex.malformed-number`](01-lexical.md#number-literals), [`lex.missing-final-newline`](01-lexical.md#source-files), [`lex.non-ascii`](01-lexical.md#identifiers), [`lex.non-ascii-byte`](01-lexical.md#text-literals), [`lex.not-at-line-start`](01-lexical.md#tokens), [`lex.rejected-code-point`](01-lexical.md#rejected-code-points), [`lex.tab`](01-lexical.md#indentation-and-whitespace), [`lex.trailing-whitespace`](01-lexical.md#indentation-and-whitespace), [`lex.unexpected-character`](01-lexical.md#tokens), [`lex.unknown-escape`](01-lexical.md#text-literals), [`lex.unterminated-literal`](01-lexical.md#text-literals) |
| `item` | error | [`item.body-opener`](02-items.md#heads), [`item.clause-order`](02-items.md#clause-region), [`item.const-cycle`](02-items.md#constants), [`item.const-type`](02-items.md#constants), [`item.const-untyped`](02-items.md#constants), [`item.duplicate`](02-items.md#order-and-names), [`item.fails-missing`](02-items.md#heads), [`item.fails-unneeded`](02-items.md#heads), [`item.foreign-form`](02-items.md#heads), [`item.indented`](02-items.md#item-shape), [`item.label-mismatch`](02-items.md#item-shape), [`item.method-misplaced`](02-items.md#methods), [`item.missing-body`](02-items.md#heads), [`item.modifier-conflict`](02-items.md#modifiers), [`item.modifier-misplaced`](02-items.md#modifiers), [`item.multiple-lines`](02-items.md#item-shape), [`item.openness-missing`](02-items.md#modifiers), [`item.openness-not-pub`](02-items.md#modifiers), [`item.reserved-word`](02-items.md#reserved-forms), [`item.self-field-set`](02-items.md#reserved-forms), [`item.self-misplaced`](02-items.md#methods), [`item.stray-column-zero`](02-items.md#item-shape), [`item.transparent-limits`](02-items.md#transparent-functions), [`item.unit-result`](02-items.md#heads), [`item.visibility-not-allowed`](02-items.md#item-kinds), [`item.wait-missing`](02-items.md#heads), [`item.wait-unneeded`](02-items.md#heads) |
| `manifest` | error | [`manifest.any-not-alone`](03-packages-modules.md#manifest), [`manifest.bad-source`](03-packages-modules.md#aliases), [`manifest.codegen-library`](03-packages-modules.md#manifest), [`manifest.codegen-target`](03-packages-modules.md#manifest), [`manifest.deny-self`](03-packages-modules.md#manifest), [`manifest.duplicate`](03-packages-modules.md#manifest), [`manifest.misplaced`](03-packages-modules.md#manifest), [`manifest.missing`](03-packages-modules.md#packages), [`manifest.missing-directive`](03-packages-modules.md#manifest), [`manifest.reserved-target`](03-packages-modules.md#manifest), [`manifest.unknown-codegen`](03-packages-modules.md#manifest), [`manifest.unknown-target`](03-packages-modules.md#manifest) |
| `module` | error | [`module.cycle`](03-packages-modules.md#modules), [`module.variant-mismatch`](03-packages-modules.md#target-variants), [`module.variant-missing`](03-packages-modules.md#target-variants), [`module.variant-mixed`](03-packages-modules.md#target-variants), [`module.variant-overlap`](03-packages-modules.md#target-variants), [`module.variant-unmatched`](03-packages-modules.md#target-variants) |
| `name` | error | [`name.case`](03-packages-modules.md#naming-conventions), [`name.clash`](03-packages-modules.md#naming-conventions), [`name.colon-for-member`](03-packages-modules.md#paths), [`name.core-redefined`](03-packages-modules.md#core-vocabulary), [`name.dot-for-path`](03-packages-modules.md#paths), [`name.moved`](03-packages-modules.md#unknown-names), [`name.newer-release`](03-packages-modules.md#unknown-names), [`name.prefix-mismatch`](03-packages-modules.md#name-prefixes), [`name.private`](03-packages-modules.md#visibility), [`name.private-in-interface`](03-packages-modules.md#visibility), [`name.redundant-path`](03-packages-modules.md#paths), [`name.reserved`](03-packages-modules.md#aliases), [`name.unknown`](03-packages-modules.md#unknown-names) |
| `pattern` | error | [`pattern.const-kind`](04-types.md#patterns), [`pattern.duplicate-binding`](04-types.md#pattern-bindings), [`pattern.empty-range`](04-types.md#patterns), [`pattern.open-enum-no-else`](04-types.md#closed-and-open-types), [`pattern.open-struct`](04-types.md#closed-and-open-types), [`pattern.path-kind`](04-types.md#patterns), [`pattern.range-kind`](04-types.md#patterns), [`pattern.refutable`](04-types.md#refutability) |
| `predicate` | error | [`predicate.default-unproven`](04-types.md#derived-default), [`predicate.exposed-field`](04-types.md#module-boundary), [`predicate.outside-write`](04-types.md#module-boundary) |
| `type` | error | [`type.closed-hidden-field`](04-types.md#closed-and-open-types), [`type.duplicate-label`](04-types.md#construction), [`type.duplicate-member`](04-types.md#structs), [`type.hidden-field`](04-types.md#structs), [`type.literal-range`](04-types.md#expected-types-and-literals), [`type.literal-untyped`](04-types.md#expected-types-and-literals), [`type.mismatch`](04-types.md#expected-types-and-literals), [`type.missing-field`](04-types.md#construction), [`type.not-a-type`](04-types.md#structs), [`type.open-construction`](04-types.md#closed-and-open-types), [`type.payload-shape`](04-types.md#construction), [`type.qualified-core-variant`](04-types.md#option-and-result), [`type.recursive`](04-types.md#structs), [`type.unlabelled-field`](04-types.md#construction), [`type.unlabelled-payload`](04-types.md#enums) |
| `prop` | error | [`prop.capability-derive`](05-properties.md#capability-types), [`prop.copy-ineligible`](05-properties.md#copy), [`prop.linear-argument`](05-properties.md#generic-parameters), [`prop.linear-undeclared`](05-properties.md#linear-types), [`prop.promise-not-open`](05-properties.md#promises), [`prop.promise-unmet`](05-properties.md#promises), [`prop.property-impl`](05-properties.md#property-interfaces), [`prop.token-argument`](05-properties.md#state-tokens), [`prop.token-wait`](05-properties.md#state-tokens), [`prop.view-argument`](05-properties.md#view-arguments), [`prop.view-result`](05-properties.md#view-positions), [`prop.view-undeclared`](05-properties.md#views) |
| `expr` | error | [`expr.argument-count`](06-statements-expressions.md#arguments-and-labels), [`expr.argument-label`](06-statements-expressions.md#arguments-and-labels), [`expr.branch-form`](06-statements-expressions.md#value-yielding-if-and-match), [`expr.chained-comparison`](06-statements-expressions.md#operators), [`expr.field-call`](06-statements-expressions.md#field-access-and-method-calls), [`expr.float-pattern`](06-statements-expressions.md#match-arms), [`expr.guard-outside-first`](06-statements-expressions.md#match-first), [`expr.marker-order`](06-statements-expressions.md#markers), [`expr.marker-target`](06-statements-expressions.md#markers), [`expr.method-value`](06-statements-expressions.md#field-access-and-method-calls), [`expr.misplaced-jump`](06-statements-expressions.md#returns), [`expr.missing-else`](06-statements-expressions.md#value-yielding-if-and-match), [`expr.non-exhaustive`](06-statements-expressions.md#disjointness-and-exhaustiveness), [`expr.or-bindings`](06-statements-expressions.md#match-arms), [`expr.overlapping-arms`](06-statements-expressions.md#disjointness-and-exhaustiveness), [`expr.parentheses`](06-statements-expressions.md#operators), [`expr.range-type`](06-statements-expressions.md#ranges), [`expr.unreachable-arm`](06-statements-expressions.md#disjointness-and-exhaustiveness) |
| `stmt` | error | [`stmt.assign-target`](06-statements-expressions.md#assignment), [`stmt.discard-unit`](06-statements-expressions.md#expression-statements), [`stmt.else-not-diverging`](06-statements-expressions.md#let-else), [`stmt.irrefutable-else`](06-statements-expressions.md#let-else), [`stmt.missing-return`](06-statements-expressions.md#returns), [`stmt.no-loop`](06-statements-expressions.md#conditionals-and-loops), [`stmt.return-value`](06-statements-expressions.md#returns), [`stmt.shadowing`](06-statements-expressions.md#bindings), [`stmt.unknown-label`](06-statements-expressions.md#conditionals-and-loops), [`stmt.unreachable`](06-statements-expressions.md#divergence), [`stmt.unused-label`](06-statements-expressions.md#conditionals-and-loops), [`stmt.unused-result`](06-statements-expressions.md#expression-statements), [`stmt.wildcard-binding`](06-statements-expressions.md#bindings) |
| `threshold` | error | [`threshold.empty-reason`](06-statements-expressions.md#waivers), [`threshold.unused-waiver`](06-statements-expressions.md#waivers) |
| `threshold` | incompleteness | [`threshold.depth`](06-statements-expressions.md#depth), [`threshold.length`](06-statements-expressions.md#length) |
| `closure` | error | [`closure.capture-duplicate`](07-ownership.md#captures), [`closure.capture-mode`](07-ownership.md#captures), [`closure.capture-unused`](07-ownership.md#captures), [`closure.projection-value`](07-ownership.md#function-values), [`closure.second-class`](07-ownership.md#closures), [`closure.short-effect`](07-ownership.md#closures) |
| `mode` | error | [`mode.copy-taken`](07-ownership.md#marking), [`mode.immutable-binding`](07-ownership.md#modes), [`mode.mismatch`](07-ownership.md#marking), [`mode.unmarked`](07-ownership.md#marking) |
| `own` | error | [`own.conflict`](07-ownership.md#exclusivity), [`own.linear-dropped`](07-ownership.md#linear-consumption), [`own.move-not-owned`](07-ownership.md#ownership), [`own.not-indexable`](07-ownership.md#indexing), [`own.not-iterable`](07-ownership.md#iteration), [`own.proj-receiver`](07-ownership.md#projections), [`own.proj-source`](07-ownership.md#projection-results), [`own.read-only-view`](07-ownership.md#projection-results), [`own.temporary-source`](07-ownership.md#projection-results), [`own.use-after-move`](07-ownership.md#ownership), [`own.view-escape`](07-ownership.md#holds), [`own.wrapped-part-use`](07-ownership.md#projection-results) |
| `derive` | error | [`derive.constituent-missing`](08-generics-interfaces.md#derives), [`derive.duplicate`](08-generics-interfaces.md#derives), [`derive.enum-default`](08-generics-interfaces.md#derives), [`derive.unknown`](08-generics-interfaces.md#derives) |
| `generic` | error | [`generic.argument-kind`](08-generics-interfaces.md#instantiation), [`generic.arity`](08-generics-interfaces.md#instantiation), [`generic.const-type`](08-generics-interfaces.md#generic-parameters), [`generic.parameter-unused`](08-generics-interfaces.md#generic-parameters), [`generic.uninferred`](08-generics-interfaces.md#inference), [`generic.unsatisfied-bound`](08-generics-interfaces.md#conformance), [`generic.where-misplaced`](08-generics-interfaces.md#generic-parameters), [`generic.where-unmet`](08-generics-interfaces.md#instantiation) |
| `interface` | error | [`interface.ambiguous-method`](08-generics-interfaces.md#method-resolution), [`interface.duplicate-impl`](08-generics-interfaces.md#coherence), [`interface.dyn-view-mode`](08-generics-interfaces.md#dynamic-dispatch), [`interface.head-mismatch`](08-generics-interfaces.md#conformance), [`interface.impl-misplaced`](08-generics-interfaces.md#impl-lines), [`interface.impl-parameters`](08-generics-interfaces.md#impl-lines), [`interface.impl-target`](08-generics-interfaces.md#impl-lines), [`interface.impl-visibility`](08-generics-interfaces.md#conformance), [`interface.method-clause`](08-generics-interfaces.md#interfaces), [`interface.method-collision`](08-generics-interfaces.md#conformance), [`interface.missing-method`](08-generics-interfaces.md#conformance), [`interface.not-dyn-usable`](08-generics-interfaces.md#dynamic-dispatch), [`interface.not-implemented`](08-generics-interfaces.md#method-resolution), [`interface.not-interface`](08-generics-interfaces.md#impl-lines), [`interface.unqualified-call`](08-generics-interfaces.md#method-resolution) |
| `num` | error | [`num.conversion-undefined`](09-numbers.md#conversions), [`num.conversion-value`](09-numbers.md#conversions), [`num.from-bits-not-anybits`](09-numbers.md#bit-patterns), [`num.same-type-conversion`](09-numbers.md#conversions), [`num.signed-division`](09-numbers.md#division), [`num.signed-shift-right`](09-numbers.md#shifts), [`num.unsigned-negation`](09-numbers.md#overflow), [`num.widen-applies`](09-numbers.md#conversions), [`num.widen-lossy`](09-numbers.md#conversions) |
| `effect` | error | [`effect.await-missing`](10-effects-digests.md#awaited-calls), [`effect.await-unneeded`](10-effects-digests.md#awaited-calls), [`effect.fit-fails`](10-effects-digests.md#fit), [`effect.fit-head`](10-effects-digests.md#fit), [`effect.fit-kind`](10-effects-digests.md#fit), [`effect.fit-wait`](10-effects-digests.md#fit), [`effect.foreign-wait`](10-effects-digests.md#foreign-functions-and-trusted-code), [`effect.undo-irreversible`](10-effects-digests.md#compensation), [`effect.undo-target`](10-effects-digests.md#compensation), [`effect.uses-unknown`](10-effects-digests.md#waiting) |
| `limit` | error | [`limit.allocates`](10-effects-digests.md#limits), [`limit.duplicate`](10-effects-digests.md#limits), [`limit.impure`](10-effects-digests.md#limits), [`limit.may-not-terminate`](10-effects-digests.md#limits), [`limit.may-trap`](10-effects-digests.md#limits), [`limit.redundant`](10-effects-digests.md#limits), [`limit.reversibility-exceeded`](10-effects-digests.md#limits), [`limit.stack-exceeded`](10-effects-digests.md#stack-limits), [`limit.stack-size`](10-effects-digests.md#limits), [`limit.stack-unbounded`](10-effects-digests.md#stack-limits) |
| `error` | error | [`error.cancel-unmapped`](11-errors-domains.md#cancellation), [`error.failure-type`](11-errors-domains.md#failures), [`error.from-ambiguous`](11-errors-domains.md#error-enums), [`error.from-duplicate`](11-errors-domains.md#error-enums), [`error.from-shape`](11-errors-domains.md#error-enums), [`error.marker-unneeded`](11-errors-domains.md#attempt), [`error.unconvertible`](11-errors-domains.md#fail), [`error.unhandled`](11-errors-domains.md#unhandled-failures) |
| `encode` | error | [`encode.flexible`](13-text-encode.md#flexible-structs), [`encode.linear`](13-text-encode.md#requirements), [`encode.manual-impl`](13-text-encode.md#requirements), [`encode.migrate-duplicate`](13-text-encode.md#migrations), [`encode.migrate-effect`](13-text-encode.md#migrations), [`encode.migrate-form`](13-text-encode.md#migrations), [`encode.migrate-inverse-alone`](13-text-encode.md#migrations), [`encode.migrate-limits`](13-text-encode.md#migrations), [`encode.migrate-schema`](13-text-encode.md#migrations), [`encode.not-inert`](13-text-encode.md#requirements), [`encode.view`](13-text-encode.md#requirements) |
| `text` | error | [`text.argument-duplicate`](13-text-encode.md#templates), [`text.argument-unused`](13-text-encode.md#templates), [`text.bare-len`](13-text-encode.md#lengths-and-slicing), [`text.hole-unbound`](13-text-encode.md#templates), [`text.integer-index`](13-text-encode.md#lengths-and-slicing), [`text.template-malformed`](13-text-encode.md#templates), [`text.template-not-literal`](13-text-encode.md#templates), [`text.template-position`](13-text-encode.md#templates) |
| `authority` | error | [`authority.capability-impl`](14-authority.md#revocation), [`authority.denied-dependency`](14-authority.md#denied-dependencies), [`authority.dependency-ungranted`](14-authority.md#manifest-grants), [`authority.kind-foreign`](14-authority.md#capability-kinds), [`authority.kind-form`](14-authority.md#capability-kinds), [`authority.ungranted`](14-authority.md#manifest-grants), [`authority.unknown-name`](14-authority.md#manifest-grants), [`authority.widening-unapproved`](14-authority.md#authority-summaries) |
| `secret` | error | [`secret.bits-impl`](15-untrusted.md#secret-values), [`secret.public-use`](15-untrusted.md#secret-values) |
| `untrusted` | error | [`untrusted.inspect-outside-validator`](15-untrusted.md#validators), [`untrusted.validator-interface`](15-untrusted.md#validators), [`untrusted.validator-no-input`](15-untrusted.md#validators), [`untrusted.wrapped-alias`](15-untrusted.md#untrusted-values) |
| `task` | error | [`task.capture-not-send`](16-concurrency.md#task-closures), [`task.lend-local`](16-concurrency.md#lending), [`task.lend-outside-scope`](16-concurrency.md#lending), [`task.task-fn-call`](16-concurrency.md#task-closures), [`task.unscoped-failure`](16-concurrency.md#joins), [`task.when-duplicate`](16-concurrency.md#await-when), [`task.when-escape`](16-concurrency.md#await-when), [`task.when-not-cown`](16-concurrency.md#await-when), [`task.when-take`](16-concurrency.md#await-when), [`task.when-wait`](16-concurrency.md#await-when) |
| `defer` | error | [`defer.abandon-head`](17-suspension-io.md#abandoning-resources), [`defer.deferred-place-moved`](17-suspension-io.md#deferred-places), [`defer.fails`](17-suspension-io.md#defer), [`defer.has-result`](17-suspension-io.md#defer), [`defer.not-call`](17-suspension-io.md#defer), [`defer.operand-form`](17-suspension-io.md#defer), [`defer.undefer-unmatched`](17-suspension-io.md#deferred-places), [`defer.waits`](17-suspension-io.md#defer) |
| `suspend` | error | [`suspend.stack-missing`](17-suspension-io.md#static-bounds), [`suspend.token-across-await`](17-suspension-io.md#views-across-suspension-points) |
| `contract` | error | [`contract.assume-untrusted`](18-contracts.md#assume), [`contract.decreases-unneeded`](18-contracts.md#decreases), [`contract.decreases-unproven`](18-contracts.md#decreases), [`contract.empty-reason`](18-contracts.md#assume), [`contract.ill-defined`](18-contracts.md#contract-terms), [`contract.impl-requires`](18-contracts.md#interface-contracts), [`contract.impure-condition`](18-contracts.md#expect), [`contract.old-form`](18-contracts.md#ensures), [`contract.quantifier-outside`](18-contracts.md#quantifiers), [`contract.taken-parameter`](18-contracts.md#ensures), [`contract.vacuous`](18-contracts.md#vacuous-contracts) |
| `example` | error | [`example.failed`](18-contracts.md#examples), [`example.form`](18-contracts.md#examples), [`example.trapped`](18-contracts.md#examples) |
| `todo` | error | [`todo.empty-reason`](18-contracts.md#todo), [`todo.untyped`](18-contracts.md#holes) |
| `todo` | incompleteness | [`todo.disputed`](18-contracts.md#disputed-clauses), [`todo.hole`](18-contracts.md#holes), [`todo.missing-spec`](18-contracts.md#missing-specifications) |
| `nir` | error | [`nir.malformed`](19-nir.md#encoding), [`nir.version-unsupported`](19-nir.md#encoding) |
| `proof` | error | [`proof.refuted`](20-prover.md#outcomes), [`proof.unknown`](20-prover.md#outcomes) |
| `const` | error | [`const.assert-false`](21-interpreter.md#const-evaluation), [`const.steps-exhausted`](21-interpreter.md#run-ends), [`const.trapped`](21-interpreter.md#run-ends), [`const.trusted-violation`](21-interpreter.md#run-ends), [`const.uninterpretable`](21-interpreter.md#run-ends), [`const.view-not-static`](21-interpreter.md#const-evaluation) |
| `test` | error | [`test.emulator-unavailable`](22-testing.md#emulator-runs), [`test.escaped-failure`](22-testing.md#test-runs), [`test.generate-manual-impl`](22-testing.md#generation), [`test.generate-type`](22-testing.md#generation), [`test.parameter-form`](22-testing.md#test-items), [`test.parameter-type`](22-testing.md#test-items), [`test.replay-token`](22-testing.md#replay-tokens), [`test.stalled`](22-testing.md#test-runs), [`test.target-kind`](22-testing.md#test-items), [`test.target-not-run`](22-testing.md#test-runs), [`test.target-unreachable`](22-testing.md#test-items), [`test.test-only-item`](22-testing.md#simulation-boundary), [`test.timeout`](22-testing.md#test-runs) |
| `test` | incompleteness | [`test.dbg`](22-testing.md#debug-output), [`test.generator-gap`](22-testing.md#constraints) |
| `hw` | error | [`hw.atomic-type`](23-low-level.md#trusted-atomics), [`hw.dma-completion`](23-low-level.md#dma), [`hw.dma-operand`](23-low-level.md#dma), [`hw.intrinsic-target`](23-low-level.md#privileged-intrinsics), [`hw.landing-called`](23-low-level.md#landing-functions), [`hw.landing-signature`](23-low-level.md#landing-functions), [`hw.layout-unfixed`](23-low-level.md#address-types), [`hw.mint-root-misplaced`](23-low-level.md#landing-functions) |
| `layout` | error | [`layout.endianness`](23-low-level.md#layouts), [`layout.field-form`](23-low-level.md#layouts), [`layout.field-type`](23-low-level.md#layouts), [`layout.literal`](23-low-level.md#layouts), [`layout.out-of-range`](23-low-level.md#layouts), [`layout.overlap`](23-low-level.md#layouts), [`layout.repr`](23-low-level.md#layouts), [`layout.tag-value`](23-low-level.md#layouts), [`layout.uncovered`](23-low-level.md#layouts) |
| `register` | error | [`register.array-access`](23-low-level.md#register-windows), [`register.bank-reserved`](23-low-level.md#registers), [`register.literal`](23-low-level.md#registers), [`register.misaligned`](23-low-level.md#registers), [`register.not-readable`](23-low-level.md#register-windows), [`register.not-writable`](23-low-level.md#register-windows), [`register.operand`](23-low-level.md#register-windows), [`register.overlap`](23-low-level.md#registers), [`register.port-target`](23-low-level.md#register-windows), [`register.port-width`](23-low-level.md#register-windows), [`register.stride`](23-low-level.md#registers), [`register.type`](23-low-level.md#registers), [`register.window-type`](23-low-level.md#registers) |
| `trusted` | error | [`trusted.block-value`](23-low-level.md#trusted-code), [`trusted.convention-target`](23-low-level.md#calling-conventions), [`trusted.convention-unknown`](23-low-level.md#calling-conventions), [`trusted.empty-premise`](23-low-level.md#trusted-code), [`trusted.foreign-signature`](23-low-level.md#foreign-functions), [`trusted.foreign-unresolved`](23-low-level.md#foreign-functions), [`trusted.nested`](23-low-level.md#trusted-code), [`trusted.primitive-outside`](23-low-level.md#trusted-primitives) |
| `kernel` | error | [`kernel.anchor-outside-entry`](24-kernel.md#anchor), [`kernel.anchor-type`](24-kernel.md#anchor), [`kernel.float-use`](24-kernel.md#floating-point), [`kernel.irq-call`](24-kernel.md#interrupt-functions), [`kernel.irq-head`](24-kernel.md#interrupt-functions), [`kernel.irq-limit`](24-kernel.md#interrupt-functions), [`kernel.irq-wait`](24-kernel.md#interrupt-functions) |
| `registry` | error | [`registry.duplicate-key`](25-registries.md#tables), [`registry.duplicate-unique`](25-registries.md#tables), [`registry.entry-type`](25-registries.md#registries), [`registry.grant-not-alias`](25-registries.md#contributors), [`registry.key-type`](25-registries.md#registries), [`registry.not-registry`](25-registries.md#contributions), [`registry.platform-entry`](25-registries.md#platform-registries), [`registry.platform-target`](25-registries.md#platform-registries), [`registry.ungranted`](25-registries.md#contributors), [`registry.unique-repeated`](25-registries.md#registries), [`registry.unique-type`](25-registries.md#registries), [`registry.unknown-field`](25-registries.md#registries) |
| `component` | error | [`component.env-not-abandonable`](26-components.md#component-state), [`component.not-component`](26-components.md#runner), [`component.operation-class`](26-components.md#protocols), [`component.operation-clause`](26-components.md#protocols), [`component.operation-name`](26-components.md#protocols), [`component.operation-type`](26-components.md#protocols), [`component.request-value`](26-components.md#protocols), [`component.unavailable`](26-components.md#runner), [`component.state-generic`](26-components.md#component-state), [`component.state-not-encode`](26-components.md#component-state), [`component.state-not-inert`](26-components.md#component-state), [`component.summary-type`](26-components.md#protocols) |
| `target` | error | [`target.dep-program`](27-program-model.md#programs-and-libraries), [`target.dependency-uncovered`](27-program-model.md#targets), [`target.entry-head`](27-program-model.md#entry), [`target.entry-missing`](27-program-model.md#entry), [`target.link-failed`](27-program-model.md#images), [`target.program-targets`](27-program-model.md#programs-and-libraries) |
| `dep` | error | [`dep.cycle`](29-identity-versioning.md#lock), [`dep.epoch-unapproved`](29-identity-versioning.md#approvals), [`dep.lock-stale`](29-identity-versioning.md#lock), [`dep.release-missing`](29-identity-versioning.md#lock), [`dep.split-package`](29-identity-versioning.md#lock), [`dep.unresolved`](29-identity-versioning.md#resolution) |
| `retired` | error | [`retired.ambiguous-use`](29-identity-versioning.md#forwarding), [`retired.bad-rewrite`](29-identity-versioning.md#tombstones), [`retired.foreign-path`](29-identity-versioning.md#tombstones), [`retired.form-taken`](29-identity-versioning.md#tombstones), [`retired.not-preserved`](29-identity-versioning.md#publishing), [`retired.rewrite-hidden`](29-identity-versioning.md#tombstones) |
| `retired` | incompleteness | [`retired.forwarded-use`](29-identity-versioning.md#forwarding) |
| `version` | error | [`version.breaking-target`](29-identity-versioning.md#behaviour-epochs), [`version.parent-missing`](29-identity-versioning.md#publishing), [`version.path-dependency`](29-identity-versioning.md#publishing), [`version.store-name-taken`](29-identity-versioning.md#local-store) |
| `artefact` | error | [`artefact.bad-signature`](30-artefacts.md#sections), [`artefact.malformed`](30-artefacts.md#artefacts), [`artefact.recheck-failed`](30-artefacts.md#releases), [`artefact.reserved-section`](30-artefacts.md#sections), [`artefact.root-mismatch`](30-artefacts.md#artefacts), [`artefact.source-mismatch`](30-artefacts.md#releases), [`artefact.symbol-collision`](30-artefacts.md#host-symbol-names), [`artefact.version-unsupported`](30-artefacts.md#artefacts) |
| `api` | incompleteness | [`api.stale`](31-trust.md#api-snapshots), [`api.weakening-pending`](31-trust.md#weakening) |
| `key` | error | [`key.bad-form`](31-trust.md#principals), [`key.exists`](31-trust.md#principals), [`key.missing`](31-trust.md#principals) |
| `seal` | error | [`seal.bad-principal`](31-trust.md#seals), [`seal.unneeded`](31-trust.md#seals) |
| `seal` | incompleteness | [`seal.same-author`](31-trust.md#author-rule) |
| `trust` | incompleteness | [`trust.unsealed`](31-trust.md#trust-escapes) |
| `unused` | incompleteness | [`unused.dependency`](31-trust.md#unused-code), [`unused.field`](31-trust.md#unused-code), [`unused.grant`](14-authority.md#manifest-grants), [`unused.item`](31-trust.md#unused-code), [`unused.local`](31-trust.md#unused-code), [`unused.tombstone`](31-trust.md#unused-code) |
| `doc` | error | [`doc.code-block`](32-docs.md#code-in-docs), [`doc.digest-claim`](32-docs.md#digest-claims), [`doc.misplaced`](32-docs.md#doc-comments), [`doc.module-file-missing`](32-docs.md#module-summaries), [`doc.private-reference`](32-docs.md#references), [`doc.summary-missing`](32-docs.md#file-summaries), [`doc.unresolved-reference`](32-docs.md#references), [`doc.variant-mismatch`](32-docs.md#module-summaries) |
| `doc` | incompleteness | [`doc.stale`](32-docs.md#doc-binding) |
| `edit` | error | [`edit.check-failed`](33-toolchain.md#compare-and-swap-edits), [`edit.identity-changed`](33-toolchain.md#compare-and-swap-edits), [`edit.move-invalid`](33-toolchain.md#rename-and-move), [`edit.not-one-item`](33-toolchain.md#compare-and-swap-edits), [`edit.stale`](33-toolchain.md#compare-and-swap-edits) |
| `merge` | error | [`merge.conflict`](33-toolchain.md#conflicts), [`merge.semantic-conflict`](33-toolchain.md#conflicts), [`merge.unresolved`](33-toolchain.md#conflicts) |
| `tool` | error | [`tool.no-workspace`](33-toolchain.md#commands), [`tool.unknown-scope`](33-toolchain.md#commands), [`tool.usage`](33-toolchain.md#commands) |
| `internal` | internal error | [`internal.invariant`](#internal-errors), [`internal.resource-exhausted`](#internal-errors) |
| `syntax` | error | [`syntax.foreign`](#syntax-failures), [`syntax.mismatched-delimiter`](#syntax-failures), [`syntax.unclosed-delimiter`](#syntax-failures), [`syntax.unexpected-token`](#syntax-failures) |
| `format` | error | [`format.machine-invalid`](35-format.md#common-syntax), [`format.machine-version`](35-format.md#common-syntax), [`format.reserved-field`](35-format.md#lock) |
| `build` | error | [`build.cache-exposed`](36-builds.md#cache), [`build.image-unreadable`](36-builds.md#stage-comparison), [`build.stage-mismatch`](36-builds.md#stage-comparison), [`build.symbolic-link`](36-builds.md#build-scripts) |
| `language` | error | [`language.downgrade`](37-evolution.md#language-upgrades), [`language.newer-dependency`](37-evolution.md#artefacts-and-linking), [`language.unsupported`](37-evolution.md#language-versions), [`language.version-form`](37-evolution.md#language-versions) |

## Diagnostics

| Code | Class | Rule |
| --- | --- | --- |
| `internal.invariant` | internal error | [Internal errors](#internal-errors) |
| `internal.resource-exhausted` | internal error | [Internal errors](#internal-errors) |
| `syntax.foreign` | error | [Syntax failures](#syntax-failures), [Idiom table](#idiom-table), [Punctuation and operators](01-lexical.md#punctuation-and-operators) |
| `syntax.mismatched-delimiter` | error | [Syntax failures](#syntax-failures) |
| `syntax.unclosed-delimiter` | error | [Syntax failures](#syntax-failures) |
| `syntax.unexpected-token` | error | [Syntax failures](#syntax-failures) |
