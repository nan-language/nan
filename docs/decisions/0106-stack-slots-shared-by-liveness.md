# 0106. Stack slots shared by liveness

Status: accepted

## Context

Lowering gives every value held in memory a stack slot of its own: each call's result, each copy and each binding. Cranelift 0.135 lays out every slot for the whole function, so a frame grows with the number of call sites: sixteen calls giving a 1 KiB value took a 32 KiB frame, and ordinary functions of NaN OS reached the 64 KiB task stack. Lowering builds values through 251 slot sites, many of them in supplied operations, so tracking lifetimes where slots are made would touch each of them. NaN has no references: a function sees an argument's memory only while the call lasts, unless it returns an address derived from it; a task closure lends places only to tasks that end before `task::scope` returns, and its environment is drawn from the heap. Support routines are the only code that registers addresses with the runtime. A lane value's code folds its loads into SSE operands, which need 16-byte alignment, while slots declared the lanes' own alignment; every slot was 16-aligned only because the sizes before it summed to multiples of 16.

## Decision

- **Share slots after building each function's IR**, in both code generators, so both give the same IR: slots share one slot when the addresses derived from theirs are never live at one point. A slot's group is the slots whose addresses meet in one call or one stored in the other's memory; a load from a group that holds addresses yields addresses of that group.
- **Keep a slot to itself when its address may outlive the values derived from it**: stored outside every slot, or given to a callee other than an instance, a glue function or a routine over byte ranges.
- **Align every slot of 16 bytes or more to 16**, so layouts that slot sharing changes keep lane values aligned.

Rejected alternatives:

- **Reusing slots at the end of each NIR statement**: would need the lifetime of each value at each of the slot sites, supplied operations included.
- **Sharing only slots of equal size**: keeps the alignment luck but leaves most temporaries of different types unshared.

## Consequences

- A frame grows with the values live at once: the frame of the sixteen calls takes about 3 KiB.
- An address passed to a support routine pins its slot; a routine that keeps none must be listed with the byte routines to let it be shared.
- A slot of 16 bytes or more costs up to 15 bytes of padding.
