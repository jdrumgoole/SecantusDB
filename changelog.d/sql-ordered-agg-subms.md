### `array_agg(t ORDER BY t)` returns the microseconds it stored

An ordered aggregate over a timestamp column sorted by microseconds and then
returned every element rounded to the millisecond — times that were never
stored. The sub-millisecond remainder was attached to the sort key but not to
the value being collected.

This was invisible from the shape that already worked: `array_agg(x ORDER BY
t)` aggregates a *different* column, so only the key mattered there. It only
appears when the column being collected is the timestamp itself.

#### Fixed

- `array_agg(t ORDER BY …)` and the ordered-aggregate path generally return
  microsecond-exact timestamps.
