# Canonical format and machine files

This chapter defines the canonical format of source files, with line breaking and the layout of heads beyond four parameters, depth and indentation, spacing, blank lines, number grouping and comment placement; the canonical order of items, clauses and members; the guarantees of `nan fmt`, idempotence among them; and the machine files `.nan/lock.nan`, `.nan/history.nan` and the API snapshots, with the reserved lock fields.

## Canonical format

The **canonical format** of a source file is the one layout `nan fmt` produces for it. `nan fmt` takes no option, and its result depends only on the file's bytes and the package's [language version](37-evolution.md#language-versions).

- Beyond the rules of [Lexical structure](01-lexical.md) and [Items](02-items.md), layout never makes source invalid: the checker reports no diagnostic because a file differs from its canonical format. Layout changes no meaning, and the length that [thresholds](06-statements-expressions.md#length) count is measured on the file as written.
- Every source text the toolchain writes, such as an [exact fix](34-diagnostics.md#exact-fixes), an edit, a merge result or a tombstone, is in canonical format.
- No line has a maximum length; `nan fmt` never breaks a line for its width.
- Apart from trailing commas, [punned](06-statements-expressions.md#arguments-and-labels) labels and the reorderings of [Canonical order](#canonical-order), the canonical format keeps every token of the file that is not virtual, every `NL` token and every comment, in their order. Otherwise it changes only line breaks after continuing tokens, indentation, the spaces between tokens, blank lines, the space after a comment's opener and the spelling of number literals.

### Line breaking

A **breakable list** is the parenthesised list of the parameters of a function item, an interface method, a test, a tombstone's old head, a function type or a closure, of the arguments of a call or construction, of a variant payload, or of field patterns. A breakable list is **flat** when no line break follows its opener or any of its own commas and no comma follows its last element. It is **broken** when its opener and each of its own commas are the last token of their line, a comma follows its last element, and its closer is the first token of its line.

- The parameter list of a function item, an interface method or a test is broken when it has more than 4 parameters, `self` included, or when a comment stands between its opener and its closer; otherwise it is flat.
- Every other breakable list is broken when, in the source, its opener or one of its own commas is the last token of its line; otherwise it is flat. An empty list is flat.
- Every other comma-separated list lies on one line: generic parameters and arguments, impl parameters, capture lists, `derives` and `limits` clauses, `when` accesses, `unique` lists, and the target and name lists of directives.
- Every `{` is the last token of its line and every `}` the first token of its line, except that no line break follows the `{` of a [trusted expression](23-low-level.md#trusted-code) or precedes its `}`.
- Every other line break is kept as written: after an operator or another continuing token, `nan fmt` adds no line break and removes none.
- A line ending in a comment is never joined to the next; where a rule would join them, the break is kept as written.

```nan
pub closed struct Reading {
    pub raw: u32
    pub gain: u32
} Reading

/// Scales a reading into the band from floor to ceiling.
pub fn scaled_level(
    reading: Reading,
    offset: u32,
    floor: u32,
    ceiling: u32,
    divisor: u32,
    ) -> u32
    requires divisor > 0
{
    // Saturating steps keep large gains in range.
    let scaled = reading.raw.saturating_mul(reading.gain).saturating_add(offset) /
        divisor
    if scaled < floor {
        return floor
    }
    return clamp(
        scaled,
        high: ceiling,
    )
} scaled_level

pub fn clamp(value: u32, high: u32) -> u32 {
    if value > high {
        return high
    }
    return value
} clamp
```

### Depth

Each line holding a token has a **depth**, and its indentation is 4 spaces per unit of depth.

- The **context** of a token is the innermost pair of delimiters enclosing it, a delimiter not enclosing itself, or the file when none does. The **separators** of a context are its opener, its closer, and the commas and `NL` tokens whose context it is.
- A **unit** of a context is a maximal run of consecutive tokens that lie inside the context and are none of its separators. A statement, a field, a clause line and an argument are units.
- The **indent anchor** of an opening delimiter is the depth of the line holding the first token of the unit of the delimiter's context that holds it.
- The **base depth** at a position is the indent anchor of the innermost delimiter open there, plus 1, or 0 when none is open.

The first of these rules that applies to a line gives its depth:

1. A line beginning with a closing delimiter has the indent anchor of the delimiter it closes plus 1 when it closes the broken parameter list of a function item, an interface method or a test, or when that indent anchor is 0 and the line is not the closing line of a braced item; otherwise it has that indent anchor.
2. With d the depth of the first line of the head of a function item or interface method, the first line of each clause of its clause region has depth d + 1, and the body opener after the clause region has depth d.
3. A line of a text run after its first line has the depth of the run's first line.
4. A line whose first token begins a unit has the base depth. Any other line has the depth of the line holding the first token of its unit, plus 1.

In the block of [Line breaking](#line-breaking), the closer `) -> u32` has depth 1 by rule 1, the `requires` line depth 1 by rule 2, and the line `divisor` depth 2 by rule 4, since `divisor` continues the unit begun by `let` at depth 1.

### Spacing

Exactly one space separates two tokens on a line, except that no space stands:

- after `(` and `[`, and before `)`, `]`, `,` and `:`;
- on either side of `.`, `::`, `..<` and `..=`, and of the `-` inside a [target](27-program-model.md#targets) name such as `x86_64-host`;
- after a unary `~`, and after a unary `-` that no `-` follows;
- before a `(` or `[` that follows an identifier, `self`, `fn`, `dbg`, `)` or `]`, except the `(` opening the parameters of a test, which follows its target after one space.

### Blank lines

- No line within an item is blank. An item's lines run from its first doc comment line, or its first token, to its last line.
- A file has no blank line before its first line that is not blank, and none after its last.
- Exactly one blank line follows the file summary and each item, unless the file ends there.
- Every other run of blank lines outside items becomes one blank line.

### Number grouping

`nan fmt` writes each number literal as follows, keeping its prefix, its digits and its literal suffix:

- In a decimal integer, and in the integer part of a float, `_` separates each group of 3 digits counted from the right when there are 4 digits or more; otherwise no `_` appears.
- The fractional part of a float holds no `_`.
- In a hexadecimal literal, the digits `a` to `f` are uppercase, and `_` separates each group of 4 digits counted from the right when there are more than 4 digits.
- In a binary literal, `_` separates each group of 4 digits counted from the right when there are more than 4 digits.

```nan
/// Largest reading of the 20-bit converter.
pub const MAX_READING: u32 = 1_048_575 // full scale

pub const SATURATED: u32 = 1_000_000

pub const STATUS_MASK: u8 = 0b1000_0001

pub const MMIO_BASE: u64 = 0xFEB0_0000

pub const MAGIC: u16 = 0x414E

pub const SCALE: f64 = 0.125
```

### Comment placement

- A plain comment ending a line follows the line's last token after one space.
- A comment-only line outside an item stands at column 0. Inside an item, it has the depth of the next line holding a token, except that it has the base depth at its position when that line begins with a closing delimiter, and depth d + 1 of [Depth](#depth) rule 2 when that line is the body opener after a clause region.
- After the opener `//`, `///` or `//!`, comment text that is not empty and does not start with a space gains one space. Comment text is otherwise kept as written.

```nan
pub fn capped_sum(values: Slice[u32], cap: u32) -> u32
    requires cap > 0
    // The loop stops adding once the cap is reached.
{
    var total: u32 = 0
    for value in values {
        if total >= cap {
            break
        }
        total = total.saturating_add(value)
        // The last addition may pass the cap.
    }
    return total
} capped_sum
```

## Canonical order

The **canonical order** is the order `nan fmt` gives the parts of a file. Apart from the reorderings of this section, `nan fmt` moves nothing: items, members, statements, match arms, parameters, arguments and the entries of every other list keep their written order.

- Clause lines follow the group order of the [clause region](02-items.md#clause-region), keeping their written order within a group.
- Type modifiers follow [Modifiers](02-items.md#modifiers), directives and the names of their lists follow [Manifest](03-packages-modules.md#manifest), and limits follow [Limits](10-effects-digests.md#limits).
- The entries of a `derives` clause follow the order `Copy`, `Eq`, `Ord`, `Hash`, `Default`, `Render`, `Encode`, `Generate`, `Send`, `Inert`, `AnyBits`, any other entry following them in its written order.
- Listed across the files of a module, items follow their files in byte order of path relative to the package directory, then their order within the file.
- A command that adds an item places it where the rule defining the command or fix states. A tombstone that `nan edit rename` or `nan edit move` writes directly follows the item it retires, or the type holding the member it retires, at that item's new place. Any other added item ends its file.

`nan fmt` writes `derives Render, Eq, Copy` as `derives Copy, Eq, Render`, as in this item:

```nan
pub closed struct Level derives Copy, Eq, Ord, Hash, Default, Render {
    pub value: u32
} Level
```

## Idempotence

`nan fmt` is idempotent: applied to its own output, it changes nothing. For every source file, its output holds:

- the same tokens apart from the rewrites that [Canonical format](#canonical-format) lists;
- the same comments in the same order relative to those tokens, with the same text apart from the space after the opener;
- the same items with the same [item hashes and interface hashes](29-identity-versioning.md#hashes).

`nan fmt` leaves unchanged each line that holds a lexical diagnostic, and every line from the first line of an item that holds a lexical diagnostic, or that the parser abandons ([resynchronisation](02-items.md#resynchronisation)), to the line before the next item the parser reads. It leaves a file that is not well-formed UTF-8 unchanged, and it never writes a machine file.

## Machine files

A **machine file** is one of these files of a package, written only by the toolchain: the [lock](29-identity-versioning.md#lock) `.nan/lock.nan`, the [history](29-identity-versioning.md#history) `.nan/history.nan`, the [API snapshot](31-trust.md#api-snapshots) `.nan/api/<module>.nan` of each module that has one, and the [principals file](31-trust.md#principals) `.nan/principals.nan`. A machine file is not a source file.

### Common syntax

```ebnf
check_record = 'check' TEXT ;
```

- A machine file is UTF-8 text with LF line ends and a final newline, and lexes into tokens by the rules of [Lexical structure](01-lexical.md). It holds no comment and no blank line.
- Each record is one logical line whose first token stands at column 0, its tokens separated as [Spacing](#spacing) fixes. Only a tombstone in a history may span several physical lines, as its canonical format gives.
- The first line is `nan`, the file's kind and its format version: `nan lock 1`, `nan history 1`, `nan api 1` or `nan principals 1`. The kind matches the file's name, and the format version of language 1 is 1.
- The last line is a `check_record` whose `TEXT` holds the hash of every byte before that line.
- A hash is a `TEXT` holding its [full form](29-identity-versioning.md#hash-values), and a [canonical path](29-identity-versioning.md#canonical-paths) a `TEXT` holding the path.
- In every `TEXT`, `\` is written `\\`, `"` is written `\"`, LF is written `\n`, every other code point below U+0020, U+007F and every [rejected code point](01-lexical.md#rejected-code-points) is written `\u{H}` with uppercase hexadecimal digits and no leading zeros, and every other code point is written as itself.
- The toolchain writes a machine file entirely from the state it records, so equal states give identical bytes. A file that would hold no record between its first and last lines is not written, and an existing one is deleted.

A machine file is **valid** when it lexes, its first line names its kind, it parses by the rule of its kind, its records follow the order its section gives with no two equal keys where that order is by key, and its check and every line checksum match.

- A format version the toolchain does not implement is an error (`format.machine-version`), reported on line 1.
- Otherwise a machine file that is not valid, other than through a reserved lock field, is an error (`format.machine-invalid`), reported on its first line that breaks these rules, or on its check line when only the check differs. It has no exact fix.
- A command reads nothing from a machine file that is not valid and never rewrites it.

### Lock

```ebnf
lock_file = COL0 'nan' 'lock' INT NL COL0 lock_tail ;
lock_tail = lock_record NL COL0 lock_tail | check_record NL EOF ;
lock_record = 'snapshot' INT
    | 'dep' IDENT ( 'alias' IDENT )? TEXT TEXT ( 'signed' TEXT )? TEXT
    | 'approved' IDENT summary_entry
    | 'epoch' TEXT INT
    | "registry" ( IDENT | INT | TEXT )* {!format.reserved-field}
    | 'floor' ( IDENT | INT | TEXT )* {!format.reserved-field} ;
summary_entry = 'capability' TEXT
    | 'alloc'
    | 'device'
    | 'priv'
    | "validator" TEXT
    | 'escape' escape_kind TEXT ;
escape_kind = "trusted" | "assume" | "foreign" | "landing" | "validator" | 'decoder' | 'grant' ;
```

The records hold the fields of the lock that [Lock](29-identity-versioning.md#lock) defines:

- `snapshot N` holds the snapshot.
- `dep P alias A S R signed K C` holds the line of package P: A is its alias when it is a direct dependency, and the record has no `alias` part otherwise; S is its source text, a path source being written relative to the lock's package directory; R is the code root of its locked release; K is the [key form](31-trust.md#principals) of its publisher's public key for a release whose artefact holds sections, and the record has no `signed` part otherwise; and C, the line checksum, is the hash of the bytes of the record before the space preceding C.
- `approved P E` holds one pair of the approved [closure summary](14-authority.md#authority-summaries), P being the package name and E the entry in the form this table gives.
- `epoch Q N` holds one approved epoch, Q being the canonical path and N the epoch.

| Summary entry | Form of E |
| --- | --- |
| a primitive capability type | `capability` and the type's canonical path |
| `alloc`, `device` or `priv` | that word |
| a validator or decoder of a validators set | `validator` and its canonical path |
| a [trust escape](31-trust.md#trust-escapes) | `escape`, its [ledger](31-trust.md#ledger) kind and the canonical path of the item holding it |

A trust-escape entry is identified as [authority summaries](14-authority.md#authority-summaries) state.

- The `snapshot` record comes first, then the `dep` records in byte order of P, then the `approved` records in byte order of P and then of E, then the `epoch` records in byte order of Q and then by N.
- Language 1 reserves two lock fields: the registry snapshot, record word `registry`, and policy floors, record word `floor`. A lock holding either is an error (`format.reserved-field`).

This diagram shows the lock `app/.nan/lock.nan` of the package `app` of the example of [Lock](29-identity-versioning.md#lock), written by `nan dep sync` with an otherwise empty local store, each hash written `<...>`:

```text
nan lock 1
snapshot 2
dep meter alias meter "../meter" "<code root of meter>" "<checksum>"
dep report alias report "../report" "<code root of report>" "<checksum>"
check "<hash>"
```

### History

```ebnf
history_file = COL0 'nan' 'history' INT NL COL0 history_tail ;
history_tail = history_record NL COL0 history_tail
    | retired_item COL0 history_tail
    | check_record NL EOF ;
history_record = 'name' TEXT | 'head' TEXT | 'module' IDENT ;
```

- `name S` holds the store name and `head H` the code root of the [history head](29-identity-versioning.md#publishing). Each comes once, in that order.
- Each `module M` record follows them, in byte order of M, followed by the tombstones of module M that the history holds, in byte order of their text.
- A tombstone is written as its `retired` item in canonical format, as it stood in the source of M. Its paths and rewrite resolve as in M.

This diagram shows the history `meter/.nan/history.nan` of a package `meter` first published without `--name`, after a release that moved the tombstone `retired Counter.bump(mut self) => mut self.record() changes "the count saturates instead of trapping"` of its root module out of source, with the head hash written `<...>`:

```text
nan history 1
name "meter"
head "<code root of the release>"
module meter
retired Counter.bump(mut self) => mut self.record() changes "the count saturates instead of trapping"
check "<hash>"
```

### API snapshots

```ebnf
api_file = COL0 'nan' 'api' INT NL COL0 api_tail ;
api_tail = api_record NL COL0 api_tail | check_record NL EOF ;
api_record = 'item' TEXT
    | 'text' TEXT
    | 'promises' promise ( "," promise )*
    | "interface" TEXT
    | 'digest' target TEXT
    | 'doc' ( IDENT | INT )? TEXT TEXT
    | 'method' IDENT
    | 'head' TEXT
    | 'accepted' TEXT
    | "test" TEXT TEXT TEXT*
    | 'pending' TEXT TEXT
    | 'seal' TEXT TEXT TEXT
    | 'closure' ( "interface" | 'body' ) TEXT TEXT
    | 'signature' TEXT ;
promise = IDENT ( "[" IDENT ( "," IDENT )* "]" )? ;
```

An API snapshot holds one **entry** for each item of its module that is `pub`, is an impl line whose impl target and interface are both `pub`, has an accepted spec that is not empty, has a pending weakening, or has a [seal](31-trust.md#seals), in byte order of canonical path; the `package` item's entry, under the package name, belongs to the root module's snapshot. An item with [target variants](03-packages-modules.md#target-variants) has one entry, whose text and docs are those of its variant first in byte order of path. An entry is an `item P` record, P being the item's canonical path, followed by these records in this order:

1. For a `pub` item or impl line: one `text` record per line of its interface text; for a struct or enum that promises any of `Copy`, `Send`, `Inert` and `AnyBits` ([promises](05-properties.md#promises)), `promises` with those properties in that order, each followed in brackets by the generic parameters it rests on, if any; `interface H` with its interface hash; for an item with a [digest](10-effects-digests.md#digests), one `digest T D` per declared target T in byte order, D being the digest as `nan show --api` prints it after `digest: `; and one `doc K B D` per [public doc](32-docs.md#doc-binding) of the item and its members in source order, K being the doc's key (absent for the item's own doc, a name for a member, a position for an invariant line), B its bound hash and D its doc text.
2. For an item whose [accepted spec](31-trust.md#api-snapshots) is not empty: for a function, `head H` with the flat text of its accepted head, one `accepted C` per accepted clause, C being its flat text, in the order of the accepted spec, and one `test Q T H...` per accepted targeting test in byte order of Q, its canonical path, T being its parameter types joined by `, ` and the H its expect hashes in byte order, each as often as it occurs; for a struct, one `accepted C` per accepted field line and invariant line; for an interface, for each method with accepted clauses, in byte order of name N, a `method N` record followed by that method's `head` and `accepted` records. For an item with a pending weakening, after those records, one `pending I W` per weakened part in byte order of W, I being the item hash it is bound to and W the part: `requires`, `predicates`, the flat text of an accepted ensures or example clause, or `test`, a space and the test's canonical path; an interface's are written after the `accepted` records of the method holding the part.
3. For each seal, in byte order of principal: `seal R I C`, R being the principal, I the [sealed item hash](31-trust.md#seals) and C the closure hash it records; one `closure K P H` per closure entry, in byte order of P, then of K, then of H; and `signature S` when the seal holds a signature, S being its [signature form](31-trust.md#signatures).

The **flat text** of a head, clause, field line or invariant line is its tokens on one line, without comments, spaced as [Spacing](#spacing) fixes, each breakable list flat and each text run written as the text literal that the [normalised form](29-identity-versioning.md#normalised-form) writes for it.

The **interface text** of an item is the lines of its canonical format that hold its [interface form](29-identity-versioning.md#item-hash-and-interface-hash), without comments and doc comments:

- for a function, its head lines and the lines of its requires, ensures and example clauses; a transparent function's line ends before ` = ` when its body is not part of its interface form;
- for a struct, its head line, the lines of the fields and invariant lines that enter its interface form, and its closing line;
- for an interface, its lines other than those of decreases, waive and todo clauses and of default bodies, the `{` opening each default body being written as the word `default`;
- for every other kind, all its lines.

The module `example` of this block has the API snapshot `example/.nan/api/example.nan` in the next diagram, with each hash written `<...>`:

```nan
/// A count that stops at its limit.
pub open struct Meter {
    count: u32 where count <= 100
} Meter

pub fn Meter.empty() -> Meter = Meter(count: 0)

pub fn Meter.value(self) -> u32 = self.count

/// Adds one unit, staying at the limit once reached.
pub fn Meter.bump(mut self)
    ensures self.value() >= old(self.value())
{
    if self.count < 100 {
        self.count += 1
    }
} Meter.bump

test bump_counts for Meter.bump {
    var meter = Meter.empty()
    mut meter.bump()
    expect meter.value() == 1
} bump_counts
```

```text
nan api 1
item "example::Meter"
text "pub open struct Meter {"
text "} Meter"
interface "<hash>"
doc "<hash>" "A count that stops at its limit."
accepted "count: u32 where count <= 100"
item "example::Meter.bump"
text "pub fn Meter.bump(mut self)"
text "    ensures self.value() >= old(self.value())"
interface "<hash>"
digest any "effects none | authority none | may-trap no | terminates"
doc "<hash>" "Adds one unit, staying at the limit once reached."
head "pub fn Meter.bump(mut self)"
accepted "ensures self.value() >= old(self.value())"
test "example::bump_counts" "" "<hash>"
item "example::Meter.empty"
text "pub fn Meter.empty() -> Meter"
interface "<hash>"
digest any "effects none | authority none | may-trap no | terminates"
item "example::Meter.value"
text "pub fn Meter.value(self) -> u32"
interface "<hash>"
digest any "effects none | authority none | may-trap no | terminates"
check "<hash>"
```

### Principals file

```ebnf
principals_file = COL0 'nan' 'principals' INT NL COL0 principals_tail ;
principals_tail = principal_record NL COL0 principals_tail | check_record NL EOF ;
principal_record = 'principal' TEXT TEXT ;
```

Each `principal P K` record gives the principal P the public key whose [key form](31-trust.md#principals) is K, in byte order of P, P being in the principal form.

## Diagnostics

| Code | Class | Rule |
| --- | --- | --- |
| `format.machine-invalid` | error | [Common syntax](#common-syntax) |
| `format.machine-version` | error | [Common syntax](#common-syntax) |
| `format.reserved-field` | error | [Lock](#lock) |
