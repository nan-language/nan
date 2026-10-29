# Packages, modules and names

This chapter defines packages and their manifest, aliases, modules, target variants, paths and members, visibility, the core vocabulary, naming conventions, checked name prefixes and the diagnostics for uses that name nothing.

## Packages

A **package** is a directory holding a `package.nan` file; it is the unit of dependency and release. It consists of the [source files](01-lexical.md#source-files) whose nearest directory at or above them holding a `package.nan` is its directory. The toolchain ignores files and directories whose names start with `.`.

- A source file that no package holds, and a `package.nan` without a `package` item, are errors (`manifest.missing`), reported on the file's first line.
- The package name is the `IDENT` after `package` in its manifest; the name of its directory carries no meaning.

```nan
// file: hello/package.nan
//! Greets on the host log.

package hello {
    language 1
    targets x86_64-host
    entry main
} hello

// file: hello/hello.nan
//! Entry of the greeting program.

fn main(take world: host::World) fails task::Cancelled uses wait {
    world.log.info("answer", value: 42u32)
} main
```

## Manifest

The **manifest** is the `package` item of `package.nan`. That file holds its `//!` summary, comments and the manifest, and nothing else: a `package` item in another file and any second item in `package.nan` are errors (`manifest.misplaced`), reported on that item.

```ebnf
package_item = "package" IDENT "{" directive* COL0 "}" item_label NL ;
directive = language_directive | targets_directive | entry_directive
    | dep_directive | allow_directive | deny_directive ;
language_directive = 'language' INT NL ;
targets_directive = 'targets' target_list NL ;
target_list = target ( "," target )* ;
target = IDENT ( "-" IDENT )? ;
entry_directive = 'entry' IDENT NL ;
dep_directive = 'dep' IDENT TEXT NL ;
allow_directive = 'allow' authority "in" name_list NL ;
authority = 'device' | 'priv' | "trusted" | 'validate' | "contribute" path ;
name_list = IDENT ( "," IDENT )* ;
deny_directive = 'deny' IDENT "->" IDENT NL ;
```

A **directive** is one manifest line:

- `language` states the [language version](37-evolution.md#language-versions) of the package.
- `targets` lists the [targets](27-program-model.md#targets) the package is checked and built for, among `x86_64-host`, `x86_64-uefi`, `x86_64-bare`, `aarch64-host`, `aarch64-uefi`, `aarch64-bare` and `any`. `x86_64-nanos` and `aarch64-nanos` are reserved: listing either is an error (`manifest.reserved-target`). Any other name is an error (`manifest.unknown-target`). `any` listed with another target is an error (`manifest.any-not-alone`).
- `entry` names the [entry function](27-program-model.md#entry).
- `dep` declares an [alias](#aliases).
- `allow` is a [manifest grant](14-authority.md#manifest-grants) of its authority to the names listed; `contribute` takes the path of a registry.
- `deny a -> b` is a [denied dependency](14-authority.md#denied-dependencies) of module `a` on `b`; `a` and `b` differ (`manifest.deny-self`).
- [Manifest grants](14-authority.md#manifest-grants) fixes which names `allow` and `deny` directives may list.

`language` and `targets` appear exactly once and `entry` at most once; a missing `language` or `targets` directive is an error (`manifest.missing-directive`), reported on the `package` line. These are errors (`manifest.duplicate`), reported on the later occurrence:

- a second `language`, `targets` or `entry` directive;
- two `dep` directives with the same alias, or with sources naming the same store name or the same directory once `.` and `..` segments are resolved;
- two `allow` directives with the same authority, `contribute` with one registry path counting as one authority;
- two `deny` directives with the same `a` and `b`;
- a name listed twice in one `targets` or `allow` directive.

`nan fmt` writes directives in this order: `language`, `targets`, `entry`, then `dep` directives by alias, `allow` directives by authority as written and `deny` directives by `a` and then `b`, with the names of each list sorted; every ordering it applies is byte order.

```nan
// file: wire/package.nan
//! Link header decoding.

package wire {
    language 1
    targets any
    allow validate in wire
    deny wire -> host
} wire

// file: wire/wire.nan
//! Decodes the version byte of link headers.

pub closed enum VersionError derives Eq, Render {
    Empty
} VersionError

pub validator fn decode_version(raw: Untrusted[Slice[u8]]) -> u8 fails VersionError
    example attempt decode_version(Untrusted.of(b"")) == Err(VersionError.Empty)
{
    let bytes = raw.inspect()
    let Some(head) = bytes.prefix(1) else {
        fail VersionError.Empty
    }
    return head[0]
} decode_version
```

```nan
// file: wire/package.nan
//! Link header limits.

package wire {
    language 1
    targets any, x86_64-host // error: manifest.any-not-alone
} wire

// file: wire/wire.nan
//! The largest link payload.

pub const MAX_PAYLOAD: u16 = 1_400
```

## Aliases

An **alias** is the name a package uses for a dependency or a std package; it is the first segment of every path reaching that package's items.

`dep alias "source"` declares `alias` for the package at `source`, the value of the `TEXT` literal, which is one of:

- a relative path: `.` or `..`, then zero or more segments each preceded by `/`, each segment non-empty and made of ASCII letters, digits, `.`, `-` and `_`; it names the dependency's package directory relative to the directory holding `package.nan`;
- a store name: one or more segments separated by `/`, each made of lowercase ASCII letters, digits, `.`, `-` and `_` and starting with a letter or digit; it names the [named releases](29-identity-versioning.md#local-store) under that store name.

Any other source, such as one carrying a version or an absolute path, is an error (`manifest.bad-source`). The [lock](29-identity-versioning.md#lock) fixes the release each source resolves to. An alias need not equal the dependency's package name.

The aliases `core`, `mem`, `text`, `task`, `sys`, `hw`, `boot`, `host` and `testing` are reserved: in every package they name the [std](28-stdlib.md#std-layers) packages without a `dep` directive, and in a std package its own reserved alias names its root module. `ui` is reserved: a path of two or more segments whose first segment is `ui` is an error (`name.reserved`).

An alias in a path is one its package declares or a std alias; the dependencies of a dependency are not reachable by path.

```nan
// file: units/package.nan
//! Units of length.

package units {
    language 1
    targets any
} units

// file: units/units.nan
//! Length types.

pub closed struct Metres {
    pub value: u32
} Metres

// file: units/convert/convert.nan
//! Conversions between units of length.

const MILLIMETRES_PER_METRE: u64 = 1_000

pub fn millimetres(length: units::Metres) -> u64 {
    return u64.widen(length.value) * MILLIMETRES_PER_METRE
} millimetres

// file: survey/package.nan
//! Reports a surveyed length on the host.

package survey {
    language 1
    targets x86_64-host
    entry main
    dep units "../units"
} survey

// file: survey/survey.nan
//! Entry of the survey program.

fn main(take world: host::World) fails task::Cancelled uses wait {
    let distance = units::Metres(value: 42)
    world.log.info("millimetres", value: units::convert::millimetres(distance))
} main
```

```nan
// file: survey/package.nan
//! Reports a surveyed length on the host.

package survey {
    language 1
    targets x86_64-host
    entry main
    dep units "nan.dev/units@1.2" // error: manifest.bad-source
} survey

// file: survey/survey.nan
//! Entry of the survey program.

fn main(take world: host::World) fails task::Cancelled uses wait {
    let distance = units::Metres(value: 42)
    world.log.info("millimetres", value: units::convert::millimetres(distance))
} main
```

## Modules

A **module** is a directory of source files inside a package, whose items share one namespace.

- The **root module** consists of the source files directly in the package directory other than `package.nan`, and is named like the package.
- Every other directory of the package directly holding a source file is a module, or a [target variant](#target-variants) of one, named by its last path segment without its `@` suffix. The directories above it carry no meaning: `kernel/drivers/uart/` is module `uart`.
- A module's source files are those directly in its directory.
- Files carry no semantics: moving an item between files of its module changes neither its meaning nor its [hashes](29-identity-versioning.md#hashes).
- Two plain module directories named alike after folding are an error ([`name.clash`](#naming-conventions)).
- The root module is declared by the `package` item, and every other module or variant directory at the first line of its first source file in byte order of path, where diagnostics about it are reported. A diagnostic about a source file's name is reported on its first line.

Module A **depends on** module B when an item of A names an item or member of B by a path. Under each declared target the module graph of a package outside [std](28-stdlib.md#std-layers) is acyclic. Each maximal set of two or more modules that each depend on every other, directly or through modules of the set, is one error (`module.cycle`). Its diagnostic lists the set's modules and is reported on the first such path, in byte order of file path and then position, from the set's first module in byte order of name to another module of the set.

```nan
// file: geo/package.nan
//! Plane geometry.

package geo {
    language 1
    targets any
} geo

// file: geo/geo.nan
//! Areas of plane shapes.

pub closed enum Shape derives Eq {
    Square
    Strip
} Shape

pub fn area(shape: Shape, side: u32) -> u64 {
    let wide = u64.widen(side)
    return match shape {
        Shape.Square => wide * wide
        Shape.Strip => strips::strip_area(side)
    }
} area

// file: geo/flat/strips/strips.nan
//! Areas of fixed-length strips.

const STRIP_LENGTH: u64 = 10

pkg fn strip_area(side: u32) -> u64 {
    return u64.widen(side) * STRIP_LENGTH
} strip_area
```

```nan
// file: geo/package.nan
//! Plane geometry.

package geo {
    language 1
    targets any
} geo

// file: geo/geo.nan
//! Strip areas.

pkg const STRIP_LENGTH: u64 = 10

pub fn strip_area(side: u32) -> u64 {
    return strips::scaled(side) // error: module.cycle
} strip_area

// file: geo/strips/strips.nan
//! Strip scaling.

pkg fn scaled(side: u32) -> u64 {
    return u64.widen(side) * geo::STRIP_LENGTH
} scaled
```

## Target variants

A **target variant** is a module directory `name@selector` holding module `name` for the declared targets its selector matches. The selectors are exactly the names of the six concrete targets, each matching that target; `x86_64` and `aarch64`, each matching the three targets starting with it; and `host`, `uefi` and `bare`, each matching the two targets ending in it. Any other selector matches no target.

- A module is one plain directory or a set of variants; a plain directory and a variant of one module are an error (`module.variant-mixed`), reported on the variant. The root module is a plain directory.
- Each declared [concrete target](27-program-model.md#targets) matches exactly one variant of each module that has variants. A target matching none is an error (`module.variant-missing`), reported on the `targets` directive; a target matching two is an error (`module.variant-overlap`), reported on the later of the two variant directories in byte order of path. A variant matching no declared target, as every variant of a package declaring `any` does, is an error (`module.variant-unmatched`).
- The variants of a module export identical items. For each `pub` or `pkg` item, impl line or [tombstone](29-identity-versioning.md#tombstones) of a variant, every other variant holds an item with the same identity ([order and names](02-items.md#order-and-names)) and the same [interface hash](29-identity-versioning.md#hashes), or for a tombstone the same [item hash](29-identity-versioning.md#hashes). Otherwise it is one error per identity (`module.variant-mismatch`), reported on the item in the first variant directory, in byte order of path, that holds one with that identity. Private items may differ.

The checker checks a package once per declared concrete target, with the variants that target matches, and a package declaring `any` as [Targets](27-program-model.md#targets) states. A diagnostic arising under some declared targets only names them.

```nan
// file: probe/package.nan
//! Stack reserves for host and bare builds.

package probe {
    language 1
    targets x86_64-bare, x86_64-host
} probe

// file: probe/probe.nan
//! Reports the stack reserve of the target.

pub fn stack_reserve() -> usize {
    return platform::default_stack()
} stack_reserve

// file: probe/platform@bare/platform.nan
//! Stack defaults per machine.

pkg fn default_stack() -> usize {
    return 16_384
} default_stack

// file: probe/platform@host/platform.nan
//! Stack defaults per machine.

pkg fn default_stack() -> usize {
    return 65_536
} default_stack
```

```nan
// file: probe/package.nan
//! Stack reserves for host and bare builds.

package probe {
    language 1
    targets x86_64-bare, x86_64-host
} probe

// file: probe/probe.nan
//! Reports the stack reserve of the target.

pub fn stack_reserve() -> usize {
    return platform::default_stack()
} stack_reserve

// file: probe/platform@bare/platform.nan
//! Stack defaults per machine.

pkg fn default_stack() -> usize {
    return 16_384
} default_stack

pkg fn has_firmware() -> bool { // error: module.variant-mismatch
    return false
} has_firmware

// file: probe/platform@host/platform.nan
//! Stack defaults per machine.

pkg fn default_stack() -> usize {
    return 65_536
} default_stack
```

```nan
// file: probe/package.nan
//! Register counts for both host targets.

package probe {
    language 1
    targets aarch64-host, x86_64-host
} probe

// file: probe/probe.nan
//! Counts the general-purpose registers of the architecture the package was built for.

pub fn register_count() -> u32 {
    return cpu::general_registers()
} register_count

// file: probe/cpu@aarch64/cpu.nan
//! The architecture's registers.

pkg fn general_registers() -> u32 {
    return 31
} general_registers

// file: probe/cpu@x86_64/cpu.nan
//! The architecture's registers.

pkg fn general_registers() -> u32 {
    return 16
} general_registers
```

```nan
// file: probe/package.nan
//! Register counts for both bare targets.

package probe {
    language 1
    targets aarch64-bare, x86_64-bare // error: module.variant-missing
} probe

// file: probe/probe.nan
//! Counts the general-purpose registers of the architecture the package was built for.

pub fn register_count() -> u32 {
    return cpu::general_registers()
} register_count

// file: probe/cpu@x86_64/cpu.nan
//! The architecture's registers.

pkg fn general_registers() -> u32 {
    return 16
} general_registers
```

## Paths

A **path** is a `::`-separated name reaching an item.

```ebnf
path = IDENT ( "::" IDENT )* ;
```

A path has one of four forms:

- `name`: a [binding](06-statements-expressions.md#bindings), parameter or quantifier variable in scope, else a [generic parameter](08-generics-interfaces.md#generic-parameters) in scope, else an item of the current module, else a [core vocabulary](#core-vocabulary) name.
- `module::item`: an item of a module of this package, the package name naming the root module.
- `alias::item`: an item of the aliased package's root module.
- `alias::module::item`: an item of another module of the aliased package.

The first segment of a longer path is a module or an alias. No import, glob, re-export, rename or shadowing form exists: code outside an item's module writes its path, and each item has one spelling from a given module.

- `::` separates path segments and `.` precedes a member. `::` after a segment naming an item is an error (`name.colon-for-member`). `a.b` in an expression, where `a` names no binding, parameter or item but a module or alias, and `a.b` in a type expression, are errors (`name.dot-for-path`). Each has an exact fix replacing the separator, for a type expression only when `a` names a module or alias.
- A path whose module segment names the current module or the aliased package's root module, and `core::` before a core vocabulary name, are errors (`name.redundant-path`); the exact fix deletes the extra segment.
- Any other path names nothing ([Unknown names](#unknown-names)).

```nan
pub closed enum Signal derives Eq {
    Red
    Green
} Signal

pub fn is_go(signal: Signal) -> bool {
    return signal == Signal.Green
} is_go

pub fn signal_log(alloc: mem::Alloc) -> mem::List[Signal] {
    return mem::List[Signal].empty(alloc)
} signal_log
```

```nan
// file: geo/package.nan
//! Plane geometry.

package geo {
    language 1
    targets any
} geo

// file: geo/geo.nan
//! Strip areas.

pub fn strip_area(side: u32) -> u64 {
    return strips.scaled(side) // error: name.dot-for-path
} strip_area

// file: geo/strips/strips.nan
//! Strip scaling.

pkg fn scaled(side: u32) -> u64 {
    return u64.widen(side) * 10
} scaled
```

```nan
pub closed enum Signal derives Eq {
    Red
    Green
} Signal

pub fn is_go(signal: Signal) -> bool {
    return signal == Signal::Green // error: name.colon-for-member
} is_go
```

```nan
pub fn half(value: u32) -> core::Option[u32] { // error: name.redundant-path
    if value % 2 == 1 {
        return None
    }
    return Some(value / 2)
} half
```

### Members

A **member** is a field, method, associated function or variant, reached with `.` after a value or a type: `self.count`, `Ring.pop`, `Shape.Square`, `hw::IrqExit.Resume`. Generic arguments precede the `.`, as in `mem::List[u32].empty(alloc)`. The members of a type are its fields, its variants, the tags of a [layout](23-low-level.md#layouts) and the functions named after it ([methods](02-items.md#methods)); those of an interface are its methods.

```nan
pub closed enum Phase derives Eq {
    Idle
    Busy
} Phase

pub closed struct Pump {
    pub phase: Phase
} Pump

pub fn Pump.idle() -> Pump = Pump(phase: Phase.Idle)

pub fn Pump.is_busy(self) -> bool = self.phase == Phase.Busy
```

## Visibility

```ebnf
visibility = "pub" | "pkg" ;
```

**Visibility** decides where an item or member may be named. An item without a visibility word is private and visible in its module; `pkg` makes it visible in every module of its package, and `pub` also to consumers. A **consumer** is code outside the package that defines an item, using that item.

- Variants, layout fields and tags, registers fields and [interface methods](08-generics-interfaces.md#interfaces) have their item's visibility, and struct fields the one [Structs](04-types.md#structs) gives them. A function named `Type.name` has its own visibility and is visible only where `Type` also is.
- Naming an item, or a member other than a field, where it is not visible is an error (`name.private`); fields follow [structs](04-types.md#structs) (`type.hidden-field`).
- In the parts of an item listed next, a `pub` item names only `pub` items and a `pkg` item only `pkg` or `pub` items (`name.private-in-interface`), a type naming every item its type expression mentions, type arguments included: the bounds of its generic parameters; for a function, its parameter, result and failure types and, when it is named `Type.name`, `Type`; for a struct, layout or registers block, each field's type, the field counting at the lesser of its own visibility and its item's; for an enum, each payload type; for an interface, each method head; for a const, its type; for a registry, its entry type.
- Contracts, examples and transparent bodies may name less visible items; consumers see the terms naming them as [opaque](18-contracts.md#opacity).

```nan
pub closed enum Phase derives Eq {
    Idle
    Busy
} Phase

pub fn initial_phase() -> Phase {
    return Phase.Idle
} initial_phase
```

```nan
enum Phase {
    Idle
    Busy
} Phase

pub fn initial_phase() -> Phase { // error: name.private-in-interface
    return Phase.Idle
} initial_phase
```

```nan
// file: geo/package.nan
//! Plane geometry.

package geo {
    language 1
    targets any
} geo

// file: geo/geo.nan
//! Strip areas.

pub fn strip_area(side: u32) -> u64 {
    return strips::scaled(side) // error: name.private
} strip_area

// file: geo/strips/strips.nan
//! Strip scaling.

fn scaled(side: u32) -> u64 {
    return u64.widen(side) * 10
} scaled

pkg fn doubled_strip(side: u32) -> u64 {
    return scaled(side) * 2
} doubled_strip
```

## Core vocabulary

The **core vocabulary** is the set of names usable without a path everywhere: the 14 [scalars](04-types.md#scalars), `Unit`, `Never`, `Option`, `Some`, `None`, `Result`, `Ok`, `Err`, `Array`, `Slice`, `SliceMut`, `Str`, `Untrusted`, `Dyn`, `Eq`, `Ord`, `Hash`, `Copy`, `Default`, `Render`, `Encode`, `Generate`, `Send`, `Inert`, `AnyBits` and `Any`. Each is an item of `core`. In every package other than `core`:

- A name of a module's types namespace ([naming conventions](#naming-conventions)) or a generic parameter whose folded form is that of a core vocabulary name is an error (`name.core-redefined`).
- A name of a module's values namespace whose folded form is a scalar's name is an error (`name.core-redefined`), and so is a binding or parameter whose name differs from a scalar's but folds to it. One spelt like a core vocabulary name is [`stmt.shadowing`](06-statements-expressions.md#bindings).
- Members may carry core vocabulary names.

```nan
pub closed enum Fill derives Eq {
    None
    Solid
    Striped
} Fill

pub fn is_filled(fill: Fill) -> bool {
    return fill != Fill.None
} is_filled
```

```nan
pub closed struct Result { // error: name.core-redefined
    pub code: u32
} Result
```

## Naming conventions

Every declared name follows the convention of its kind; a name that does not is an error (`name.case`).

| Convention | Spelling | Names |
| --- | --- | --- |
| snake_case | lowercase letters and digits, starting with a letter, with single underscores between words | packages, modules, aliases, source file names before `.nan`, functions, interface methods, fields, payload labels, bindings, parameters, quantifier variables, loop labels, tests, registries, contributions |
| PascalCase | an uppercase letter followed by letters and digits, holding a lowercase letter unless it is one letter | structs, enums, interfaces, layouts, registers blocks, variants, layout tags, generic parameters |
| SCREAMING_SNAKE | uppercase letters and digits, starting with a letter, with single underscores between words, at least 2 characters | consts |

The name of a [foreign fn](23-low-level.md#foreign-functions) is the external symbol it declares and follows no convention.

For a name declared in source, the exact fix of `name.case` renames the declaration and its uses in the package. It splits the name into words at underscores and before each uppercase letter that follows a lowercase letter or a digit, drops empty words, and joins them lowercase with `_` for snake_case, each capitalised with the rest lowercase for PascalCase, or uppercase with `_` for SCREAMING_SNAKE. No fix is offered when the result breaks a naming rule.

The **folded form** of a name is the name with each uppercase letter made lowercase and each underscore removed. Names in one namespace are distinct after folding:

| Namespace | Names |
| --- | --- |
| package modules | the package's modules, the root module's name being the package name; its `dep` aliases; the reserved aliases other than a std package's own, `ui` included |
| module types | a module's structs, enums, interfaces, layouts and registers blocks |
| module values | a module's functions not named `Type.name`, consts, registries, contributions and tests |
| type members | the fields, variants and tags of a type and the functions named after it; the methods of an interface |
| payload labels | the labels of one variant's payload |

An item's generic parameters, with those of `Type` for a function named `Type.name`, are compared with each other and with its module's types. A parameter, binding or quantifier variable is compared with its module's values and with every parameter, binding and quantifier variable whose scope overlaps its own.

Two compared names whose folded forms are equal are an error (`name.clash`), unless they are identical and reported as [`item.duplicate`](02-items.md#order-and-names), [`type.duplicate-member`](04-types.md#structs) or [`stmt.shadowing`](06-statements-expressions.md#bindings). A clash is reported on the generic parameter, parameter, binding or quantifier variable when exactly one of the two is one, and otherwise on the later declaration in byte order of file path and then position, an alias counting as declared before every module and a reserved alias before every `dep` alias.

```nan
pub const RETRY_LIMIT: u32 = 5

pub closed struct Retries {
    pub count: u32 where count <= RETRY_LIMIT
} Retries

pub fn retries() -> Retries {
    return Retries(count: 0)
} retries
```

```nan
pub fn byteCount(data: Slice[u8]) -> usize { // error: name.case
    return data.len()
} byteCount
```

```nan
const LIMIT: u32 = 8

pub fn clamp_count(count: u32) -> u32 {
    let limit: u32 = LIMIT // error: name.clash
    if count > limit {
        return limit
    }
    return count
} clamp_count
```

## Name prefixes

A function item or interface method whose name, after any `Type.`, starts with one of these prefixes followed by at least one character has the head shape given; a head contradicting it is an error (`name.prefix-mismatch`).

| Prefix | Head shape |
| --- | --- |
| `try_` | the head has a `fails` clause |
| `checked_` | the result type is an `Option` |
| `is_` | the result type is `bool` |
| `to_` | the first parameter is `self` in read mode, the function is not a `proj fn`, and its result type is not a [view](05-properties.md#views) |
| `as_` | the function is a `proj fn` |
| `into_` | the first parameter is `take self` |
| `with_` | the first parameter is `mut self` |

```nan
pub fn is_empty_frame(frame: Slice[u8]) -> bool {
    return frame.len() == 0
} is_empty_frame
```

```nan
pub fn is_empty_frame(frame: Slice[u8]) -> usize { // error: name.prefix-mismatch
    return frame.len()
} is_empty_frame
```

## Unknown names

A use **names nothing** when its name, path or member reaches nothing under [Paths](#paths), visible or not, and matches no `preserves` old form ([forwarding](29-identity-versioning.md#forwarding)). The first of these rules that applies diagnoses it:

1. A use matching a `changes` or `manual` old form is an error (`name.moved`) quoting the [tombstone](29-identity-versioning.md#tombstones): for `changes` with the note and the exact fix a forwarding use of that old form would have, for `manual` with the reason and no fix. A call matching such an old head while its path names a live function gets this error in place of its own diagnostic.
2. A path through an alias a `dep` directive declares that names a `pub` item or member in a newer release of the alias's source is an error (`name.newer-release`) naming that release and the command `nan dep sync`, without an exact fix. The newer releases are the current state of a path source whose code root differs from the locked release's, and the named releases under a store name with a greater [publication index](29-identity-versioning.md#local-store) than the locked one, of which the diagnostic names the earliest.
3. Any other such use is an error (`name.unknown`).

A `name.unknown` diagnostic ranks **candidates**, the names legal at the use's position: names in scope, paths to items visible from the module as it writes them, and, after `.`, the visible members of the type before it. A candidate **fits** when the use's line checks without error with the candidate in its place. Candidates rank fitting first, then by the distance between their folded form and the use's, then in byte order. The folded form of a path is its segments' folded forms joined by `::`, and the distance is the fewest insertions, deletions, substitutions and transpositions of adjacent characters turning one into the other, no character being edited twice. A candidate has distance 0 when replacing a run of the use's underscore-separated words that the [controlled vocabulary](28-stdlib.md#controlled-vocabulary) lists as a synonym with that synonym's verb gives the candidate. The diagnostic lists the first 5 candidates whose distance is at most a third of the length of the longer of the two folded forms, rounded up, and offers each fitting one as an exact fix.

When no candidate is within that distance and the use is the callee of a call, as a bare name or as a member of a type declared in the current module, the diagnostic offers a stub as an exact fix: a private function named like the use, `Type.name` for a member, inserted after the item holding the call, whose body is `return todo "<name>"`, or `todo "<name>"` without a result, `<name>` being the function's name.

- Its parameters follow the arguments in order, each with the argument's label, or, for an unlabelled first argument, its name when it is a binding or parameter and `value` otherwise; the argument's type; and the mode the argument's marker states, read when unmarked. The receiver of a call on a value becomes `self`.
- Its result is the type expected at the call, omitted when the call is an expression statement.
- Under `try` it fails with the enclosing function's failure type; under `attempt`, expecting `Result[T, E]`, its result is `T` and it fails with `E`; under `await` it uses wait.
- No stub is offered when an argument's type or the expected type is not fixed, or when two parameters would share a name.

```nan
pub fn checksum(data: Slice[u8]) -> u8 {
    var sum: u32 = 0
    for byte in data {
        sum = sum.wrapping_add(u32.widen(byte))
    }
    return u8.truncate(sum)
} checksum

pub fn frame_checksum(frame: Slice[u8]) -> u8 {
    return check_sum(frame) // error: name.unknown
} frame_checksum
```

```nan
pub closed struct Counter {
    pub hits: u32
} Counter

pub fn Counter.record(mut self) {
    self.hits = self.hits.saturating_add(1)
} Counter.record

retired Counter.bump(mut self) => mut self.record() changes "the count saturates instead of trapping"

pub fn record_twice(mut counter: Counter) {
    mut counter.record()
    mut counter.bump() // error: name.moved
} record_twice
```

## Diagnostics

| Code | Class | Rule |
| --- | --- | --- |
| `manifest.any-not-alone` | error | [Manifest](#manifest) |
| `manifest.bad-source` | error | [Aliases](#aliases) |
| `manifest.deny-self` | error | [Manifest](#manifest) |
| `manifest.duplicate` | error | [Manifest](#manifest) |
| `manifest.misplaced` | error | [Manifest](#manifest) |
| `manifest.missing` | error | [Packages](#packages) |
| `manifest.missing-directive` | error | [Manifest](#manifest) |
| `manifest.reserved-target` | error | [Manifest](#manifest) |
| `manifest.unknown-target` | error | [Manifest](#manifest) |
| `module.cycle` | error | [Modules](#modules) |
| `module.variant-mismatch` | error | [Target variants](#target-variants) |
| `module.variant-missing` | error | [Target variants](#target-variants) |
| `module.variant-mixed` | error | [Target variants](#target-variants) |
| `module.variant-overlap` | error | [Target variants](#target-variants) |
| `module.variant-unmatched` | error | [Target variants](#target-variants) |
| `name.case` | error | [Naming conventions](#naming-conventions) |
| `name.clash` | error | [Naming conventions](#naming-conventions), [Modules](#modules), [Interfaces](08-generics-interfaces.md#interfaces) |
| `name.colon-for-member` | error | [Paths](#paths) |
| `name.core-redefined` | error | [Core vocabulary](#core-vocabulary) |
| `name.dot-for-path` | error | [Paths](#paths) |
| `name.moved` | error | [Unknown names](#unknown-names) |
| `name.newer-release` | error | [Unknown names](#unknown-names) |
| `name.prefix-mismatch` | error | [Name prefixes](#name-prefixes) |
| `name.private` | error | [Visibility](#visibility), [Registries](25-registries.md#registries), [Contributions](25-registries.md#contributions) |
| `name.private-in-interface` | error | [Visibility](#visibility) |
| `name.redundant-path` | error | [Paths](#paths) |
| `name.reserved` | error | [Aliases](#aliases), [Reserved std names](28-stdlib.md#reserved-std-names) |
| `name.unknown` | error | [Unknown names](#unknown-names), [Untrusted values](15-untrusted.md#untrusted-values), [Array and slice members](28-stdlib.md#array-and-slice-members), [Option and Result members](28-stdlib.md#option-and-result-members) |
