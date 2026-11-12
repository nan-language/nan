# 0182. Hash index probes that branch

Status: accepted

## Context

After [0181](0181-scalar-map-keys-folded-in-place.md), a hash map lookup ran 94 instructions, most of them in its probe loop. Each probe computed whether its slot was vacant, whether it held the key's fragment and their combination as flags, chose between comparing the entry's key and a constant, and combined the outcome with the vacancy into the loop's condition: 22 instructions per slot, with five callee-saved registers kept for them.

## Decision

- **A probe branches**: a vacant slot ends the search unfound, a slot holding another fragment goes on to the next slot, and only a slot holding the key's fragment compares its entry's key, ending the search found when it matches.
- An unfound search gives index 0.

## Consequences

- A probe runs about half the instructions it did. The hash map program runs 19% fewer instructions, and 1.35 times Rust's time instead of 1.68.
- A key whose fragment is 0 needs no test against vacant slots, since vacancy is tested first.
