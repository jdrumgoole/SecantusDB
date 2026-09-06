### Registering an enum, end to end

`EnumInfo.fetch` works now — psycopg's own enum-discovery API, unmodified — and
with it the whole slice of the driver that hangs off it: fetching a type's
labels, registering a Python enum against it, casting values in and out.

The query it sends is the reason this took a batch of its own: a `FROM`
subquery whose body is a `LEFT JOIN` of `pg_type` to `pg_enum`, with
`array_agg` and `GROUP BY` on the outside. So this adds a joined subquery as an
aggregate source, a two-table join (inner or left, one ON equality, an optional
filter and one ORDER BY — the shape every catalog query actually uses, nothing
more), the `pg_enum` virtual table, and `array_agg` — which keeps NULLs, the
way a left-join miss surfaces as `[None]` rather than `[]` and lets a client
tell a non-enum apart from an empty one.

Enum values round-trip in both wire formats: a label's binary form is just its
UTF-8, so binary cursors get the format they asked for, and an unspecified-oid
parameter carrying an enum label decodes as that label. An enum array reports
its own derived array oid rather than `varchar`, so a client that registered
the array type decodes it.

#### Added

- `EnumInfo.fetch`: a joined subquery as an aggregate source, `pg_enum`,
  `array_agg`, and a two-table `LEFT`/`INNER` join limited to the catalog
  query shape.
- Enum values in both wire formats, and enum arrays with their real oid.
