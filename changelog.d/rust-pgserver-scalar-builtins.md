### Scalar functions, of which the Rust PostgreSQL server had none

`upper`, `length`, `abs`, `round`, `coalesce` — none of these worked. Not a gap
in a corner: the Rust PostgreSQL server had no scalar function table at all, so
every built-in taking an argument answered "not supported yet". A survey of
thirty-seven common ones found thirty-seven missing.

They are here now, along with `COALESCE`, `NULLIF`, `GREATEST` and `LEAST`,
which a user writes like functions but which arrive as their own kinds of
expression and so needed handling of their own.

The result *type* turns out to be as much of the answer as the value, and it is
where the surprises live. `sign` answers a floating-point number even when given
an integer. `div` answers an exact numeric, because that is the type it is
defined on, not the integer its arithmetic suggests. `round` splits by argument
type exactly as casting does — half away from zero for an exact numeric, half to
even for a floating-point one — so `round(2.5)` is 3 and `round(2.5::float8)`
is 2. And a handful of functions ignore NULL arguments rather than propagating
them: `concat` skips them, and `greatest` and `least` pick the extreme of
whatever is left, so `greatest(1, NULL)` is 1.

`nullif` deserves its own note. It answers the type of its left argument even
when the answer itself is NULL — and a NULL cannot tell you what type it is, so
reading the type from the value gave `text` where PostgreSQL gives `integer`. A
literal carries its type in the query, and that is where it is now read from.

#### Added

- Forty-odd scalar built-ins: string, numeric, and the conditional expressions.

#### Fixed

- A literal's type was inferred from its value, so an expression producing NULL
  reported `text` regardless of what it was computed from.
