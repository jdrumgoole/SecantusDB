### An error in a constant expression says so — `Failed to optimize pipeline`, not `Executor error`

mongod carries an expression error under one of two prefixes, chosen by **when**
it failed. A **constant** expression is folded at optimization time and reports
`Failed to optimize pipeline :: caused by ::`; one that reads the document fails
per document under `Executor error during aggregate command on namespace: … ::
caused by ::`.

Both servers always used the executor form. That was **618 of the Python
server's message-only differences and 148 of the Rust server's** — the single
largest item left in the expression sweep, and a plan entry deferred three times
as "modelling constant folding".

#### The rule, measured

It is statically decidable, which is what made this cheap. Probed on mongod
8.2.11, the predicate is simply *does the expression read the document*:

| folds | does not fold |
|---|---|
| literals, `$literal` | a field path (`$s`) |
| `$$NOW`, `$$CLUSTER_TIME` | `$$ROOT`, `$$CURRENT` |
| the command's own `let` values | a variable bound from the input (`$$this` in `$map`) |
| `$let` whose bindings are constant | `$let` bound from a field |
| nested constants | `$rand` |

#### Fixed

Both servers now fold constant sub-expressions before running the pipeline —
which is what mongod's optimizer does — and report an error in one under its own
prefix. Python's message-only differences fall **669 → 143**, and the Rust
server's **148 → 4**.

The predicate is conservative: anything unrecognised counts as non-constant,
which keeps the previous behaviour rather than risking the wrong prefix.

Two stages are deliberately not folded, both found by the differential gate:

- **`$switch`** folds to a *different error* than it raises at execution — 40069
  `Cannot execute a switch statement where all the cases evaluate to false
  without a default`, not 40066 — and a dedicated path already models that.
  Folding it here reported the execution-time error under the optimization-time
  prefix, which is neither answer.
- **`$redact`**, whose decision variables are bound by the stage itself with
  marker values the folding evaluator does not hold; it reads the document
  anyway, so there is nothing to fold.

#### What is left of the message-only residue

The remaining 143 on Python are a different family: **number rendering inside
the message**. mongod prints `1.09951e+12` where we print `1099511627776`, and
`0` where we print `0.0`. Recorded.

17 cases added to `tests/test_mongod_differential.py`, each a pair — the same
error reached both ways, so the two prefixes are pinned against each other.
