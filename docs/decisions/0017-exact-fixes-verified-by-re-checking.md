# 0017. Exact fixes verified by re-checking

Status: accepted

## Context

An [exact fix](../spec/34-diagnostics.md#exact-fixes) is offered only after an in-memory re-check of the workspace with the fix passes. Rules name fixes of many shapes: deletions, insertions at both ends of a construct, rewrites across files, alternatives tried in order, and renames of a declaration with its uses. `nan fix` applies the fixes of a pass together, and prefix checking adds the pending clauses that fixes write.

## Decision

- **Fixes as replacements.** A fix is a list of byte replacements in workspace files. A diagnostic lists the fixes its rule names with how they are offered: each that passes, the first that passes of alternatives, or one only when exactly one passes.
- **Re-check after reporting.** Rules attach candidate fixes when they report; the command then applies each to a copy of the workspace and keeps it when no diagnostic of the same code remains on its replacements or construct and every error is one reported before at the position the replacements move it to, or one the fixed diagnostic kept unchecked: anywhere for a declaration, else later on its line or item. Expected and found are derived from the first offered fix when the rule states neither.
- **Fixes needing the whole check.** A fix renaming a declaration with its uses, as `name.case` does, is attached once every package is checked, from the uses that checking recorded.
- **`nan fix`.** One pass applies the single fix of each diagnostic offering exactly one, skipping a fix sharing a line with an earlier one; `--propagate` repeats for the codes the specification lists.

Alternatives rejected:

- **Fixes as edited lines**: fixes touching one line twice, or two places of one line, could not be combined or mapped.
- **Trusting fixes without a re-check**: a fix the checker then rejects costs an agent a turn.

## Consequences

- Checking costs one re-check per candidate fix, which is acceptable for the scopes agents check.
- A rule adding a fix only states its replacements; the re-check decides whether it is offered.
