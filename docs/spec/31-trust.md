# Trust and the release gate

This chapter defines incompleteness with its complete list and the unused code it includes, the release gate `nan verify` and how it differs from `nan check` and `nan test`, the ledger, trust escapes, seals with their principals, closure hashes, lapse and author rule, principals files and secret keys, signatures, API snapshots with the spec, the accepted spec and `nan api update`, weakening, pending weakenings and acceptances, and the change classes `nan diff` reports.

## Incompleteness

**Incompleteness** is the diagnostic class for work that is unfinished or unaccepted. `nan check` and `nan test` report it and succeed; the release gate rejects it: `nan verify`, and `nan publish`, which runs it. No option, directive or file changes the class of a diagnostic or whether a command accepts it.

The incompleteness codes are exactly:

| Code | Condition | Rule |
| --- | --- | --- |
| `todo.hole` | a typed `todo` expression | [Holes](18-contracts.md#holes) |
| `todo.missing-spec` | a `todo` clause marking a missing specification | [Missing specifications](18-contracts.md#missing-specifications) |
| `todo.disputed` | a disputed clause or test | [Disputed clauses](18-contracts.md#disputed-clauses) |
| `test.dbg` | a `dbg` expression | [Debug output](22-testing.md#debug-output) |
| `test.generator-gap` | a generator gap | [Constraints](22-testing.md#constraints) |
| `threshold.depth` | an unwaived nesting excess | [Depth](06-statements-expressions.md#depth) |
| `threshold.length` | an unwaived body-length excess | [Length](06-statements-expressions.md#length) |
| `retired.forwarded-use` | a use of the old form of a `preserves` tombstone | [Forwarding](29-identity-versioning.md#forwarding) |
| `unused.item`, `unused.local`, `unused.field`, `unused.dependency`, `unused.tombstone` | unused code | [Unused code](#unused-code) |
| `unused.grant` | a manifest grant nothing uses | [Manifest grants](14-authority.md#manifest-grants), [Contributors](25-registries.md#contributors) |
| `doc.stale` | a stale doc | [Doc binding](32-docs.md#doc-binding) |
| `api.stale` | an API snapshot that is not current | [API snapshots](#api-snapshots) |
| `api.weakening-pending` | a pending weakening, or a weakening accepted without a seal of another author | [Weakening](#weakening), [Acceptances](#acceptances) |
| `trust.unsealed` | a trust escape without a current seal | [Trust escapes](#trust-escapes) |
| `seal.same-author` | a seal breaking the author rule | [Author rule](#author-rule) |

The **gate codes** are the codes of the `api`, `seal` and `trust` families. `nan check` and `nan test` never compute them; `nan verify` reports them, and `nan api update`, `nan seal`, `nan key` and `nan publish` report those their rules name.

```nan
pub fn retry_delay(retries: u32) -> u32
    todo "is the delay capped?" // incomplete: todo.missing-spec
{
    return retries.saturating_mul(100)
} retry_delay

test first_retry_waits for retry_delay {
    expect retry_delay(1) == 100
} first_retry_waits
```

### Unused code

Each of these rules holds under every declared target: code is used when it is used under at least one. Each diagnostic is reported on the first line of the unused declaration, and its exact fix deletes that declaration unless the rule names another fix.

- The `package` item, every item that is not private, every test, impl line, contribution and const assert, every landing fn and the function the `entry` directive names are **reachable**, and so is each private item that the [unit](19-nir.md#units) of a reachable item names. An impl line also names the methods implementing its interface and, when it names `sys::Component`, the [abandon method](17-suspension-io.md#abandoning-resources) of its Env. A private item that is not reachable is incompleteness (`unused.item`). A tombstone never makes an item reachable and is never an unused item.
- A `var` binding, [pattern binding](06-statements-expressions.md#bindings) or name a `for` pattern binds, whose type is not linear and that no expression in its scope uses, is incompleteness (`unused.local`); its appearance as the target of `=` is not a use. For `let name = e` or `var name = e`, with or without a type, the exact fix writes `discard e`, or `e` alone when `e` has type `Unit` or `Never`, and exists only when no statement assigns the binding; for any other binding it writes `_` in place of the name.
- A field of a struct is exempt when the field and its struct are both `pub`. Any other field that no item of the package reads is incompleteness (`unused.field`). A field is read by a field access other than the target of `=`, by a struct pattern giving it a pattern other than `_`, by a predicate naming it, and by the methods its struct's derives of `Eq`, `Ord`, `Hash`, `Render` and `Encode` write; construction does not read it. The exact fix also deletes its `label: value` in each construction and struct pattern, and exists only when each such value is a place, a literal or `_`.
- A `dep` directive is incompleteness (`unused.dependency`) when no item other than the `package` item names its alias and no `allow contribute` directive lists the alias as a name that [contributors](25-registries.md#contributors) does not report as `unused.grant`. The exact fix also deletes the alias from each manifest grant listing it, and each grant it leaves listing nothing.
- A tombstone is incompleteness (`unused.tombstone`) when the package's history has a [history head](29-identity-versioning.md#publishing) and that release has no item or member visible to consumers whose canonical path is the old form's and, for an old head, that is a function whose parameters have the old head's modes and types. A tombstone keeping an `Encode` [key](13-text-encode.md#keys) is never unused.

```nan
pub fn busiest(loads: Slice[u32]) -> u32 {
    var peak: u32 = 0
    for load in loads {
        if load > peak {
            peak = load
        }
    }
    return peak
} busiest
```

```nan
pub fn busiest(loads: Slice[u32]) -> u32 {
    let count = loads.len() // incomplete: unused.local
    var peak: u32 = 0
    for load in loads {
        if load > peak {
            peak = load
        }
    }
    return peak
} busiest
```

```nan
fn halve(load: u32) -> u32 { // incomplete: unused.item
    return load / 2
} halve

pub fn double(load: u32) -> u32 {
    return load.saturating_mul(2)
} double
```

```nan
fn halve(load: u32) -> u32 {
    return load / 2
} halve

pub fn half_load(load: u32) -> u32 {
    return halv(load) // error: name.unknown
} half_load
```

```nan
struct Reading {
    level: u32
    sensor: u8 // incomplete: unused.field
} Reading

pub fn doubled_level(level: u32, sensor: u8) -> u32 {
    let reading = Reading(level: level, sensor: sensor)
    return reading.level.saturating_mul(2)
} doubled_level
```

```nan
// file: monitor/package.nan
//! Link monitoring.

package monitor {
    language 1
    targets any
    dep ring "../ring" // incomplete: unused.dependency
} monitor

// file: monitor/monitor.nan
//! Counts link events.

pub fn next_count(count: u32) -> u32 {
    return count.saturating_add(1)
} next_count

// file: ring/package.nan
//! Fixed-capacity rings.

package ring {
    language 1
    targets any
} ring

// file: ring/ring.nan
//! Ring capacity.

pub const CAPACITY: usize = 64
```

## Release gate

The **release gate** is `nan verify`. It accepts a package exactly when, for the package and for each package it reaches through a chain of `dep` directives with relative-path sources:

1. `nan check` reports no diagnostic under any declared target;
2. every test passes under `nan test`, [emulator runs](22-testing.md#emulator-runs) included;
3. every [API snapshot](#api-snapshots) is current (`api.stale`);
4. no [pending weakening](#weakening) remains (`api.weakening-pending`);
5. every item holding a trust escape has a current seal (`trust.unsealed`) that meets the [author rule](#author-rule) (`seal.same-author`);
6. in a VCS workspace, every acceptance of a weakening was made by a seal of another author (`api.weakening-pending`), as [acceptances](#acceptances) states.

A package reached through a store-name source is a named release, which passed the release gate when it was [published](29-identity-versioning.md#publishing), and `nan verify` checks it no further.

## Ledger

The **ledger** of an item is the list of its entries of the kinds this table gives; the ledger of a package lists the ledgers of its items. Each entry is its kind, its [span](19-nir.md#spans) and the parts the table gives, in that order. The ledger is computed from source and is part of the [code root](30-artefacts.md#code-roots).

| Kind | Entry | Parts | Item holding it | Trust escape |
| --- | --- | --- | --- | --- |
| `trusted` | [trusted code](23-low-level.md#trusted-code) | its premise | the item containing it | yes |
| `assume` | an [`assume`](18-contracts.md#assume) | its condition and reason | the item containing it | yes |
| `foreign` | a [foreign fn](23-low-level.md#foreign-functions) | none | the foreign fn | yes |
| `landing` | a [landing fn](23-low-level.md#landing-functions) | none | the landing fn | yes |
| `validator` | a [validator](15-untrusted.md#validator-classes), a method of an impl of `core::Decoder` included | its class | the validator | when declassifying |
| `decoder` | a declassifying decoder | none | the type item whose `derives` clause writes it | yes |
| `grant` | a `device` or `priv` [grant](14-authority.md#device-and-priv-grants) | its authority and the names it lists | the `package` item | yes |
| `waiver` | a [waiver](06-statements-expressions.md#waivers) | its threshold and reason | the function | no |
| `todo` | a `todo` | its text | the item containing it | no |
| `dbg` | a `dbg` | none | the item containing it | no |

```nan
// file: procinfo/package.nan
//! Process facts on the host.

package procinfo {
    language 1
    targets x86_64-host
    allow trusted in procinfo
} procinfo

// file: procinfo/procinfo.nan
//! Reads process facts.

foreign fn getpid() -> i32 limits notrap, terminates, noalloc convention "sysv"

pub fn process_id() -> i32 {
    return getpid()
} process_id

pub fn first_byte(source: Slice[u8]) -> u8
    requires source.len() > 0
{
    let start = hw::Ptr.of_view(source)
    return trusted "source is a live view of at least one byte" { start.read() }
} first_byte
```

The ledger of this package holds a `foreign` entry on `getpid` and a `trusted` entry on `first_byte` with its premise; `process_id` has none. `nan check` reports nothing, and `nan verify` reports `trust.unsealed` on `getpid` and on `first_byte` until each has a current seal.

## Trust escapes

A **trust escape** is a ledger entry whose soundness rests on review rather than on a rule the checker enforces: exactly the entries the ledger table marks. An item **needs a seal** when it holds a trust escape or has a [pending weakening](#weakening). An item holding a trust escape without a current seal is incompleteness (`trust.unsealed`), reported on its first line, for an item that variants hold in the first variant directory in byte order of path that holds it, naming each of its trust escapes and, for each lapsed seal, whether its sealed item hash changed and each closure entry added, removed or changed.

## Seals

A **seal** is a record binding a principal to an item; `nan seal <item> --as <principal>` writes it, `<item>` naming the item as [commands](33-toolchain.md#commands) state. A **principal** is the identity a seal names: a text of 1 to 64 characters, each an ASCII letter, digit, `.`, `_`, `-` or `@`. A seal record holds, in order:

- the principal;
- the canonical path of the item;
- its **sealed item hash**: its [item hash](29-identity-versioning.md#item-hash-and-interface-hash), or, for an item that two or more [target variants](03-packages-modules.md#target-variants) hold, the hash of the bytes `variants` and LF followed, for each such variant in byte order of its directory's path, by the full form of its item hash and LF;
- its closure hash, with the closure entries it covers;
- its signature, when the package's [principals file](#principals) gives the principal a key: the [signature](#signatures) of its **seal message** by the principal's secret key, the seal message being `nan seal 1`, the principal, the canonical path and the full forms of the sealed item hash and the closure hash, each followed by LF.

A seal is recorded in the [API snapshot](#api-snapshots) of the sealed item's module, a seal of the `package` item in that of the root module.

The **closure entries** of an item are:

- for each item other than itself that its [unit](19-nir.md#units) names, or that the body of a transparent function it unfolds names: the kind `interface`, that item's canonical path and its interface hash;
- for each transparent function whose calls it [unfolds](02-items.md#transparent-functions), directly or inside an unfolded body: the kind `body`, the function's canonical path and its item hash;
- for an item holding a declassifying validator or the declassifying decoder of a type item: for each function of the item's package other than a transparent function that the item's unit names, and recursively each such function that the unit of one of these names, the kind `body`, the function's canonical path and its item hash.

The closure entries of an item that variants hold are the union of those of its variants. A `package` item has no unit, so its closure entries are empty. The **closure hash** is the [hash](29-identity-versioning.md#hash-values) of the bytes `closure` and LF, followed by one line per closure entry, sorted by canonical path, then by kind, then by hash: the kind, a space, the canonical path, a space and the full form of the entry's hash, then LF.

- A seal is **current** while the item's sealed item hash and closure hash equal those it records and, when the package's principals file gives its principal a key, it holds a signature that [verifies](#signatures) by that key, or else it holds none; otherwise it has **lapsed**.
- A seal binds no toolchain hash and no hash of the module's grants.
- `nan seal` replaces an earlier seal of the same principal on the item. Several principals may seal one item.
- A principal outside that form is an error (`seal.bad-principal`), and so is sealing an item that needs no seal (`seal.unneeded`). `nan seal` writes nothing when it reports either, `key.missing` for a principal the principals file gives a key when the user directory holds no secret key of the principal with that public key, `api.stale` for the item's module snapshot, or `seal.same-author` for the seal it would write; it also reports `seal.same-author`, and writes nothing, when the seal would accept a pending weakening and its author is the author of a change introducing the item's current spec.
- When the item has a pending weakening, `nan seal` also sets its accepted spec to its current spec and current head, and removes the pending weakening.

The seal of `first_byte` in the procinfo package lapses when `first_byte` is edited, or when the interface of `hw::Ptr.of_view`, `hw::Ptr.read` or any other item it names changes; an edit of `process_id` leaves it current.

### Author rule

A **VCS workspace** is a workspace inside a git working tree. A seal meets the **author rule** when it meets each of these conditions that applies:

- in a VCS workspace, the author of each change writing the seal differs from the author of each change introducing the sealed item hash or a hash that one of its closure entries records;
- when [publishing](29-identity-versioning.md#publishing) names author principals, the seal's principal is none of them.

The first condition reads the git history:

- The changes are the checked-out commit and its ancestors, and the author of each is its author email after ASCII lowercasing. The uncommitted state of the working tree is one more change, whose parent is the checked-out commit and whose author is the email the git configuration gives, or the empty text when it gives none.
- A change introduces an item hash when the item has that hash in the change's tree and in none of its parents' trees, and writes a seal when the seal's record is in its tree and in none of its parents' trees. It introduces the hash of a closure entry when that entry's item, an item of the workspace, has that hash in the change's tree and in none of its parents' trees, and, for an entry naming an item of a package outside the workspace, when it introduces the checked package's current [lock](29-identity-versioning.md#lock) line for that package, that line being in its tree and in none of its parents' trees.
- An item holding a trust escape whose current seals all break the author rule is incompleteness (`seal.same-author`), reported on its first line, for an item that variants hold in the first variant directory in byte order of path that holds it, and naming each seal's principal and the author it shares, its principal itself for an author principal.
- Outside a VCS workspace and outside publishing, every seal meets the author rule. An artefact's [provenance](30-artefacts.md#sections) names its author principals beside the seals its attestation file records, so the second condition can be judged from the artefact alone.

### Principals

The **principals file** `.nan/principals.nan` of a package is a [machine file](35-format.md#principals-file) giving principals their public keys: the keys whose signatures the package's seals hold, and those of the publishers and sealers of the releases it reads ([sections](30-artefacts.md#sections)). `nan key trust <principal> <key>` gives the principal the key in the principals file of the current directory's package, in place of any other, `tool.usage` outside every package. A **key form** is the checksummed base32 of a public key's 32 bytes, 54 characters.

The **secret key** of a principal is the 32-byte seed the file `keys/<principal>.key` of the [user directory](36-builds.md#cache) holds; a file of another length holds none. `nan key new <principal>` writes a principal's secret key from random bytes and prints `key <principal> <key>` with its public key's key form, and `nan key show <principal>` prints that record for the principal's secret key. Both read and write only the user directory.

- `nan key new` for a principal that has a secret key is an error (`key.exists`), `nan key show` for one that has none is an error (`key.missing`), and `nan key trust` with a text that is no key form is an error (`key.bad-form`). A principal outside the principal form is `seal.bad-principal`. A command reporting one of these writes nothing.
- The principals file is part of the package's source tree hash and not of its code root, so trusting a key changes no release.

### Signatures

A **signature** is an Ed25519 signature as RFC 8032 defines it, over SHA-512: 64 bytes, the encoding of a point R then a scalar S, by the public key A, of 32 bytes, that a 32-byte secret seed determines. Signing is deterministic. A signature of a message **verifies** by a key exactly when:

1. A and R are canonical point encodings: the value of their low 255 bits is below 2^255 − 19, a point satisfies the curve equation with it, and the sign bit is clear when x is 0;
2. S, least significant byte first, is below the group order L;
3. [8]A is not the neutral point;
4. [S]B − [k]A encodes as R, B being the base point and k the SHA-512 digest of R, A and the message, least significant byte first, modulo L.

A **signature form** is the checksummed base32 of a signature's 64 bytes, 105 characters. `core::crypto` gives NaN code the same keys, signatures and verification:

```nan-api
// module: core::crypto

pub closed struct PublicKey derives Copy, Eq, Ord, Hash {
    pub bytes: Array[u8, 32]
} PublicKey

pub closed struct Signature derives Copy, Eq {
    pub bytes: Array[u8, 64]
} Signature

pub open struct SecretKey {
} SecretKey

pub fn SecretKey.of_seed(seed: Array[u8, 32]) -> SecretKey limits pure, terminates

pub fn SecretKey.public_key(self) -> PublicKey limits pure, notrap, terminates

pub fn SecretKey.sign(self, message: Slice[u8]) -> Signature limits pure, terminates

pub fn PublicKey.verify(self, message: Slice[u8], signature: Signature) -> bool limits pure, terminates
```

```nan
pub fn accepts(key: core::crypto::PublicKey, update: Slice[u8], signature: core::crypto::Signature) -> bool {
    return key.verify(update, signature)
} accepts
```

## API snapshots

An **API snapshot** is the machine file `.nan/api/<module>.nan` that `nan api update` writes for a module of a package, named by the module's last segment; [machine files](35-format.md#machine-files) gives its syntax. It holds:

- for each `pub` item of the module and each impl line of it whose impl target and interface are both `pub`: its interface text, promised properties, interface hash, [digest](10-effects-digests.md#digests) under each declared target, and public docs with their [bound hashes](32-docs.md#doc-binding);
- for each item of the module whose accepted spec is not empty or that has a pending weakening: its accepted spec and its pending weakening, if any;
- the seals of the module's items.

An item that target variants hold has one entry, whose text and docs are its first variant's in byte order of path ([API snapshot syntax](35-format.md#api-snapshots)).

The **spec** of an item is:

- for a function, and for each method head of an interface: the text of each requires, ensures and example clause that is not [disputed](18-contracts.md#disputed-clauses), and, for a function, its targeting tests;
- for a struct: the line of each field that has a `where` predicate, and each invariant line.

The **targeting tests** of a function are the tests of its package that are not disputed and whose [test target](22-testing.md#test-items) it is. Each is recorded by its canonical path, its parameter types in order and its **expect hashes**: one per `expect` statement its body holds at any depth, the hash of the bytes `expect`, LF and the statement's text in the test's [normalised form](29-identity-versioning.md#normalised-form) with the bindings it names renamed `$1`, `$2` and so on in order of first occurrence in the statement.

The **accepted spec** of an item is the spec its snapshot records as accepted, with, for a function, its **accepted head**: the head the function had when that spec was accepted. Only `nan api update` and `nan seal` change it.

`nan api update` computes the snapshots of every module of the package and rewrites those of the modules in its scope:

1. It writes the `pub` entries and public docs from the current source.
2. For each item with a spec or an accepted spec, it compares the current spec with the accepted spec. When the current spec [weakens](#weakening) the accepted spec, the accepted spec stays and a pending weakening is recorded in place of any earlier one; otherwise the current spec and current head become the accepted spec and any pending weakening is removed.
3. It removes the accepted specs, pending weakenings and seals of items that no longer exist, except that the accepted spec and pending weakening of an item a `preserves` tombstone retires move to the canonical path of its replacement.
4. It removes each lapsed seal of an item that needs no seal.

A snapshot is **current** when `nan api update` would leave it unchanged, absence included. A snapshot that is not current is incompleteness (`api.stale`), reported on the first line of each item whose entries differ and, for entries of items no longer present, on the summary line of the module's [module summary file](32-docs.md#module-summaries), the first variant's in byte order of path for a module with target variants, or of `package.nan` when the module no longer exists.

## Weakening

A **weakening** is a change after which an item's current spec no longer keeps its accepted spec. The **weakened parts** of an item are exactly:

- `requires`, when the accepted requires do not [entail](20-prover.md#entailment) the current requires;
- each accepted ensures that the accepted requires and the current ensures do not entail;
- `predicates`, for a struct whose current predicates do not entail its accepted predicates;
- each accepted example that has no current example of the same [normalised form](29-identity-versioning.md#normalised-form);
- each accepted targeting test for which no current targeting test has the same parameter types and expect hashes including all of its own, counted with repetition.

The current spec **weakens** the accepted spec when the item has a weakened part. Names in accepted texts resolve as in the item's module, and each entailment is judged from the current item ([entailment](20-prover.md#entailment)). Each entailment matches the parameters of the accepted head and the current head by position and fields by name. A parameter is unmatched when the other head has no parameter of the same type at its position, and `result` when the two result types differ; a clause naming an unmatched parameter, an unmatched `result` or a field only one side has is left out of the premises, and a goal holding one is not entailed. Adding ensures clauses, predicates, examples, tests and expect lines, renaming or moving a test, and removing an item never weaken. Before comparing, `nan api update` rewrites each accepted text with the rewrites of the forms that the package's [language version](37-evolution.md#successive-versions) retires, so a change made only by `nan upgrade --language` never weakens.

A **pending weakening** is the record `nan api update` writes for an item whose current spec weakens its accepted spec: the item hash it was computed for and each weakened part. Each pending weakening is incompleteness (`api.weakening-pending`), reported on the item's first line with each weakened part; `nan api update` reports it when recording it. Only a [seal](#seals) of the item accepts it.

```nan
pub fn clamp_level(level: u32, ceiling: u32) -> u32
    ensures result <= ceiling
{
    if level > ceiling {
        return ceiling
    }
    return level
} clamp_level

test clamp_level_caps for clamp_level {
    expect clamp_level(9, ceiling: 4) == 4
    expect clamp_level(2, ceiling: 4) == 2
} clamp_level_caps
```

With that block's spec accepted, the next block has two weakened parts: `requires`, since the accepted requires, which are empty, do not entail `ceiling > 0`; and the test `clamp_level_caps`, whose second expect hash no current targeting test has. `nan api update` records one pending weakening on `clamp_level` with both parts.

```nan
pub fn clamp_level(level: u32, ceiling: u32) -> u32
    requires ceiling > 0
    ensures result <= ceiling
{
    if level > ceiling {
        return ceiling
    }
    return level
} clamp_level

test caps_at_ceiling for clamp_level {
    expect clamp_level(9, ceiling: 4) == 4
} caps_at_ceiling
```

### Acceptances

In a [VCS workspace](#author-rule), an accepted spec A' **keeps** an accepted spec A when A' taken as the current spec does not weaken A, an absent accepted spec counting as empty. A change **introduces** a spec of an item when the item has that spec in the change's tree and in none of its parents' trees; two specs are equal when their clauses' flat texts and their targeting tests' records are equal. For each item of the package and each change C that the author rule reads in which the item's accepted spec keeps the accepted spec of none of C's parents, C writes a seal of the item that meets the author rule and whose author differs from the author of each change introducing that accepted spec as the item's spec. Each other such pair of an item and a change is incompleteness (`api.weakening-pending`), reported on the item's first line and naming the change.

## Change classes

A **change class** is one of the kinds of change `nan diff` reports for each item changed between two states of a workspace ([commands](33-toolchain.md#commands)), listing every class the change belongs to in the order of this table. An item of the old state is matched with the item of the new state that has its canonical path or, when a rename tombstone that only the new state's source or history holds retires that path, the path its rewrite names; an unmatched item is added or removed. The **rewritten form** of a matched item is its old [normalised form](29-identity-versioning.md#normalised-form) with each canonical path such a tombstone retires replaced by the path its rewrite names.

| Class | An item's change belongs to it when |
| --- | --- |
| format | its text changed while its canonical path and item hash stayed equal: layout, comments, doc comments or the file holding it |
| move | its canonical path changed, or its item hash changed while its rewritten form equals its new normalised form |
| body | its rewritten form differs from its new normalised form while their [interface forms](29-identity-versioning.md#item-hash-and-interface-hash) are equal |
| interface | the interface forms of its rewritten form and its new normalised form differ, or it was added or removed |
| spec | its spec differs between the two states |
| authority | for a `package` item, its `dep`, `allow` or `deny` directives or the package's [authority summary](14-authority.md#authority-summaries) changed, or, for a [final-link root](27-program-model.md#final-builds), the approvals its lock records |
| encode | it derives `Encode` and its [schema hash](13-text-encode.md#schema-hashes) changed, or it gained or lost `derives Encode`, or an Encode key tombstone of it changed |
| trust | its ledger, a seal of it, its accepted spec or its pending weakening changed, or a seal of it lapsed |

Between the two blocks of [Weakening](#weakening), `clamp_level` is in the interface, spec and trust classes, the last once `nan api update` records the pending weakening.

## Diagnostics

| Code | Class | Rule |
| --- | --- | --- |
| `api.stale` | incompleteness | [API snapshots](#api-snapshots), [Seals](#seals) |
| `api.weakening-pending` | incompleteness | [Weakening](#weakening), [Acceptances](#acceptances) |
| `key.bad-form` | error | [Principals](#principals) |
| `key.exists` | error | [Principals](#principals) |
| `key.missing` | error | [Principals](#principals), [Seals](#seals), [Publishing](29-identity-versioning.md#publishing) |
| `seal.bad-principal` | error | [Seals](#seals), [Principals](#principals) |
| `seal.same-author` | incompleteness | [Author rule](#author-rule), [Seals](#seals) |
| `seal.unneeded` | error | [Seals](#seals) |
| `trust.unsealed` | incompleteness | [Trust escapes](#trust-escapes) |
| `unused.dependency` | incompleteness | [Unused code](#unused-code) |
| `unused.field` | incompleteness | [Unused code](#unused-code) |
| `unused.grant` | incompleteness | [Manifest grants](14-authority.md#manifest-grants), [Contributors](25-registries.md#contributors) |
| `unused.item` | incompleteness | [Unused code](#unused-code) |
| `unused.local` | incompleteness | [Unused code](#unused-code) |
| `unused.tombstone` | incompleteness | [Unused code](#unused-code) |
