### The Rust PostgreSQL server: two silent data-loss fixes, rules, event triggers, foreign data, CREATE CAST and COLLATION, pgcrypto's PGP, and view definitions as PostgreSQL prints them

Batch 11 started from a re-measured backlog: every open Rust-server entry was
probed against PostgreSQL 15. Two of them were losing committed data without an
error; both are fixed. The batch then closes the refusals and wrong answers that
probe found, and adds `CREATE CAST` and pgcrypto's encryption functions. Every
change is measured against PostgreSQL 15 (and 14 where a corpus needs it).

#### Fixed

- **A block's earlier write could vanish.** An explicit transaction that
  wrote, then collided with another session on a later statement, retried that
  statement on a fresh transaction and silently discarded its earlier writes,
  while every statement and the `COMMIT` reported success. The "has this
  transaction written?" check read a flag before the snapshot refresh that
  sets it. The block now fails whole with `40001`.
- **A prepared transaction lost its locks across a restart.** Recovered from
  its record, it held nothing: another session could update one of its rows
  and commit, and `COMMIT PREPARED` then overwrote that commit. A prepared
  transaction is now revived as a live transaction when the store opens, so a
  conflicting write waits (`55P03` under `lock_timeout`), as in PostgreSQL.
- **`inet` / `cidr` ordered and compared as text.** `10.0.0.1` sorted before
  `9.0.0.1`, `a < '9.255.0.0'::inet` compared strings, `min` / `max` were
  wrong, and `a = '10.0.0.1'` missed the stored `10.0.0.1/32`. They now follow
  PostgreSQL's `network_cmp`.
- **Deep and long expressions.** A 10-term `a || b || ...` never finished
  (typing re-walked each operand per arm, about 5^depth). A 24-term chain
  overflowed a worker's stack and aborted the whole server, and past 50 levels
  the parse tree failed to decode. Typing is now linear. Workers have a large
  reserved stack, and a statement nested past PostgreSQL's own limit is its
  `54001`, never a crash.
- **`now()`** is the transaction's start, as in PostgreSQL, where it was the
  statement's. `statement_timestamp()` and `clock_timestamp()` keep their own
  meanings.
- **Window `RANGE` frames** over numeric values past 15 significant digits
  compare exactly.
- **`greatest` / `least`** keep an array's lower bounds.
- **An aggregate over a derived source** (`VALUES`, a subquery, a function)
  with a WHERE that is not a plain filter works; it was `0A000`.
- **`DISTINCT ON` over a grouped query** works; it was `0A000`.
- **Set operations** type their columns as PostgreSQL does:
  - `varchar UNION name` is `name`;
  - `null::text UNION 1` is `42804`, with its position.
- **Error codes and messages now match PostgreSQL:**
  - `max(boolean)`, `md5(1)`, `substr(1, 1)`, `array_length(1, 1)` and
    `to_hex(text)` are `42883`;
  - a cross-type comparison on a subquery or CTE column is `42883`, as it
    already was for a table's;
  - `1::inet` is `42846`.
- **`to_tsvector`** follows the `C.UTF-8` locale the server reports: every
  letter lowercases, and only letters make words.
- **SQL functions returning composites.** A function returning a composite
  returns the whole row. A composite-returning function in `FROM`, and
  `(f()).*`, expand to its fields.
- **Composite operands.** A composite beside another type in an operator is
  `42883`, as in PostgreSQL.
- **Dropping a type** that a function's signature names now refuses (`2BP01`)
  or cascades, as PostgreSQL does.
- **`DROP FUNCTION f(), g()`** with several functions works. It runs as
  one transaction: when one is missing, none is dropped (it was `0A000`).
- **A PL/pgSQL record's array field** keeps its array type. `r.a` of a
  `text[]` column was typed `text`, so storing it failed with `42804`.
- **A computed column's type modifier.** `x::numeric(5,2)` in a select list
  or a view now reports `numeric(5,2)`, in the row description and in
  `pg_attribute`, where it was bare `numeric`.
- **`pg_table_size`** counts a table's TOAST index (8192 bytes) when a
  column can be TOASTed, as PostgreSQL does.
- **`pg_class.relacl`** shows the grants on a table (`grantee=arwd/owner`,
  `*` for WITH GRANT OPTION, per privilege); it was always NULL.
  `relhasrules` is true for a table with a rule.

#### Added

- **Collations:**
  - ICU ones: any `<tag>-x-icu` name, and `CREATE COLLATION ... provider =
    icu` with the locale's `ks` / `kn` / `kf` / `co` / `ka` keywords
    (`de-u-co-phonebk`, `und-u-kn-true`).
  - Applied in `ORDER BY`, comparisons, `IN`, `min` / `max`, `greatest` /
    `least`, `DISTINCT`, `GROUP BY` and `UNIQUE` indexes.
  - Nondeterministic (case-insensitive) collations compare equal where their
    strength says so; `LIKE` under one is refused, as in PostgreSQL.
  - A column's `COLLATE`, which was silently dropped, is recorded and
    honoured.
  - `COLLATE` in an index key, `DROP COLLATION` with its column dependencies,
    and the `42P21` / `42P22` conflicts.
  - Catalog: `pg_collation`, `pg_attribute.attcollation`,
    `information_schema.columns.collation_name`, `regcollation` and
    `pg_collation_for` / `COLLATION FOR`.
  - Built on ICU4X, over the same CLDR data as the ICU PostgreSQL links.
- **`DISTINCT ON` over an expression** (`DISTINCT ON (lower(x))`).
- **`CREATE CAST` / `DROP CAST`:**
  - function, `WITH INOUT` and binary casts;
  - explicit, `AS ASSIGNMENT` and `AS IMPLICIT` contexts, through `INSERT` and
    `UPDATE` too;
  - `pg_cast`, with PostgreSQL's built-in rows;
  - PostgreSQL's validation and dependency errors.
- **pgcrypto's PGP functions:**
  - `pgp_sym_encrypt` / `pgp_sym_decrypt` (and `_bytea`) with all of
    pgcrypto's options;
  - `pgp_pub_encrypt` / `pgp_pub_decrypt` over RSA and Elgamal keys,
    password-protected keys included;
  - `armor`, `dearmor` and `pgp_key_id`.
  - Messages decrypt on PostgreSQL and the other way round. Blowfish and CAST5
    are checked against GnuPG, since the reference servers' OpenSSL 3 builds
    lack them.
- **pgcrypto's raw ciphers:** `encrypt` / `decrypt` / `encrypt_iv` /
  `decrypt_iv`, byte for byte with PostgreSQL.
- **The network functions:** `host`, `masklen`, `network`, `broadcast`,
  `netmask`, `hostmask`, `family`, `abbrev`, `text`, `set_masklen`,
  `inet_same_family`, `inet_merge`, and the containment operators `<<`,
  `<<=`, `>>`, `>>=` and `&&`.
- **`CREATE RULE` / `DROP RULE`** on INSERT, UPDATE and DELETE: `DO ALSO`,
  `DO INSTEAD`, `DO INSTEAD NOTHING` and a `WHERE`, with `pg_rewrite` and
  `pg_rules`. `ALTER TABLE ... ENABLE` / `DISABLE RULE` and `TRIGGER`
  (including `ALL` and `USER`) work too.
- **Event triggers:** `CREATE` / `ALTER` / `DROP EVENT TRIGGER` on
  `ddl_command_start`, `ddl_command_end` and `sql_drop`, with `TG_EVENT`,
  `TG_TAG`, a `WHEN TAG IN` filter, `pg_event_trigger_dropped_objects()`,
  `pg_event_trigger_ddl_commands()` and `pg_event_trigger`.
- **Foreign data:** `CREATE` / `ALTER` / `DROP FOREIGN DATA WRAPPER`,
  `SERVER`, `USER MAPPING` and `FOREIGN TABLE`, with their options,
  dependencies and `CASCADE`, and the `pg_foreign_*` catalogs and
  `information_schema` views. There is no FDW handler here, so reading or
  writing a foreign table, and `IMPORT FOREIGN SCHEMA`, answer PostgreSQL's
  own `55000` for a wrapper without one.
- **View definitions as PostgreSQL prints them.** `pg_get_viewdef` and
  `pg_views.definition` now follow ruleutils' layout: qualified columns,
  typed literals, implicit casts written out, and parenthesised operators.
  The pretty form `pg_get_viewdef(v, true)` (which psql's `\d+` uses) works
  too. `pg_rules.definition`, and `pg_get_expr` over generated columns,
  CHECK constraints and policies, use the same printer.
- **`LANGUAGE internal` wrappers** are callable when the C function they
  name has a SQL form here (`int4pl`, `textlen`, `upper`, ...).
- **New corpora:** `user_casts`, `pgcrypto_ciphers`, `expr_depth`, `clocks`,
  `triage_fixes`, `collations`, `rules`, `event_triggers`, `fdw`,
  `catalog_b11`, `views_ruleutils`, `expr_ruleutils` and
  `internal_functions`.
