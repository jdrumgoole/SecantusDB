### Reaching into a json document

`'{"a": 1}'::json ->> 'a'` was an error. So was every other way of getting at
part of a json value: the Rust PostgreSQL server could parse json, store it,
cast it and hand it back whole, and could not reach inside it.

All of the navigation and key operators work now — `->` and `->>` by name or by
array index, `#>` and `#>>` down a path, `?`, `?|` and `?&` for keys, and `@>` /
`<@` for containment. A negative index counts from the end, which is
PostgreSQL's rule and not most JSON libraries'; a lookup that does not apply — a
missing key, an index past the end, a name against an array — is SQL NULL rather
than an error, which is the whole reason these operators are usable; and `->>`
reads a json string without its quotes while a json *null* becomes a SQL NULL.

Containment compares by value rather than by text, so key order and whitespace
do not count, and neither does a number's scale: `{"a": 1.0}` contains
`{"a": 1}`.

A json value is carried here as its text, so by the time two operands reach the
evaluator there is nothing to tell `{"a": 1}` from any other string. The left
operand's static type is what makes these json operators at all — the same
mechanism that resolves a range parameter from the operand beside it.

#### Added

- `->`, `->>`, `#>`, `#>>`, `?`, `?|`, `?&`, `@>` and `<@` over `json` and
  `jsonb`, with their result types (`->` keeps the json flavour, `->>` is text,
  the key and containment tests are boolean).
