# 0003. Agent-first guarantees

Status: accepted

## Context

An agent never holds a whole program. It reads a window or a grep hit, writes left to right, edits by exact string replacement, starts every session without memory and works beside other agents on the same files.

Mainstream languages assume a reader who holds the project in an editor: an import list at the top of the file, a method found through traits in scope, a type inferred from a later use, a signature inferred from a body, a block comment or string opened above the window, a macro that defines names nobody wrote. Each turns a correct-looking line into a guess. Agents' most frequent failures follow from them: missing imports, misresolved names, swapped arguments, and textual merge conflicts between parallel changes.

## Decision

Three guarantees are normative ([guarantees](../spec/00-charter.md#guarantees)), and the syntax is chosen to keep them.

- **Closure.** A line's meaning, acceptance and diagnostics depend only on its closure set: its item, what the items and types it names expose, its module's grants, its package's targets, its aliases and the core vocabulary. `nan context` prints that set, and its completeness is tested ([context](../spec/33-toolchain.md#context)).
- **Prefix viability.** Text already written fixes all meaning. Every file or prefix has a defined first dead token, and every prefix before it closes, with pending clauses, typed holes and closers, into a file the grammar derives ([prefix viability](../spec/41-grammar.md#prefix-viability)).
- **Item granularity.** The column-0 item is the unit of identity, checking, editing, merging, sealing and versioning.

Closure rests on these choices:

- `::` is the only path separator and `.` the only member access, so a local never collides with a module ([paths](../spec/03-packages-modules.md#paths)). No imports, globs, re-exports, renames or shadowing.
- Heads are complete: modes and types of every parameter, the result, `fails`, `uses wait` and `limits` are written, never inferred ([heads](../spec/02-items.md#heads)). A call checks against the callee's head alone.
- `x.f()` reaches only methods of x's type, of its package's interfaces and of core interfaces; other interface methods are called qualified ([method resolution](../spec/08-generics-interfaces.md#method-resolution)). No overloading, inheritance, blanket impls, orphans, implicit conversions or macros.

Prefix viability rests on these:

- An LL(1) grammar published with each language version. Types are fixed at definition, and a literal's type comes from its suffix or its expected type.
- Heads precede bodies, and contracts sit between them in the clause region. A `fails`, `uses wait` or `from` entry the body needs but the head lacks is a pending lower bound while writing.
- A normative left-to-right evaluation order ([evaluation order](../spec/06-statements-expressions.md#evaluation-order)).

Item granularity rests on these:

- One item shape, `[pub|pkg] [modifiers] kind Name ...` at column 0; braced items close with `}` and their name. Fourteen kinds, with function kinds as modifiers of `fn`; methods are flat items named `Type.name`. Order carries no meaning, and the parser resynchronises at every column-0 keyword ([item shape](../spec/02-items.md#item-shape)).
- Every item has an item hash and an interface hash. Edits are compare-and-swap on the item hash, the merge driver merges per item and per order-free member ([merge driver](../spec/33-toolchain.md#merge-driver)), and seals and compatibility are per item. Source files hold no hashes or versions.

Windows and grep hits stand alone through these:

- Line-local lexing: every token fits on one line, comments are `//` lines, and multi-line text is a run of `\\` lines with no closing fence ([line-local lexing](../spec/01-lexical.md#line-local-lexing)). Source is UTF-8 with LF and four-space indentation; identifiers are ASCII; invisible and bidirectional code points are rejected.
- One statement per line, continued only after an operator, comma or open delimiter; no tail-expression results; `and`, `or` and `not`; precedence only within arithmetic and between arithmetic and comparison. No single token changes the meaning of its line.
- Every transfer of control, ownership or effect carries a word at the site (`try`, `attempt`, `fail`, `await`, `mut`, `take`, `defer`, `discard`, `trusted`, `expect`), and every argument after the first is labelled, so swapped arguments fail to check.
- Nesting deeper than 4 or a body longer than 80 lines is incompleteness unless waived ([thresholds](../spec/06-statements-expressions.md#thresholds)), so an item fits in one window.
- Familiar forms keep their mainstream meaning, and foreign idioms such as `&&` or `?` get exact rewrites ([false friends](../spec/40-false-friends.md)). Name prefixes such as `try_` and `is_` are checked against heads.
- Keywords are chosen to be one token in major tokenizers. This is a design goal, not a rule, because tokenizers change outside the language.

`nan fmt` defines one canonical format ([canonical format](../spec/35-format.md#canonical-format)): no options, no reflow for width, a fixed order of items and members. Every text the toolchain writes, from exact fixes to merge results, is canonical, so an exact-string edit made after a tool run still finds its text.

Rejected alternatives:

- **Imports and `.` paths:** a second, distant edit for every new use, and a local can shadow a module.
- **Inferred heads:** inference across bodies breaks closure and lets a body edit change an interface.
- **Scope-dependent method lookup and macros:** an unseen upstream addition or expansion changes a line.
- **Significant indentation and tail expressions:** whitespace or a trailing `;` would carry meaning that harmless-looking edits change.
- **Block comments, raw strings and fenced multi-line text:** a window that starts inside one is lexed wrongly.
- **Literal defaulting, octal and non-ASCII identifiers:** each is a silent misreading or a look-alike name.
- **Nested impl blocks and more item kinds:** an item's identity would depend on its enclosing block, and each kind multiplies the grammar, formatter, hasher and merge rules.
- **Profiles and `cfg`:** meaning would vary with configuration. Per-target variants are directories exporting identical items, and every declared target is checked.
- **Textual merges:** parallel changes to unrelated members of one item conflict.

## Consequences

- Code is longer than in mainstream languages: qualified paths, full heads, labels and named closers. Tools write much of it, such as `nan fix --propagate` for head clauses and stubs for unknown names, and the primer's token budget prices every concept.
- Every item checks on its own, and a body edit never changes an interface, so the query engine reuses results per item hash, interface hash and digest. The per-edit latency target rests on this.
- Rules that relate several items (unique names, coherence, the module graph, registries, stack limits, the release gate, version commands) are listed as outside closure, and nothing else may be.
- Any feature that leaks non-local meaning breaks closure. The completeness test of `nan context` guards against it.
- The surface works against model priors from Rust, C and Python. The idiom table, the false-friends chapter and nanbench carry and measure the correction.
- Disjoint additions by parallel agents to one enum, struct, match or manifest merge without conflict, except members whose order carries meaning.
