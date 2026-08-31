### Every command that takes a `hint` now resolves it, on both servers

A hint tells MongoDB which index to use, and MongoDB refuses a command whose
hint names no index rather than quietly scanning instead. SecantusDB got that
right for six of the seven commands that accept one. `distinct` was the
exception: it accepted the field and then ignored it, so a typo'd index name
returned a full result set where a real server returns an error.

Probing that gap across all seven commands turned up three more divergences,
two of them on the Rust server only — the kind that a Python-versus-Rust parity
test cannot see, because both servers were consistent with each other and both
were wrong. One of them returned **wrong data rather than an error**: the Rust
server ignored the direction in `{$natural: -1}` and walked the collection
forwards.

The hint surface now matches mongod 8.2.11 exactly on all eleven shapes
probed, on the Python server, the Rust server, and a real `mongod`
side-by-side.

#### Fixed

- `distinct` resolves its `hint` like every other read. A valid index name or
  key spec is honoured (including a sparse index's reduced document set); one
  naming no index answers `BadValue` (code 2) instead of returning every value.
- The `$natural` **string** is rejected on both servers. MongoDB accepts
  `$natural` only in the document form, and `findAndModify` already enforced
  that here while `find` did not.
- **Rust server:** `{$natural: -1}` walks the collection backwards. It
  previously resolved to the same token as `{$natural: 1}` and returned forward
  insertion order — the Python server had fixed this and the port had not.
- **Rust server:** an unresolvable `hint` on `delete` / `update` is a
  per-statement `writeErrors` entry with `ok: 1`, matching mongod, instead of
  failing the whole batch command.

#### Changed

- **`hint="$natural"` is now an error.** This is a behaviour change for anyone
  passing the string, including `pymongo`'s `.hint("$natural")` — but it is the
  same error a real MongoDB server gives, which is the point. Use
  `.hint([("$natural", 1)])` or `hint={"$natural": 1}`; both work against
  SecantusDB and MongoDB alike. `docs/indexes.md` and
  `docs/feature-comparison.md` record the document form.
