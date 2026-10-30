# Identity and versioning

This chapter defines canonical paths and nominal identity; the normalised form, item and interface hashes, instantiation keys and hash values; tombstones, forwarding and the history; compatibility per item and role; releases with their classes and publish summaries; behaviour epochs; requires-sets; the local store and publishing; and the lock with resolution, upgrades and approvals.

## Canonical paths

The **canonical path** of a package's [root module](03-packages-modules.md#modules) is the package name, and that of another module is the package name, `::` and the module's name. An item's canonical path is its module's, `::` and its name: `meter::Total`, `meter::units::Scale`. A [member](03-packages-modules.md#paths)'s is its type's or interface's, `.` and its name: `meter::Total.add`.

- The `package` item's canonical path is the package name.
- An impl line's is its module's, `::impl[`, its impl target's, `: `, its interface's and `]`: `meter::impl[meter::Total: core::Hash]`.
- A function named `Type.name` declared outside `Type`'s package has the canonical path of the impl line of its module whose impl target is `Type` and whose interface declares `name`, then `.` and `name`: `meter::impl[core::u64: meter::Weighted].weight`.
- A const assert's is its module's, `::assert[`, the short form of its item hash and `]`. A tombstone has none.
- A core vocabulary name's is `core::` and the name.
- Generic arguments, [target variant](03-packages-modules.md#target-variants) suffixes and file names never enter a canonical path.
- Tool output writes a path as the reading package would ([paths](03-packages-modules.md#paths)), and writes the canonical path of an item it reaches through no alias.

An item has **nominal identity**: two items are the same item exactly when their canonical paths are equal, and two types are the same type exactly when their canonical paths and generic arguments are equal, whatever their structure. Renaming or moving an item gives it a new identity, which only a [tombstone](#tombstones) connects to the old one.

Among the canonical paths of the next example are `meter::units`, `meter::Total.sum` and `meter::units::Scale.Kilo`.

```nan
// file: meter/package.nan
//! Running totals.

package meter {
    language 1
    targets any
} meter

// file: meter/meter.nan
//! Totals of samples.

pub closed struct Total {
    pub sum: u64
} Total

pub fn Total.add(mut self, sample: u64) {
    self.sum = self.sum.saturating_add(sample)
} Total.add

pub fn Total.scaled(self, scale: units::Scale) -> u64 {
    return match scale {
        units::Scale.Base => self.sum
        units::Scale.Kilo => self.sum / 1_000
    }
} Total.scaled

// file: meter/units/units.nan
//! Scales of reported totals.

pub closed enum Scale {
    Base
    Kilo
} Scale
```

## Hashes

Every item has an item hash and an interface hash. Source never contains a hash or a version.

### Normalised form

The **normalised form** of an item is the text `nan fmt` writes for it in [canonical format](35-format.md#canonical-format), changed by these steps in order:

1. Comments, doc comments and blank lines are removed; every [breakable list](35-format.md#line-breaking) is written flat and every logical line on one line, spaced as [Spacing](35-format.md#spacing) states.
2. Each path naming an item, or a member through its type, becomes its canonical path; core vocabulary names stay bare, and a member reached through a value (`self.sum`) stays a name.
3. Each literal keeps its suffix and is written canonically: an integer in decimal without separators or leading zeros; a float as the shortest decimal, with a digit after `.`, that reads back as the same value of its type, the nearest to that value among those, the greater of two equally near; a text run as one `"..."` literal; in text, char, byte and byte string literals, `\xHH` with uppercase digits for the delimiter, the backslash, each code point or byte below `0x20` or equal to `0x7F` and each byte above `0x7F`, every other character as itself.
4. Order-free members are sorted in byte order of their text with every binding name replaced by `$`, inner lists first: fields, labelled payload fields, variants, invariant lines, `limits` and `derives` entries, and the arms of a `match` other than `match first`. The fields, labelled payload fields and variants of a type deriving `Ord`, `Render` or `Generate` keep their declaration order.
5. Each binding (`let` and `var` bindings, pattern bindings, loop elements, closure parameters, quantifier variables, loop labels) is renamed `$1`, `$2` and so on in order of its first introduction in the text, and each capture with the binding it captures. Parameters and generic parameters of the item keep their names.

Every line ends with LF. The normalised form of a statement or clause follows the same steps from its own text, numbering only the bindings it introduces.

### Item hash and interface hash

The **item hash** is the hash of the UTF-8 bytes `item`, LF and the item's normalised form.

The **interface hash** is the hash of the UTF-8 bytes `interface`, LF and the item's **interface form**, which is its normalised form reduced to what code naming the item sees:

| Kind | Interface form |
| --- | --- |
| `fn` | its visibility, modifiers, head and convention clause, then its `requires`, `ensures` and `example` clauses with their `todo` markers; for a transparent function that is not generic, is no member of a generic type, and whose body names only fields, items and members visible wherever the function is visible, also `=` and the body |
| `struct` | the head with visibility, modifiers, generic parameters and `derives`; each field visible wherever the struct is visible, with its type and `where` predicate; each invariant line naming only such fields; then the facts line |
| `enum` | the whole normalised form, then the facts line |
| `layout`, `registers`, `impl`, `registry`, `contribute`, `retired`, `const assert` | the whole normalised form |
| `interface` | the normalised form with each default body replaced by the word `default` |
| `const` | the head, `=` and, separated by single spaces, the lowercase hexadecimal of the [canonical NIR encoding](19-nir.md#encoding) of the value that [const evaluation](21-interpreter.md#const-evaluation) gives under each declared concrete target, in byte order of target name, or once for a package declaring `any` |
| `test` | the line `test <name> for <target>` |
| `package` | the `package` line and the `language` and `targets` directives |

- The **facts line** is `facts` followed, each after one space, by those of `linear`, `view`, `token` (a state token), `holds-token` (containing one), `Copy`, `Send`, `Inert`, `AnyBits`, `parsing` (a `pub` struct or enum that is a [parsing type](15-untrusted.md#validator-classes)) and `holds-untrusted` (a `pub` struct or enum deriving `Encode` that [holds an Untrusted instantiation](15-untrusted.md#validator-classes)) that the type's [interface facts](05-properties.md#interface-facts) state, in that order, each fact resting on dependent parameters followed by their names in `[ ]`, separated by `, `.
- Bodies, `decreases`, `waive` and clause-region `todo` clauses never enter an interface form; a change confined to a body changes the item hash and never the interface hash. [Dyn-usability](08-generics-interfaces.md#dynamic-dispatch) follows from the method heads.

The next item's normalised form is the first text and its interface form the second.

```nan
pub closed struct Gauge {
    pub level: u32
} Gauge

/// Raises the level by `step`, saturating at the maximum.
pub fn Gauge.raise(mut self, step: u32)
    ensures self.level >= old(self.level)
{
    let next = self.level.saturating_add(step)
    self.level = next
} Gauge.raise
```

```text
pub fn example::Gauge.raise(mut self, step: u32)
    ensures self.level >= old(self.level)
{
    let $1 = self.level.saturating_add(step)
    self.level = $1
} example::Gauge.raise
```

```text
pub fn example::Gauge.raise(mut self, step: u32)
    ensures self.level >= old(self.level)
```

### Instantiation keys

The **instantiation key** of an [instantiation](08-generics-interfaces.md#instantiation) is the hash of the UTF-8 bytes `instance`, LF, the full form of the generic item's item hash, LF, then one line per generic argument:

- a const argument writes its literal in canonical form;
- a type or interface argument writes the full form of its **type hash**: for a function type, the hash of `type`, LF and its normalised text; otherwise the hash of `type`, LF, its core vocabulary name or the full form of its item's item hash, LF, then one line per generic argument of its own as here.

Two instantiations with equal keys are one instantiation.

### Hash values

A **hash** is 33 bytes: the **algorithm tag** `0x01`, naming BLAKE3-256, then the 32-byte BLAKE3-256 digest of the hashed bytes. Other tag values are reserved; a byte string holding one is no hash. Two hashes are equal exactly when all their bytes are.

- An absent optional field, such as a clause, a modifier or a record field, contributes no bytes to what is hashed, so a format gaining an optional field leaves the hashes of everything without it unchanged.
- The **checksummed base32** of a byte string is its encoding in the alphabet `abcdefghijklmnopqrstuvwxyz234567`, most significant bit first, the last character padded with zero bits, followed by 2 characters encoding the first 10 bits of the BLAKE3-256 digest of the byte string.
- The **short form** of a hash is the checksummed base32 of its tag and the first 9 bytes of its digest, 18 characters; the **full form** that of all 33 bytes, 55 characters. Tools print short forms; [machine files](35-format.md#machine-files) hold full forms.
- A text is a hash's short or full form only when it has 18 or 55 characters of the alphabet, zero padding bits and a correct checksum.

## Tombstones

A **tombstone** is a `retired` item recording a retired form of its package and the form's rewrite.

```ebnf
retired_item = "retired" retired_old "=>" retired_new NL ;
retired_old = path ( "." IDENT )? retired_head? ;
retired_head = generic_params? "(" param_list? ")" fn_result? fails_clause? uses_clause? ;
retired_new = 'manual' TEXT | expr retired_class ;
retired_class = 'preserves' | 'changes' TEXT ;
```

`manual` is a context word exactly when a `TEXT` token follows it.

- The **old form** is `retired_old`: a path, optionally `.` and a member name, and for a retired function the **old head**. Its path is read as written in the tombstone's module, except that a module segment may name a module without files. An old path whose first segment is an alias is an error (`retired.foreign-path`).
- The **rewrite** is the `expr` of `retired_new`. `preserves` states that it behaves as the old form did, and `changes` that it behaves differently, as its **note** says; `manual` gives no rewrite, only a reason.
- A rewrite is an error (`retired.bad-rewrite`) unless:
  - with an old head, it is a call or a path whose only markers are `mut` and `take`, and it checks as the one statement of a function of the tombstone's module having the old head: `return` and the rewrite, or the rewrite alone without a result, preceded by the `try` and `await` its outermost call needs;
  - without an old head, it is a path, optionally followed by `.` and a member name, and a member of the old form's type when the old form is a member;
  - each name in it resolves to a live item or member, directly or through `preserves` tombstones of the package.
- A **rename tombstone** is a `preserves` tombstone without an old head; [edits](33-toolchain.md#edits) write them.
- The **replacement** is what the rewrite's path or outermost call names. A `preserves` or `changes` old form is visible exactly where its replacement is, and a `manual` one to consumers. Every other item and member a rewrite names must be visible wherever the replacement is (`retired.rewrite-hidden`).
- A tombstone's **identity** is its old form's canonical path and, with an old head, its parameters' modes and normalised types in order; two in one module with one identity are an error ([`item.duplicate`](02-items.md#order-and-names)).
- An old form is **taken** by a live item or member with its canonical path that, for an old head, is not a function or has the old head's parameter modes and types, and by a tombstone of another module or of the [history](#history) with its identity. A tombstone in source whose old form a live item or member or a history tombstone takes is an error (`retired.form-taken`), and so is each tombstone in source but the first, by file path and line, of a set in different modules with one identity. A live item or member taking a history tombstone's old form is that error, reported on it.

```nan
pub closed struct Counter {
    pub hits: u32
} Counter

pub fn Counter.record(mut self) {
    self.hits = self.hits.saturating_add(1)
} Counter.record

pub fn Counter.total(self) -> u32 = self.hits

retired Counter.count(self) -> u32 => self.total() preserves

retired Counter.bump(mut self) => mut self.record() changes "the count saturates instead of trapping"

retired Counter.clear => manual "build a new Counter with hits 0"
```

```nan
pub closed struct Counter {
    pub hits: u32
} Counter

pub fn Counter.total(self) -> u32 = self.hits

retired Counter.count(self) -> u64 => self.total() preserves // error: retired.bad-rewrite
```

```nan
pub closed struct Counter {
    pub hits: u32
} Counter

pub fn Counter.total(self) -> u32 = self.hits

pub fn Counter.count(self) -> u32 = self.hits

retired Counter.count(self) -> u32 => self.total() preserves // error: retired.form-taken
```

```nan
pkg const CAP: u32 = 1_000

pub closed struct Counter {
    pub hits: u32
} Counter

pub fn Counter.capped(self, ceiling: u32) -> u32 {
    if self.hits > ceiling {
        return ceiling
    }
    return self.hits
} Counter.capped

retired Counter.count(self) -> u32 => self.capped(CAP) preserves // error: retired.rewrite-hidden
```

### Forwarding

A use **matches** an old form visible to it when the canonical path its path, or its member through a type or value, would have is the old form's, and:

- without an old head, that path or member names no live item or member;
- with an old head, the use is a call whose arguments, labels and markers check against the old head's parameters as against a function's ([calls](06-statements-expressions.md#calls)) and not against the live function its path names, if any.

A use within its package may match the tombstones of the package's source and history, and a use through an alias those of the [code root](30-artefacts.md#code-roots) of the release the [lock](#lock) names.

- A use matching two old forms is an error (`retired.ambiguous-use`), quoting both tombstones.
- A use matching a `preserves` old form **forwards**. With an old head it [elaborates](19-nir.md#elaboration) as its arguments, evaluated in [evaluation order](06-statements-expressions.md#evaluation-order) into the old head's parameters, then the rewrite over them; without one, as the same use with the rewrite's path or member name in place of the old.
- Each forwarding use is incompleteness (`retired.forwarded-use`). Its exact fix writes the rewrite in its place with the using module's path forms, each parameter replaced by its argument's text without label or marker, keeping the use's `try`, `attempt` and `await`. It exists only when the parameter of each argument other than a place or a literal occurs exactly once in the rewrite, these occurrences being evaluated in argument order and before any call.
- A use matching a `changes` or `manual` old form does not forward; it is [`name.moved`](03-packages-modules.md#unknown-names).

```nan
pub closed struct Counter {
    pub hits: u32
} Counter

pub fn Counter.total(self) -> u32 = self.hits

retired Counter.count(self) -> u32 => self.total() preserves

pub fn is_idle(counter: Counter) -> bool {
    return counter.count() == 0 // incomplete: retired.forwarded-use
} is_idle
```

```nan
pub closed struct Step {
    pub by: u64
} Step

pub closed struct Counter {
    pub hits: u64
} Counter

pub fn Counter.add(mut self, step: Step) {
    self.hits = self.hits.saturating_add(step.by)
} Counter.add

retired Counter.add(mut self, step: u32) => mut self.add(Step(by: u64.widen(step))) preserves

retired Counter.add(mut self, step: u16) => mut self.add(Step(by: u64.widen(step))) preserves

pub fn tally(mut counter: Counter) {
    mut counter.add(Step(by: 5))
    mut counter.add(1) // error: retired.ambiguous-use
} tally
```

### History

The **history** of a package is its [machine file](35-format.md#machine-files) `.nan/history.nan`, recording its store name, its [history head](#publishing) and every tombstone [publishing](#publishing) moved out of source; tombstones keeping an `Encode` [key](13-text-encode.md#keys) stay in source while their type derives `Encode`. A release's [code root](30-artefacts.md#code-roots) holds the history's tombstones, never its history head or store name. History tombstones forward, match and take old forms exactly as tombstones in source do.

## Compatibility

**Compatibility** decides whether an item a consumer used, in the roles it used it in, may be replaced by the item of another release with its canonical path, or by the replacement a `preserves` tombstone there forwards its old form to.

### Roles

A **role** is a way of using an item; a consumer's [requires-set](#requires-sets) records the roles in which it uses each foreign item:

| Role | The consumer |
| --- | --- |
| `call` | calls the function, directly or by forwarding, or uses it as a function value |
| `build` | constructs or destructures the closed struct, or matches the closed enum without `else` |
| `implement` | conforms a type to the interface by an impl line |
| `dispatch` | calls the interface's methods through a bound, `Dyn` or a qualified call |
| `name` | uses the item otherwise: as a type, through a field, by constructing a variant, reading a const or contributing to a registry |

### Compatible changes

An item is compatible with another for a role when their interface hashes are equal, or when each difference between their interface forms is a change listed here as compatible for that role. Every other difference is breaking.

| Change | Compatible for |
| --- | --- |
| A new variant of an open enum that leaves every [promise](05-properties.md#promises) of the enum in place | every role |
| A new field of an open struct that leaves every promise in place, of a type implementing `Default` when the struct derives `Encode` | every role |
| A new interface method with a default body, the interface staying Dyn-usable if it was | every role |
| A weaker `requires` or a stronger `ensures` on a function that is not an interface method | `call`, `name` |
| Changed examples, every example of the old item holding when the [interpreter](21-interpreter.md#interpreter) runs it against the new release | every role |
| A new promise: a property gained, or a limit other than `stack N` added | every role |
| The removal of the item while a `preserves` tombstone forwards its old form, its replacement being compatible as if it had the old canonical path | every role |
| A `pub` struct or enum that stops being a [parsing type](15-untrusted.md#validator-classes) | no role |

- Weaker and stronger are decided by [entailment](20-prover.md#entailment): the old requires entail the new requires, and the new ensures entail the old ensures under the old requires. A query that is not proved makes the change breaking. Each entailment is judged from the new release's item, matches the old and new heads' parameters by position and fields by name, and treats a clause naming a parameter, `result` or field that only one side has as [weakening](31-trust.md#weakening) treats it.
- A new item, impl lines and contributions included, changes no item a consumer used; a duplicate key or unique value it brings into a closure is found by [resolution](#resolution), which checks the package against the assignment.

### Facts outside the interface hash

- A [digest fact](10-effects-digests.md#digests) or digest-set element that an item gains, and terminates when the item loses it, breaks exactly the consumers whose requires-set recorded reliance on its absence, or on terminates; the opposite changes are compatible.
- A differing [behaviour epoch](#behaviour-epochs) is breaking for every role.
- Final-link roots [approve](#approvals) epoch changes and [widenings](14-authority.md#authority-summaries); a widening breaks consumers only through the digest facts and elements giving it.
- Any other fact these rules do not model is breaking whenever its value differs, except a fact kind a later language version introduces, read as [Requires-sets](#requires-sets) states.

## Releases

A **release** is a state of a package recorded in the [local store](#local-store), a named release or a path release, identified by the hash of its [code root](30-artefacts.md#code-roots): two releases are one release exactly when their code roots are equal.

The **exported items** of a release are its `pub` items, impl lines and contributions, and the tombstones whose old forms are visible to consumers. Under each declared target, each has a class relative to the release's [parent](#publishing):

- **identical** when its interface hash, behaviour epoch and digest are unchanged;
- **additive** when it is new, or when each change to its interface form is compatible for every role, its epoch is unchanged and its digest gains no fact or set element and keeps terminates;
- **breaking** otherwise, including when it is removed or loses `pub` without a `preserves` tombstone forwarding it.

The **release class** is the worst class over the exported items and targets, in the order identical, additive, breaking. A release without a parent is additive. No option, file or command lowers a computed class.

A release of the second block after one of the first is additive: `Mode` gains a variant of an open enum, `Mode.level` changes only its body and `Mode.is_idle` is new. Its encode class is additive too.

```nan
pub open enum Mode derives Eq, Encode {
    Idle
    Busy
} Mode

pub fn Mode.level(self) -> u8 {
    return match self {
        Mode.Idle => 0
        Mode.Busy => 2
    }
} Mode.level
```

```nan
pub open enum Mode derives Eq, Encode {
    Idle
    Busy
    Draining
} Mode

pub fn Mode.level(self) -> u8 {
    return match self {
        Mode.Idle => 0
        Mode.Busy => 2
        Mode.Draining => 1
    }
} Mode.level

pub fn Mode.is_idle(self) -> bool = self == Mode.Idle
```

### Encode class

The **encode class** of a release is the worst class of the exported types implementing `Encode` in it or its parent, comparing their [schema trees](13-text-encode.md#schema-hashes): identical when the schema hash is unchanged, additive when the type is new or its new tree accepts its old one, and breaking otherwise, including when the type no longer is such a type. A new tree N **accepts** an old tree O when they are nodes of one kind and:

- `Unsigned`, `Signed` and `Float` nodes have equal widths, a `BoundedText` node has a capacity at least O's, and a `Back` node has O's depth;
- a `Sequence` node accepts O's element, has O's length, has no capacity or one at least O's, and is unique only when O is;
- a `Map` node accepts O's key and value;
- in a `Record` node, a field of N whose key or aliases include the key of a field of O has a value accepting that field's; a field of O that no field of N matches exists only when N is open; and a field of N matching no field of O is defaulted;
- in a `Variant` node, each variant of O is matched by a variant of N whose key or aliases include its key, without a payload when O's has none and with a payload accepting O's otherwise;
- nodes of the other kinds always.

### Publish summaries

The **publish summary** of a release lists its release class and encode class; each exported item with its class and behaviour epoch; each epoch bump with its reason; the entries its [authority summary](14-authority.md#authority-summaries) gains and loses against the parent; and, for each exported function and each target, the methods of primitive capabilities it reaches and its trap sites, each with its [trap kind](11-errors-domains.md#traps), file path and line.

## Behaviour epochs

A **behaviour epoch** is a per-item counter recording behaviour changes that the interface does not show. An item new in a release has epoch 0, or the epoch of the item a rename tombstone of that release retires into it; every other item keeps its parent's epoch unless the publish bumps it.

`nan publish --local <dir> --breaking <item>... "<reason>"` bumps by 1 the epoch of each listed item, named as [commands](33-toolchain.md#commands) state, which makes it breaking, and records the reason in the publish summary. A listed item that is not an exported item of both the parent and the new release is an error (`version.breaking-target`), reported on the `package` line.

The second release of `mean` rounds to the nearest integer. Its interface is unchanged, so the release is identical unless published with `--breaking mean "rounds to the nearest integer"`.

```nan
pub fn mean(total: u64, count: u64) -> u64
    requires count > 0
{
    return total / count
} mean
```

```nan
pub fn mean(total: u64, count: u64) -> u64
    requires count > 0
{
    return total.saturating_add(count / 2) / count
} mean
```

## Requires-sets

The **requires-set** of a package records what its acceptance relies on in other packages, std included: one entry per foreign item whose head, declaration, contract, example, digest, impl line or body a rule consults while checking the package. An entry holds, in this order:

1. the item's canonical path, or the old form a forwarding use matched together with its replacement's canonical path;
2. the roles in which the package uses the item;
3. its interface hash and behaviour epoch in the release checked against;
4. each digest fact, and each capability or validator of a digest set, whose absence the package relied on, and terminates when it relied on its presence, through a limit, a fit or purity ([digests](10-effects-digests.md#digests));
5. for each method name resolved on the item as a type, the canonical path of the function [method resolution](08-generics-interfaces.md#method-resolution) found;
6. the item hash, when the package instantiates the item or unfolds its transparent body;
7. for an interface the package implements by an impl line, the names of the functions named after that line's impl target in the line's module that are not methods of the interface.

A requires-set **is satisfied by** a set of releases, one per package, when for each entry the item of that package's release is compatible for each recorded role, each recorded reliance still holds with digests re-derived across the set, each recorded method name resolves to the same function, no name recorded for an implemented interface is a method of that interface, and each recorded item hash is equal. A release whose requires-set its closure does not satisfy is checked again from its NIR ([reuse](30-artefacts.md#reuse)).

An artefact of an earlier [language version](37-evolution.md#language-versions) holds no value for a fact kind a later version introduces; a toolchain reading it takes that fact at its worst value ([digests](10-effects-digests.md#digests)).

In the next example, the requires-set of `report` has an entry for `meter::Total.add` with role `call`, its interface hash, epoch 0 and the relied absence of alloc, and one for `meter::Total` with role `name` and `add` resolving to `meter::Total.add`.

```nan
// file: meter/package.nan
//! Running totals.

package meter {
    language 1
    targets any
} meter

// file: meter/meter.nan
//! Totals of samples.

pub closed struct Total {
    pub sum: u64
} Total

pub fn Total.add(mut self, sample: u64) {
    self.sum = self.sum.saturating_add(sample)
} Total.add

// file: report/package.nan
//! Reports meter totals.

package report {
    language 1
    targets any
    dep meter "../meter"
} report

// file: report/report.nan
//! Adds pairs of samples to a meter total.

pub fn add_pair(mut total: meter::Total, left: u64, right: u64) limits noalloc {
    mut total.add(left)
    mut total.add(right)
} add_pair
```

## Local store

The **local store** is the store of one user's releases, in the user directory ([cache](36-builds.md#cache)), whose entries are never removed or rewritten. Each entry is a release with its artefact, package name and **publication index**, 1 for the first entry and increasing by 1; a named release also has its store name, parent and publish summary.

- A **named release** is appended by `nan publish --local` under a **store name**, which store-name sources of `dep` directives name ([aliases](03-packages-modules.md#aliases)). Publishing a package under a store name holding releases of another package is an error (`version.store-name-taken`), reported on the `package` line.
- A **path release** is the state of a path source that `nan dep sync` locks, appended without a store name.
- Appending a named release whose code root the store holds under that store name, or a path release whose code root the store holds, appends nothing.

### Publishing

`nan publish --local <dir> [--name <store name>]` publishes the package in directory `<dir>`:

1. It runs the [release gate](31-trust.md#release-gate) on the package and stops when the gate rejects it.
2. A package with a path-source dependency is not published (`version.path-dependency`), reported on that `dep` directive.
3. The release's **parent** is the **history head**, the code root hash of the release last published from the package; without a history head, it has none. A history head that is not in the local store is an error (`version.parent-missing`), reported on the `package` line.
4. It checks each `preserves` tombstone that the parent's code root does not hold and whose old form names an exported item or member of the parent. A path rewrite preserves when the replacement, given the old canonical path, has the old item's interface form in the parent, or, for a field or variant, when the live member that the rewrite's member reaches through the release's `preserves` tombstones has the old declaration line with its own name in place of the old one. A head rewrite preserves when the prover proves every obligation of the **forwarding function**, which has the old head, the old item's requires and ensures in the parent, and a body returning the rewrite; or else when the old item has examples and each holds when the interpreter runs it against the new release, its uses of the old item forwarding. Any other such tombstone is an error (`retired.not-preserved`).
5. It applies `--breaking` and computes each exported item's class and epoch, the release class, the encode class and the publish summary.
6. It records the store name in the history: `--name` when given, else the one recorded, else the package name. It then moves every tombstone in source into the history, except a tombstone keeping an `Encode` [key](13-text-encode.md#keys) of a type that derives `Encode`; then it builds the [artefact](30-artefacts.md#artefacts), appends the release under the store name and records it as the history head.

## Lock

The **lock** of a package is its machine file `.nan/lock.nan`, written only by `nan dep add`, `nan dep sync`, `nan dep approve`, `nan upgrade` without `--language` and the [merge driver](33-toolchain.md#merge-driver). It records:

- the **snapshot**, the greatest publication index of the local store when the lock was written;
- one checksummed line per package of the [dependency closure](27-program-model.md#final-builds) other than the package itself and std: the package's name and, for a direct dependency, its alias ([lock syntax](35-format.md#lock)), its source as its `dep` directive writes it with a path source rewritten relative to the locking package's directory, and the code root hash of its **locked release**;
- for a [final-link root](27-program-model.md#final-builds), its [approvals](#approvals).

[Canonical format and machine files](35-format.md#lock) defines the lock's syntax, validity and reserved fields. The checker reads each other package of the closure but std through its locked release: from source when that release is the current state of its path source, and otherwise from its artefact in the local store.

- A `dep` directive without a lock line for its source, a lock line whose source differs from its directive's, and a missing or surplus line for a package reached through others are errors (`dep.lock-stale`), reported on the directive or else on the `package` line.
- A locked release that is neither the current state of its path source nor in the local store is an error (`dep.release-missing`), reported on the first `dep` directive, in alias order, that reaches it.
- A dependency closure holds one release per package name. Two sources or two releases of one package name are an error (`dep.split-package`), reported on each `dep` directive of the checked package that reaches that name, except the first in alias order.
- A package in its own dependency closure is an error (`dep.cycle`), reported on the first `dep` directive, in alias order, that reaches it.

In the next example, `app` and `report` use the one release of `meter` that the lock of `app` names.

```nan
// file: meter/package.nan
//! Running totals.

package meter {
    language 1
    targets any
} meter

// file: meter/meter.nan
//! Totals of samples.

pub closed struct Total {
    pub sum: u64
} Total

pub fn Total.add(mut self, sample: u64) {
    self.sum = self.sum.saturating_add(sample)
} Total.add

// file: report/package.nan
//! Reports meter totals.

package report {
    language 1
    targets any
    dep meter "../meter"
} report

// file: report/report.nan
//! Adds samples in pairs.

pub fn add_pair(mut total: meter::Total, left: u64, right: u64) {
    mut total.add(left)
    mut total.add(right)
} add_pair

// file: app/package.nan
//! Tallies samples.

package app {
    language 1
    targets any
    dep meter "../meter"
    dep report "../report"
} app

// file: app/app.nan
//! Tallies of sample pairs.

pub fn tally(left: u64, right: u64) -> u64 {
    var total = meter::Total(sum: 0)
    report::add_pair(mut total, left, right)
    return total.sum
} tally
```

### Resolution

`nan dep sync` resolves the dependency closure of a package P and writes its lock. A source's **candidates** are:

- for a path source, exactly the release of the path's current state, appended to the local store as a path release when locked;
- for a store name under which P's lock names a release, that release and every named release under that store name of which it is an ancestor through parents;
- for any other store name, every named release under it.

An **assignment** gives one candidate to each source of P's `dep` directives and, in turn, to each source that a `dep` directive of an assigned release names. It is **acceptable** when:

1. its closure holds one release per package name and no cycle;
2. P and the package of each assigned path release check from source against it without error, with digests re-derived across the closure;
3. the [reuse](30-artefacts.md#reuse) of every other assigned release reports no error;
4. P's [reliance tests](22-testing.md#reliance-tests) and [affected tests](22-testing.md#affected-tests) pass, and so do the reliance tests recorded in each assigned release whose targets lie in a package assigned another release than the lock names.

`nan dep sync` writes the greatest acceptable assignment, comparing assignments by the publication index of each package's release, 0 for a path release or an absent package, over the union of their package names in byte order, the first difference deciding. When none is acceptable, it searches again ignoring P's own errors in condition 2 and, while P has any, P's tests in condition 4, so that a package whose own source does not check still locks its closure; when none is acceptable then either, it is an error (`dep.unresolved`), reported on P's `package` line with the diagnostics of each condition that the assignment of each source's newest candidate fails. For each package whose locked release changes, `nan dep sync` prints both code roots, the release class of the new one with the old as its parent, and the epoch changes and widenings it brings.

### Upgrades

`nan upgrade` without `--language` resolves as `nan dep sync` does, except that condition 2 accepts `name.moved` errors and checks P's source after replacing in memory each use matching a `preserves` or `changes` tombstone of a candidate by the exact fix forwarding gives it, when one exists. It writes the lock and those replacements, and reports each use still matching a `changes` or `manual` tombstone as `name.moved`.

### Approvals

The lock of a final-link root records **approvals**: pairs of its [closure summary](14-authority.md#authority-summaries), and **epoch pairs** of a canonical path and a behaviour epoch.

- A final closure **needs** each pair of its [closure summary](14-authority.md#authority-summaries); a needed pair the lock does not approve is an error (`authority.widening-unapproved`), as authority summaries state.
- It also needs, for each requires-set entry of each of its packages that names a non-std item whose epoch in the assigned release is above 0, the pair of that item's canonical path and epoch. A needed epoch pair the lock does not approve is an error (`dep.epoch-unapproved`), reported on the root's `package` line and listing every such pair; `nan check`, `nan test`, `nan build` and `nan run` of the root report it.
- `nan dep approve` adds every needed pair of both kinds to the lock, and a lock written for a package that had none approves every pair it needs. Nothing else adds an approval, and a written lock holds only needed approvals.

## Diagnostics

| Code | Class | Rule |
| --- | --- | --- |
| `dep.cycle` | error | [Lock](#lock) |
| `dep.epoch-unapproved` | error | [Approvals](#approvals) |
| `dep.lock-stale` | error | [Lock](#lock) |
| `dep.release-missing` | error | [Lock](#lock) |
| `dep.split-package` | error | [Lock](#lock) |
| `dep.unresolved` | error | [Resolution](#resolution) |
| `retired.ambiguous-use` | error | [Forwarding](#forwarding) |
| `retired.bad-rewrite` | error | [Tombstones](#tombstones) |
| `retired.foreign-path` | error | [Tombstones](#tombstones) |
| `retired.form-taken` | error | [Tombstones](#tombstones) |
| `retired.forwarded-use` | incompleteness | [Forwarding](#forwarding) |
| `retired.not-preserved` | error | [Publishing](#publishing) |
| `retired.rewrite-hidden` | error | [Tombstones](#tombstones) |
| `version.breaking-target` | error | [Behaviour epochs](#behaviour-epochs) |
| `version.parent-missing` | error | [Publishing](#publishing) |
| `version.path-dependency` | error | [Publishing](#publishing) |
| `version.store-name-taken` | error | [Local store](#local-store) |
