# 0014. Parser from the published grammar

Status: accepted

## Context

Language 1 publishes its grammar as `ebnf` blocks in [Grammar](../spec/41-grammar.md), and every implementation must lex and parse exactly by it. Several outputs are defined by the grammar itself rather than by a parse result: `syntax.unexpected-token` lists every token the grammar accepts at the failure, a rejected alternative reports its own code once parsed, `item.indented` asks whether the grammar would accept the token after a `COL0`, and [prefix checking](../spec/33-toolchain.md#prefix-checking) needs the first dead token and the fewest tokens closing a prefix. A hand-written recursive descent parser restates the grammar in code, and each of these outputs then needs a further restatement that nothing keeps in step with the published rules.

## Decision

- **Generated tables.** `bootstrap/nan/build.rs` parses the `ebnf` blocks of `docs/spec/41-grammar.md` and generates the enums of rules, hard keywords, punctuation, foreign spellings and context words, the rejected code points, and a table of the syntactic rules `item` reaches: each rule a choice of sequences, each sequence its elements and rejection code.
- **LL(1) driver.** The parser is one generic driver over those tables with an explicit stack of frames. It computes FIRST sets once, chooses alternatives by the next token, and applies the resolutions the grammar publishes in code: context words over `IDENT` and their three exceptions, PascalCase before generic arguments, the comma of a `limits` clause, and greedy optional parts. Alternatives outside every rejection win over rejected ones. The rules of [Items](../spec/02-items.md) that name codes for syntax failures, single-line items, resynchronisation, misplaced doc comments and the idiom table are written around the driver.
- **Trees.** Each item parses into its own concrete syntax tree with one node per rule the parse expands, over the file's tokens. Comments stay outside the tree, attached by line.

Alternatives rejected:

- **A hand-written recursive descent parser**: expected-token sets, rejected alternatives and prefix closings would each restate the grammar by hand.
- **Parsing the grammar at run time**: rules would be named by strings, and a renamed rule would break the checker only when the code naming it runs. Generated enums turn it into a build error.

## Consequences

- A change to the grammar in the specification changes the parser at the next build; the toolchain hash covers `41-grammar.md`.
- A unit test checks that no two alternatives of a choice share a terminal outside the published resolutions, so the published grammar stays LL(1).
- The tree mirrors the grammar's rules: operator precedence appears as the grammar's tails, which later stages re-associate, and lists nest after each comma.
- The build script reads a file outside its crate.
