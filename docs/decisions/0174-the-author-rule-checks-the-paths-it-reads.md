# 0174. The author rule checks past states for the paths it reads

Status: accepted

## Context

In a VCS workspace the [author rule](../spec/31-trust.md#author-rule) and [acceptances](../spec/31-trust.md#acceptances) read, for changes of the git history, item hashes, specs, seals and lock lines, and the author of each change. Item hashes and specs exist only once the workspace of a change is checked. [0032](0032-author-rule-reads-git.md) checked every item of each change read, though the rules read only the sealed items and what their seals record: on the NaN toolchain's workspace, whose foreign functions are sealed, `nan verify` checked 36 past states at about 10 s each, 388 s of its 7.8 minutes.

## Decision

- The toolchain runs the `git` program: `rev-parse` and `config` to find the working tree and the uncommitted author, `rev-list --parents` limited to the workspace's directory for the history, `ls-tree` and `cat-file --batch` for a commit's files, and `log` for authors. It links no git library.
- The history walked is the one git simplifies to the commits changing the workspace's directory, the uncommitted state's parent being the most recent of them; a commit changing no file of the workspace holds the state of its nearest such ancestor.
- Each change's machine records are read once per command, and its checked facts by checking the commit's files in memory, its locks reading releases from the local store.
- **A change's checked facts are those of the canonical paths the rules ask about.** The gate wants the items holding trust escapes and the paths their seals' closure entries name before it walks, then the items of judged acceptances; a check of a change gives the facts of every path wanted when it runs, and a path wanted later checks the change again.
- An accepted spec keeps an earlier one when their texts show it, and otherwise when [judged functions](0031-judged-functions.md) over the current workspace's item show the earlier spec entailed, so the acceptances of past changes read their API snapshots without checking them.

## Consequences

- The VCS rules need `git` on the path; outside a git working tree they do not apply.
- The cost of judging a seal grows with the commits back to the change introducing each hash it records, each commit costing the items the rules ask about: in this repository, `nan verify` over `std/`, whose trust escapes are sealed, took 3.9 minutes instead of 15.3.
- A commit's facts are checked again by each command that reads them, and within a command when an acceptance asks for an item no seal led to.
- An acceptance whose specs differ in text checks the current workspace again.
