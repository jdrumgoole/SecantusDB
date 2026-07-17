### Range operators now order embedded documents, like mongod

`$gt` / `$gte` / `$lt` / `$lte` against an embedded-document bound returned
*nothing* on both the Python and Rust servers — `{a: {$gt: {x: 1}}}` matched
no documents at all — because Python's `operator.gt` raises `TypeError` on two
dicts (swallowed to a silent no-match) and the Rust matcher treated any
document operand as an unconditional no-match. mongod orders embedded
documents field-by-field (first differing key compares as a string, else
recurse into the value, else the shorter document sorts first), so those
queries should have matched. Found while triaging the driver-gauge results and
verified against a live mongod 7.0.12 probe; now three-way parity (Python ==
Rust == mongod). The type bracket is preserved — a document-valued field still
never matches a scalar bound, and vice versa.

#### Fixed

- Range operators order two embedded documents field-by-field (both servers)
  instead of matching nothing.
