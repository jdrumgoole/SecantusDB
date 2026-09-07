### The Rust server leaked Rust type names into the bad-hint error

A hint that names no index came back with Rust's `Debug` formatting, so a
MongoDB client was shown Rust's own type names:

```
hint String("x") does not correspond to an existing index
hint Document({"nope": Int32(1)}) does not correspond to an existing index
```

`String(…)`, `Document(…)` and `Int32(…)` mean nothing to a client. Both now
name the value: `hint "x"` and `hint { nope: 1 }`. The string form leaked from
the command layer (`update` / `delete` / `findAndModify`) and the key-spec form
from the storage layer (every command that takes a hint).

The rest of this message is a **deliberate** difference and is unchanged: mongod
answers a bad hint with a multi-line planner diagnostic that names the whole
query plan, and this project reproduces the CODE and names the hint instead —
the wording moved between 6.0.16 and 8.2.11, so the gate asserts the rejection
rather than the text. That decision is why the divergence stays in the probe's
message-only column on **both** servers; what was not deliberate was leaking
Rust's internals into it.

#### Fixed

- `secantus-commands`: the bad-hint error renders the hint value, not
  `format!("{hint:?}")` on a `Bson`.
- `secantus-storage`: the key-spec branch of `resolve_hint` likewise.
