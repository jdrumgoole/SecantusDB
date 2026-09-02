### The jsonb function family: four no-ops and a family of wrong renderings

`jsonb_set('{"a":1}','{b}','2')` returned its input **unchanged**. So did
`jsonb_strip_nulls`. Both are implemented — they only ever worked when the
argument carried an explicit `::jsonb` cast. A bare `'{"a":1}'` literal is
PostgreSQL's `unknown`, and there the function's declared parameter type
resolves it; here it stayed a Python `str`, the navigation had nothing to walk,
and the call returned the input. A no-op that looks like a success.

#### Fixed

- `jsonb_set`, `jsonb_insert`, `jsonb_strip_nulls`, `jsonb_typeof`,
  `jsonb_pretty` and `jsonb_array_length` coerce an untyped string argument, as
  PostgreSQL's parameter types do.
- `jsonb_build_array(1,'x',true)::text` renders `[1, "x", true]` rather than
  the PostgreSQL array `{1,x,t}`, and `to_jsonb('x'::text)::text` renders
  `"x"`. Their values are ordinary Python lists, dicts and strings, so only the
  **call** says the rendering should be JSON. For `json_agg` the call is no
  longer visible by the time the cast runs — its operand is a synthetic column
  — so the planner marks the cast instead.
- `jsonb_object_keys` yields PostgreSQL's storage order (shorter keys first,
  then bytewise). `json_object_keys` keeps the input's own order and was right.
- `jsonb_typeof(v->'arr')` was `0A000 unsupported scalar expression`: inside a
  function call, `v -> 'arr'` looks like an arrow-**lambda** to sqlglot's
  parser, and only reaches `JSONExtract` when the left side is something an
  identifier cannot be. PostgreSQL has no lambda syntax, so a lambda there is
  always that misparse.
- `jsonb_array_length(NULL)` is NULL, not an error.
- `to_char`'s ISO-week tokens `IYYY` / `IW` / `ID` — `'IYYY-IW-ID'` came out as
  the literal `I20Y-IW-I3`, the lone `Y` and `D` having matched and the `I`s
  not. `IYY` / `IY` / `I` and `IDDD` have no strftime directive and are
  recorded rather than guessed at.
- A `json_agg` inside a computed projection is typed `json` again. It had been
  typed by its ELEMENT since the nested-`array_agg` fix, which those two share
  a registrar with.

#### Still divergent

`string_agg(DISTINCT x, sep)` and `jsonb_agg(...)` inside a computed
projection — sqlglot models the latter as an anonymous call, which the
aggregate collector does not look for.
