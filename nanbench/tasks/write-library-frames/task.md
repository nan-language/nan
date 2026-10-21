kind: write
area: library

In the package `frames`, write `pub validator fn decode_frame(raw: Untrusted[Slice[u8]]) -> Frame fails FrameError`, which checks one received frame. A frame is a version byte, which must be 1; a flags byte, whose bit 0 marks the frame urgent; the payload length L as a big-endian `u16`; L payload bytes; then a checksum byte, the sum modulo 256 of every byte before it. A valid frame gives `Frame(urgent: ..., start: 4, length: L)`. The checks run in this order: fewer than 4 bytes fails with `Truncated`; a version v other than 1 with `BadVersion(v)`; fewer than L + 5 bytes with `Truncated`; more with `Trailing`; a wrong checksum with `BadChecksum`.
