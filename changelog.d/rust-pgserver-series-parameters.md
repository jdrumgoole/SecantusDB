### A series whose bounds are parameters

`select * from generate_series(1, %s)` — the way clients actually write a series
— was refused outright by the Rust PostgreSQL server. A bound parameter with no
declared type arrives as text, and PostgreSQL resolves it against the function's
own signature; this server saw text where it wanted an integer and said the
feature was unsupported. Every parameterised series failed, which is most of
them, and an explicit `%s::int4` failed too.

A NULL bound is now an empty result rather than an error, matching PostgreSQL,
where a series with any NULL bound produces no rows at all.

The probe that established those two also caught the server being *more*
permissive than PostgreSQL in one place: `generate_series(1, 3::float8)` has no
matching overload there and is refused with `42883`, while this server truncated
the bound to an integer and answered rows. Truncating an argument a real server
rejects is a wrong answer, so it is now the same refusal.

#### Added

- Series bounds given as parameters, in every position, including a NULL bound.

#### Fixed

- `generate_series` with a `float8` bound truncated it instead of refusing.
