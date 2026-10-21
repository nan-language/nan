# 0045. The primer core within its cap

Status: accepted

## Context

Counted with the three tokenizers before the freeze, the primer's core held 9,125 tokens against its cap of 8,000 ([primer tiers](../spec/39-primer.md#primer-tiers)), though [0013](0013-language-cost-and-scope.md) estimated it at 7,700. Every topic stood far below its own cap, the largest at 1,622 of 3,000, and the tool card at 964 of 1,000. The core is what an agent writes correctly without a diagnostic. Rules that an exact fix repairs, and what a tool prints on demand, stay out of it.

## Decision

- **Encoding moves to the packages and trust topic**, where Encode tombstones and the decoding of open and closed types already were: `derives Encode`, decoders and the binary and JSON formats. The rule telling parsing validators from declassifying ones moves beside the seals it decides, and the interface facts consumers see of an open type beside dependencies.
- **The core drops what it states twice or what a tool supplies:** the table rows its sections restate, the list of item kinds and of trap kinds, the head layout beyond four parameters that `nan fmt` writes, niches, result locations, wide multiplication, and the example functions that restate their sections.
- **Predicates, property tests and stores keep a line each**, their rules staying in the contracts topic and the memory chapter.

## Consequences

- The core counts 7,960 tokens, so a new core concept needs a trade-in at once; the packages and trust topic counts 1,746 of 3,000.
- An agent writing a type that derives `Encode` or a declassifying validator reads the packages and trust topic, as one sealing an item does.
