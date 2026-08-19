### Multi-table joins filter each table before combining it

A query like `SELECT … FROM a, b, c WHERE a.x = 1 AND b.y = 2 AND c.z = 3` has no
join condition at all — it is a cross product with a filter on each table.
Postgres narrows each table first, so the combination is over a handful of rows.
SecantusDB was building the whole cross product and applying the filters at the
end, which made the work grow with the *product* of the table sizes rather than
with the answer: three 100-row tables meant a million intermediate rows for a
single-row result.

Each single-table condition now moves to the stage that produces its rows — the
base table's own conditions run before the first lookup, and a joined table's run
inside its lookup. A condition combining tables with `OR` moves too, as long as
every column it mentions belongs to the same table. On three tables with one
condition each, time was previously cubic in table size (a million-row product
took 2.5 seconds); it is now flat, and a 343-million-row product answers in
hundredths of a second.

Separately, a join written the way SQL test suites usually write it —
`WHERE a3 = b9`, with no table prefixes — was not being recognised as a join
condition at all, so those tables were combined exhaustively and filtered
afterwards. Unqualified columns are now resolved to the table that declares them
(only when the name is unambiguous), so the join is performed as a join.

One case is deliberately left alone: a condition on the right-hand table of a
`LEFT JOIN` is *not* moved into the lookup. `WHERE` is applied after the join, so
such a condition has to remove the unmatched row entirely; filtering earlier
would leave it in the result with empty columns. Every shape here was checked
against a real PostgreSQL 14, including that trap and its `ON`-clause
counterpart.

#### Changed

- A multi-table join filters each table as it enters the pipeline instead of
  after the full cross product.
- Unqualified equality conditions between tables are recognised as join keys.
