# 0020. Edits from checker references

Status: accepted

## Context

`nan edit rename` and `nan edit move` rewrite every reference to an item or member across the workspace, doc references included, and write nothing when the result reports an error the workspace did not ([edits](../spec/33-toolchain.md#edits)). A reference through a value, such as a field access or a method call, is known only once its receiver is typed. The last-green record of each agent is written by `nan check`, which holds only the shared lock.

## Decision

- **Recorded member names.** Typing a body records, for each field access, member call, construction label and pattern naming a member, the token writing the name and the member it names. Path references are found by resolving each path of the workspace's items through the database; doc references by parsing the bracketed references of doc comments.
- **Edits as fixes.** A rename or move is a set of replacements applied like a fix ([0017](0017-exact-fixes-verified-by-re-checking.md)). Its check compares the errors after it with those before, positions moved through the replacements and, for a move, the lines of each relocated item mapped back to the lines they left.
- **Last-green record.** `.nan/green/<agent>` is replaced alone by renaming a new file of the writing process into place, not through the journal, whose single file concurrent checks would share.

Alternatives rejected:

- **Finding member references by name alone**: every field or method of that name on another type would be renamed.
- **Rewriting moved items in place and re-parsing**: positions inside a moved item would be lost, and errors it already reported would count as new.

## Consequences

- A new kind of member reference in the checker must record its name, or renaming that member misses it.
- Rename tombstones for items published before depend on the history, written once publishing exists; renamed fields and variants of types deriving `Encode` get theirs now.
