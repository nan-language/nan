# Artefacts

This chapter defines artefacts with their attestation files, code roots, the sections signing them, the host ELF section `.nan`, reuse, lowering with type layout, niche packing and the ABI, and host symbol names.

## Artefacts

An **artefact** is the `.nart` file of a state of a package: its code root followed by its **attestation file**, built from the package's source files and machine files as they stand. [Publishing](29-identity-versioning.md#publishing) builds the artefact of a named release once it has moved the tombstones, `nan dep sync` that of a [path release](29-identity-versioning.md#local-store), and a package checked from source contributes that of its current state.

An artefact is written in the forms of the [canonical NIR encoding](19-nir.md#encoding). Its bytes are, in order:

```text
4E 41 52 54          magic
uvar                 format version, 1 in language 1
bytes                code root
bytes                attestation file
record               sections
```

- A hash is bytes holding its 33 bytes ([hash values](29-identity-versioning.md#hash-values)), and a path or name is a text; the other forms are the NIR encoding's.
- A list whose order this chapter does not state is sorted by the bytes of its elements' encodings.
- An implementation must write exactly this encoding. An artefact that does not decode, does not start with the magic, holds a record key this chapter does not define, or is not the canonical encoding of what it decodes to is an error (`artefact.malformed`). A defect of the NIR stream of part 4 is reported only as [Encoding](19-nir.md#encoding) states (`nir.malformed`, `nir.version-unsupported`). An artefact of a format version the toolchain does not implement is an error (`artefact.version-unsupported`).
- An artefact whose code root hash differs from the code root that its package's lock line names is an error (`artefact.root-mismatch`).
- The errors of this section and of [Reuse](#reuse) are reported on the `dep` [directive](03-packages-modules.md#manifest) through which the checked package reaches the artefact's package, and for a std package without a source line, naming the package.

The attestation file is a record with these keys:

| Key | Part | Content |
| --- | --- | --- |
| 1 | toolchain hash | the [toolchain hash](36-builds.md#toolchain-hash), for the package's language version, of the toolchain that wrote the artefact |
| 2 | source tree hash | the hash of the encoding of a list of pairs, one per [source file](01-lexical.md#source-files) and [machine file](35-format.md#machine-files) of the package: its path relative to the package directory, with `/` separators, and its content as bytes |
| 3 | seals | a list of records, one per [seal](31-trust.md#seals) that the package's API snapshots record, keyed as the next table states |

| Seal key | Content |
| --- | --- |
| 1 | the principal |
| 2 | the canonical path of the sealed item |
| 3 | its [sealed item hash](31-trust.md#seals) |
| 4 | its closure hash |
| 5 | its closure entries, each a record of 1 a uvar, 0 for an entry with an interface hash and 1 for one with an item hash, 2 the canonical path and 3 the hash |
| 6 | its [signature](31-trust.md#signatures), as bytes, and 7 the public key the package's [principals file](31-trust.md#principals) gives its principal, as bytes; both present exactly when the seal holds a signature and the principals file gives its principal a key |

The attestation file is not part of release identity: artefacts with equal code roots are artefacts of one release.

## Code roots

A **code root** is the reproducible part of an artefact ([reproducibility](36-builds.md#reproducibility)). The hash of its bytes is the **code root hash**, which identifies its release. A code root is a record with exactly these keys:

| Key | Part | Content |
| --- | --- | --- |
| 1 | language version | the package's [language version](37-evolution.md#language-versions), a uvar |
| 2 | manifest | the text of `package.nan` in [canonical format](35-format.md#canonical-format) |
| 3 | interface | a list of pairs, one per module: the module's name and the text of its [API snapshot](31-trust.md#api-snapshots) as `nan api update` writes it for the package's sources, without its `seal`, `closure`, `signature` and `check` records and without the `item` record of an entry left with no other record, or the empty text when `nan api update` writes no snapshot for the module |
| 4 | NIR | a [NIR stream](19-nir.md#encoding), as bytes, of every [unit](19-nir.md#units) of the package except the units of tests whose [test target](22-testing.md#test-items) is an item of the package |
| 5 | requires-set | the package's [requires-set](29-identity-versioning.md#requires-sets), a list of records keyed as the entry table states |
| 6 | source tombstones | a list of the [normalised forms](29-identity-versioning.md#normalised-form) of the `retired` items in the package's source |
| 7 | history tombstones | a list of the normalised forms of the tombstones its [history](29-identity-versioning.md#history) holds |
| 8 | ledger | the package's [ledger](31-trust.md#ledger), a list of records keyed as the ledger table states |
| 9 | line tables | a list of pairs, one per source file of the package holding the first line of a [span](19-nir.md#spans) in part 4: the file's path as spans write it, and a list of pairs, in line order, of each such line's number and its text without its line end |
| 10 | schema trees | a list of pairs, one per exported type implementing `Encode` ([encode class](29-identity-versioning.md#encode-class)) and per [component state](26-components.md#component-state) the package declares: its canonical path and the `mem::wire` encoding, as bytes, of its [schema tree](13-text-encode.md#schema-hashes) |

| Entry key | Content |
| --- | --- |
| 1 | the canonical path of the item, or, for a forwarding use, of its replacement |
| 2 | for a forwarding use, the matched old form as its tombstone's normalised form writes it; absent otherwise |
| 3 | the roles, each a uvar, its row's position from 0 in the [role table](29-identity-versioning.md#roles) |
| 4 | the interface hash |
| 5 | the behaviour epoch, a uvar |
| 6 | the relied facts, each a pair of the field's key in the encoding of [digests](19-nir.md#encoding) and the canonical path of the capability or validator, the bound as a digest line writes it for the class, or the empty text for another field; a pair naming the terminates field records reliance on its presence, one naming the class reliance on the class being at most its bound, any other pair reliance on an absence |
| 7 | the method resolutions, each a pair of the method name and the function reference found, as the NIR encoding writes one |
| 8 | the item hash, when the entry records one; absent otherwise |
| 9 | the implementer names, each a text, in byte order |
| 10 | the schema hashes, each as bytes, in byte order |

| Ledger key | Content |
| --- | --- |
| 1 | the canonical path of the item holding the entry |
| 2 | its kind, a uvar, its row's position from 0 in the ledger table |
| 3 | the path, as spans write it, of the file holding its line |
| 4 | its line number |
| 5 | the texts its row says it holds, in the row's order: each premise, reason and `todo` text as its literal's content; each condition in normalised form; a validator's class as `parsing` or `declassifying`; a waiver's threshold as written and its reason as its literal's content; a grant as `device` or `priv` followed by its modules and aliases; a device premise's class as a digest line writes it; an undoer as its canonical path |

- A unit holds its digests and its `example` functions, and the units of [reliance tests](22-testing.md#reliance-tests) are in part 4.
- Trap reports and diagnostics quote the lines of a package whose units come from an artefact from its line tables.

## Sections

The sections record of an artefact holds none of its fields, or exactly fields 1, 4 and 5, which `nan publish --as` writes ([publishing](29-identity-versioning.md#publishing)); keys 2, native cache, and 3, availability, are reserved. A sections record holding key 2 or 3 is an error (`artefact.reserved-section`), and one holding another set of keys is `artefact.malformed`.

| Key | Section | Content |
| --- | --- | --- |
| 1 | signature | the publisher's signature: a record of 1 the principal, 2 its public key, as bytes, and 3 the [signature](31-trust.md#signatures), as bytes, of `nan artefact 1`, the principal, the [key form](31-trust.md#principals) of the public key, and the full forms of the hashes of the code root, of the attestation file and of the provenance section, each followed by LF |
| 4 | certificate | the builder's attestation that the package's dependency closure passed the [release gate](31-trust.md#release-gate) under a toolchain: a record of 1 the principal, 2 its public key, as bytes, 3 the toolchain hash, 4 a list of pairs, one per package of the closure, the package itself included and std excluded, of its package name and code root hash, and 5 the signature, as bytes, of `nan certificate 1` and LF followed by the encoding of the record of fields 1 to 4 |
| 5 | provenance | a record of 1 the author principals, a list of texts, and 2 the package's [lock](29-identity-versioning.md#lock) as bytes, empty when it has none |

An artefact **verifies** when every signature it holds [verifies](31-trust.md#signatures) by the public key beside it: each signed seal's of its seal message, the publisher's, and the builder's, whose certificate names the publisher's principal and key and lists the artefact's code root hash. Its bytes alone decide it, so a device verifies an artefact without the toolchain. A key binds a signature to a principal only for a verifier that gives that principal that key: a package **accepts** the artefact of a release that its lock locks when the artefact verifies, its publisher key is the one its lock line records, none when it records none, its [principals file](31-trust.md#principals) gives its publisher exactly the key the artefact holds, and it gives the principal of each signed seal that key or none, such a seal then counting for nothing more than an unsigned one.

- An artefact of a locked release that the locking package does not accept is an error (`artefact.bad-signature`), and its release is not read: a signature by a principal the package does not trust, by another key under a trusted principal's name, or sections stripped from or added to the release its lock records.

## Host ELF section

Every image of a host target's [final build](27-program-model.md#final-builds) holds exactly one ELF section named `.nan`, of type `SHT_PROGBITS`, with no flags and alignment 1. Its content is a list of bytes, one per package of the final closure, std included, in byte order of package name, each holding that package's artefact. A package checked from source contributes the artefact of its current state. An image a toolchain written in NaN writes also holds exactly one ELF section named `.nan.tree`, of the same type, flags and alignment, holding the hash of the encoding of the list of the code roots of the artefacts of its `.nan` section, in that section's order.

The runtime never reads these sections; a toolchain written in NaN reads its own image's for its [toolchain hash](36-builds.md#toolchain-hash). Images of the machine targets hold no artefact.

## Reuse

**Reuse** is using a dependency's checked NIR without checking it again. In a dependency closure, the checked package and each package whose locked release is the current state of a path source ([lock](29-identity-versioning.md#lock)) are checked from source. The units of every other package come from the artefact of its locked release, or for a std package from the toolchain's.

For each package whose units come from an artefact, after every package it depends on, the checker:

1. decides anew, under the package's language version, each obligation site whose proven or assumed [status](18-contracts.md#statuses) an [advisory](37-evolution.md#prover-monotonicity) the toolchain carries retracts;
2. when the closure [satisfies](29-identity-versioning.md#requires-sets) the package's requires-set, reuses its units: their well-formedness and other statuses stand;
3. otherwise checks the package again from its NIR: each unit must name only items of the closure that the package may name ([visibility](03-packages-modules.md#visibility)) and be [well formed](19-nir.md#well-formedness), the prover decides each obligation site anew under the package's language version, the interpreter runs each [example](21-interpreter.md#example-runs), and each schema tree of the code root must be the tree its type has in the closure;
4. re-derives the digest of each function of each unit, for each target, by [propagation](10-effects-digests.md#propagation) from its NIR, its statuses and the digests of the closure, and checks each [limit](10-effects-digests.md#limits) of the unit other than `stack N` against them.

- Reuse changes a unit only in its digests and statuses.
- A violation found in step 3 or 4 is an error (`artefact.recheck-failed`), with the diagnostic of the violated rule, when it has one, in its because-chain.

## Lowering

**Lowering** is the translation of NIR to machine code for one target. It runs for each image of a final build and for each process or test image of a [test run](22-testing.md#test-runs); the final closure of a test run is the dependency closure of the tested package together with that of the [platform package](28-stdlib.md#platform-packages) of the run target's descriptor. Lowering reads the units of the final closure, never source, taking for each item the unit of the [target variant](03-packages-modules.md#target-variants) its target matches, and:

1. computes the type layout of every type the image's code uses;
2. resolves every [symbolic field](19-nir.md#symbolic-fields) to its offset in that layout;
3. instantiates each generic item once per [instantiation key](29-identity-versioning.md#instantiation-keys) the image reaches;
4. emits the `body`, `entry`, `closure` and `default` functions of the units in the image's reach ([final builds](27-program-model.md#final-builds), [test items](22-testing.md#test-items)) and every function their kept checks call, with a run-time check for each [obligation site](19-nir.md#obligations) whose status is checked, and never emits a call of an `assume` function; a call of a `requires`, `ensures`, `old`, `decreases`, `predicate`, `invariant` or `where` function whose result feeds only checks that are not kept, or only `assume` instructions, is not emitted, and each such function that a kept check reads, directly or through an `old` value, is emitted;
5. emits the [registry tables](25-registries.md#tables), [compiler stubs](23-low-level.md#compiler-stubs) and [support routines](27-program-model.md#runtime-support) the image needs.

- Lowered code behaves exactly as its NIR does, with the same results, failures, traps, trap reports and bit patterns.
- Lowered code computes on Secret values as [constant-time lowering](15-untrusted.md#constant-time-lowering) states.
- Machine code lowered for one final closure is part of no image of another.
- The [stack limits](10-effects-digests.md#stack-limits) check reads the frame sizes lowering reports.

### Type layout

The **type layout** of a final closure gives every type a size and an alignment in bytes. Every alignment is a power of 2 and every size a multiple of its type's alignment. Within one final closure every type has exactly one layout.

The types with a [fixed layout](21-interpreter.md#interpreter-memory) have these layouts on every target, and `Array[T, N]` has its row's layout for every T:

| Type | Size | Alignment | Bytes |
| --- | --- | --- | --- |
| `u8`, `i8`, `bool` | 1 | 1 | the value; `bool` as 0 or 1 |
| `u16`, `i16` | 2 | 2 | the value, least significant byte first |
| `u32`, `i32`, `f32`, `char` | 4 | 4 | the value, least significant byte first; a float as its IEEE bits; a `char` as its scalar value |
| `u64`, `i64`, `usize`, `isize`, `f64` | 8 | 8 | as for 4-byte types |
| `hw::Ptr[T]` | 8 | 8 | the address, as a `u64` |
| integer layout | that of its representation | that of its representation | as [layout values](23-low-level.md#layout-values) state |
| bytes layout `bytes N` | N | 1 | as layout values state |
| `Array[T, N]` | N × the size of T | that of T | element i at byte i × the size of T |

Signed integers are in two's complement. The layout of every other type is part of the [ABI](#abi).

```nan
pub layout Header: bytes 8 le {
    magic: u16 at 0
    lanes: Array[u16, 3] at 2
} Header
```

```nan
pub layout Header: bytes 10 le { // error: layout.uncovered
    magic: u16 at 0
    lanes: Array[u16, 3] at 2
} Header
```

### Niche packing

Lowering represents the values of some enums inside the [niches](04-types.md#niches) of a payload. The **niche count** of a type is the number of its niches that packing uses:

- 254 for `bool`, and 4_293_855_232 for `char`;
- 1 for `Slice[T]`, `SliceMut[T]`, `Str`, `Dyn[I]` and `mem::Box[T]`: the zero address;
- 2^w minus the number of tags for a tags layout whose representation has w bits;
- for any other layout, the largest niche count among its fields of a layout or `Array` type, 0 when it has none;
- for a struct, an array or a payload, the largest niche count among its fields, elements or payload fields, 0 when it has none;
- for a packed enum, the niche count of its payload minus the number of its unit variants;
- 0 for every other type.

An **empty type** is `Unit`, a struct without fields, or a struct or array whose fields or elements all have empty types. A variant whose payload types are all empty counts as a unit variant in this section.

An enum is **packed** when exactly one of its variants has a payload that is not empty, every other variant counts as a unit variant, and that payload's niche count is at least the number of those variants. A packed enum has the size and alignment of its payload, a payload of several fields being a struct of them: the variant with the payload is represented as its payload, and each unit variant as a distinct niche of the payload.

| Type | Size |
| --- | --- |
| `Option[bool]` | 1 |
| `Option[Option[bool]]` | 1 |
| `Option[char]` | 4 |
| `Option[mem::Box[T]]` | the size of `mem::Box[T]` |
| `Result[mem::Box[T], Unit]` | the size of `mem::Box[T]` |
| `Result[Unit, bool]` | 1 |

### ABI

The **ABI** of a final closure is every choice of representation and calling that this chapter leaves open: the layouts of structs, enums that are not packed, views, boxes, function values, closures, `Dyn` values and task frames; field order, padding and tag values; the niche representing each unit variant of a packed enum; how NaN functions pass arguments, results and failures; the encoding of an image's line tables and frame records; the binding of symbols, every symbol name the specification does not fix, and the objects lowering produces. The ABI is unspecified.

Between code of different final closures, the only defined representations are the fixed layouts and the conventions of [foreign fns](23-low-level.md#foreign-functions) and function types with a [convention clause](23-low-level.md#calling-conventions).

## Host symbol names

In machine code lowered for a host target, each lowered function, each const item placed in memory and each registry table has a **host symbol name** in the symbol table of the object holding it. The name is the concatenation of:

1. `_N`;
2. the package name, preceded by its length in decimal;
3. `_` and the **code root prefix**: the first 12 characters of the [checksummed base32](29-identity-versioning.md#hash-values) of the 32 digest bytes of the package's code root hash;
4. `_`, each segment of the item's [canonical path](29-identity-versioning.md#canonical-paths) after the package name preceded by its length in decimal, a segment that follows `.` also preceded by `D` before its length, then `E`; a segment `impl[T: I]` is written `I` followed by the nested forms of the canonical paths of T and I, a **nested form** being `N`, the package name preceded by its length, each later segment as this step writes it, and `E`;
5. for a NIR function of a role other than `body`, `F`, the role's word preceded by its length, and the function's index in decimal, 0 for a role without an index;
6. for code of an instantiation, `_` and the first 16 characters of the checksummed base32 of the 32 digest bytes of its instantiation key.

| Lowered entity | Host symbol name |
| --- | --- |
| `ring::Ring.pop` instantiated with `u8` and `256` | `_N4ring_<C>_4RingD3popE_<I>` |
| the closure of index 0 in `linkmon::main` | `_N7linkmon_<C>_4mainEF7closure0` |
| the checking entry of `stats::percent_of` | `_N5stats_<C>_10percent_ofEF5entry0` |
| `meter::impl[core::u64: meter::Weighted].weight` | `_N5meter_<C>_IN4core3u64EN5meter8WeightedED6weightE` |

In the table, `<C>` stands for the package's code root prefix and `<I>` for the 16 characters of the instantiation.

- The program entry seam has the symbol `main`, which the C library's start-up code calls. The symbols of the other compiler stubs and of support routines start with `_NS`.
- Two distinct entities lowered for one image or process with the same host symbol name are an error (`artefact.symbol-collision`), reported on the `package` line of the final-link root or tested package, quoting both entities.

## Diagnostics

| Code | Class | Rule |
| --- | --- | --- |
| `artefact.bad-signature` | error | [Sections](#sections) |
| `artefact.malformed` | error | [Artefacts](#artefacts), [Sections](#sections) |
| `artefact.recheck-failed` | error | [Reuse](#reuse) |
| `artefact.reserved-section` | error | [Sections](#sections) |
| `artefact.root-mismatch` | error | [Artefacts](#artefacts) |
| `artefact.symbol-collision` | error | [Host symbol names](#host-symbol-names) |
| `artefact.version-unsupported` | error | [Artefacts](#artefacts) |
