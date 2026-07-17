### $all against a scalar field now matches, like mongod

The `$all` array query operator silently missed documents whose field held a
*scalar* value rather than an array — `{tags: {$all: ["red"]}}` matched
`{tags: ["red", ...]}` but not `{tags: "red"}`, on both the Python and Rust
servers. mongod treats a scalar field like a one-element array for `$all`
(equality and regex elements alike), so those documents should have matched.
This dual-server correctness bug was found while triaging the driver-gauge
results and is verified fixed against a live mongod 7.0.12 probe (three-way:
Python == Rust == mongod). In the same fix, `$all: []` now correctly matches
nothing (it previously matched every array-valued document), and `$elemMatch`
clauses inside `$all` still correctly require an actual array.

#### Fixed

- `$all` matches a scalar field value like a one-element array (both servers).
- `$all: []` matches nothing rather than every array-valued document.
