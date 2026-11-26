# 0234. Frames moved for what has gone

Status: accepted

## Context

A call or reply past 256 KiB over a bridge, and a message past 1 MiB up a link's first channel, move their pages as frames to the isolation that reads them, whose kernel keeps them under a token until the end or runner capability they moved for maps or reads them ([0225](0225-bridges-between-isolated-spaces.md), [0226](0226-moving-large-messages-off-a-links-first-channel.md)). Once that end closes or its job or instance ends, nothing can map or read them, yet they stayed until the isolation returned. NaN OS keeps an isolation for as long as its principal runs anything, so each reply left unread withdrew up to 1 GiB from its kernel for good. The frames an isolation draws from belong to the task running its space; one task keeps the frames moving into it, from any isolation; and an ended job's or instance's own task ends it.

## Decision

- **Moved frames follow the table.** The task keeping moved frames asks the isolation's table whether it still names the end or runner capability they moved for, as they arrive and when asked to give up what it no longer names.
- **What has gone gives its frames back.** The task running the space asks for them once it closes an end, and once a job or instance ends, that job's or instance's task, having taken back its entries, asks the task running the space to. Frames arriving for what the table no longer names go back to the writing side's isolation, and the move fails as one into an isolation that takes no more.

Alternatives rejected:

- **The writing side's kernel asking the table before moving:** a close between its question and the frames' arrival would keep them until the isolation returns.
- **The ended job's or instance's task giving the frames back:** it holds none of the isolation's frames, and an ask carrying frames could not be dropped with the asks of an isolation that takes no more.

## Consequences

- The writing side's kernel waits for the keeping task's answer before it gives the token of a move.
- A live end or instance that never maps or reads what moved for it still holds those frames.
- Supersedes 0225's consequence that frames moved but never mapped are kept until the isolation returns, and 0226's that what a space moves to its kernel without naming it takes frames of its isolation until that isolation returns.
