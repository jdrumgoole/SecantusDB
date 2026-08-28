### Merging two jsonb objects with `||` produces JSON again

`'{"x":1}'::jsonb || '{"y":2}'::jsonb` returned `{'x': 1}{'y': 2}` — the two
values rendered as Python dictionaries and glued together as text. Single
quotes, no merge, and not valid JSON. Nothing errored; the wrong value simply
came back, and any code parsing the result failed somewhere further along.

Two objects now merge, with the right-hand operand winning on conflicting keys,
as PostgreSQL does.

Concatenations where either side is an array or a scalar are unchanged and
still differ from PostgreSQL in how the result is *typed* — the values are
right but render as an array literal rather than JSON. That is tracked
separately.

#### Fixed

- `jsonb || jsonb` merges two objects instead of concatenating their Python
  representations as text.
