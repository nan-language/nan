---
name: resolve-issue
description: Fix, resolve or triage an issue of nan-language/nan given by number or URL; judge whether it stands and close it with a comment when it does not, otherwise choose the fix that suits NaN rather than the one the issue proposes, make it and close the issue
---

An issue is a report, not a decision: what it diagnoses, expects and proposes are claims to check against the code and the specification.

1. Read it: `gh issue view <N>`, then `gh issue view <N> --comments` when it has comments.
2. Fetch, then search the issues, open and closed, and the log of `origin/main` since the version it names, for an earlier report of it or its fix.
3. Reproduce it on `origin/main`, in a worktree when the checkout holds other work, with a bootstrap built from it, or with the NaN toolchain the bootstrap builds (`nan build nan` in `toolchain/`) when the bootstrap does not show it. For a bug, write the workspace of its `nan` block in a scratch directory, each file at the path of its `// file:` line, and run its commands; for a feature request, write the code it says cannot be written.
4. Judge whether it stands. It does not when it is:
   - a duplicate: add to the other issue anything only this one shows;
   - already fixed: name the commit that fixed it. When no test would catch the defect's return, adding one is its fix: go on at step 7;
   - not a defect: it does not reproduce, or NaN does what the specification says and that rule is right;
   - already served: something NaN has meets the need; show it as code that `nan check` accepts;
   - unneeded: no program needs it, or what it would add to the language, std or toolchain outweighs what it serves; the effort of making it does not count.

   Close an issue that does not stand with a comment giving the evidence (the specification's chapter and section, a command and its output, a commit), and stop. Write the comment to a file, since backticks in a quoted argument run as commands: `gh issue close <N> --comment "$(cat <file>)"` with `--duplicate-of <M>`, `--reason completed` when already fixed, or `--reason "not planned"`. When only part of it stands, go on with that part.
5. Find the cause in the code. What the issue says of it is a lead, not a finding. Look for the same cause elsewhere (other commands, diagnostics, targets or std packages): the fix covers those too, while a defect of another cause is an issue of its own.
6. Choose the fix that suits NaN, which need not be the issue's. Weigh its proposal against the other ways: fixing the cause where it arises rather than the symptom where it shows, reusing a mechanism NaN already has, or changing the specification when its rule is what fails. Among those that remove the cause for every case sharing it, take the one that adds the least language, API, code and run-time cost and handles the problem as the codebase already handles its kind.
7. Make it, with a test that fails without it: a specification example when the specification states the behaviour, as the self-check also runs it on the NaN toolchain, otherwise the narrowest unit test, std test or program. Run the issue's reproduction again: it now gives what the fix intends.
8. Once the user approves the commit, as AGENTS.md requires, commit each issue's fix alone, its subject ending in `(fixes #N)` so that the issue closes when the commit reaches `main`; a fix in several commits ends the earlier ones with `(#N)`.
9. Once the fix is on `main`, comment on the issue, without waiting to be asked, whenever the fix differs from what the issue proposes or expects (another API, name, mechanism or behaviour) or leaves part of it: say how and why, and how to use what was made instead. `gh issue comment <N> --body-file <file>`.
