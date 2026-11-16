### A malformed multirange literal says what is wrong with it

A multirange literal is now read the way PostgreSQL's `multirange_in` reads
it, member by member in place, so every malformed one is a 22P02
`malformed multirange literal` naming the input as written, with the DETAIL
PostgreSQL gives: `Missing left brace.`, `Unexpected end of input.`,
`Expected range start.`, `Expected comma or end of multirange.` or
`Junk after closing right brace.`. A stray token where a member should be
(`'{[1,2),x}'`) is no longer reported as a malformed range literal.

#### Fixed

- Rust PG server: `split_members` is a port of `multirange_in`'s scanner,
  with its DETAILs. Fixes RP-878
  (tests/backlog/test_rp_found_rp510.py::test_multirange_literal_detail)
