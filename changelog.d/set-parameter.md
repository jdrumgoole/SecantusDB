### `setParameter` works, and refuses the way mongod refuses

Both servers reported their parameters through `getParameter` and had no way to
change one: `setParameter` answered `59 CommandNotFound`. It is implemented now,
matching mongod 8.2.11 on every outcome for the parameters SecantusDB registers
— the previous value in `was`, and four distinct refusals that are easy to
conflate. A parameter that *exists* but is startup-only is `20
IllegalOperation`, not the `72 InvalidOptions` an unknown name gets; answering
72 there would claim the parameter does not exist while `getParameter` was
still reporting it.

The coercion rules are more permissive than their types suggest, and every one
below was measured rather than inferred. `logLevel` accepts any number or bool,
truncates toward zero (`1.9` → `1`), clamps at 5 (`99` → `5`), and refuses only
a negative that does not truncate to zero. A boolean parameter accepts *every*
BSON type and stores its truthiness — `null`, `0` and `0.0` are false, while an
empty string, an array and a document are all true.

A parameter SecantusDB does not implement is refused rather than accepted and
ignored. mongod's `ingressConnectionEstablishment*` family tunes a connection
rate limiter this server has no equivalent of; accepting those names would let a
client ask for rate limiting and silently get none.

#### Added
- `setParameter` on both servers, with `getParameter` reporting whatever was last set. The store is server-wide, so a value set on one connection is visible on every other.
- A `setParameter` privilege action, granted to `clusterAdmin` as mongod grants it to `hostManager`.

#### Fixed
- `getParameter` and `setParameter` can no longer disagree: the getter overlays the changed values onto its defaults instead of keeping a second copy.
