kind: read
area: driver

Read the package `serial`, then answer each question with an answer block holding the answer alone.

- q1: What does `LineControl.framing(9, parity: Parity.Even).to_bits()` give? Answer in decimal.
- q2: What is the smallest value of `data_bits` for which `LineControl.framing` traps?
- q3: Does `LineControl.checked_from_bits(0x0102)` give `Some` or `None`?
- q4: Is `LineControl` an AnyBits type? Answer `yes` or `no`.
