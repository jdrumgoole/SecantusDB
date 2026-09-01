### A join on a timestamp column returned rows that do not match

`… a JOIN b ON a.t = b.t` compared the stored dates, which hold whole
milliseconds, so every timestamp inside the same millisecond joined to every
other. Over four distinct times a self-join returned **ten rows where
PostgreSQL returns four** — a wrong answer, not a lost digit. Joining now
compares the microseconds too, and a joined timestamp comes back with the
microseconds it was stored with rather than rounded to the millisecond.

Two other timestamp routes are fixed with it, both found in the same sweep.
`string_agg(t::text, …)` rendered the rounded time, even though `t::text` on
its own was already exact. And a timestamp rendered as text padded the
fractional seconds to six digits — `00:00:00.123100` where PostgreSQL prints
`00:00:00.1231` — which also made `concat(t, '')` and `t::text` disagree with
each other.

#### Fixed

- A join on a timestamp column matches only equal times, and its result keeps
  microseconds.
- `string_agg(t::text, …)` keeps microseconds.
- A timestamp rendered as text prints PostgreSQL's shortest form, and every
  route that renders one (`::text`, `concat`, `||`) agrees.
