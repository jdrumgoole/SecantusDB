### An upsert seeds every equality the query implies, not just the bare ones

When an upsert finds no match, mongod builds the new document from the QUERY —
and it reads more than bare equality. Both servers seeded only bare equality, so
five query forms lost their fields entirely: a silently wrong **insert**, since
the document mongod would have written is missing a field.

Measured against mongod 8.2.11 over 20 queries × 6 updates: **24 of 120
divergent**, now 0.

#### Fixed

| query | mongod seeds | was |
| --- | --- | --- |
| `{a: {$eq: 1}}` | `a: 1` | nothing |
| `{a: {$in: [1]}}` (one element) | `a: 1` | nothing |
| `{a: {$all: [1]}}` (one element) | `a: 1` | nothing |
| `{$and: [{a: 1}, {b: 2}]}` | `a: 1, b: 2` | nothing |
| `{$or: [{a: 1}]}` (one branch) | `a: 1` | nothing |

A longer `$in`, a two-branch `$or`, `$nor`, and the range / `$exists` / `$type`
/ `$not` / `$elemMatch` operators seed nothing on mongod either, and still seed
nothing here.

- **Two clauses implying the same path are an error**, not a silent pick:
  `{$all: [1, 2]}` and `{$and: [{a: 1}, {a: 1}]}` are both
  `54 cannot infer query fields to set, path 'a' is matched twice`.

#### Deliberately not reproduced

mongod emits the seeded fields in its own **hash-table order** — `{a: 1, b: 2}`
gives `b, a`, `{aa: 1, ab: 2}` gives `aa, ab`, `{one, two, three}` gives
`three, one, two`. It ignores the query's own order and is neither sorted nor
reversed, so it is an implementation detail rather than a contract; CLAUDE.md
already records that it *changed* between 6.0.16 (sorted) and newer servers.
Both servers keep emitting the seeded fields sorted, and the tests compare
field/value pairs rather than order.
