# 0200. Syntax given by the worker tasks of the NaN toolchain

Status: accepted

## Context

After [0199](0199-head-reads-untraced-and-syntax-kept-by-the-bootstrap.md), a check of the NaN toolchain's workspace after a function-body edit still took the NaN toolchain, built by itself, 1.6 s on 4 threads at 2.1 GHz, against the [latency](../spec/33-toolchain.md#query-engine) requirement. Its command task did most of that work alone. The worker tasks only parsed the source files whose syntax the build cache lacked; the command task then read and hashed each file again, decoded its syntax and lowered its items, one file after another. The std workspace loaded only when the check first read it, in a second parallel phase in the middle of the check. Keeping the last-green record decoded every item's green entry twice and copied every line of the record into a text of its own.

## Decision

- **The worker tasks give each source file its syntax.** Parsing ahead decodes the syntax the build cache holds of each file, or parses the file and writes its syntax there. It also lowers the file's items and hashes its contents. A workspace file holds its syntax shared with the batch, and takes the batch's lowering and content hash when the load reads the same bytes the batch read.
- **Each worker task draws what it gives through its own allocator**, so that the tasks giving syntax carve from pools of their own instead of waiting on the posting task's pool lock.
- **The std files are posted to the worker tasks before the workspace loads**, so that their syntax is given in the same parallel phase as the workspace's; the std workspace takes it when first read.
- **The last-green record is kept as pieces of the text it is read from**, and a check tells whether the snapshot it restored from holds an item's green entry without decoding that entry.

## Consequences

- After a function-body edit, a check of the toolchain's workspace takes the self-built toolchain about 1.0 s instead of 1.6 s on 4 threads at 2.1 GHz.
- What a worker task gives stays charged to its budget until the command ends.
- A command that never reads a std file still has the worker tasks give the std files their syntax.
