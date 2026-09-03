### Multiranges, and the difference between touching and overlapping

A multirange is a set of ranges, and PostgreSQL keeps it in one normal form:
members sorted, empty ones dropped, and any two that meet folded into a single
member. So `{[10,20),[1,5)}` comes back sorted, `{empty}` comes back as `{}`,
and `{[1,5),[3,8)}` comes back as `{[1,8)}`.

The interesting rule is the last one, and it is not quite "overlapping".
`{[1,5),[5,8)}` also collapses to `{[1,8)}` — the two do not overlap at all,
but nothing lies between them either, so they are one continuous stretch.
`{[1,5),[6,8)}` stays two members, because 5 is missing from it. The test is
whether the next member starts at or before the previous one ends, and at the
exact meeting point it comes down to the bounds: over a continuous type,
`[1.0,2.0)` and `[2.0,3.0)` join, while `[1.0,2.0)` and `(2.0,3.0)` do not,
because the second leaves 2.0 out.

Members are canonicalised before any of this happens, so `{[1,5]}` is stored as
`{[1,6)}` and merging sees the same bounds a client would.

All six multirange types are supported as literals, constructors, cast targets
and bound parameters, each with its own type oid.

#### Added

- `int4multirange`, `int8multirange`, `nummultirange`, `datemultirange`,
  `tsmultirange` and `tstzmultirange`, with sorting, empty-member removal,
  merging of overlapping and adjacent members, and their own type oids.
