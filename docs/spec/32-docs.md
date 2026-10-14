# Documentation

This chapter defines doc comments, their code spans and doc references, file summaries and module summaries, the confinement of code in docs to examples, digest claims, the binding of public docs to interface hashes and the confirmation of docs.

## Doc comments

A **doc comment** is a run of `DOC` lines with no other token between them, directly before an item or a member, and documents it. Doc comments are admitted exactly before an [item](02-items.md#item-kinds), a [field](04-types.md#structs) or invariant line, a [variant](04-types.md#enums), an [interface](08-generics-interfaces.md#interfaces) method, a [layout](23-low-level.md#layouts) field or tag, and a [registers block](23-low-level.md#registers) line.

- Each run of `DOC` lines at any other position, and each `FILE_DOC` line that is not the first token of its file, is one error (`doc.misplaced`), reported on its first line. The parser reads its lines as plain comments and abandons no item; the exact fix replaces the opener of each line with `//`.
- The text of a `DOC` or `FILE_DOC` line is its token text without its first character when that character is a space. The **doc text** of a doc comment is the text of its lines joined by LF; that of a file summary is the text of its line.
- The toolchain reads each line of doc text from left to right: a backtick opens a **code span** ending at the next backtick, and a `[` opens a [doc reference](#references) ending at the next `]`. A backtick or `[` with no closing character later on its line, and every character outside code spans and doc references, is prose, printed as written.
- Doc text never changes the meaning of code.

```nan
/// Frames seen on one link, counted by [Tally.record].
pub closed struct Tally {
    /// Frames recorded since [Tally.empty], kept at the `u32` maximum once reached.
    pub frames: u32
} Tally

/// A tally of no frames.
pub fn Tally.empty() -> Tally = Tally(frames: 0)

/// Adds one frame to the tally.
pub fn Tally.record(mut self) {
    self.frames = self.frames.saturating_add(1)
} Tally.record
```

```nan
pub fn frame_limit() -> u32 {
    /// The limit grows with the window. // error: doc.misplaced
    return 64
} frame_limit
```

### References

A **doc reference** is the text between the brackets of a `[` ... `]` pair of doc text. It is lexed as source ([Lexical structure](01-lexical.md)) and parsed by this rule:

```ebnf
doc_reference = path ( "." IDENT )? ;
```

- The [path](03-packages-modules.md#paths) resolves as if written in the module of the documented item, or of the file for a file summary, the root module for `package.nan`, and names a live item; a one-segment path names an item of that module or a [core vocabulary](03-packages-modules.md#core-vocabulary) name. `.IDENT` names a [member](03-packages-modules.md#members) of that item.
- A doc reference whose text holds a space, is not a `doc_reference` or names no live item or member visible from that module is an error (`doc.unresolved-reference`). Its exact fixes write each candidate that `name.unknown` ranks for the same text ([Unknown names](03-packages-modules.md#unknown-names)), and, when the text matches the old path of a rename [tombstone](29-identity-versioning.md#tombstones), the new path.
- The visibility of an item is its own, private for a kind without visibility; that of a field is the narrower of its own and its struct's; that of any other member is its item's. A doc reference in the doc comment of an item or member, naming an item or member with narrower visibility, is an error (`doc.private-reference`).
- A doc reference creates no module dependency and is not a use of what it names.

```nan
pub const WINDOW: u32 = 64

/// Frames accepted before [WINDOW] acknowledgements are due, as a [u32].
pub fn window_size() -> u32 {
    return WINDOW
} window_size
```

```nan
const WINDOW: u32 = 64

/// Frames accepted before [WINDOW] acknowledgements are due. // error: doc.private-reference
pub fn window_size() -> u32 {
    return WINDOW
} window_size
```

```nan
pub closed struct Tally {
    pub frames: u32
} Tally

/// Frames seen on one link, counted by [Tally.add]. // error: doc.unresolved-reference
pub fn Tally.record(mut self) {
    self.frames = self.frames.saturating_add(1)
} Tally.record
```

## File summaries

A **file summary** is the `FILE_DOC` line that is the first token of a source file. Every source file, `package.nan` included, has exactly one; the summary of `package.nan` describes the package. A file whose first token is not a `FILE_DOC`, or whose summary text is empty or only spaces, is an error (`doc.summary-missing`), reported on line 1.

```nan
// file: tally/package.nan
//! Frame counting per link.

package tally {
    language 1
    targets any
} tally

// file: tally/tally.nan
//! Frame tallies and their limit.

pub const LIMIT: u32 = 1_000
```

```nan
// file: tally/package.nan
//! Frame counting per link.

package tally {
    language 1
    targets any
} tally

// file: tally/tally.nan
pub const LIMIT: u32 = 1_000 // error: doc.summary-missing
```

### Module summaries

The **module summary file** of a [module](03-packages-modules.md#modules) is its source file named like the module, `<module>.nan`, or `<package>.nan` for the root module, `<package>` being the package name. Each [target variant](03-packages-modules.md#target-variants) directory holds its own, named like the module without the `@` suffix. The **module summary** is the summary of that file; tools print it as the module's description.

- A module or variant with source files, none of them its module summary file, is an error (`doc.module-file-missing`), reported where [Modules](03-packages-modules.md#modules) reports diagnostics about its directory.
- The module summaries of a module's variants are identical, and so are the doc comments of each item that every variant must hold and of its members. A difference is one error per module or item (`doc.variant-mismatch`), reported on the summary line or the item's first line in the first variant directory, in byte order of path, that differs from the first variant.

```nan
// file: tally/package.nan
//! Frame counting per link.

package tally {
    language 1
    targets any
} tally

// file: tally/tally.nan
//! Frame tallies and their limit.

pub const LIMIT: u32 = 1_000

// file: tally/window/window.nan
//! Acknowledgement windows over a tally.

pub fn window_for(frames: u32) -> u32 {
    return frames / SLOT_FRAMES
} window_for

// file: tally/window/sizes.nan
//! Frames per window slot.

const SLOT_FRAMES: u32 = 8
```

```nan
// file: tally/package.nan
//! Frame counting per link.

package tally {
    language 1
    targets any
} tally

// file: tally/tally.nan
//! Frame tallies and their limit.

pub const LIMIT: u32 = 1_000

// file: tally/window/sizes.nan
//! Frames per window slot. // error: doc.module-file-missing

pub const SLOT_FRAMES: u32 = 8
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

/// The stack reserve of one task.
pkg fn default_stack() -> usize {
    return 16_384
} default_stack

// file: probe/platform@host/platform.nan
//! Stack defaults per machine.

/// The stack reserve of one task.
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

/// The stack reserve of one task.
pkg fn default_stack() -> usize {
    return 16_384
} default_stack

// file: probe/platform@host/platform.nan
//! Stack defaults per machine.

/// The stack reserve of one host task. // error: doc.variant-mismatch
pkg fn default_stack() -> usize {
    return 65_536
} default_stack
```

## Code in docs

Doc text holds code only in code spans; code showing how an item is used is an [example clause](18-contracts.md#examples) or a [test](22-testing.md#test-items). A doc comment or file summary holding a line whose text, after leading spaces, starts with three backticks is an error (`doc.code-block`), reported once, on the first such line.

```nan
pub closed struct Tally derives Eq {
    pub frames: u32
} Tally

/// The tally after one more frame.
pub fn Tally.after_frame(self) -> Tally
    example Tally(frames: 1).after_frame() == Tally(frames: 2)
{
    return Tally(frames: self.frames.saturating_add(1))
} Tally.after_frame
```

```nan
pub closed struct Tally derives Eq {
    pub frames: u32
} Tally

/// The tally after one more frame:
/// ``` // error: doc.code-block
/// Tally(frames: 1).after_frame() == Tally(frames: 2)
/// ```
pub fn Tally.after_frame(self) -> Tally {
    return Tally(frames: self.frames.saturating_add(1))
} Tally.after_frame
```

## Digest claims

Doc text never restates a [digest fact](10-effects-digests.md#digests). A **digest claim** is, in doc text outside code spans, a claim word followed by a fact word with only spaces and LF between them, or a claim term. Words are maximal runs of ASCII letters, digits, hyphens and apostrophes, U+2019 counting as an apostrophe, compared without regard to case. The claim words are `never`, `not`, `cannot`, `can't`, `doesn't`, `don't`, `won't`, `may`, `might`, `always`, `neither`, `nor` and `without`.

| Fact | Fact words | Claim terms |
| --- | --- | --- |
| wait | `wait`, `waits`, `waiting`, `block`, `blocks`, `suspend`, `suspends`, `suspending` | `blocking`, `non-blocking` |
| alloc | `allocate`, `allocates`, `allocating` | `non-allocating`, `allocation-free`, `noalloc` |
| may-trap | `trap`, `traps`, `trapping`, `panic`, `panics`, `panicking` | `trap-free`, `panic-free`, `notrap` |
| terminates | `terminate`, `terminates`, `terminating`, `halt`, `halts`, `halting` | `non-terminating` |
| [purity](10-effects-digests.md#purity) | none | `pure`, `side-effect-free` |

A digest claim is an error (`doc.digest-claim`), reported once per line on which one starts.

```nan
pub closed struct Tally {
    pub frames: u32
} Tally

/// Adds one frame, keeping the count at its maximum once reached.
pub fn Tally.record(mut self) limits noalloc {
    self.frames = self.frames.saturating_add(1)
} Tally.record
```

```nan
pub closed struct Tally {
    pub frames: u32
} Tally

/// Adds one frame and never allocates. // error: doc.digest-claim
pub fn Tally.record(mut self) {
    self.frames = self.frames.saturating_add(1)
} Tally.record
```

## Doc binding

A **public doc** is the doc comment of a `pub` item, or of a member that enters the [interface form](29-identity-versioning.md#hashes) of a `pub` item. Each public doc is bound to the interface hash of its item, the enclosing item for a member: the module's [API snapshot](31-trust.md#api-snapshots) records, per public doc, its doc text and its **bound hash**.

- A public doc's snapshot entry is the one recorded for its item and its key: empty for the item's own doc; the member's name for a field, variant, interface method, layout field, tag or register; for an invariant line, its position, from 1, among the item's invariant lines entering the interface form.
- `nan api update` records each public doc with the bound hash of its snapshot entry when that entry holds the same doc text, and with its item's current interface hash otherwise.
- A **stale doc** is a public doc whose snapshot entry holds the same doc text and a bound hash other than its item's current interface hash. An item with stale docs is incompleteness (`doc.stale`), reported once, on the first line of its first stale doc in file order; the diagnostic lists every stale doc of the item and the command confirming them.

```nan
/// Frames a window of `slots` slots holds, at 8 frames per slot.
pub fn window_frames(slots: u32) -> u32 {
    return slots.saturating_mul(8)
} window_frames
```

Here, changing only the body of `window_frames` leaves its doc current. Adding a parameter `per_slot: u32` without editing the doc makes the doc stale (`doc.stale`) until its text is edited or `nan api update --confirm-docs example::window_frames` runs.

### Confirming docs

`nan api update --confirm-docs <item>...` performs `nan api update`, then sets the bound hash of every public doc of each named item and its members to the item's current interface hash. A path naming nothing is an error (`tool.unknown-scope`), and an item without public docs or outside the command's scope is an error (`tool.usage`); either way the command writes nothing.

## Diagnostics

| Code | Class | Rule |
| --- | --- | --- |
| `doc.code-block` | error | [Code in docs](#code-in-docs) |
| `doc.digest-claim` | error | [Digest claims](#digest-claims) |
| `doc.misplaced` | error | [Doc comments](#doc-comments) |
| `doc.module-file-missing` | error | [Module summaries](#module-summaries) |
| `doc.private-reference` | error | [References](#references) |
| `doc.stale` | incompleteness | [Doc binding](#doc-binding) |
| `doc.summary-missing` | error | [File summaries](#file-summaries) |
| `doc.unresolved-reference` | error | [References](#references) |
| `doc.variant-mismatch` | error | [Module summaries](#module-summaries) |
