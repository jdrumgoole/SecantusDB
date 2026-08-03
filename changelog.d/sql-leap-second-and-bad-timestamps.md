### Leap seconds are accepted, and a bad timestamp says what is wrong

`'2015-06-30 23:59:60'` — a real leap second, and a value Postgres accepts by
rolling it forward to the next minute — crashed with an internal error, because
Python has no room for a second numbered 60. It now rolls forward the same way,
carrying across the minute, day and year boundaries.

The same path had a wider problem: *any* timestamp that could not be parsed
reached the client as an internal error rather than saying so. Even
`'not-a-date'` did. Unparseable timestamps now report invalid input syntax,
naming the value, and the out-of-range near-misses Postgres also rejects —
`23:59:61`, or a fractional leap second like `23:59:60.5` — are among them.

#### Fixed

- A `:60` leap second in a timestamp literal no longer fails with an internal
  error.
- An unparseable timestamp reports `invalid input syntax` instead of an
  internal error.
