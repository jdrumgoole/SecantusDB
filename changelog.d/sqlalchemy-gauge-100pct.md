### The SQLAlchemy compliance gauge reaches 100% — every executed suite test passes

The final round on the SQLAlchemy dialect-compliance gauge closes the
residual tail: **731 of 731 executed suite tests pass, with zero failures
and zero errors**, up from 77.5% at the gauge's first landing. Nothing is
deselected; the only declared divergence is `datetime_microseconds`
(BSON datetimes are int64 milliseconds, and the shared dual-protocol
document store is the product), closed through the suite's own capability
mechanism — the same switch MySQL-family dialects close.

As before, the score is a by-product of real engine work. A FROM-less
`SELECT … WHERE EXISTS (…)` now routes through the constant path with the
subquery evaluated against real storage; parenthesized set-operation arms
carry their own ORDER BY and LIMIT; derived tables can be set operations,
`VALUES` lists with column aliases (the shape SQLAlchemy's insertmanyvalues
sentinel emits), or FROM-less selects; `INSERT` accepts any constant
expression in a VALUES cell (`nextval('seq')` included); and covering
indexes (`CREATE INDEX … INCLUDE (…)`) store their columns and reflect
through `pg_index`'s `indnkeyatts` split.

One fix in this round was a silent wrong-answer, the worst kind: a scalar
subquery ignored its ORDER BY and LIMIT, so `(SELECT id FROM t ORDER BY id
DESC LIMIT 1)` returned the *first* row in storage order instead of the
last. Ordered, limited, grouped, or joined scalar subqueries now run through
the full query engine, and a subquery returning more than one row raises
PG's `21000` instead of picking one arbitrarily.

#### Added

- `sql/engine.py` + `sql/planner.py` + `sql/executor.py`: FROM-less
  `WHERE EXISTS`; parenthesized union arms; set-operation / VALUES /
  FROM-less derived tables; constant expressions (incl. `nextval`) in
  INSERT VALUES cells; covering-index `INCLUDE` metadata + reflection
  (`pg_index.indnkeyatts`).
- `sqlalchemy_validation/requirements.py`: `supports_distinct_on` opened;
  `datetime_microseconds` closed with the BSON-millisecond rationale.

#### Fixed

- `sql/scalar.py`: a scalar subquery with ORDER BY / LIMIT / GROUP BY /
  joins silently ignored them (wrong row returned); it now runs through the
  engine, and >1 result row raises `21000`.
- `sql/engine.py`: extended-protocol Describe answered NoData for a set
  operation whose first arm was parenthesized, then Execute sent DataRows —
  a protocol violation that crashed libpq clients.
