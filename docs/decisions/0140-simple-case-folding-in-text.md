# 0140. Simple case folding in text

Status: accepted

## Context

Code comparing names without regard to case, such as NaN OS's FAT component over long names, could fold only ASCII. Comparing over all of Unicode needs Unicode's case folding, its data and a Unicode version, which no part of NaN named before.

## Decision

- **Simple folding.** `text::case::fold` maps a char by the mappings of status C and S of `CaseFolding.txt`, and `text::case::eq` compares two texts char by char under it. Each char folds to one char, so neither allocates and both run on every target.
- **Unicode 18.0.0.** The specification names the version, the latest at the time; moving to a later one is a change of std and of the specification together.
- **The data is NaN source.** `std/text/case/table.nan` holds the mappings as runs of chars folding with one offset, every char or every other one, in ascending order, built by a const; `fold` finds a char's run by binary search after an ASCII shortcut. It is written from `CaseFolding.txt` once per Unicode version.

Alternatives rejected:

- **Full folding (status F):** it maps some chars to up to three, so a comparison must buffer expansions, and `ß` then matches `ss`, which FAT's per-unit up-casing does not do.
- **A supplied operation:** the interpreter, both code generators and the NaN toolchain would each carry the table; as NaN source it is checked, interpreted and lowered as any std code.
- **A table per code point or per block:** some kilobytes against under four for the 219 runs, for a lookup that is already a few comparisons.

## Consequences

- Turkic mappings (status T) are not applied: `I` folds to `i` and `İ` and `ı` to themselves.
- A new Unicode version means regenerating the table and changing the version the specification names.
