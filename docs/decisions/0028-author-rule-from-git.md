# 0028. The author rule reads git

Status: accepted

## Context

In a VCS workspace the [author rule](../spec/31-trust.md#author-rule) and [acceptances](../spec/31-trust.md#acceptances) read, for changes of the git history, item hashes, specs, seals and lock lines, and the author of each change. Item hashes and specs exist only once the workspace of a change is checked.

## Decision

- The toolchain runs the `git` program: `rev-parse` and `config` to find the working tree and the uncommitted author, `rev-list --parents` limited to the workspace's directory for the history, `ls-tree` and `cat-file --batch` for a commit's files, and `log` for authors. It links no git library.
- The history walked is the one git simplifies to the commits changing the workspace's directory, the uncommitted state's parent being the most recent of them; a commit changing no file of the workspace holds the state of its nearest such ancestor.
- Each change's machine records and checked facts are read once per command: the checked ones by checking the commit's files in memory, its locks reading releases from the local store.
- Whether an accepted spec keeps an earlier one is judged from the two specs' texts, as `nan api update` judges weakening, so the acceptances of past changes read their API snapshots without checking them.

## Consequences

- The VCS rules need `git` on the path; outside a git working tree they do not apply.
- The cost of judging a seal grows with the commits back to the change introducing each hash it records.
- A commit's facts are checked again by each command that reads them.
