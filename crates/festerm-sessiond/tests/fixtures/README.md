# Released recovery schema-2 fixtures

These are complete `FSD2` recovery preludes generated with the unmodified
`festerm-core` from v0.10.4, commit
`54b50769d23eb71551e54875ec1d9312741ffa56`, and bincode 1.3.3's fixed-width
encoding. They are not produced by the new migration decoder.

The `schema2_fixture_terminal` test helper records the exact input recipe:
8 x 3 cells, 4096-byte history, scrolling, title, bracketed paste, hyperlink,
and combining text. `schema2-primary.bin` ends inside a true-color CSI;
`schema2-alternate.bin` ends inside a UTF-8 character on the alternate screen.
Tests continue those fragments, resize, switch buffers, grow/evict history,
and re-encode/decode schema 3 without losing state or allocation accounting.

`schema2-queued-reply.bin` uses the common recipe without the unfinished CSI,
then ingests `ESC[6n`. It deliberately serializes the unsanitized terminal,
so migration must reject its queued reply before restoring allocations or
acknowledging adoption.

To regenerate, run those recipes against the pinned old core, serialize
`terminal.recovery_clone()` (the raw terminal for the invalid queued-reply
case) with `bincode::serialize`, and prefix `FSD2` plus the eight-byte
big-endian payload length. Do not regenerate with the current `Terminal`
layout: that would stop proving compatibility with the released writer.
