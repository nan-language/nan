# 0021. NIR, interpreter and prover after checking

Status: accepted; its natives for std superseded by 0022

## Context

Check time discharges obligations with the [prover](../spec/20-prover.md), runs const evaluation and examples in the [interpreter](../spec/21-interpreter.md), and applies rules whose facts come from them: may-trap, proofs demanded by limits and transparency, vacuity, refinement of requires, `where` conditions of instantiations and registry tables. Checking itself reads const values: two instantiations are one type when their const arguments are equal values, and constant patterns decide disjointness and exhaustiveness. Both read [NIR](../spec/19-nir.md), which only an item without errors has. Which items have errors is known once every package is checked, while reading commands such as `nan show`, `nan context` and `nan explain` check only the items they read. Std is declarations until it is written in NaN, so its functions have no NIR.

## Decision

- **A pass after checking.** Elaboration, the interpreter and the prover run once every package is checked and the items holding an error line are known; the rules needing them run in that pass, item by item, then per judging root and per instantiation. Before every package is checked, an item whose NIR is needed is judged alone: it is checked once, and its errors and syntax errors decide.
- **Const values on demand.** Checking that reads a const's value runs its const evaluation then, each item the run executes being judged alone first. An evaluation that needs a value still being computed leaves every const in that cycle without a value; the cycles are reported from the needs the evaluations recorded, once every const is evaluated.
- **NIR in memory.** Each unit is elaborated on demand and memoized for the run. The text notation exists only for tests comparing units with the specification's; the canonical encoding waits for artefacts.
- **Natives for std.** The interpreter supplies the std operations without a body that the specification's examples run, and a run reaching another ends as missing. The contracts that chapters of the specification state in prose are restated, with the declarations they complete, in `stdlib/declarations.nan`.
- **May-trap apart from the other digest facts.** It depends on the statuses of a function's obligations, which depend on the purity of its callees' digests, so it is its own query over NIR, shared by the members of a cycle.
- **Interpreted terms where no sum stands for them.** An interpreted arithmetic term that is an ite's arm or an application's operand gets the equality with the sum it interprets, and a field step or tag of an ite term is the ite of its arms', so that propagation relates them to their operands.

Alternatives rejected:

- **Elaborating every item while checking**: an item would be elaborated before its errors are known, and a failed construct has no translation.
- **Const cycles from the names initializers reach**: types and patterns read const values while checking, so a cycle through them would leave a const without a value and without a diagnostic.
- **Reading commands checking the whole workspace**: their cost would grow with the workspace instead of with what they read.
- **Computing may-trap inside the digest**: every purity question would run the prover, and a cycle would ask for its own statuses.
- **Natives for every std operation**: the operations of std's library code get NaN bodies, which would leave those natives dead.

## Consequences

- A rule that needs NIR, an interpreter run or a status runs in the pass after checking, never while an item is checked, except the reads of const values.
- Context packs include what that pass reads, so `nan context` runs it for the item it prints.
- Each std contract stated in prose and not in a `nan-api` block must be restated in `declarations.nan` for the prover to know it.
- An example or const reaching a std operation the interpreter lacks is not judged.
