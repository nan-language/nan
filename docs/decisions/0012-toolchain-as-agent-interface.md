# 0012. The toolchain as the agent interface

Status: accepted

## Context

An agent meets nan through the toolchain's output far more than through its own reading of the code. Every token of output is read again on each later turn and dilutes attention well before the context limit. The quality of feedback bounds how often an agent repairs code correctly: precise causal feedback raises repair rates between 1.6 and 3.3 times, and guarded edits with immediate errors measurably raise task success. Agents edit by exact string replacement, so a fix in that shape applies in one step, while column numbers, carets and aligned text depend on a visual layout that tokenizers destroy.

Agents also work in parallel on one codebase, in which the item is the unit of identity, editing and merging ([0003](0003-agent-first-guarantees.md)). Textual merge conflicts between agents run at 20 to 42%, 5 to 10% of merges conflict semantically with no textual overlap, and agents resolve conflicts themselves only 0.5 to 29% of the time. Every edit must be checked before the agent moves on, so checking must be fast on a large workspace.

## Decision

- **Output contract.** Commands are non-interactive and scoped. Output starts and ends with the same verdict line, stays within a budget whose elisions name the command revealing them, has an equivalent JSON Lines form, and holds no colour, columns or absolute paths ([output contract](../spec/33-toolchain.md#output-contract)).
- **Diagnostics.** A diagnostic gives a stable dotted code, path, line, item, the exact quote, expected and found written in nan, and a because-chain. Its fixes are exact edits of a unique old text and its new text, offered only after an in-memory re-check passes. Only root causes are reported. An internal error has its own exit class and a reproducer ([diagnostics](../spec/34-diagnostics.md)).
- **Help against priors.** `name.unknown` ranks candidates by spelling and type and offers stubs for unknown local names; `name.moved` and `name.newer-release` consult the tombstone history; `syntax.foreign` rewrites foreign idioms through an idiom table grown from the errors agents make ([idiom table](../spec/34-diagnostics.md#idiom-table)).
- **Item query engine.** The checker is an incremental engine of queries over one item and target, keyed by the toolchain hash, the target, the item hash, the interface hashes and digest facts it read, the transparent bodies it unfolded and the functions its interpreter ran. After a body edit, `nan check` on a warm 100,000-line workspace completes within 200 ms on a stated reference machine ([query engine](../spec/33-toolchain.md#query-engine)).
- **Partial programs and context.** `nan check --prefix` closes an unfinished file with typed holes and reports its first dead token. `nan context <item|file:line> --budget N` prints the closure set deciding a line, and a conformance test checks that the set is complete.
- **Compare-and-swap edits.** `nan edit replace <item> --expect <hash>` writes only while the item hash is unchanged and the result checks. `nan edit rename` and `move` rewrite the workspace atomically and write rename tombstones. Writes go through a journal under an advisory lock ([edits](../spec/33-toolchain.md#edits)).
- **Merge driver.** `nan merge` merges per item, ignores item order, takes the union of distinct additions to order-free members (variants, fields, invariant lines, match arms, limits, derives, manifest directives), ports an edit of X onto Y when the other side wrote `retired X => Y preserves`, keeps the later lock and both sides' pending weakenings, reports a semantic conflict when one side changed an interface the other newly references, then checks the result. A per-agent last-green record reports the interfaces that changed since that agent's last check without errors ([merge driver](../spec/33-toolchain.md#merge-driver)).

Alternatives rejected:

- **Warnings**: see [0011](0011-one-release-gate.md).
- **Human-oriented output**: colour, columns, carets, pagers and prompts cost tokens and break non-interactive use.
- **Fixes written as advice, or offered without re-checking**: an agent applying a fix that does not check loses a turn and trusts the next fix less.
- **Cascades of follow-on errors**: they spend context and bury the root cause.
- **Checking per file or module**: it cannot meet 200 ms on large workspaces, and the latency target shapes the compiler's first design, so it cannot be added later.
- **Textual merges**: the conflict rates above, resolved poorly by agents.
- **Read sets**: per-session records of what each change read, which a git merge driver cannot see. The interface-change rule and the check after merging catch the same conflicts with no stored state.
- **Editing through a syntax tree, JSON patches or a code database**: they leave the files, grep and exact replacement that agent harnesses are built on. Text stays the source of truth.
- **Sets of admissible continuations for constrained decoding**: tooling that changes no rule; the LL(1) grammar and the prefix output format keep room for it.

## Consequences

- The toolchain is the largest part of language 1, and the bootstrap is built around the query engine from its first version.
- Rules that an exact fix always repairs can be taught by their diagnostics instead of by the primer, which lowers the language's cost ([0013](0013-language-cost-and-scope.md)).
- Diagnostic codes, the idiom table and the exact fixes each rule names belong to a language version, so agents can depend on them.
- Fields and variants of a type deriving `Ord`, `Render` or `Generate` are not order-free for the merge driver, since their order carries meaning; concurrent additions to them conflict.
- The merge driver reports semantic conflicts only for interfaces; a behaviour change behind an unchanged interface surfaces through the check, the tests and behaviour epochs.
- `nan context` completeness is regression-tested from the first milestone, so any later feature that leaks meaning from outside the closure set fails a test.
- Primer cost: the tool card, about 0.7k of its 1k tokens; the core spends a few lines on reading a diagnostic.
