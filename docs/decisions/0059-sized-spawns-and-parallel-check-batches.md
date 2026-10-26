# 0059. Sized spawns, and per-item check work in parallel batches

Status: accepted

## Context

The NaN toolchain checked on one executor thread whatever the processors: every query of a checker ran on the command's task, its dynamic state (the sink reported into, muted probes, the generic scope, the evaluations and searches under way, the item whose own work is traced) held in the checker itself. Most of a check is per-item work over memoised, order-free queries ([0053](0053-order-free-queries-and-greens-from-the-check.md)): the rules reading an item's NIR, its prover obligations among them, the items each item names and the last-green entries; and a cold load parses every source file. Two things stood in the way of running it on several tasks.

- A task a scope spawns gets the 64 KiB default stack, and `limits stack N` proves a bound no recursive body has. Only the first task of a domain could run on a stack of a chosen size ([0048](0048-sized-domain-runs-for-internal-errors.md)), and a domain per worker would pass every value a worker inserts into a shared memo to the command's domain, walking it.
- The checker's code calls no waiting primitive; spawning from it would make every function above it wait.

## Decision

- **`task::Scope.spawn_sized(child, stack)`** spawns as `spawn` does, the child's stack holding at least `stack` bytes. Every rule on `spawn` holds for it, lexical spawns included.
- **The crew.** `nan`'s command domain opens a scope and spawns one sized worker task per other processor of the affinity set, at most seven, each drawing on a cell of its own split from the command's allocator, so that workers allocate from pools of their own. Workers poll for posted batches, sleeping from one up to sixteen milliseconds while none is left; the scope is cancelled once the command ends. Code that posts never waits: it posts a batch, runs jobs of it itself, and spins until every job has ended.
- **Batches.** A batch is one kind of work over a list, one job per entry: the source files of a cold load whose syntax the build cache lacks, parsed into the cache; the items each item of a package names, after the package's signatures; the rules reading an item's NIR, after the consts and contributions of every package are evaluated; and the last-green entries. Recorded reads and comparisons of the prover with the proof cache keep every job on the posting task, and without workers loads parse as before.
- **The work under way is per task.** A checker handle is the shared state and a state of the work under way; each task helping a batch runs its jobs with one of its own. Memo computations of different tasks may race; the first insertion is kept, and order-free queries make the values equal. The type interner gives every racing task the first insertion's index.
- **Order.** A job logs its diagnostics, those of quiet packages and those of unused code apart, and the poster appends them in item order, so a batch logs what running its jobs in order logs; a job logging such a diagnostic for another item traps. Logs whose consumers read them as sets or by owner (members named, field reads and labels, authorities performed, traced reads, const needs) are appended at once, deduplicated by first claim; holes and instantiations, which only judgments log, trap when a job logs them for another judged item. Snapshot reads are written sorted. Long-lived results go into few large allocations, the green entries into one byte arena per task, since many small ones pin otherwise free chunks.

## Consequences

- Checking and cold loads use up to eight processors for their per-item work; on one processor the batches run on the command's task, giving what the sequential check gave.
- Outputs, green files and snapshots do not depend on the number of threads or the schedule. Memo races may compute a query twice.
- Every command runs a scope with its worker tasks, which sleep while no batch is posted.
- Each worker adds about a megabyte to a check's peak memory, and past one thread per core of a common machine workers hardly speed a check up, since judging stays sequential; seven keep a check of the NaN toolchain's workspace within the bootstrap's peak memory on any machine.
- Judging stays sequential: a judgment reads other items' rejections and evaluations while they are being decided.
