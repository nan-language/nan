# Charter and guarantees

This chapter defines the terms agent and toolchain, the reading conventions, the three guarantees, the systems guarantees, the scope of language 1 and conformance, then gives the glossary and the chapter map.

## Terms

An **agent** is a human or model that reads and writes NaN source through the toolchain from partial views, such as a window of lines or a command's output, with no memory between sessions.

The **toolchain** is the `nan` command and everything it runs. The components that rules name as subjects include the checker, which decides validity and reports diagnostics, the prover, the interpreter, the query engine, `nan fmt`, the merge driver, lowering, and the runtime, which is exactly the [support routines](27-program-model.md#runtime-support) an image holds.

## Reading conventions

### Normative wording

Every sentence and example is normative, and each rule is a present-tense sentence with one meaning. These wordings carry a rule's force:

| Wording | Meaning |
| --- | --- |
| "X is an error (`family.code`)." | Source containing X is rejected with a diagnostic of class error and that code, by every command applying the rule ([check time](33-toolchain.md#check-time)). |
| "X is incompleteness (`family.code`)." | X is reported with a diagnostic of class [incompleteness](31-trust.md#incompleteness) and that code; `nan check` and `nan test` accept the source, and the [release gate](31-trust.md#release-gate) (`nan verify`, `nan publish`) rejects it. |
| "X traps (`trap.kind`)." | Executing X is a [trap](11-errors-domains.md#traps) of that kind. |
| "must", "must not" | A requirement. On a program, the sentence or the next names the code a violation produces; on an implementation, the subject names the bound component. |
| "may" | A permission. |
| "only", "never", "exactly" | A closed statement: what is not listed is excluded. |
| "is unspecified" | Any choice conforms, and no rule's outcome depends on it. |
| "is the implementation's choice" | Each implementation makes the choice, identically for one [toolchain hash](36-builds.md#toolchain-hash); used only for memory placement, draw timing and draw sizes ([budgets](12-memory.md#budgets)). |

Each diagnostic code belongs to one family, whose owning chapter lists the code and its class in its Diagnostics section; [Diagnostics](34-diagnostics.md#codes) catalogues them all. Trap kinds form the `trap` family and are never diagnostic codes.

### Notation

- Code font marks NaN text, file names, commands and diagnostic codes. Terms are plain text, bold where defined and linked to that definition at their first use in a section.
- Core vocabulary names appear without a path; every other std item appears with its alias path, as `mem::Box`, and a member as `Type.member`, as `mem::Alloc.split`.
- Fenced blocks are tagged `nan` for source, `nan-api` for std declarations, `ebnf` for grammar, and `text` for commands with their output, for other exact text a sentence names, and for diagrams.

### Grammar notation

Grammar blocks use one EBNF dialect, written here in itself:

```text
grammar = rule+ ;
rule = ( RULE_NAME | TOKEN_NAME ) "=" choice ";" ;
choice = sequence ( "|" sequence )* ;
sequence = element+ rejection? ;
element = atom ( repeat | "-" atom )? ;
repeat = "?" | "*" | "+" ;
atom = RULE_NAME | TOKEN_NAME | CONTEXT | char_atom | "(" choice ")" ;
char_atom = char ( ".." char )? ;
char = QUOTED | CODE_POINT ;
rejection = "{!" CODE "}" ;
```

- A lower_snake_case `RULE_NAME` names a syntactic rule, over tokens. An UPPER_SNAKE_CASE `TOKEN_NAME` names a lexical rule, over characters; only [Lexical structure](01-lexical.md) defines lexical rules, the tokens they produce and the virtual tokens `NL`, `COL0` and `EOF`.
- `QUOTED` is text between double quotes. In a lexical rule it matches those characters; in a syntactic rule it matches one token, a hard keyword when the text has identifier shape and otherwise the punctuation or operator token so spelt.
- `CONTEXT` is a word between single quotes, used only in syntactic rules. It matches an `IDENT` token so spelt. In the rules that `source_file` and the classification rules reach, such a word is a context word, or a reserved word where only rejected alternatives reach it; in machine files and the NIR text notation it is matched by spelling alone. Where an `IDENT` atom could match the same token, [one-token lookahead](41-grammar.md#one-token-lookahead) decides between them.
- `CODE_POINT` is `U+` and 4 to 6 uppercase hexadecimal digits. The double quote, single quote and backslash are always written `U+0022`, `U+0027` and `U+005C`; quoted text has no escapes. `CODE` is a diagnostic code.
- In lexical rules only, `a .. b` matches one character in that inclusive range, and `A - B` matches what `A` matches unless `B` matches the same text.
- Postfix `?`, `*` and `+` bind tightest, then sequence, then `|`; parentheses group.
- A `rejection` marks an alternative that is parsed, then rejected with the diagnostic code it names. Reserved syntax is written so, and a rejected alternative is not part of the language.
- Prose after a block states the constraints the notation cannot express. Every `ebnf` block is normative; [Grammar](41-grammar.md) collects them.

## Guarantees

### Closure

The **closure set** of a line is exactly:

- its enclosing item;
- the names of the items its module declares;
- each item and member it names, each [tombstone](29-identity-versioning.md#forwarding) whose old form one of its uses matches, and the type of each expression in it, with what each exposes at the line's position under the [visibility](03-packages-modules.md#visibility) rules: head and shape, contracts and examples, structural properties, digest, codec uses, a const's value, a transparent body, the impl lines naming it and a type's members;
- for a line holding a [const context](21-interpreter.md#const-evaluation), an example clause or an obligation whose outcome a [refutation confirmation](21-interpreter.md#refutation-confirmation) decides, the NIR of every function those [interpreter](21-interpreter.md#interpreter) runs execute;
- the manifest directives naming its module; its package's `language`, `targets` and `entry` directives; and the `dep` directive of each alias it uses, with the release the [lock](29-identity-versioning.md#lock) names for it;
- the [core vocabulary](03-packages-modules.md#core-vocabulary).

A fact the toolchain computes for an item, such as a structural property, a digest fact or a const's value, is computed from the closure sets of that item's own lines.

The **closure guarantee**: a line's meaning, its acceptance and the diagnostics reported on it depend only on its closure set, so a change outside that set changes none of them. The candidates and stubs offered for a name that resolves to nothing, and the choice of `name.newer-release` over `name.unknown`, are exempt ([unknown names](03-packages-modules.md#unknown-names)). `nan context` prints the closure set of an item or a line ([Toolchain](33-toolchain.md#context)).

Exactly these rules relate several items instead of deciding one line, and the closure guarantee does not cover them:

- item names unique in a module ([Items](02-items.md#order-and-names)), distinct after folding ([naming conventions](03-packages-modules.md#naming-conventions)), and at most one impl line per type and interface ([coherence](08-generics-interfaces.md#coherence));
- the acyclic [module](03-packages-modules.md#modules) graph, identical items and docs across [target variants](03-packages-modules.md#target-variants), and each module's summary file ([module summaries](32-docs.md#module-summaries));
- taken tombstone forms ([Tombstones](29-identity-versioning.md#tombstones)) and [unused code](31-trust.md#unused-code) and grants;
- a dependency holding an authority its alias is not granted, and the closure summary ([manifest grants](14-authority.md#manifest-grants), [authority summaries](14-authority.md#authority-summaries));
- registry admission and [tables](25-registries.md#tables), and [stack limits](10-effects-digests.md#stack-limits);
- [test runs](22-testing.md#test-runs);
- the rules of the [release gate](31-trust.md#release-gate) and [doc binding](32-docs.md#doc-binding);
- the rules of [commands](33-toolchain.md#commands) that compare or write versions: edits, merges, publishing and dependency resolution.

### Prefix viability

**Prefix viability** is the guarantee that text already written fixes all meaning:

- The meaning of a token (its syntactic role, the declaration a name resolves to, the type of each expression it ends, the mode of each access) depends only on the text of its item before it and on its line's closure set other than its enclosing item. Later text of its item never changes it.
- Every source file, complete or a prefix, has a defined [first dead token](41-grammar.md#prefix-viability) or none.
- In a prefix, a `fails`, `uses wait` or `from` entry that the written text requires and the head lacks is a [pending clause](41-grammar.md#prefix-viability), a lower bound, not an error.
- Every prefix ending before its first dead token becomes a file that `source_file` derives by adding its pending clauses and appending typed holes and closers ([closing](41-grammar.md#prefix-viability)). `nan check --prefix` reports the first dead token, the pending clauses, the holes and the closers ([prefix checking](33-toolchain.md#prefix-checking)).

### Item granularity

**Item granularity** is the guarantee that the [item](02-items.md#item-shape) is the unit of identity, checking, editing, merging, sealing and versioning:

- Every member and every item other than a tombstone has a [canonical path](29-identity-versioning.md#canonical-paths), and every item has an item hash and an interface hash.
- An edit leaving an item's interface hash and digest unchanged changes no diagnostic of another item, except through interpreter runs executing the item and through the rules relating several items.
- A [compare-and-swap edit](33-toolchain.md#compare-and-swap-edits) replaces one item only while its item hash is unchanged, and the [merge driver](33-toolchain.md#merge-driver) merges per item and per order-free member.
- A [seal](31-trust.md#seals) binds one item hash; [compatibility](29-identity-versioning.md#compatibility), requires-sets and behaviour epochs are per item.
- Source files are the only source of truth and hold no hashes or versions; every other file the toolchain writes in a package's directory is a [machine file](35-format.md#machine-files).

## Systems guarantees

NaN has no garbage collector, no mandatory runtime and no undefined behaviour in safe code:

- Memory returns to its [store](12-memory.md#stores) at [end of scope](07-ownership.md#ownership) or through the store's own operations; no collector traces or scans memory.
- An image holds only the [support routines](27-program-model.md#runtime-support) that its code and stubs call, no item is initialised at run time, and no item holds mutable state ([authority](14-authority.md#authority)).
- [Safe code](23-low-level.md#safe-code) has no undefined behaviour: each operation has the behaviour its rule defines, and the [memory model](23-low-level.md#memory-model) makes safe code data-race-free and sequentially consistent.

The exceptions are exactly these, and undefined behaviour arises only through them, as [Safe code](23-low-level.md#safe-code) states:

- [trusted code](23-low-level.md#trusted-code), `assume` and [foreign fns](23-low-level.md#foreign-functions), which `allow trusted` permits;
- the device and privileged operations that `allow device` and `allow priv` permit ([manifest grants](14-authority.md#manifest-grants)), which reach memory and machine state no type describes.

Trusted code, an `assume`, a foreign fn and a `device` or `priv` grant are [trust escapes](31-trust.md#trust-escapes) of the item holding them, the `package` item for a grant, as are landing fns and declassifying validators and decoders. The [ledger](31-trust.md#ledger) lists each, and `nan verify` requires a current seal on every item holding one.

## Scope

This specification, NaN 1.0, defines language 1, which a manifest selects with `language 1` ([Evolution and governance](37-evolution.md#language-versions)). It fixes every rule deciding whether source is valid and what it means, and every output it states exactly, from diagnostic codes to hashes and file formats.

Meaning never varies with target or environment, and varies with implementation only through the choices a rule leaves to it ([normative wording](#normative-wording)): targets and environments only restrict which programs are valid ([descriptors](27-program-model.md#descriptors)), and the release gate only restricts which are released.

A [reservation](37-evolution.md#reservations) has no meaning in language 1; the reservation register lists each one with what it produces.

Where a rule permits several behaviours, such as the interleavings of tasks, an execution has one of them. Exactly one thing is unspecified: the [ABI](30-artefacts.md#abi) inside a final closure.

## Conformance

An **implementation** is a toolchain claiming conformance to this specification. A conforming implementation:

- accepts exactly the valid source of language 1 and gives it exactly the meaning stated here, on every [target](27-program-model.md#targets);
- reports exactly the diagnostics the rules name, root causes only ([Diagnostics](34-diagnostics.md));
- reaches for every obligation the outcome of the [prover](20-prover.md#procedure), proving neither more nor less;
- produces exactly every output this specification fixes;
- has no option, setting or environment variable changing validity or meaning;
- passes every conformance test.

A **conformance test** is a specification example run against an implementation, with the expectations its marker comments state. Every `nan` block, every `nan-api` block, and every `text` block holding commands or giving what a program of a `nan` block of its section writes when run is one.

### Example workspaces

The conformance harness places each `nan` block in a fresh workspace:

- A block without `// file:` lines is the file `example/example.nan`, to which the harness prepends the line `//! Specification example.`. The manifest `example/package.nan` is, in canonical format, that line and the item `package example` holding the directives `language 1` and `targets any`.
- In a block with column-0 lines `// file: <path>`, the lines after each such line, up to the next one and without trailing blank lines, form the file at that path, relative to the workspace root, whose first segment is the package directory. The block writes each file's `//!` summary.
- Before checking, the harness takes each package of the block, dependencies first, and runs `nan dep sync` and `nan api update` in it, then, when another package of the block names it by a store-name source, `nan publish --local <dir> --name <store name>`, `<dir>` being its package directory and `<store name>` that source's store name, into a local store holding nothing else.

### Marker comments

A **marker comment** is the text ` // error: family.code`, ` // incomplete: family.code` or ` // traps: trap.kind` ending a line of a `nan` block, with the space before `//`, whatever token the lexer makes of it: a comment, a doc comment, a file summary or an unterminated literal. A marked block holds one root cause and exactly one marker comment, on the line holding it. Marker comments stay in the files every run reads; on a doc comment or file summary line, their text is part of the doc text.

The harness runs `nan check` on every `nan` block; then, unless the block is marked with an error that [check time](33-toolchain.md#check-time) reports, `nan test`; unless it is marked with such an error or a [test-run code](22-testing.md#test-runs), `nan build` on each program of the block; and, when it is marked with a gate code ([incompleteness](31-trust.md#incompleteness)), `nan verify`:

- An unmarked block reports no diagnostic in any run, passes every test and builds every image.
- `// error: family.code`: for a code that check time reports, `nan check` reports exactly one diagnostic, of class error, with that code, on that line. For a test-run code, `nan check` reports none and `nan test` reports exactly one failing test, with that code as its cause and its head on that line. For a code that only a [final build](27-program-model.md#final-builds) reports, `nan check` reports none, every test passes and `nan build` reports exactly one diagnostic, with that code, on that line.
- `// incomplete: family.code`: `nan check` reports exactly one diagnostic, of class incompleteness, with that code, on that line; for a gate code, `nan check` reports none and `nan verify` reports exactly that one. Every test passes and every image builds.
- `// traps: trap.kind`: no run reports a diagnostic, every image builds, and exactly one test fails, through a trap of that kind whose [trap report](11-errors-domains.md#trap-reports) names that line as the trapping operation.

With each marker comment and the space before it removed, `nan fmt` rewrites no file of a `nan` block, except that it inserts a blank line after the summary line the harness prepends and, in a block marked `item.clause-order`, moves clause lines into their [canonical order](35-format.md#canonical-order).

A `nan-api` block starts with `// module: <alias or alias::module>`, and that std module declares each item of the block with exactly its text, the block omitting only bodies and members consumers cannot see.

In a `text` block, each line starting `$ ` is a command, run at the root of the workspace of the nearest earlier `nan` block of the same section, and the lines up to the next command are its exact output. A `text` block without commands is exactly the text the sentence before it names; when that text is what a program of the nearest earlier `nan` block of the same section writes, the harness compares it with what `nan run` of the program writes; the run of a machine target's program ends at its write to the [exit port](27-program-model.md#descriptors), or by a termination request from the harness once it has written as many bytes as the text holds. A `text` block that the sentence before it calls a diagram illustrates only and is not a conformance test.

```nan
// file: wire/package.nan
//! Link frame sizes.

package wire {
    language 1
    targets any
} wire

// file: wire/wire.nan
//! Frame size arithmetic.

pub fn frame_bytes(frames: u32) -> u64 {
    let frame_size: u64 = 1_500
    return u64.widen(frames) * frame_size
} frame_bytes
```

```nan
pub fn frame_bytes(frames: u32) -> u64 {
    let frame_size = 1_500 // error: type.literal-untyped
    return u64.widen(frames) * frame_size
} frame_bytes
```

```nan
pub fn frames_in(bytes: u32, frame_size: u32) -> u32 {
    return bytes / frame_size // traps: trap.division-by-zero
} frames_in

test frames_without_size for frames_in {
    expect frames_in(3_000, frame_size: 0) == 0
} frames_without_size
```

## Glossary

Each term links to its home, whose definition governs.

| Term | Definition |
| --- | --- |
| [accepted spec](31-trust.md#api-snapshots) | Spec as last accepted: by `nan api update` when it keeps the previous one, by a seal otherwise. |
| [acknowledged cancellation](17-suspension-io.md#acknowledged-cancellation) | Cancellation returning once the kernel or device confirms. |
| [advisory](37-evolution.md#prover-monotonicity) | Notice retracting unsound proofs. |
| [affine](05-properties.md#linear-types) | Of a type neither Copy nor linear. |
| [agent](#terms) | Human or model using the toolchain from partial views. |
| [alias](03-packages-modules.md#aliases) | Name a package gives a dependency or std package. |
| [allocator](12-memory.md#allocators) | `mem::Alloc`, the capability to allocate. |
| [ambient authority](14-authority.md#authority) | Authority without a capability; NaN has none. |
| [anchor](24-kernel.md#anchor) | The one value per kernel reached without being passed. |
| [AnyBits](05-properties.md#anybits) | Of a type whose every bit pattern is valid. |
| [API snapshot](31-trust.md#api-snapshots) | Machine-written interface and accepted spec of a module. |
| [argument](06-statements-expressions.md#calls) | Expression supplied for a parameter. |
| [argument label](06-statements-expressions.md#arguments-and-labels) | Parameter name written before an argument; optional on the leading argument, absent for `self`. |
| [artefact](30-artefacts.md#artefacts) | `.nart` file of a release. |
| [associated function](02-items.md#methods) | `Type.name` function without `self`. |
| [attenuation](14-authority.md#attenuation) | Deriving a weaker capability. |
| [attestation file](30-artefacts.md#artefacts) | Seals and build hashes appended to an artefact. |
| [authority](14-authority.md#authority) | Power to act beyond a function's own values. |
| [authority summary](14-authority.md#authority-summaries) | Primitive capabilities, `alloc`, `device`, `priv`, validators and decoders that a package's digests hold, and its items' trust escapes. |
| [base type](05-properties.md#structural-properties) | Std type whose owning chapter fixes its six structural properties. |
| [because-chain](34-diagnostics.md#diagnostic-record) | Reasons of a diagnostic, root cause first. |
| [behaviour epoch](29-identity-versioning.md#behaviour-epochs) | Per-item counter `nan publish --breaking` bumps. |
| [binding](06-statements-expressions.md#bindings) | Local introduced by `let` or `var`. |
| [body](02-items.md#heads) | Statements of a function, or its `= expr`. |
| [bootstrap](38-self-hosting.md#bootstrap) | Rust compiler implementing language 1. |
| [bound](08-generics-interfaces.md#generic-parameters) | Requirement on a generic parameter. |
| [boundary function](04-types.md#module-boundary) | Function of a struct's defining module that re-establishes and assumes its predicates: `pub` or `pkg`, implementing, used as a value, or a closure. |
| [box](07-ownership.md#box) | `mem::Box[T]`, whose contents never move. |
| [budget](12-memory.md#budgets) | Linear `mem::Budget` owning a cell. |
| [build cache](36-builds.md#cache) | Per-user cache of content-addressed entries and replaceable records. |
| [cancellation](11-errors-domains.md#cancellation) | Failure `task::Cancelled`. |
| [canonical format](35-format.md#canonical-format) | Layout `nan fmt` produces. |
| [canonical order](35-format.md#canonical-order) | Order `nan fmt` produces. |
| [canonical path](29-identity-versioning.md#canonical-paths) | Package name, then the module (left out for the root module), then the item and member: the identity of an item or member. |
| [capability](14-authority.md#capabilities) | Value conferring authority. |
| [capability type](05-properties.md#capability-types) | Type containing a primitive capability. |
| [capture](07-ownership.md#closures) | Enclosing place a closure accesses. |
| [cell](12-memory.md#budgets) | Accounting record of a budget. |
| [change class](31-trust.md#change-classes) | Kind of change `nan diff` reports. |
| [channel](16-concurrency.md#channels) | `task::Sender[T]` and `task::Receiver[T]` pair. |
| [checking entry](18-contracts.md#checking-entries) | Indirect-call entry checking requires. |
| [chunk](12-memory.md#budgets) | 64 KiB unit of memory owned by one domain. |
| [clause region](02-items.md#clause-region) | Clause lines between head and body. |
| [closed type](04-types.md#closed-and-open-types) | `pub` type whose structure consumers see. |
| [closing label](02-items.md#item-shape) | Name after a closing `}`. |
| [closure](07-ownership.md#closures) | Function expression. |
| [closure guarantee](#guarantees) | Line depends only on its closure set. |
| [closure hash](31-trust.md#seals) | Hash of what a sealed item relies on. |
| [closure set](#guarantees) | Everything that decides a line. |
| [code root](30-artefacts.md#code-roots) | Reproducible part of an artefact. |
| [codec uses](15-untrusted.md#validate-grants) | Type parameters whose decoders and encoders a function uses. |
| [column 0](01-lexical.md#lines) | Physical line's first position. |
| [command](33-toolchain.md#commands) | `nan` subcommand. |
| [compare-and-swap edit](33-toolchain.md#edits) | Item replacement conditional on its hash. |
| [compatibility](29-identity-versioning.md#compatibility) | Whether a release may replace another. |
| [compiler stub](23-low-level.md#machine-seams) | Machine code from a fixed list. |
| [completion](17-suspension-io.md#completion-io) | `task::Completion[B, T, E]` returning a taken buffer. |
| [component](26-components.md#components) | `sys::Component[Env, M]` implementation. |
| [component state](26-components.md#components) | Type implementing a component. |
| [conformance test](#conformance) | Specification example run against an implementation. |
| [const assert](02-items.md#constants) | Condition checked at check time. |
| [const context](21-interpreter.md#const-evaluation) | Code that const evaluation computes at check time. |
| [const evaluation](21-interpreter.md#const-evaluation) | Evaluating consts at check time. |
| [const item](02-items.md#constants) | Named value computed at check time. |
| [constituent type](05-properties.md#structural-properties) | Field or payload type of a struct or enum, with type arguments substituted. |
| [construction](04-types.md#construction) | Expression labelling every field. |
| [construction text](13-text-encode.md#construction-text) | Text form of values that derived `Render` writes and `text::construction` decodes. |
| [consumer](03-packages-modules.md#visibility) | Code outside an item's package using it. |
| [content-blind operation](15-untrusted.md#untrusted-values) | Untrusted operation revealing nothing of the contents. |
| [context pack](33-toolchain.md#context) | Output of `nan context`. |
| [context word](01-lexical.md#keywords) | Identifier special at some positions. |
| [contract](18-contracts.md#clauses) | Specification clauses of a function. |
| [contract term](18-contracts.md#contract-terms) | Total expression inside a contract. |
| [contribution](25-registries.md#contributions) | `contribute` item adding a registry entry. |
| [controlled vocabulary](28-stdlib.md#controlled-vocabulary) | Fixed verbs of std names. |
| [Copy](05-properties.md#copy) | Of a type whose values are copied, not moved: the types the property table makes Copy, and structs and enums deriving `Copy`. |
| [core vocabulary](03-packages-modules.md#core-vocabulary) | Names usable everywhere without a path. |
| [cown](16-concurrency.md#cowns) | `task::Cown[T]` holding shared state. |
| [CPU signal](24-kernel.md#cross-cpu-operations) | Interrupt one CPU sends another on `hw::SIGNAL_VECTOR`. |
| [cursor](07-ownership.md#iteration) | Value of a cursor type, whose `next` yields the elements of an iteration. |
| [cursor type](07-ownership.md#iteration) | Type with a method `next(mut self) -> Option[T]` stating neither `fails` nor `uses wait`; `core::Cursor[T]` is its interface. |
| [declassifying validator](15-untrusted.md#validator-classes) | Validator or decoder that is not parsing. |
| [decoder](13-text-encode.md#encode) | Validator `derives Encode` writes. |
| [deferred call](17-suspension-io.md#defer) | Call `defer` registers. |
| [defining module](04-types.md#predicates) | Module declaring a type. |
| [dependency closure](27-program-model.md#final-builds) | Package and its transitive dependencies. |
| [dependent parameter](05-properties.md#structural-properties) | Type parameter on which a property of a type follows its argument. |
| [derive](08-generics-interfaces.md#derives) | Compiler-written conformance. |
| [descriptor](27-program-model.md#descriptors) | Fixed definition of a target. |
| [device premise](10-effects-digests.md#reversibility) | Class a function's own limits state, which its device accesses take; a trust escape. |
| [diagnostic](34-diagnostics.md#diagnostic-record) | Checker report. |
| [diagnostic class](34-diagnostics.md#classes) | Error, incompleteness or internal error. |
| [diagnostic code](34-diagnostics.md#codes) | Stable `family.condition` name of a diagnostic. |
| [digest](10-effects-digests.md#digests) | Coarse computed facts of an item, with the primitive capabilities and validators it reaches. |
| [digest fact](10-effects-digests.md#digests) | One field of a digest. |
| [directive](03-packages-modules.md#manifest) | One manifest line. |
| [disputed](18-contracts.md#todo) | Of a clause, example or test preceded by `todo`. |
| [DMA buffer](23-low-level.md#dma) | `hw::Dma[T]` or `hw::DmaShared[T]`. |
| [doc comment](32-docs.md#doc-comments) | `///` comment. |
| [domain](11-errors-domains.md#domains) | Unit a trap abandons. |
| [domain run](11-errors-domains.md#domains) | Call of `run` or `run_sized` of `sys::Domain` or `sys::Abandoner`, running a body in a new domain. |
| [droppable](05-properties.md#linear-types) | Of a type that is not linear. |
| [Dyn-usable](08-generics-interfaces.md#dynamic-dispatch) | Usable through `Dyn[I]`. |
| [elaboration](19-nir.md#elaboration) | Translating checked source into NIR. |
| [end of scope](07-ownership.md#ownership) | Where memory returns without user code. |
| [entry context](24-kernel.md#anchor) | Code a machine seam enters. |
| [entry function](27-program-model.md#entry) | Function `entry` names. |
| [entry type](25-registries.md#registries) | Type after `:` of a registry item. |
| [envelope](33-toolchain.md#envelopes) | Origin label around foreign text. |
| [environment](27-program-model.md#descriptors) | Restrictions of a target. |
| [error](34-diagnostics.md#classes) | Class `nan check` rejects. |
| [error enum](11-errors-domains.md#error-enums) | Enum a `fails` clause names. |
| [evaluation order](06-statements-expressions.md#evaluation-order) | Normative order of evaluation. |
| [exact fix](34-diagnostics.md#exact-fixes) | Re-checked replacement edit. |
| [exclusivity](07-ownership.md#exclusivity) | `mut` and `take` accesses exclude overlap. |
| [executor](16-concurrency.md#executors) | Capability `task::Executor`. |
| [failure](11-errors-domains.md#failures) | Error value passed out through `fails`. |
| [false friend](40-false-friends.md) | Familiar spelling with another meaning. |
| [family](34-diagnostics.md#codes) | Diagnostic code's first segment. |
| [file summary](32-docs.md#file-summaries) | The one `//!` line of a file. |
| [final build](27-program-model.md#final-builds) | Build of a final-link root. |
| [final closure](27-program-model.md#final-builds) | Final-link root's dependency closure. |
| [final-link root](27-program-model.md#final-builds) | Program built into an image. |
| [first dead token](41-grammar.md#prefix-viability) | First token of a file or prefix that the grammar, outside rejected alternatives, does not accept at its position, or the token after a line holding a lexical diagnostic when that is earlier. |
| [flexible struct](13-text-encode.md#flexible-structs) | Struct keeping the fields its `mem::wire` decoding skips. |
| [foreign anchor](34-diagnostics.md#idiom-table) | First token of a foreign form that the grammar does not accept. |
| [foreign fn](23-low-level.md#foreign-functions) | Function declaring an external symbol. |
| [frame rule](20-prover.md#frame-rule) | Which places a call leaves unchanged. |
| [from variant](11-errors-domains.md#error-enums) | Variant `try` converts failures into. |
| [fuel](20-prover.md#fuel) | Step count bounding a proof. |
| [function type](10-effects-digests.md#function-types) | Function value's type, with a full head. |
| [generational handle](12-memory.md#generational-handles) | Copy index detecting slot reuse. |
| [generator](22-testing.md#generation) | Code `derives Generate` writes. |
| [generic parameter](08-generics-interfaces.md#generic-parameters) | Bracketed type or const parameter. |
| [growable container](12-memory.md#growth) | Container that stores its allocator. |
| [hard keyword](01-lexical.md#keywords) | Word that is never an identifier. |
| [head](02-items.md#heads) | Part of an item before its body. |
| [helper](04-types.md#module-boundary) | Function of a defining module that is not a boundary function. |
| [history head](29-identity-versioning.md#publishing) | Code root hash of the release last published from a package. |
| [hold](07-ownership.md#holds) | Live view's restriction on its source. |
| [hole](18-contracts.md#todo) | Typed missing expression. |
| [idiom table](34-diagnostics.md#idiom-table) | Foreign spellings and their rewrites. |
| [image](27-program-model.md#images) | Final build's output. |
| [impl line](08-generics-interfaces.md#impl-lines) | Item `impl Type: Interface`. |
| [impl target](08-generics-interfaces.md#impl-lines) | Path after `impl` in an impl line. |
| [implementation](#conformance) | Toolchain claiming conformance. |
| [implementing function](08-generics-interfaces.md#conformance) | Function `T.m`, or else default body, providing interface method `m` for `T`. |
| [incompleteness](31-trust.md#incompleteness) | Class `nan check` and `nan test` accept and the release gate rejects. |
| [indent anchor](35-format.md#depth) | Depth of the line starting the unit that holds an opening delimiter. |
| [indirect call](10-effects-digests.md#indirect-calls) | Call through a function value, registry field or `Dyn`. |
| [Inert](05-properties.md#inert) | Of a type without interior mutability or capabilities other than stored allocators. |
| [install token](23-low-level.md#privileged-intrinsics) | Linear proof of installed hardware state. |
| [instantiation](08-generics-interfaces.md#instantiation) | Generic item with parameters fixed. |
| [interface](08-generics-interfaces.md#interfaces) | Nominal set of method heads. |
| [interface facts](05-properties.md#interface-facts) | Linearity, view-ness, state-token content, a `Copy` derive and promises of a type, entering its interface hash. |
| [interface hash](29-identity-versioning.md#hashes) | Hash of what consumers see. |
| [interior mutability](12-memory.md#interior-mutability) | Mutation through a read access. |
| [interpreter](21-interpreter.md#interpreter) | Reference interpreter over NIR. |
| [interpreter run](21-interpreter.md#interpreter) | Execution of one NIR function by the interpreter at check time. |
| [interrupt fn](24-kernel.md#interrupt-functions) | Function run for an interrupt. |
| [IRQ-safe lock](24-kernel.md#locks) | Spin lock taken with interrupts disabled. |
| [item](02-items.md#item-shape) | Column-0 construct closed by its name. |
| [item granularity](#guarantees) | Item is the unit of identity, checking, editing, merging, sealing and versioning. |
| [item hash](29-identity-versioning.md#item-hash-and-interface-hash) | Hash of an item's whole normalised form. |
| [item kind](02-items.md#item-kinds) | One of fourteen item forms. |
| [join handle](16-concurrency.md#joins) | `task::Join[T, E]` a spawn returns. |
| [judging root](25-registries.md#contributors) | Package against which registry admission and tables are judged. |
| [kept fields](13-text-encode.md#flexible-structs) | Skipped fields a flexible struct holds as bytes. |
| [key](13-text-encode.md#encode) | Encoded name of a field or variant. |
| [key field](25-registries.md#registries) | Field of the entry type a registry is keyed by. |
| [keyed task](26-components.md#keyed-tasks) | Task that `cx.spawn(key: k, ...)` starts, identified by its key and cancelling an earlier task with that key. |
| [landing field](25-registries.md#platform-registries) | Registry entry field holding a landing fn: `hw::Vector.user`, `hw::SyscallEntry.handler`, `hw::PreemptEntry.handler`. |
| [landing fn](23-low-level.md#landing-functions) | Function a machine seam enters with raw values. |
| [language version](37-evolution.md#language-versions) | Integer of the `language` directive. |
| [last-green record](33-toolchain.md#last-green) | Interfaces at an agent's last clean state. |
| [layout](23-low-level.md#layouts) | `layout` item fixing representation. |
| [leading argument](06-statements-expressions.md#arguments-and-labels) | Argument for the first parameter other than `self`, which may omit its label. |
| [ledger](31-trust.md#ledger) | Per-item list of trust escapes, waivers, todos and `dbg` uses. |
| [lending](16-concurrency.md#lending) | Task closures capturing by read or `mut`. |
| [library](27-program-model.md#programs-and-libraries) | Package without `entry`. |
| [limit](10-effects-digests.md#limits) | Demanded digest fact. |
| [linear](05-properties.md#linear-types) | Of a type whose values must be consumed. |
| [literal suffix](01-lexical.md#number-literals) | Type name ending a number literal. |
| [local store](29-identity-versioning.md#local-store) | Append-only store of releases. |
| [lock](29-identity-versioning.md#lock) | File `.nan/lock.nan`. |
| [logical line](01-lexical.md#lines) | Physical line joined with the following ones while the last joined line ends in a continuing token or continues a text run. |
| [lowering](30-artefacts.md#lowering) | Translating NIR to machine code. |
| [machine file](35-format.md#machine-files) | Lock, history or API snapshot under a package's `.nan/`. |
| [machine seam](23-low-level.md#machine-seams) | Stub at a fixed hardware entry. |
| [manifest](03-packages-modules.md#manifest) | `package` item of `package.nan`. |
| [manifest grant](14-authority.md#manifest-grants) | `allow` directive. |
| [marker](06-statements-expressions.md#markers) | Call-site word such as `try` or `mut`. |
| [member](03-packages-modules.md#paths) | Field, method, associated function or variant. |
| [memory model](23-low-level.md#memory-model) | Rules for shared memory. |
| [merge driver](33-toolchain.md#merge-driver) | `nan merge`, merging per item. |
| [message](26-components.md#components) | Value delivered to a component. |
| [method](02-items.md#methods) | `Type.name` function taking `self`. |
| [method resolution](08-generics-interfaces.md#method-resolution) | Finding the function `x.f(...)` names. |
| [migration](13-text-encode.md#migrations) | Conversion from or to an earlier shape of a type. |
| [milestone](38-self-hosting.md#milestones) | One of M1 to M6, checks the bootstrap passes before language 1 freezes. |
| [mode](07-ownership.md#modes) | Read, `mut` or `take`. |
| [modifier](02-items.md#modifiers) | Word before the item kind. |
| [module](03-packages-modules.md#modules) | Directory of sources sharing a namespace. |
| [move](07-ownership.md#ownership) | Transfer of ownership. |
| [named conversion](09-numbers.md#conversions) | Built-in function changing a value's type, as `T.fit(x)`. |
| [NaN toolchain](38-self-hosting.md#nan-toolchain) | Toolchain written in NaN, checked against the bootstrap once it builds itself. |
| [nanbench](37-evolution.md#nanbench) | Benchmark gating language versions. |
| [nested rendering](13-text-encode.md#render) | Rendering of a value inside another's, text and chars as literals. |
| [niche](04-types.md#niches) | Bit pattern a type never holds. |
| [NIR](19-nir.md#nir) | Specified checked intermediate representation. |
| [obligation](18-contracts.md#obligations) | Condition required at a program point. |
| [open type](04-types.md#closed-and-open-types) | `pub` type that may grow compatibly. |
| [operation](26-components.md#protocols) | Typed message or request a protocol declares. |
| [order-free member](33-toolchain.md#merge-driver) | Member whose order means nothing. |
| [outbox](26-components.md#keyed-tasks) | `sys::Outbox[M]` posting to a component. |
| [outcome](20-prover.md#outcomes) | Proved, refuted or unknown. |
| [output budget](33-toolchain.md#output-contract) | Size limit of a command's output. |
| [output contract](33-toolchain.md#output-contract) | Rules of command output. |
| [owner](07-ownership.md#ownership) | What holds a value. |
| [package](03-packages-modules.md#packages) | Directory with a `package.nan` manifest. |
| [parameter](02-items.md#heads) | Named, typed, moded input. |
| [parsing validator](15-untrusted.md#validator-classes) | Validator or decoder whose export types are all parsing types. |
| [path](03-packages-modules.md#paths) | `::`-separated name. |
| [pattern](04-types.md#patterns) | Form a value is matched against. |
| [pending clause](41-grammar.md#prefix-viability) | `fails` clause, `uses wait` clause or from variant that an exact fix adds to a prefix's head or enum, kept as a lower bound. |
| [pending weakening](31-trust.md#weakening) | Weakening awaiting a seal. |
| [physical frame](24-kernel.md#physical-memory) | `hw::PhysFrame[S]` owning physical memory. |
| [physical line](01-lexical.md#lines) | One LF-terminated line. |
| [place](07-ownership.md#places) | Local and a field path. |
| [platform package](28-stdlib.md#platform-packages) | Std package minting a target's root grant and implementing its target-dependent operations. |
| [platform registry](25-registries.md#platform-registries) | Registry binding stubs to handlers. |
| [poisoning](11-errors-domains.md#poisoning) | State of a cown held by a domain that trapped. |
| [predicate](04-types.md#predicates) | `where` or `invariant` condition. |
| [prefix viability](#guarantees) | Written text fixes all meaning: the first dead token is defined, and every prefix before it closes with typed holes and closers into a file the grammar derives. |
| [premise](23-low-level.md#trusted-code) | What trusted code relies on. |
| [primer](39-primer.md#primer-tiers) | Priced text teaching NaN. |
| [primitive capability](05-properties.md#capability-types) | Base type that its owning chapter declares a capability. |
| [principal](31-trust.md#seals) | Identity a seal names. |
| [principals file](31-trust.md#principals) | Machine file giving principals their public keys. |
| [privileged intrinsic](23-low-level.md#privileged-intrinsics) | Privileged `hw::Ring0` operation. |
| [program](27-program-model.md#programs-and-libraries) | Package with `entry`. |
| [projection](07-ownership.md#projections) | `proj` function, deriving its result from its receiver. |
| [promise](05-properties.md#promises) | Fact consumers may rely on. |
| [property test](22-testing.md#property-tests) | Test with generated inputs. |
| [protocol](26-components.md#protocols) | Item declaring the operations of the components serving it. |
| [prover](20-prover.md#procedure) | Specified procedure discharging obligations. |
| [punning](06-statements-expressions.md#calls) | Omitting a label matching the binding. |
| [pure](10-effects-digests.md#purity) | Of a function without effects or authority. |
| [query engine](33-toolchain.md#query-engine) | Incremental checking engine. |
| [quiescent](26-components.md#components) | Of a component between messages. |
| [read cursor](07-ownership.md#iteration) | Cursor whose `next` yields read-only parts. |
| [receiver](06-statements-expressions.md#calls) | First argument of a call. |
| [refutation](20-prover.md#outcomes) | Confirmed counterexample. |
| [registers block](23-low-level.md#registers) | `registers` item describing device registers. |
| [registry](25-registries.md#registries) | Table filled by contributions. |
| [release](29-identity-versioning.md#releases) | Package state recorded in the local store. |
| [release class](29-identity-versioning.md#releases) | Identical, additive or breaking. |
| [release check](30-artefacts.md#releases) | Checking a release whole from its source tree. |
| [release gate](31-trust.md#release-gate) | `nan verify`. |
| [reliance test](22-testing.md#reliance-tests) | Test of a dependency's item. |
| [render budget](13-text-encode.md#render) | `Render` call's output limit. |
| [replay token](22-testing.md#replay) | Value reproducing a failure. |
| [request](26-components.md#protocols) | Operation whose caller waits for its handler's result or failure. |
| [requires-set](29-identity-versioning.md#requires-sets) | What a consumer relied on. |
| [reservation](37-evolution.md#reservations) | Space reserved without meaning. |
| [reserved word](01-lexical.md#keywords) | Word rejected where reserved. |
| [resource](14-authority.md#resources) | Value opened through a capability. |
| [result location](07-ownership.md#result-location) | Where a value is built. |
| [retired form](37-evolution.md#successive-versions) | Form a later language version rejects with an exact rewrite. |
| [reversibility class](10-effects-digests.md#reversibility) | Digest fact bounding what code does to state outside the program. |
| [revocation](14-authority.md#revocation) | Ending every copy of a capability. |
| [root grant](14-authority.md#root-grants) | Capability minted at entry. |
| [root module](03-packages-modules.md#modules) | Package root's module. |
| [run report](21-interpreter.md#run-reports) | Description of how an interpreter run ended. |
| [run-time check](18-contracts.md#run-time-checks) | Trapping test of an obligation. |
| [runner](26-components.md#runner) | `host::Runner[S]`, running a component. |
| [safe code](23-low-level.md#safe-code) | Code outside trusted code. |
| [scalar](04-types.md#scalars) | One of 14 built-in types. |
| [schema hash](13-text-encode.md#schema-hashes) | Hash of an encoded structure. |
| [seal](31-trust.md#seals) | Binding of a principal to item and closure hashes. |
| [Secret](15-untrusted.md#secret-values) | Wrapper `core::Secret[T]` whose operations run in constant time. |
| [secret key](31-trust.md#principals) | Seed a principal signs with, in the user directory. |
| [Send](05-properties.md#send) | Of a type whose values may pass to another task. |
| [settlement](11-errors-domains.md#domains) | Settling a trapped domain's I/O before freeing. |
| [shootdown](24-kernel.md#cross-cpu-operations) | Invalidation one x86_64 CPU posts to the others with the CPU signal. |
| [shrinking](22-testing.md#property-tests) | Minimising a failing input. |
| [signature](31-trust.md#signatures) | Ed25519 signature of a seal or an artefact. |
| [signal](24-kernel.md#signals) | `hw::Signal[T]`, through which interrupt fns wake tasks. |
| [sim](22-testing.md#simulated-capabilities) | Simulated capability. |
| [single-line item](02-items.md#item-shape) | Item on one logical line: impl lines, const items, const asserts, registries, contributions, tombstones, transparent functions and foreign fns. |
| [source file](01-lexical.md#source-files) | `.nan` file. |
| [stage comparison](36-builds.md#stage-comparison) | Comparison of two toolchains' code roots for one final closure. |
| [stale doc](32-docs.md#doc-binding) | Doc older than its interface. |
| [state token](05-properties.md#state-tokens) | Zero-size view of machine state. |
| [statement](06-statements-expressions.md#statements) | One logical line of a body. |
| [status](18-contracts.md#statuses) | Proven, checked, assumed or open. |
| [std](28-stdlib.md#std-layers) | Nine packages the reserved std aliases name, and the three platform packages. |
| [store](12-memory.md#stores) | Manager of values in memory. |
| [structural property](05-properties.md#structural-properties) | Property computed from structure. |
| [summary](26-components.md#servers) | Value a component serving a protocol publishes at quiescence. |
| [support routine](27-program-model.md#runtime-support) | Fixed runtime routine. |
| [suspension point](17-suspension-io.md#stackful-tasks) | An `await`, where a task may suspend. |
| [table](25-registries.md#tables) | Sequence of a registry's admitted contributions under a target. |
| [target](27-program-model.md#targets) | Platform for checking and building. |
| [target variant](03-packages-modules.md#target-variants) | `name@target` module directory. |
| [task](16-concurrency.md#task-scopes) | Stackful unit of concurrency. |
| [task closure](16-concurrency.md#task-closures) | `task fn` closure. |
| [task scope](16-concurrency.md#task-scopes) | Region `task::scope` opens. |
| [term order](20-prover.md#canonicalisation) | Prover's order on terms, atoms and clauses, by canonical encoding. |
| [test](22-testing.md#test-items) | `test name for target` item. |
| [test target](22-testing.md#test-items) | Item a test names. |
| [text](13-text-encode.md#text-types) | UTF-8 character data. |
| [text run](01-lexical.md#text-literals) | Multi-line text literal. |
| [thread context](24-kernel.md#scheduling) | Schedulable kernel execution. |
| [threshold](06-statements-expressions.md#thresholds) | Nesting depth 4 or body length 80 lines. |
| [token](01-lexical.md#tokens) | Smallest lexical unit. |
| [token count](37-evolution.md#nanbench) | Largest count the three nanbench tokenizers give. |
| [tombstone](29-identity-versioning.md#tombstones) | `retired` item recording an old form. |
| [tool card](39-primer.md#primer-tiers) | Primer tier of commands. |
| [toolchain](#terms) | `nan` command and everything it runs. |
| [toolchain hash](36-builds.md#toolchain-hash) | Hash keying every cache entry. |
| [trade-in](37-evolution.md#trade-in) | Entering the primer by removing a concept. |
| [transparent function](02-items.md#transparent-functions) | Function whose `= expr` body is its contract. |
| [trap](11-errors-domains.md#traps) | Abandoning the innermost domain on a bug. |
| [trap kind](11-errors-domains.md#traps) | Trap cause `trap.kind`. |
| [trap report](11-errors-domains.md#trap-reports) | At most 15 lines describing a trap. |
| [trust escape](31-trust.md#trust-escapes) | Ledger entry whose item `nan verify` requires sealed. |
| [trusted code](23-low-level.md#trusted-code) | `trusted` block or expression. |
| [undefined behaviour](23-low-level.md#safe-code) | Behaviour the specification does not define. |
| [undo clause](10-effects-digests.md#compensation) | Claim, sealed as a trust escape, that another function puts back what a function changes. |
| [Untrusted](15-untrusted.md#untrusted-values) | Wrapper `Untrusted[T]` for outside data. |
| [user directory](36-builds.md#cache) | Per-user `.nan` directory holding the build cache and local store. |
| [validator](15-untrusted.md#validators) | `validator fn`, reading Untrusted contents. |
| [variant](04-types.md#enums) | Enum alternative. |
| [verdict line](33-toolchain.md#output-contract) | First or last line of a command's output. |
| [view](05-properties.md#views) | Value into memory it does not own. |
| [visibility](03-packages-modules.md#visibility) | Private, `pkg` or `pub`. |
| [volatile access](23-low-level.md#registers) | Register access never merged or reordered. |
| [waiting](10-effects-digests.md#waiting) | Suspension at `await`. |
| [waiver](06-statements-expressions.md#thresholds) | `waive` clause accepting a threshold excess. |
| [weakening](31-trust.md#weakening) | Change weakening an accepted spec. |
| [widening](14-authority.md#authority-summaries) | Gain in an authority summary. |

## Chapter map

The chapters in reading order are [Charter and guarantees](00-charter.md), [Lexical structure](01-lexical.md), [Items](02-items.md), [Packages, modules and names](03-packages-modules.md), [Data types](04-types.md), [Structural properties](05-properties.md), [Statements and expressions](06-statements-expressions.md), [Ownership and access](07-ownership.md), [Generics and interfaces](08-generics-interfaces.md), [Numbers](09-numbers.md), [Effects, digests and limits](10-effects-digests.md), [Errors, traps and domains](11-errors-domains.md), [Memory and allocation](12-memory.md), [Text, rendering and Encode](13-text-encode.md), [Capabilities and authority](14-authority.md), [Untrusted and secret data](15-untrusted.md), [Structured concurrency](16-concurrency.md), [Suspension, I/O and cleanup](17-suspension-io.md), [Contracts](18-contracts.md), [NIR](19-nir.md), [The normative prover](20-prover.md), [The reference interpreter](21-interpreter.md), [Testing and evidence](22-testing.md), [Low-level programming](23-low-level.md), [Kernel execution model](24-kernel.md), [Registries](25-registries.md), [Components](26-components.md), [Targets and program model](27-program-model.md), [Standard library 1.0](28-stdlib.md), [Identity and versioning](29-identity-versioning.md), [Artefacts](30-artefacts.md), [Trust and the release gate](31-trust.md), [Documentation](32-docs.md), [Toolchain](33-toolchain.md), [Diagnostics](34-diagnostics.md), [Canonical format and machine files](35-format.md), [Builds](36-builds.md), [Evolution and governance](37-evolution.md), [Self-hosting and milestones](38-self-hosting.md), [Primer and tool card](39-primer.md), [False friends](40-false-friends.md) and [Grammar](41-grammar.md).
