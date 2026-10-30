# Evolution and governance

This chapter defines language versions and the `language` directive, what a version fixes, successive versions, linking across versions, `nan upgrade --language`, prover monotonicity with advisories, trade-in, nanbench and the reservation register.

## Language versions

A **language version** is a positive integer naming one fixed definition of NaN. This specification defines version 1, called language 1.

The `language` [directive](03-packages-modules.md#manifest) states the version of its package. Every source file of the package is checked, formatted, proved and elaborated under exactly that version; no file, item, command option or environment variable states another.

- The version is the value of the directive's `INT`. An `INT` that is not written in decimal, or that carries a literal suffix, is an error (`language.version-form`); the exact fix writes its value in decimal without a suffix, and the package is checked under that value.
- A version the toolchain does not implement is an error (`language.unsupported`), reported on the directive. No command then checks, formats or builds any other file of the package.
- An implementation of NaN 1.0 implements exactly version 1.

```nan
// file: wire/package.nan
//! Link header decoding.

package wire {
    language 1
    targets any
} wire

// file: wire/wire.nan
//! Link header limits.

pub const MAX_PAYLOAD: u16 = 1_400
```

```nan
// file: wire/package.nan
//! Link header decoding.

package wire {
    language 2 // error: language.unsupported
    targets any
} wire

// file: wire/wire.nan
//! Link header limits.

pub const MAX_PAYLOAD: u16 = 1_400
```

```nan
// file: wire/package.nan
//! Link header decoding.

package wire {
    language 0x1 // error: language.version-form
    targets any
} wire

// file: wire/wire.nan
//! Link header limits.

pub const MAX_PAYLOAD: u16 = 1_400
```

### Contents of a version

A language version fixes:

- its grammar, as [Grammar](41-grammar.md) publishes it, and the keyword table generated from it ([keywords](01-lexical.md#keywords));
- every rule deciding validity and meaning, including the std items and contracts this specification declares;
- the [canonical format](35-format.md#canonical-format);
- the diagnostic codes and their classes ([codes](34-diagnostics.md#codes));
- the [idiom table](34-diagnostics.md#idiom-table) and the [exact fixes](34-diagnostics.md#exact-fixes) each rule names;
- the [prover](20-prover.md#procedure), which only an [advisory](#prover-monotonicity) amends;
- the target [descriptors](27-program-model.md#descriptors);
- the [NIR encoding](19-nir.md#encoding) version, the [artefact](30-artefacts.md#artefacts) format version and the [machine file](35-format.md#machine-files) format versions.

### Successive versions

Versions are ordered by their integers. Each version after 1 obeys these rules:

- It changes the meaning of no source that its predecessor accepts, except source using a form it retires. It gives meaning only to source its predecessor rejects, [reservations](#reservations) included.
- It keeps every std item of its predecessor, with the same interface and meaning.
- A **retired form** is a form of the predecessor that the version rejects. The version retires a form only with an exact rewrite into its own forms with the same meaning, and its grammar keeps the retired form as a rejected alternative whose diagnostic offers that rewrite as an exact fix.

Language 1 has no predecessor and retires no form.

### Artefacts and linking

The [code root](30-artefacts.md#code-roots) of every artefact records its package's language version. A toolchain implementing version N implements every version from 1 to N, and one dependency closure may hold packages of any of them.

- Each package is checked, proved and elaborated under its own version. A consumer's lines are checked under the consumer's version against its dependencies' facts; a fact kind that a dependency's version lacks is read as [requires-sets](29-identity-versioning.md#requires-sets) states.
- A dependency's version is the one its artefact records or, for a package checked from source, the one its manifest states. Std packages have the latest version the toolchain implements.
- A dependency whose version the toolchain does not implement is an error (`language.unsupported`). An artefact whose format version the toolchain does not implement is reported as [`artefact.version-unsupported`](30-artefacts.md#artefacts) instead.
- Otherwise, a package other than std whose version is later than that of a package depending on it is an error (`language.newer-dependency`).
- Both errors are reported on the `dep` directive of the checked package through which it reaches the dependency.
- A final closure lowers to one image whatever versions its packages state.

## Language upgrades

`nan upgrade --language N` ([commands](33-toolchain.md#commands)) moves each package of its scope from the version M its manifest states to version N:

1. N that the toolchain does not implement is an error (`language.unsupported`), reported without a source line. N below a package's M is an error (`language.downgrade`), reported on that package's `language` directive.
2. A package with an error under M has those errors reported.
3. For each package and each version K from M + 1 to N in order, every use of a form K retires is replaced by that form's rewrite, and the `language` directive is set to K.
4. Every changed file is put in the canonical format of N.
5. The workspace with these changes is checked, and its errors are reported.

The command writes the changed source files only when no step reports an error; otherwise it writes nothing. With `--language`, `nan upgrade` performs no resolution, fetches nothing and writes no machine file.

Upgrading the package of this block to the version it states changes no file:

```nan
pub const MAX_PAYLOAD: u16 = 1_400
```

```text
$ nan upgrade --language 1
ok upgrade . --language 1: files 0, errors 0, incomplete 0
ok upgrade . --language 1: files 0, errors 0, incomplete 0
```

## Prover monotonicity

**Prover monotonicity** relates the provers of successive versions: for every package that versions N − 1 and N both accept with the same meaning, every obligation site whose [outcome](20-prover.md#outcomes) under N − 1 is proved, by a proof no advisory retracts, is proved under N.

An **advisory** is a published notice retracting proofs after a prover soundness fix. It holds:

- its number, a positive integer unique among advisories;
- the language versions whose prover it amends;
- the amendment: a change to one step of the procedure that removes the unsound derivation, such that the amended procedure proves no query the unamended one does not;
- the retraction condition: a condition on a query's [canonical encoding](20-prover.md#determinism) that holds for every query whose proof by the unamended procedure may use the unsound derivation.

A toolchain carries an advisory when the advisory is part of its source tree, so carrying one changes its [toolchain hash](36-builds.md#toolchain-hash). A toolchain's prover for a version is the procedure amended by every advisory it carries that names that version ([determinism](20-prover.md#determinism)).

- A proven or assumed [status](18-contracts.md#statuses) recorded in an artefact is retracted when the query of its obligation site, built from the unit's NIR, meets the retraction condition of an advisory the toolchain carries that names the artefact's version. [Reuse](30-artefacts.md#reuse) decides that site anew.
- A diagnostic whose because-chain passes through a retracted status names the number of the advisory that retracted it.

## Trade-in

A **concept** is a rule, form or word that a [primer tier](39-primer.md#primer-tiers) teaches. The primer delta of a change to the language or the primer is the change in each tier's token count, as [nanbench](#nanbench) counts tokens. Every such change is recorded with its primer delta and its nanbench results.

**Trade-in** is the rule that a change whose delta takes a tier above its cap is accepted only together with the removal of concepts from that tier that brings it back within its cap.

## Nanbench

**Nanbench** is the benchmark of first-compile success, fix rate per diagnostic and comprehension that gates language versions. It consists of:

- a task corpus of write, repair and read tasks over library, host program, driver and kernel code, each with a starting workspace and acceptance tests;
- a set of models it names, each given only `nan primer` output and the command outputs of the toolchain under test;
- three tokenizers: `o200k_base`, `cl100k_base` and the tokenizer of Llama 3;
- a reference machine of the configuration that [latency](33-toolchain.md#query-engine) states, on which the latency requirement and the [measurements](38-self-hosting.md#measurements) are taken.

The **token count** of a text is the largest of the counts the three tokenizers give for its UTF-8 bytes, with no special tokens added.

Nanbench reports, each with its 95% confidence interval:

- first-compile success: the share of write tasks whose first submitted workspace `nan check` accepts without error;
- fix rate per diagnostic: for each diagnostic code, the share of diagnostics with that code that the agent's next edit removes without introducing an error;
- comprehension: the share of read-task questions answered correctly.

A language version is published only when, for each metric and each code's fix rate, the upper end of its confidence interval is at least the value its predecessor measured on the same corpus and models. The nanbench results that admit a version, and for language 1 the [measurements](38-self-hosting.md#measurements), are published with that version.

## Reservations

A **reservation** is a word, syntax, name, format or seam space that language 1 reserves without meaning. Exactly the reservations of this register exist; each owner states its rule.

| Reservation | Kind | Position | In language 1 | Owner |
| --- | --- | --- | --- | --- |
| `stable`, `flexible`, `migrate`, `machine`, `protocol`, `ipc` | word | item-head modifier or kind | `item.reserved-word` | [Reserved forms](02-items.md#reserved-forms) |
| `bank` | word | start of a `registers` member line | `register.bank-reserved` | [Registers](23-low-level.md#registers) |
| `self.{a, b}` | syntax | a `self` parameter | `item.self-field-set` | [Reserved forms](02-items.md#reserved-forms) |
| `uses` with a word other than `wait` | syntax | a `uses` clause | `effect.uses-unknown` | [Waiting](10-effects-digests.md#waiting) |
| `ui` | name | first segment of a path of two or more segments | `name.reserved` | [Aliases](03-packages-modules.md#aliases) |
| `sys::infer`, `sys::load`, `core::simd` | name | first two segments of a path | `name.reserved` | [Reserved std names](28-stdlib.md#reserved-std-names) |
| `x86_64-nanos`, `aarch64-nanos` | name | the `targets` directive | `manifest.reserved-target` | [Manifest](03-packages-modules.md#manifest) |
| coverage classes other than `sc` | name | the class of a replay token | `test.replay-token` | [Coverage classes](22-testing.md#coverage-classes) |
| framing of record fields by key, and by length in `mem::wire` | format | Encode records | skipped fields are discarded | [Decoding rules](13-text-encode.md#decoding-rules) |
| sections native cache and availability | format | an artefact | `artefact.reserved-section` | [Sections](30-artefacts.md#sections) |
| registry snapshot and policy floor fields | format | the lock | `format.reserved-field` | [Lock](35-format.md#lock) |
| algorithm tags other than `0x01` | format | a hash | the bytes are no hash | [Hash values](29-identity-versioning.md#hash-values) |
| codegen values other than the base instruction set | format | a descriptor | never selected | [Descriptors](27-program-model.md#descriptors) |
| the user-origin seams, `hw::Vector.user` and `hw::SyscallEntry.handler` | seam | an exception raised in user mode, and the system call instruction | contributions are checked; the seams are never entered | [User exceptions and syscalls](24-kernel.md#user-exceptions-and-syscalls) |

## Diagnostics

| Code | Class | Rule |
| --- | --- | --- |
| `language.downgrade` | error | [Language upgrades](#language-upgrades) |
| `language.newer-dependency` | error | [Artefacts and linking](#artefacts-and-linking) |
| `language.unsupported` | error | [Language versions](#language-versions), [Artefacts and linking](#artefacts-and-linking), [Language upgrades](#language-upgrades) |
| `language.version-form` | error | [Language versions](#language-versions) |
