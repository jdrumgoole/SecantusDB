### information_schema and the catalog views on the Rust PostgreSQL server

`information_schema.columns` — the single most-read relation in any
PostgreSQL deployment, because every ORM, migration tool and `\d` starts
there — did not exist. Nor did `.tables`, `.table_constraints`,
`.key_column_usage`, `.sequences`, `pg_class`, `pg_namespace`, `pg_index`,
`pg_indexes` or `pg_attrdef`; `pg_attribute` existed but listed only composite
types, so `attrelid = 't'::regclass` found nothing for a table.

Against a live PostgreSQL 14.13 the catalog corpus went from 19 divergences
of 22 to 4, and the sequences corpus finished at 0. A second corpus of 34
lines written for this change is clean.

Two findings worth stating:

**Seven catalog functions already worked — but only as a bare select-list
target, and the two halves have to be kept apart.** Three of them
(`current_database`, `current_catalog`, `current_setting`) must still DEFER to
the connection when they stand alone: the server's value is the live one, and
`current_setting` has to see a `set_config` from earlier in the session.
Making the expression form work by adding them to the scalar evaluator's name
list silently broke that — the bare-target gate matched them first and folded
them, which a planner unit test with no session installed saw as
"unrecognized configuration parameter" for a GUC that exists. CI's `rust` job
caught it; a test now pins both halves together.

**The seven:** `version()`, `current_schema()`, `current_database()`,
`current_setting()`, `format_type()`, `obj_description()` and `pg_get_expr()`
become a value the server resolves when they stand alone. Reached inside an
EXPRESSION — `version() LIKE 'PostgreSQL%'`, `current_setting('x') ~ '...'`,
`obj_description(oid) IS NULL` — the constant evaluator handled them instead
and had nowhere to ask, so every one answered `0A000`. Which is precisely how
a client writes them.

**`information_schema`'s views are called `tables`, `columns` and
`sequences`** — names a user table may perfectly well have, and a virtual
relation wins over the catalog. Registering them bare would have made a
user's own `columns` table unreachable, so they keep their schema in the name
and both resolve.

#### Added

- `information_schema.columns` / `.tables` / `.table_constraints` /
  `.key_column_usage` / `.sequences`, and `pg_class` / `pg_namespace` /
  `pg_index` / `pg_indexes` / `pg_attrdef`.
- The catalog functions reachable inside an expression, with
  `current_database()` and `current_setting()` reading a per-statement session
  snapshot the way the session user and the timezone already do.
- `server_version_num`, the numeric form every client that gates on a server
  version actually reads.

#### Fixed

- `pg_attribute` listed only composite types, so a table's columns were
  missing entirely — and a table's ROW TYPE is a composite under the same name
  and the same relation oid, so once tables were added every column appeared
  TWICE, once with `attnotnull` true and once false.
- `pg_attribute` gained `attnotnull`, `atttypmod` and `atthasdef`, without
  which a client cannot tell a nullable column from a NOT NULL one.
- `format_type(oid, NULL)` answered NULL. It is not null-propagating in its
  second argument: a NULL typmod means "no modifier", and PostgreSQL answers
  `integer`.
- `information_schema.table_constraints` under-counted every table by one:
  PostgreSQL records a NOT NULL check for the PRIMARY KEY column too.
