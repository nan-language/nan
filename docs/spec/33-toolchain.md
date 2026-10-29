# Toolchain

This chapter defines the commands, the output contract, envelopes, exit classes and internal errors, check time, the query engine, prefix checking, context packs, edits, the merge driver and the last-green record.

## Commands

A **command** is a `nan` subcommand. Every command runs in a **workspace**, the directory tree under its root, whose packages are the directories outside every `.nan` directory that hold a `package.nan`. The **workspace root** is the parent of the outermost directory at or above the current directory that holds a `package.nan`, or else the current directory. A workspace without a [package](03-packages-modules.md#packages) is an error (`tool.no-workspace`).

- A path a command takes or prints is relative to the workspace root and separated by `/`, except the versions `nan diff` and `nan merge` compare and the files of `--from` and `--compare-with`, which are relative to the current directory. A file outside the workspace is printed as its package's name, `/` and its path in its package. A **location** is `<path>:<line>`, lines counting from 1. A path in the [user directory](36-builds.md#cache) prints as `~/.nan/` followed by its path in it, and a `--compare-with` image as given. A [trap report](11-errors-domains.md#trap-reports) quoted in a record keeps the path form trap reports give.
- The files `.nan/write`, `.nan/journal`, `.nan/repro/<n>`, `.nan/green/<agent>` and `.nan/images/<target>/` lie in the `.nan` directory of the workspace root; they are neither source files nor [machine files](35-format.md#machine-files).
- In records, `<item>` is an item's or member's path as tool output writes [canonical paths](29-identity-versioning.md#canonical-paths) for the scope's package, or its canonical path when the scope holds several packages. A path naming a module, item or member, in a scope or an `<item>` argument, is its canonical path or, when the current directory lies inside a package, the path that package's root module would write for it; a text that is both is read as a canonical path. A package, and its `package` item, are named by the package name.
- A command's **scope** is the argument naming what it acts on: `.` for the workspace; the path of a package, module, item or member; a file path ending in `.nan`, for that file's items; or a location, for the item holding that line. An omitted scope is the package holding the current directory, or `.` at the workspace root. A scope naming nothing is an error (`tool.unknown-scope`).
- Every command accepts `--json` ([JSON Lines](#json-lines)), `--budget N` and `--skip N` ([output budgets](#output-budgets)). Any other option, argument or value outside its synopsis and the forms this chapter allows is an error (`tool.usage`).
- No configuration file or environment variable affects a command, except `PATH`, through which a final build finds `cc`, and, in a [VCS workspace](31-trust.md#author-rule), the git configuration the author rule reads.

The commands are:

- `nan check [<scope>] [--radius] [--agent <name>]`, `nan check --prefix <path>[:<line>]`
- `nan verify [<scope>]`
- `nan test [<scope>] [--affected]`, `nan test --replay <token>`
- `nan fix [<scope>] [--propagate]`
- `nan fmt [<scope>]`
- `nan holes [<scope>]`
- `nan show <scope> [--api]`
- `nan read <location>`
- `nan find <name>`, `nan find --contributors <registry> [<package>]`
- `nan context <item or location>`
- `nan explain <item or package>`
- `nan expand <type>`
- `nan edit replace <item> --expect <hash> --from <file>`, `nan edit rename <item> <name>`, `nan edit move <item> <module>`
- `nan merge <base> <ours> <theirs> [--path <path>]`
- `nan diff <old> <new> [--path <path>]`, `nan diff --green [<scope>] [--agent <name>]`
- `nan seal <item> --as <principal>`
- `nan api update [<scope>] [--confirm-docs <item>...]`
- `nan dep add <alias> <source>`, `nan dep sync [<package>]`, `nan dep approve [<package>]`
- `nan publish --local <dir> [--name <store name>] [--breaking <item>... <reason>]`
- `nan upgrade [<package>]`, `nan upgrade --language <N> [<scope>]`
- `nan build [<scope>] [--compare-with <image>]`
- `nan run [<package>] [-- <argument>...]`
- `nan primer [<topic> or tools]`

### Checking commands

- `nan check` runs [check time](#check-time), writes no source or machine file and prints the [diagnostics](34-diagnostics.md#diagnostic-record) of its scope, then the `changed` records of the [last-green record](#last-green), then, with `--radius`, the [closure radius](#closure-radius) records. With `--prefix` it performs [prefix checking](#prefix-checking) instead.
- `nan test` checks its scope as `nan check` does, printing the diagnostics, and runs no test when one is an error. Otherwise it runs the [tests](22-testing.md#test-items) of its scope: of a package or module, every test in it; of a test, that test; of another item, the tests targeting it. `--affected` keeps the [affected tests](22-testing.md#affected-tests); `--replay` performs one [replayed run](22-testing.md#replay-tokens). Each failing test prints a record `fail <item> <location> <run target> <cause>` at its head line, the cause being its [test run](22-testing.md#test-runs) code or trap kind, holding the trap report or the failure's variant path and rendering, each shrunk parameter's setup form, the regression-test edit as a diagnostic's `fix` lines, `trace` with the test trace, and `replay` with the replay token.
- `nan verify` performs `nan check` and `nan test` on each package holding part of its scope and on each package those reach through a chain of `dep` directives with relative-path sources, then applies the [release gate](31-trust.md#release-gate) to all of them. It fails when it reports any diagnostic or failing test.
- `nan fix` applies, in one pass, the exact fix of each diagnostic of its scope offering exactly one, skipping a fix that shares a line with an earlier one in path and line order, then checks again and prints the remaining diagnostics. With `--propagate` it then repeats, while a pass applies a fix, for the fixes of `item.wait-missing`, `item.fails-missing`, `error.cancel-unmapped`, `error.unconvertible`, `effect.await-missing` and `error.unhandled`, except fixes editing a `pkg` or `pub` function's head or a `pkg` or `pub` enum.
- A command writing source, machine or image files prints `wrote <path>` per file it writes and `deleted <path>` per file it deletes, in path order, after its other records.

### Reading commands

- `nan show <scope>` prints per item a record `item <item> <location> <item hash> <interface hash>`, hashes in [short form](29-identity-versioning.md#hash-values), holding its text. A module or package first prints its [module summary](32-docs.md#module-summaries) as a `module <path>` record, then its items in path and line order. `nan read <location>` prints the record of the item holding that line, and a line outside every item is `tool.unknown-scope`. In `nan show`, a canonical path that names nothing in the workspace names the std package, module, item or member it names among the std packages.
- `nan show <scope> --api` prints an `api <item> <location>` record per item of the scope that consumers see, and for a type per such function named after it, holding: its doc comments; its [interface text](35-format.md#api-snapshots), each ensures line followed by ` [<status>]` ([statuses](18-contracts.md#statuses)); the impl lines naming it that consumers see; for a struct or enum, `properties: ` with the [structural properties](05-properties.md#structural-properties) consumers see in the order linear, view, Copy, Send, Inert, AnyBits, separated by `, `, or `none`; for a function, its [digest line](10-effects-digests.md#digests) per declared target in byte order, followed by ` (<target>)` when there are several.
- The [named conversions](09-numbers.md#conversions) and the bit conversions of [bit patterns](09-numbers.md#bit-patterns) are built-in members, not items: no command prints an `item` or `api` record for one, and a context pack shows none.
- `nan find <name>` prints a record `found <item> <location>`, holding the declaration's first line, per item or member of the workspace and of its packages' dependency closures whose [folded](03-packages-modules.md#naming-conventions) name equals the folded argument, workspace items first, then by path. `nan find --contributors <registry>` prints a record `contribution <registry> <entry> <location>`, followed by `admitted` or `not-admitted`, per contribution in the dependency closure of the given package, by default the current directory's, judged with it as [judging root](25-registries.md#contributors).
- `nan holes` prints a record `hole <location> <item> <expected type>` per [hole](18-contracts.md#holes), holding its source line and `producers: ` with its producers, separated by `, `.
- `nan explain <item>` prints the item's digest lines as `digest:` records; `fact <fact> <location>` per line of the item raising a digest fact directly or through a call; `method <path>` per capability method reached; `obligation <location> <kind> <status>` per [obligation](18-contracts.md#obligations), the kind being its row name with `-` for spaces, or for the arithmetic row the operation's word, an obligation without a trap kind showing its [outcome](20-prover.md#outcomes), a refuted one holding its counterexample; its `ledger` records; and `radius <N>`. For a package it prints its items' `ledger` records by path.
- A record `ledger <kind> <location>` holds a [ledger](31-trust.md#ledger) entry's premise, condition, reason or text; the kind is an escape kind of the [lock](35-format.md#lock) or `waiver`, `todo` or `dbg`, followed by ` escape` for a [trust escape](31-trust.md#trust-escapes).
- `nan expand <type>` prints the items [derives](08-generics-interfaces.md#derives) write for the type as `item` records located `derived`. `nan primer` prints one record `primer <tier>` holding the [tier](39-primer.md#primer-tiers)'s text.

```nan
pub fn clamp_level(level: u32) -> u32 limits notrap
    ensures result <= 100
{
    if level > 100 {
        return 100
    }
    return level
} clamp_level
```

```text
$ nan show example::clamp_level --api
ok show example::clamp_level --api: records 1
api example::clamp_level example/example.nan:2
    pub fn clamp_level(level: u32) -> u32 limits notrap
        ensures result <= 100 [proven]
    digest: effects none | authority none | may-trap no | terminates
ok show example::clamp_level --api: records 1
$ nan show core::Option.is_some --api
ok show core::Option.is_some --api: records 1
api core::Option.is_some core/core.nan:8
    pub fn Option.is_some(self) -> bool limits pure, notrap, terminates
        example Some(3u8).is_some()
    digest: effects none | authority none | may-trap no | terminates
ok show core::Option.is_some --api: records 1
```

### Writing commands

- `nan seal`, `nan api update`, `nan dep`, `nan publish` and `nan upgrade` act as [Trust and the release gate](31-trust.md), [Identity and versioning](29-identity-versioning.md) and [language upgrades](37-evolution.md#language-upgrades) define. In `nan publish`, the last argument after `--breaking` is the reason and the others are items. `nan dep add` writes the `dep` directive into the manifest of the current directory's package, `tool.usage` outside every package, then performs `nan dep sync`.
- `nan dep sync` and `nan upgrade` without `--language` print a record `release <package> <old code root> <new code root> <class>` per package whose locked release changes, `-` for an absent release, holding `epoch <item> <epoch>` per epoch bump and `widening <entry>` per [widening](14-authority.md#authority-summaries) as a lock's `approved` record writes it. `nan publish` prints `published <store name> <code root> <release class> <encode class>`.
- `nan diff <old> <new>` compares two versions of the workspace, or with `--path` two versions of one file. It prints a record `changed <item> <location>` per changed item, located in the new version or, when removed, the old, followed by its [change classes](31-trust.md#change-classes) in order and by `weakening` or `widening` when the change is one. It holds the old and new texts merged along their longest common subsequence of lines with the least old, then new, line numbers in lexicographic order: kept lines prefixed with 2 spaces, removed ones `- `, added ones `+ `, removals first. For the `package` item of a final-link root, the record also holds the `approved` and `epoch` records its lock gains or loses, merged as texts are. `nan diff --green` prints such a record, merging `api` lines, per item of its scope whose interface hash differs from its [last-green](#last-green) entry.
- `nan build` performs the [final build](27-program-model.md#final-builds) of each program in its scope, writing its [image](27-program-model.md#images) into `.nan/images/<target>/`; a scope without a program is `tool.usage`. With `--compare-with`, the final build also performs [stage comparison](36-builds.md#stage-comparison) once its checking step reports no error, and writes no image when the comparison reports an error.
- `nan run` performs the final build of one program, any other scope being `tool.usage`. When the build fails, it prints as `nan build` does; otherwise it prints nothing, the output contract not applying, and starts the image: on a host target as a process with the arguments after `--` and the command's environment and standard streams, unless the machine running the toolchain has another architecture than the target's, when it starts nothing and ends with status 70; on a machine target under the [descriptor](27-program-model.md#descriptors)'s emulator, with the boot volume the descriptor gives `nan run`, its serial port on standard output. On a host target it passes a termination request on and ends with the process's exit status. Under the emulator, a write of byte v to the [exit port](27-program-model.md#descriptors) ends the run with status v mod 128; a termination request with signal number n stops the emulator and ends the run with status 128 + n; any other end of the emulator ends it with status 70.

## Output contract

The **output contract** is the rules every command's output follows.

- A command never prompts, opens a pager or editor, reads standard input other than for `--from -`, or writes to standard error. Its output is the same whether or not standard output is a terminal.
- Output is UTF-8 lines ending with LF, holding no other control character, no column number, no space aligning text into columns and no absolute path.
- The first and last lines are the same **verdict line**, `<verdict> <command>: <summary>`, the verdict being the [exit class](#exit-classes) word. The command is the command line without `nan`, arguments separated by single spaces, one that is empty or holds a space, `"` or `\` written as a text literal, an omitted scope written after the command's name words.
- The summary, `<name> <N>` counts separated by `, `, is `errors N, incomplete N` for `nan check` and `nan fix`, followed by `tests N, failed N` for `nan test` and `nan verify`; `dead N, pending N, holes N` for prefix checking; `entries N, radius N` for `nan context`; `files N, conflicts N, errors N` for `nan merge`; `files N, errors N, incomplete N` for every other command that can write files, files counting `wrote` and `deleted` records; and `records N` otherwise.
- Output stays within the command's **output budget** ([output budgets](#output-budgets)).

### Records

Between the verdict lines, output is a sequence of **records**: each a header line at column 0, which starts with its record word, followed by continuation lines indented 4 spaces, which the record holds. Source text keeps its own indentation after those 4 spaces. Diagnostics are records laid out as [Diagnostics](34-diagnostics.md#record-layout) defines. Records appear in the order each command defines, diagnostics by path, line and code, paths and codes in byte order.

### Output budgets

An output budget is counted in units: the byte length of the text output divided by 4, rounded up. It is N with `--budget N`, an N below 100 being `tool.usage`, and otherwise 4,000 units, or unlimited for `nan primer`. `--skip N` leaves out the first N records.

- The command prints the most records after the skipped ones, at least 1 when any remains, such that the verdict lines, those records and, when records remain, the elision line fit the budget.
- The **elision** line is `elided <N> records: nan <command> --skip <K>`, N being the number of records not printed and K the number skipped or printed, the command keeping its other arguments. Running it prints the elided records.
- The summary counts every record, printed or not.

### JSON Lines

With `--json`, a command prints the lines of its text output as JSON Lines, one object per line, members in byte order of their names, no whitespace outside strings, strings escaping only `"` and `\`:

- a verdict line is `{"command":C,"record":"verdict","summary":S,"verdict":V}`, S an object from each count name to its number;
- a record is `{"fields":F,"lines":L,"record":W}`: W its record word, F the rest of its header line split at single spaces, L its continuation lines without the 4-space indentation, each envelope among them an object `{"envelope":P,"lines":T}`;
- an elision is `{"command":C,"count":N,"record":"elided"}`, C being the command line it names.

Joining the fields and lines back reproduces the text form exactly.

## Envelopes

An **envelope** marks text from outside the scope's packages as data. Within a record, each maximal run of consecutive lines copied from another package, std included, that hold a comment or a text, byte or char literal, and each run of output lines of an external program, prints as the line `envelope <origin> <N>`, the origin being the package's or program's name, followed by those N lines each prefixed `| `, at the record's continuation indentation.

## Exit classes

A command's **exit class** is its verdict word and exit status.

| Status | Verdict | Meaning |
| --- | --- | --- |
| 0 | `ok` | It reported no error. |
| 1 | `failed` | It reported an error or a failing test, or, applying the release gate, incompleteness. |
| 2 | `invalid` | It reported a `tool` error and did nothing else. |
| 3 | `internal-error` | The implementation failed. |

Incompleteness makes only the commands applying the [release gate](31-trust.md#release-gate) fail: `nan verify` and `nan publish`.

### Internal errors

On an [internal error](34-diagnostics.md#internal-errors) the command:

1. stops;
2. writes a **reproducer**: the directory `.nan/repro/<n>`, n being the least positive integer naming no existing directory, holding `command`, the command line one argument per line; `toolchain`, the [toolchain hash](36-builds.md#toolchain-hash) of each language version the toolchain implements, one per line in version order; every file the command read in the workspace, at its path; and each version or `--from` file its command line names outside the workspace, under that argument's name (`from`, `old`, `new`, `base`, `ours`, `theirs`) as `command` names it;
3. prints, between verdict lines whose summary is `records 1`, the internal error's diagnostic naming the reproducer, and exits with status 3.

Run in the reproducer directory with the same toolchain hash and [local store](29-identity-versioning.md#local-store), the recorded command line reproduces the internal error.

## Check time

**Check time** is the run of the checker over a scope, which discharges obligations with the [prover](20-prover.md#procedure), runs [const evaluation](21-interpreter.md#const-evaluation) and [examples](18-contracts.md#examples) in the interpreter, and applies every rule naming an error or [incompleteness](31-trust.md#incompleteness) code, except:

- tests, run only by `nan test`, `nan verify` and the commands that [resolve](29-identity-versioning.md#resolution) or [publish](29-identity-versioning.md#publishing);
- the rules of a final build, applied only by `nan build` and `nan run`;
- the [gate codes](31-trust.md#incompleteness), applied only by the release gate and the commands [Trust and the release gate](31-trust.md) names.

Every command checks the items it reads. `nan show`, `nan read`, `nan find`, `nan holes`, `nan context`, `nan explain`, `nan expand`, `nan diff` and `nan primer` print no diagnostic; every other command prints those reported on lines of its scope, and a final build those of its final closure.

## Query engine

The **query engine** is the checker's incremental engine: every fact the checker computes results from a query over one item and target.

- A query's key is the [toolchain hash](36-builds.md#toolchain-hash) of the language version of the item's package, the target, the item hash, the [interface hash](29-identity-versioning.md#hashes) of each item and member it read, each digest fact it read, the item hash of each transparent function whose body it unfolded, and the item hash of each function its interpreter runs executed. The query engine must recompute a query exactly when its key changed.
- **Latency.** On a workspace of 100,000 source lines whose previous state's results are in the build cache, after one edit to a function body leaving its interface hash and digest unchanged, `nan check .` must complete within 200 ms of wall-clock time on an x86_64 machine with 8 hardware threads at 3 GHz and 16 GiB of memory.

## Prefix checking

`nan check --prefix <path>` checks the file at that path as a prefix, and `--prefix <path>:<line>` the prefix made of its lines up to and including that line. A prefix may end at any byte; every other file is the workspace's. The output is, in order:

1. the error reporting the prefix's [first dead token](41-grammar.md#prefix-viability), if any;
2. the diagnostics of the longest prefix ending before that token, or of the whole prefix, closed as [Grammar](41-grammar.md#prefix-viability) defines, except each diagnostic reported on a line holding an appended token, and each diagnostic with one of the codes `closure.capture-mode`, `closure.capture-unused`, `contract.decreases-unneeded`, `expr.argument-count`, `expr.branch-form`, `expr.missing-else`, `expr.non-exhaustive`, `generic.parameter-unused`, `item.fails-unneeded`, `item.wait-unneeded`, `own.linear-dropped`, `stmt.else-not-diverging`, `stmt.missing-return`, `stmt.unused-label`, `text.hole-unbound`, `threshold.unused-waiver`, `type.missing-field`, `type.payload-shape` and `unused.local` whose construct (the item, closure, body, block, `if`, `match`, loop, `let`-`else`, call or construction its rule judges) holds an appended token;
3. a record `pending <location> <item> <clause>` per [pending clause](41-grammar.md#prefix-viability), at its head's first line, the clause written as the head or enum would hold it (`fails ReadError`, `uses wait`, `Cancelled(task::Cancelled) from`);
4. in appending order, a record `hole <location> <item> <type>` per typed hole and `close <text>` per closer that closing appends.

A prefix with a dead token fails; pending clauses never do.

```nan
pub closed struct Reading {
    pub sensor: u16
    pub level: u32
} Reading
```

```text
$ nan check --prefix example/example.nan:3
ok check --prefix example/example.nan:3: dead 0, pending 0, holes 0
close } Reading
ok check --prefix example/example.nan:3: dead 0, pending 0, holes 0
```

## Context

A **context pack** is the output of `nan context`: the [closure set](00-charter.md#closure) of an item, or of the line at a location, within an output budget. Its records are, in order:

1. `item <item> <location>`, holding the enclosing item's text;
2. `manifest <package>`, holding the closure set's manifest directives in canonical order, each `dep` directive followed by the lock's record for it;
3. `names <module>`, holding the names of the module's items in byte order, separated by `, `;
4. an `api` record per item, tombstone and expression type of the closure set that no earlier record shows, in order of first occurrence, as `nan show --api` prints it but without doc comments and showing what the line's position sees; a field or variant appears in its type's record, a tombstone's record names its old form, and the [core vocabulary](03-packages-modules.md#core-vocabulary) has none;
5. an `item` record per other function whose NIR an interpreter run of the closure set executes.

```nan
pub const STEP: u32 = 5

pub closed struct Gauge {
    pub level: u32
} Gauge

pub fn Gauge.reset(mut self) {
    self.level = STEP
} Gauge.reset
```

```text
$ nan context example::Gauge.reset
ok context example::Gauge.reset: entries 5, radius 108
item example::Gauge.reset example/example.nan:8
    pub fn Gauge.reset(mut self) {
        self.level = STEP
    } Gauge.reset
manifest example
    language 1
    targets any
names example
    Gauge, Gauge.reset, STEP
api example::Gauge example/example.nan:4
    pub closed struct Gauge {
        pub level: u32
    } Gauge
    properties: Send, Inert, AnyBits
api example::STEP example/example.nan:2
    pub const STEP: u32 = 5
ok context example::Gauge.reset: entries 5, radius 108
```

### Closure radius

The **closure radius** of an item is the size in units of its context pack's records without budget. `nan context` and `nan explain` report it, and `nan check --radius` prints a record `radius <item> <N>` per item of the scope, largest first, then by path. No rule judges it.

### Completeness test

`nan context` is complete: it prints every fact the checker reads while checking the item. An implementation must record, for each item it checks, the facts its queries read; the **completeness test**, a conformance test, checks every item of every specification example and fails when a recorded fact outside the core vocabulary is absent from the item's context pack.

## Edits

A **compare-and-swap edit** replaces one item only while its item hash is unchanged and the result checks. Every command holds an advisory `flock` lock on `.nan/write` while it reads or writes workspace files, exclusive only when it writes source or machine files. Writes go through the **journal** `.nan/journal`: it records the path and new content, or deletion, of every file, then a line marking it complete; each file is then replaced by renaming a new file into place, and the journal is deleted. A command finding a complete journal first performs its replacements, and deletes an incomplete one.

### Compare-and-swap edits

`nan edit replace <item> --expect <hash> --from <file>` names the item by path or by a location in it, its item hash as `nan show` prints it, and the file holding the new text, `-` for standard input.

1. When the item's item hash differs from `<hash>`, the edit is an error (`edit.stale`), holding the item's current `nan show` record.
2. When the new text is not exactly one item starting at column 0, it is an error (`edit.not-one-item`); when that item's identity ([Order and names](02-items.md#order-and-names)) differs from the replaced item's, an error (`edit.identity-changed`) whose because-chain names `nan edit rename`.
3. The checker checks the workspace with the replacement; an error on a line of the new item is an error (`edit.check-failed`) holding those diagnostics.
4. Otherwise the command writes the new item in place of the old and prints `replaced <item> <location> <item hash>`, then each error of another item that the workspace did not report before.

A malformed or mis-checksummed hash is `tool.usage`.

### Rename and move

`nan edit rename <item> <name>` renames an item or member; `nan edit move <item> <module>` moves an item to another module of its package, with the functions named after it and the impl lines naming it in its module. Each:

- rewrites the declaration, its closing label and every reference in the workspace, doc references included; a move appends the moved items to the target module's [summary file](32-docs.md#module-summaries) and raises a private item to `pkg` when an item of another module then names it;
- writes the rename [tombstone](29-identity-versioning.md#tombstones) `retired <old path> => <new path> preserves`, placed as [Canonical order](35-format.md#canonical-order) states, when the package's [history head](29-identity-versioning.md#publishing) has an item or member visible to consumers at the old canonical path, or the member is a field or variant of a type deriving `Encode`;
- checks the workspace with every change, an error that the workspace did not report before being an error (`edit.check-failed`) that writes nothing.

Moving a member, a function named after a type without that type, or an item into a missing module or another package, is an error (`edit.move-invalid`).

## Merge driver

The **merge driver** is `nan merge <base> <ours> <theirs>`, merging three versions into `<ours>`: in the workspace form, of the workspace; with `--path <path>`, the file form, of that file. It also merges **order-free members**, whose order carries no meaning ([order-free members](#order-free-members)).

Files match by path and absence counts as a version; a file other than a source or machine file merges as one unit. A source file merges per item:

1. Each version splits into items at their column-0 starts, comment lines before an item belonging to it and the file summary forming one more unit. Items match by identity, an item whose head does not parse by its first line.
2. An item whose text is equal in ours and theirs, or changed on one side only, takes that text.
3. An item the base holds that both sides changed differently, neither deleting it, merges its order-free members when its text outside them changed on one side only. Every other case is a [conflict](#conflicts).
4. When one side deleted X and wrote `retired X => Y preserves` and the other changed X, the driver merges that change into Y, X's base and changed versions renamed to Y. In the file form, a Y outside the file is a conflict.
5. Items and members keep ours' order; one only theirs holds follows the nearest preceding one in theirs that the result holds, after those ours added there, or comes first.

The lock takes the version with the later [snapshot](29-identity-versioning.md#lock), ours when neither is later; the history takes the union of the tombstones, ours' winning for equal identities, and the `name` and `head` records of the version that changed them from the base, ours' when both did; an API snapshot merges per entry as items do, an entry both sides changed differently taking ours' records and theirs' `pending` records. In the workspace form, the driver then checks the merged workspace and prints its diagnostics.

### Order-free members

The order-free members are an enum's variants and a struct's fields, except those of a type deriving `Ord`, `Render` or `Generate`, whose order carries meaning ([normalised form](29-identity-versioning.md#normalised-form)); a struct's invariant lines; the entries of a `limits` or `derives` clause; the arms of a `match` (not `match first`) whose `match` line is equal in all three versions; and a manifest's directives. Arms match by pattern text, `limits` and `derives` entries and `language`, `targets` and `entry` directives by word, `dep` directives by alias, `allow` directives by authority (a `contribute` grant with its registry path), invariant lines and other directives by text, and other members by name. Within each set, members merge as items do in steps 2 and 3. When both sides changed a `targets` or `allow` directive, its list of names merges as a set: the base's names that neither side removed and the names either side added, written in [canonical format](35-format.md#canonical-format).

```nan
pub closed enum Signal {
    Start
    Stop
} Signal
```

With that base, ours adding `Pause` after `Stop` and theirs adding `Resume` after `Stop` merge into:

```nan
pub closed enum Signal {
    Start
    Stop
    Pause
    Resume
} Signal
```

### Conflicts

A conflicting item or file, or a source file with a version that does not lex, is a **conflict**: an error (`merge.conflict`). Its text is replaced by a **conflict block**: the line `<<<<<<< ours`, ours' text, the line `=======`, theirs' text and the line `>>>>>>> theirs`, each at column 0, an absent version's text being empty.

- A source file holding a conflict block is an error (`merge.unresolved`), reported on the block's first line in place of every diagnostic of its lines, with two exact fixes keeping ours' or theirs' text.
- A **semantic conflict** is an error (`merge.semantic-conflict`): one side changed the interface hash of an item I, and the other added an item J naming I or changed J so that it names I where its base did not. It is reported on J, quoting I's base and changed `api` lines; the file form reports it when I and J lie in the file.

## Last green

The **last-green record** of an agent holds, per item, its interface hash and `nan show --api` record as of the last `nan check` that reported no error over a scope holding the item. It is the file `.nan/green/<agent>`, the agent being named by `--agent` with ASCII letters, digits, `_` and `-`, or `default`.

- A `nan check` reporting no error replaces the record's entries for every item of its scope.
- A `nan check` reporting an error prints a record `changed <item> <location>`, merging its old and new `api` lines as `nan diff` does, per item of the closure sets of the lines holding an error whose interface hash differs from its entry. An item without an entry has none.

## Diagnostics

| Code | Class | Rule |
| --- | --- | --- |
| `edit.check-failed` | error | [Compare-and-swap edits](#compare-and-swap-edits), [Rename and move](#rename-and-move) |
| `edit.identity-changed` | error | [Compare-and-swap edits](#compare-and-swap-edits) |
| `edit.move-invalid` | error | [Rename and move](#rename-and-move) |
| `edit.not-one-item` | error | [Compare-and-swap edits](#compare-and-swap-edits) |
| `edit.stale` | error | [Compare-and-swap edits](#compare-and-swap-edits) |
| `merge.conflict` | error | [Conflicts](#conflicts) |
| `merge.semantic-conflict` | error | [Conflicts](#conflicts) |
| `merge.unresolved` | error | [Conflicts](#conflicts) |
| `tool.no-workspace` | error | [Commands](#commands) |
| `tool.unknown-scope` | error | [Commands](#commands), [Reading commands](#reading-commands), [Confirming docs](32-docs.md#confirming-docs) |
| `tool.usage` | error | [Commands](#commands), [Writing commands](#writing-commands), [Output budgets](#output-budgets), [Compare-and-swap edits](#compare-and-swap-edits), [Confirming docs](32-docs.md#confirming-docs), [Stage comparison](36-builds.md#stage-comparison) |
