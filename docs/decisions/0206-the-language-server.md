# 0206. The language server

Status: accepted

## Context

Agent harnesses and editors meet a language through the Language Server Protocol. Claude Code pushes a server's diagnostics into context after each edit and gives the agent a read-only tool for definitions, references, hovers, symbols, implementations and call hierarchies. Editors, where people review what agents write, offer the same and also complete, sign and highlight code as it is typed. NaN was reachable only through its commands ([0012](0012-toolchain-as-agent-interface.md)).

Language servers are commonly ranked in levels, each needing a deeper model of the program: diagnostics with quick fixes and formatting; navigation; authoring help (completion, signature help, inlay hints, semantic highlighting); refactoring. The checker already computes what each level needs: per-item queries restored from the build cache's snapshots ([0055](0055-snapshots-of-checks-in-the-build-cache.md)), exact fixes re-checked in memory ([0017](0017-exact-fixes-verified-by-re-checking.md)), the references renaming rewrites ([0020](0020-edits-from-checker-references.md)), typed bodies holding each expression's type and what it resolves to, and prefix closing ([0018](0018-prefix-closing-from-the-grammar-tables.md)). The closure guarantee ([0003](0003-agent-first-guarantees.md)) gives every name one meaning, so definitions and references need no heuristics.

## Decision

- `nan lsp` serves the protocol on standard input and output up to the authoring level: published diagnostics, their exact fixes as quick fixes, formatting, navigation, completion, signature help, inlay hints and semantic tokens ([language server](../spec/33-toolchain.md#language-server)).
- The server translates; it decides nothing the commands do not. Each answer comes from a check-time query over the workspace read afresh, the client's open documents in place of their files; the server keeps only those documents and the files it published diagnostics for. The diagnostics are those `nan check` reports on the document's package, their message the record's lines, so that an agent reading only the message still gets expected, found, the because-chain and the exact fix.
- Ranges come from the checker's byte spans, counted in the code units the client and server agree on; tool output keeps no column.
- Completion and signature help read the text before the position closed as prefix checking closes it, then the items after the one holding the position, so that the function being written types.
- Documents arrive whole, and the server answers in message order, publishing after each change, so that a session's output depends on its input alone.
- The bootstrap and the NaN toolchain both serve it until the freeze ([0120](0120-language-1-and-the-bootstrap-freeze-with-the-toolchain-1-0.md)), and the self-check compares their answers to one session.

Alternatives rejected:

- **Navigation only:** people review agents' code in editors, and the authoring answers reuse the same queries.
- **Rename and refactoring actions:** `nan edit rename` and `nan edit move` write tombstones and refuse an edit whose result reports a new error; a client applying a returned edit would skip both.
- **Checks kept warm across messages:** a checker borrows its workspace, and the snapshots of the build cache already bound a check's cost after a body edit.
- **Incremental document changes:** each change would need applying in the order the client meant, while whole documents make each one a replacement.
- **The server in the NaN toolchain alone:** the self-check would have nothing to compare its answers with.

## Consequences

- Each change costs a check of its package: one restored from the snapshot after a body edit, a full one otherwise.
- References and incoming calls type every body of the workspace's closures.
- What std or a locked release declares has no location to go to, though hovers describe it.
- Each query the server answers is ported to the NaN toolchain, as each command is.
