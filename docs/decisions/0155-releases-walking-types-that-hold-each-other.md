# 0155. Releases walking types that hold each other

Status: superseded by 0161

## Context

The release lowering generates for a type is a chain of direct calls, one per type, each calling the release of the types of the parts it holds. For a type holding itself, such as a tree whose node holds a `mem::List` of trees, that chain reaches itself, so every function releasing such a value was `limit.stack-unbounded` on the machine targets, and the because-chain ended at the caller without naming the release. Kernel code could not own a recursive type at all: [0139](0139-schema-trees-as-node-lists.md) flattened `mem::wire::Schema` into a node list for this reason, and NaN OS's AML evaluator flattened its packages. A release must never fail, wait or trap, so whatever replaces the recursion cannot draw memory, and its stack use must not depend on the value.

## Decision

- **Types holding each other form a group, released by one walk.** The group of a type is the types its release reaches, through the fields, payloads and elements laid out inline and the values boxes, shared values, lists, sets, maps and hash maps hold, that reach it back. The release of each member calls one walk per group, which releases a value of any member depth first and calls no release of a member.
- **The walk keeps its place in the values it releases.** A frame, a value of a member being released, releases its other parts on entry and frees its empty vectors. It then enters the values its holders still hold, a vector's last element first, after writing into the holder a link to its own parent: a vector's allocator word, which its release no longer needs, or a box's or shared value's word, whose value is then the frame entered. When a frame holds nothing more, its parent's first holder still holding something is the one entered from; the walk reads the link back, frees the storage the frame came from when it was the last, marks a box or shared value done with the word 1, which no allocation, link or niche value is, and goes on. Extra space is the walk's own frame, whatever the value's depth.
- **A link is an address and a member index.** Frames of different members may hold each other, so leaving a frame needs its parent's type: the link holds the parent's address in its low 48 bits, sign-extended back, and its member's index above them. Every target's addresses are canonical 48-bit ones.
- **Other recursion is reported.** A type holding itself through another value, an arena, pool, once map, cursor, cown, channel or `Dyn` box, keeps the release of a part calling the release of the part's type, so the judge reports it. Release frames are named by their type and the call releasing a field or payload of a workspace type lies on its declaration, so the because-chain names the field holding the type and, in its `raised` reason, the type released again.

Alternatives rejected:

- **A work list drawn from an allocator:** a release would allocate and could fail on exhaustion.
- **Recursion bounded by depth:** values are as deep as their data.
- **Moving each child into a stack slot of the walk:** the slot vacated in the parent has the child's size, not the bookkeeping's, and a box's storage must be freed only after its contents are released.
- **One walk per member:** each would hold every member's code, quadratic in the group's size for the toolchain's syntax trees.
- **Iterative moves into other domains, encodings and decodings:** moves must leave the values intact, and codecs build or read values in order; neither is needed for kernels to own recursive types, and moving recursive captures stays reported as [0103](0103-domains-in-kernels.md) decided.

## Consequences

- Kernel code holds trees, lists of lists, maps of values holding maps and shared graphs in bounded stack; a release's stack use is its walk's frame plus the releases of the parts outside the group.
- Releasing a vector of a group type releases its elements last first.
- A link packs its index above 48 bits of address, so a target with wider addresses needs another encoding.
- Each member's release adds one call before the walk.
