# 0246. Settlement waits for its domains' task counts

Status: accepted

## Context

An ending task leaves the list of live tasks before it switches away for the last time; its executor thread then returns its stack, counts it out of its domain and runs its done glue. Settlement stops and frees only the tasks on that list, so a task ending as its domain, or an enclosing one, was abandoned could count out into a freed domain record, and its done glue could wake a joiner, a scope's or race's waiter or a domain's waiter that settlement had already stopped and freed. A done glue can also hand the task's record to another task, which frees it, or free it itself, so the record cannot stay on the list until its glue has run.

## Decision

- **The count-out is the last thing done for a task.** The executor thread reads the task's domain before the done glue and counts the task out once the glue has returned, so a domain's task count reaches zero only once no thread still runs the end of one of its tasks. A domain's first task has no done glue: its domain's waiter wakes at that count-out.
- **Settlement waits for the counts.** It counts out the tasks it stops itself, the new, parked and ready ones, which no thread switches away from as stopped. Once every listed task has stopped, it waits until the abandoned domain and each domain nested in it have no task left to count out, reading each count under its domain's lock, before it withdraws, releases or frees anything.

Rejected alternatives:

- **Keeping an ending task on the list until its count-out:** the done glue comes after the count-out and can free the record, so settlement would still miss the glue, which is what reaches the records it frees.

## Consequences

- A domain record, its nested domains' records and the records its tasks' done glues reach outlive every count-out into them.
- Settlement spins on the counts of an abandoned subtree as it spins on its stopping tasks.
