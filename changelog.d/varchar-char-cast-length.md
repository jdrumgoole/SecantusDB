### Length-qualified character casts truncate (and char pads)

A cast to a length-qualified character type now applies the declared length,
matching PostgreSQL. `'bar'::varchar(2)` yields `ba` (was `bar` — the length
modifier was parsed but silently ignored), and `'a'::char(4)` yields `a   `
(blank-padded to width). Bare `text` / `varchar` still impose no limit.

#### Fixed

- `scalar.py`: `varchar(n)` / `char(n)` casts truncate the value to the declared
  length (`char(n)` also right-pads with spaces); previously the length was
  never applied to the cast value.
