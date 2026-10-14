# Backlog: the Python PostgreSQL server

A living list of where this server does not yet behave like the real
product it stands in for: stubs, stopgaps, deferred work and known bugs.
Update it when you stub something, defer a slice, or find a limitation, and
delete an item's lines when you fix it -- finished work lives in git
history and `changelog.d/`, not here. Don't add work that already has a fix
in flight; that lives on its branch and PR.

The backlog is four files, one per server:

| file | server |
| --- | --- |
| `tasks/backlog-rust-mongodb.md` | the Rust MongoDB server, and the Rust storage both Rust servers use; also CI, build and release items no one server owns |
| `tasks/backlog-rust-postgres.md` | the Rust PostgreSQL server |
| `tasks/backlog-python-mongodb.md` | the Python MongoDB server (and the admin UI) |
| `tasks/backlog-python-postgres.md` | the Python PostgreSQL / SQL server |

An item about more than one server is in each file it concerns. Section
numbers (`7.077`, `3.1`, ...) are the ones the single `tasks/backlog.md`
used, kept so the code comments that cite them still resolve.

**`- [ ]` is open work, and every item here is open.** An open item's bold
headline must not read as finished: if completed context belongs in it,
lead with `**OPEN —` (`tests/test_backlog_markers.py`).

**Reproduce an item before working it** -- a probe, the failing test, the
query -- and size it from that, not from the text. Entries here have
described work as missing that was done, and understated bugs by naming one
symptom of several. Correct a superseded entry rather than adding a newer
one beside it.

- [ ] **PYTHON pgserver: locale ordering without ICU.** `ORDER BY ... COLLATE
  "en_US.UTF-8"` uses `collation.sort_levels` (an ICU-shaped key computed
  without ICU), which matches PostgreSQL's `en_US.UTF-8` on 9 of 11 measured
  corpora. The two misses: `ß` (PostgreSQL expands it to `ss`) and the
  relative weight of `-` versus `_`, both pinned in
  `tests/test_sql_collation_order.py::TestKnownNonIcuLimits` as the target
  for an ICU-backed implementation. The default ordering (the database is
  `C`, so bytes) is right and must not change.

## Open from the 2026-08-26 triage

- [ ] **PYTHON pgserver: `pg_index.indpred` is NULL for a partial index.** The
  `pg_index` row in `src/secantus/sql/virtual.py` hard-codes `"indpred":
  None` (confirmed 2026-08-28 against PostgreSQL 14). `pg_indexes.indexdef`
  already renders the `WHERE` (`_render_partial_predicate`), so the predicate
  text is available.
- [ ] **PYTHON pgserver: psycopg `test_return_untyped[b]`** (`select %s::int + 1`
  with a `bytes` argument) answers **42883** (`operator does not exist:
  bytea + integer`) where PostgreSQL answers **42846** (`cannot cast type`).
  Both are typed 42xxx errors, but PostgreSQL's is about the failed CAST;
  closing it means the cast failing at bind time, before arithmetic is
  reached.
- [ ] **A reflected column in arithmetic raises from the Mongo side (open,
  minor).** `SELECT _id FROM coll WHERE v + 0 = 7` over a schema-on-read
  collection pushes `v + 0` down to `$add`, which rejects a string operand
  (`ExpressionError: $add only supports numeric or date types, not string`).
  `tests/test_sql_typecheck.py` pins the analysis rather than the end-to-end
  read for this reason.
- [ ] **OPEN — PYTHON pgserver: what the DecodeDateTime port leaves
      (batch 27, 2026-10-01).** `dt_input` is at 0 against PostgreSQL. Left:
      about 50 zone abbreviations (MSK and others) carry one FIXED offset in
      `dtparse.py`, where PostgreSQL derives a date-dependent one from the
      zone's history (the Rust server does too); `time_input` 19/37,
      `wide_timestamptz` 7/35 and `tz_abbrevs` 6/8 still diverge; and
      `dtparse.py` is ~1,100 lines, over the 500-line guideline (a straight
      port of one C function family, so the split is by table, not logic).
- [ ] **PYTHON pgserver: dotted / `$`-named columns were never checked.**
  The Rust PG server's dotted-column work (corpora `dotted_columns`,
  `b41_dotted`) did not measure the Python PG server on such names (for
  example a column `"a.b"` or `"$x"` in CREATE TABLE, WHERE, ADD UNIQUE and
  CREATE INDEX). Run those corpora against the Python server and compare with
  PostgreSQL.
- [ ] **PYTHON pgserver over a store the Rust PG server partitioned** reads the
  root table and refuses everything else with 0A000. Recorded because the
  store is shared; partitioning on the Rust server is complete.

## 3. Deferred work (skipped from a slice, ready to come back)

- [ ] **OPEN — `test_connect_error_multi_hosts_each_message_preserved` failed
      twice in a full psycopg-gauge run; cause UNKNOWN, and socket exhaustion
      is now RULED OUT (measured 2026-09-19).** The test connects to two RFC
      5737 reserved addresses and requires both attempts to fail with
      `connection timeout expired`. It never touches the server. It runs in
      the gauge (re-enabled by PR #1484, `psycopg_validation/include_paths.py`
      is shared by both servers' gauges; standalone it connects 6/6 in 4.04s
      with the expected error). The unexplained part is that two runs failed
      it in **0.04s** — an immediate `connect()` error rather than a timeout.

      Ruled out by measurement during a full gauge run: ephemeral-port
      exhaustion (peak 1,155 of 16,384, sawtoothing, no leak) and descriptor
      exhaustion (`ulimit -n` 1,048,576; weaker evidence, the fd sample
      followed the runner rather than its pytest child).
      **If it recurs, capture the exception text and errno before theorising** —
      that names the failure and distinguishes the remaining possibilities.

- [ ] **OPEN — the SQL engine provides ONE isolation level inside an explicit
  transaction: READ COMMITTED behaves as REPEATABLE READ after a
  transaction's first write.** Measured three-way against PostgreSQL 14
  (2026-08-29):

      autocommit write-write        pg blocks -> 111   us blocks -> 111   MATCH
      explicit txn READ COMMITTED   pg blocks -> 111   us blocks -> 111   MATCH*
        (* for a transaction's FIRST write only, which is the only one
           WiredTiger lets us refresh a snapshot before; a later write still
           answers 40001 rather than computing against a stale view)
      explicit txn REPEATABLE READ  pg 40001  -> 101   us 40001  -> 101   MATCH
      explicit txn SERIALIZABLE     pg 40001 (skew)    us BOTH COMMIT      DIVERGE

  An unretried client on the READ COMMITTED path does not merely see an
  error, it LOSES a write (101 where PG gives 111). Root cause: every
  explicit transaction runs on WiredTiger's snapshot isolation, which IS
  PostgreSQL's REPEATABLE READ. `BEGIN ISOLATION LEVEL x` is honoured and
  echoed correctly.

  **SERIALIZABLE — DECIDED 2026-09-01: keep accepting it and document
  loudly** (write skew is permitted). Rejecting the level breaks every driver
  and framework that requests it; echoing `repeatable read` diverges from
  PG's own echo. `docs/sql.md` "Isolation levels — read this before relying
  on `SERIALIZABLE`" and the SQL capability matrix's Transactions row carry
  it. Not open.

  **READ COMMITTED inside an explicit transaction is a REDESIGN, not a fix —
  scoped in `tasks/sql-mvcc-plan.md`.** Both obvious designs fail:
  WiredTiger's `reset_snapshot` is "an error … if the current transaction has
  already written any data", so per-statement snapshots inside one WT
  transaction are impossible; and committing each statement separately
  causes **dirty reads**, worse than the 40001 it would fix. The plan scopes
  MVCC above WT (row versions, visibility filter, durable undo, crash
  recovery, GC, row locks), with the seam ABOVE `Storage` so the Mongo side
  keeps mongod's snapshot isolation, and states kill criteria. Phase 0
  refuted the "ship the partial row-lock mode and stop" exit: psycopg's own
  transaction tests are only **46.2%** single-write (our own suite's 99.2%
  must not be quoted). Do not start Phases 3–4 without reading §8.

  All four rows are pinned by `tests/test_sql_isolation_level.py`; the two
  divergent ones are `test_known_divergence_*`.

## 5. Known bugs and edge cases to watch

- [ ] **OPEN — on Python 3.10 our `cbrt` is EXACT on perfect cubes where
      PostgreSQL is not, a ≤1 ULP divergence on Linux (measured 2026-09-28).**
      `secantus/sql/scalar.py`'s `_real_cbrt` uses `math.cbrt` (a bare libm
      call) from 3.11, and a Newton-refined power form on 3.10 because
      `math.cbrt` does not exist there. The fallback is exact on every perfect
      cube; libm is not.

      Measured by calling `libm.so.6`'s `cbrt` through `ctypes` on **glibc
      2.39**: `cbrt(27.0)` is `3.0000000000000004` — exactly one ULP high —
      while 8, 64, 125 and 1e6 come out exact. MSVC and macOS answer exactly
      `3.0`. PostgreSQL's `dcbrt` is the same bare libm call, so **PostgreSQL
      on glibc answers `3.0000000000000004` too**, and on 3.10 we answer `3.0`
      — i.e. the fallback is the path that diverges from the oracle, by being
      *more* accurate than it.

      `_real_cbrt`'s docstring already names this trade ("being more accurate
      than libm would move us AWAY from Postgres") and accepts it. Left open as
      a record, not as a task: closing it means calling libm through `ctypes` on
      3.10, which is a fragile cure for a one-ULP disease. Revisit when 3.10
      support is dropped, at which point it disappears on its own.

### 2026-09-03 SQL sweep twelve: LIMIT and row NULLs — what is still open

LIMIT / OFFSET, row comparison, conditionals and transactions probed against
PostgreSQL 14.13; `tests/test_sql_sweep_twelve.py` pins the fixes.
**Transactions came back 40 of 40** — savepoints, the aborted-transaction
state, isolation levels, READ ONLY and transactional DDL rollback all match, so
that surface needs no work. What is open:

- [ ] **`NOT IN (<subquery with UNION>)` is `0A000 unsupported subquery`.**
      `_inner_row_scopes` handles a single SELECT, not a set operation. Note the
      NULL semantics matter here too: PG's `NOT IN` over a set containing NULL
      is never true, so the correct answer for the probed case is no rows.
- [ ] **`round()` inside an aggregate is still `0A000`** — deliberately. The
      other common scalar functions now lower (`_AGG_FUNC_OPS`), but Mongo's
      `$round` rounds half-to-EVEN where Postgres rounds half-away-from-zero, so
      lowering it would make `sum(round(x))` over 1.5 and 2.5 answer 4 rather
      than 5. It needs a rounding expression the aggregation engine does not
      have, not a table entry.
- [ ] **`avg()` over a FUNCTION-CALL argument loses the exact-numeric path.**
      `avg(coalesce(n,0))` answers `11.666666666666666` where Postgres gives
      `11.6666666666666667`: `_register_numeric_avg` accumulates N and sum(X)
      for a bare column only, so a lowered expression falls back to the float
      divide. The VALUES agree to 15 digits; only the last differs.
- [ ] **`nullif(NULL, 1)` types as `text`**, not the second argument's type.
      The VALUE is right (NULL); only the oid differs.
- [ ] **`greatest()` / `least()` with no arguments** raise a sqlglot internal
      message rather than Postgres' `42601 syntax error at or near ")"`.

### 2026-09-03 SQL sweep eleven: arrays and sequences — what is still open

Arrays (20/29) and sequences / identity (23/26) probed against PostgreSQL
14.13; `tests/test_sql_sweep_eleven.py` pins the fixes. **Sequences need little
work** — nextval / currval / setval (both arities), SERIAL and BIGSERIAL,
ALTER SEQUENCE RESTART / INCREMENT, DROP, and GENERATED ALWAYS / BY DEFAULT AS
IDENTITY all match. What is open:

- [ ] **`UPDATE t SET arr[i] = v` is `0A000 expected a column`.** Array element
      assignment, including the extend-with-NULLs case (`arr[5] = 5` on a
      3-element array pads index 4 with NULL). sqlglot gives
      `EQ(Bracket(Column, [Literal]), value)` and converts the subscript to
      0-based; the SET planner has a `_composite_subfield_target` hook for
      `SET col.field = v` that this would sit beside, but it needs a new
      computed-assignment kind rather than a rewrite.
- [ ] **A multidimensional SLICE is wrong.** `m[1:2][1:1]` over
      `ARRAY[[1,2],[3,4]]` answers `{{1,2}}` where Postgres gives `{{1},{3}}` —
      the second subscript is applied to the wrong dimension.
- [ ] **`unnest(a, b)` (multiple arrays) yields only the first column.**
      Postgres zips them into one row per position.
- [ ] **`ARRAY(SELECT col FROM real_table)` types as `text[]`.** The VALUES-
      sourced form is fixed, and so is the scalar-subquery form over a table;
      this one loses it because the FROM-less `SELECT ARRAY(...)` runs through
      `plan_constant_select`, which does not publish the catalog on
      `_pipeline_subctx`. The VALUES are correct — only the oid is wrong.
- [ ] **`ta || 'c'` (text[] || unknown) is accepted**; Postgres raises
      `22P02 malformed array literal: "c"` because the unknown literal is
      resolved as an ARRAY, not an element.
- [ ] **`INSERT ... OVERRIDING SYSTEM VALUE` does not parse** (`42601`), so a
      `GENERATED ALWAYS AS IDENTITY` column cannot be written explicitly.
- [ ] **`pg_get_serial_sequence` on an unknown relation answers NULL**;
      Postgres raises `42P01`. (PYTHON server. The RUST server had the same
      divergence and an unknown column answering NULL where PG raises
      `42703`; both fixed there 2026-10-07, `pg_corpora/sequences2.sql`.)

### 2026-09-03 SQL sweep ten: DDL and catalog — what is still open

DDL (38/41) and catalog introspection (15/22) probed against PostgreSQL 14.13;
`tests/test_sql_sweep_ten.py` pins the fixes. **DDL needs little work** — every
constraint kind enforces, ALTER's add/drop/rename/type/default/not-null paths
all match, and so do CREATE INDEX / VIEW and their error surface. What is open:

- [ ] **`information_schema.columns` has no `numeric_precision` /
      `numeric_scale`** (`42703`). ORM reflection reads these to recover a
      `numeric(p, s)` declaration; without them the scale is invisible.
      **`character_maximum_length` / `character_octet_length` are missing the
      same way** (`42703`, measured 2026-09-19) — PostgreSQL 14 reports
      `(10, 40)` for `varchar(10)`, `(5, 20)` for `char(5)` and
      `(NULL, 1073741824)` for `text` / bare `varchar`. Distinct from the
      declared-char-type slice, which fixed `data_type` on the same view: that
      one had the data and ignored it, these columns do not exist.
      **NOT what pgjdbc's `getColumnsCharOctetLength` reads** — that was
      asserted here from the name and is false: the test went green when
      `pg_type` gained its `varchar` row, because JDBC's `getColumns()`
      computes the octet length from `pg_attribute.atttypmod` and never
      touches `information_schema`. Measured 2026-09-19 by diffing the gauge
      with and without that change. No test currently known to read these.
- [ ] **`information_schema.table_constraints` omits the CHECK rows Postgres
      synthesizes for NOT NULL.** We report only the PRIMARY KEY where PG
      reports two CHECKs beside it.
- [ ] **`to_regclass` is absent** (`42883`). Common in tooling that probes for a
      relation's existence without raising.
- [ ] **`pg_typeof(x::regclass)` answers `text`**, not `regclass` — the regclass
      cast resolves to the oid but does not carry its type.
- [ ] **`ANY(...)` over a CATALOG array column** (`i.indkey`) is `0A000
      unsupported array operand`. The pgjdbc / SQLAlchemy primary-key
      introspection join uses exactly this shape.
- [ ] **`getIndexInfo`'s ASC/DESC expression needs TWO things, measured
      2026-09-19 after `relpages` landed and unblocked the query.** pgjdbc
      sends `tmp.i_indoption[tmp.ordinal_position - 1] & CAST(1 AS SMALLINT)`
      over a subquery alias. Both halves fail, and neither is what the error
      text names:
      1. `0A000 unsupported scalar expression` for `alias[expr] & CAST(..)`
         — **only through a subquery alias**. The identical expression over a
         real table (`SELECT a[p - 1] & CAST(1 AS SMALLINT) FROM s`) works, as
         do subscript-with-computed-index and the bit-and on their own. So it
         is the combination in an alias context, not any one operator.
      2. `pg_index.indoption` is served as the STRING `'0'`, not a
         `smallint[]`, so `indoption[0]` is NULL. Even with the expression
         parsing, the ASC/DESC answer would be wrong — fixing only the planner
         half would turn a loud error into a silently wrong result.
      Blocks `ascDescIndexInfo`, `partialIndexInfo`, `remarkIndexInfo`.
- [ ] **`has_table_privilege('nosuchuser', 't', 'SELECT')` answers false**;
      Postgres raises `42704 role "nosuchuser" does not exist`.
- [ ] **The PYTHON server rejects `CREATE FUNCTION ... LANGUAGE internal`**
      (`only LANGUAGE sql / plpgsql`), where PostgreSQL accepts it and **the
      RUST pgserver already implements it** — see
      `tests/test_rust_pgserver_slice.py::test_internal_function_ddl_notices_and_errors`,
      which pins five behaviours measured against PostgreSQL 16: a shell
      argument/return type accepted with a 42809 NOTICE naming the type
      unquoted, an unknown return type becoming a new shell with a 42704
      NOTICE, an unknown argument type as a 42704 error, an unknown built-in
      as 42883, and a duplicate signature as 42723 unless OR REPLACE.
      Blocks pgjdbc's `getSqlTypes`, which creates `public.array_in` purely so
      `getTypeInfo()` can see it. Deliberately NOT half-built (measured
      2026-09-20): that Rust test is the specification to port, and a partial
      version is exactly the "half-implemented feature that silently diverges"
      this project prefers a clean refusal over.
- [ ] **`PARALLEL SAFE` / `UNSAFE` / `RESTRICTED` does not parse.** sqlglot
      falls back to a `Command` node, so the statement surfaces as the generic
      `command CREATE is not supported` rather than anything about PARALLEL.
      Fixable in the planner pre-parse beside the `RETURNS trigger` rewrite;
      the attribute is a no-op here (we model neither `proparallel` nor
      `provolatile`, and `STABLE` / `STRICT` already parse and are ignored).
      Measured 2026-09-20.
- [ ] **A dangling-escape LIKE pattern written as a SQL LITERAL should be a
      plan-time ERROR, not an empty result.** PostgreSQL 14 raises `LIKE
      pattern must not end with escape character` for a literal pattern, and
      returns no rows for the same pattern BOUND as a parameter (both measured
      2026-09-25). We do not distinguish the two paths and implement the bound
      semantics, because that is the path a client uses -- pgjdbc's `getTables`
      binds it. So a user writing the dangling pattern literally gets an empty
      result where PostgreSQL would error. Strictly better than before, when we
      MATCHED in both cases, but still a divergence.
- [ ] **`json` is folded onto `jsonb` end to end — FOUR measured divergences,
      one root cause.** Mapped 2026-09-28 against PostgreSQL 14 so the next
      session starts from this rather than re-deriving it.

      | surface | ours | PostgreSQL 14 |
      | --- | --- | --- |
      | `pg_attribute.atttypid` of a `json` column | 3802 | **114** |
      | `pg_typeof('{}'::json)` | `jsonb` | **json** |
      | `'{"b":1}'::json::text` | `"{\"b\":1}"` | **`{"b":1}`** |
      | stored `json` col `::text` | `{"b": 1}` | **`{"b":1}`** |

      The WIRE oid is already correct (114 on both) — only the catalog and the
      casts are wrong, so do not "fix" the wire.

      **Root cause: there is no value-level representation for raw JSON text.**
      After `::json` the value is a plain Python `str`, which is
      indistinguishable from a DECODED JSON string value (`'"hi"'::jsonb` is
      also a `str`). `scalar.py`'s `to_tag_early == "text"` branch calls
      `_render_json` on it, which is right for the decoded-string case and
      double-encodes the raw-text case. `typemap.JsonText(str)` exists but is a
      PARAMETER-substitution marker only: it is rewritten to a `::jsonb` cast at
      substitution and never survives as a value.

      **Why the cheap fix is wrong.** Making `::json` decode like `::jsonb`
      removes the double-encoding and gets `{"b": 1}` — valid, equivalent JSON,
      differing only in whitespace. Tempting, and still not `json` semantics:
      `json` preserves the input text verbatim, including key ORDER, whitespace
      and DUPLICATE keys, all of which `jsonb` discards. Shipping the decode
      would make `json` look supported while silently normalising, which is a
      worse failure than today's visibly broken cast.

      **What a real fix needs**: a value type that carries raw JSON text through
      the engine (storage, casts, operators, wire), so `json` round-trips
      verbatim and `jsonb` keeps normalising. That is a feature, not a fidelity
      patch. `jsonb` has its own separate divergence worth folding in: it does
      not SORT object keys (`'{"b":1,"a":2}'::jsonb` renders `{"b": 1, "a": 2}`
      where PostgreSQL gives `{"a": 2, "b": 1}`).

      The `pg_type` row for `json` (114) is present and correct as of
      2026-09-27; only the column identity and the casts are wrong.
- [ ] **`PARTITION BY` is accepted and SILENTLY IGNORED — needs a decision.**
      `CREATE TABLE m (...) PARTITION BY RANGE (logdate)` succeeds and produces
      an ORDINARY table: `relkind` is `r` where PostgreSQL reports `p`, and an
      INSERT with no matching partition is ACCEPTED where PostgreSQL raises
      `no partition of relation "m" found for row` (both measured 2026-09-28).
      The partition key is not stored anywhere.

      This blocks pgjdbc's `partitionedTables`, which asks
      `getTables(..., {"PARTITIONED TABLE"})` for `relkind = 'p'`. **Do NOT fix
      it by reporting `p`**: that would tell a client the table routes rows when
      it does not, which is worse than today's honest "ordinary table". The
      sibling `partitionedTablesIndex` already passes because `getPrimaryKeys`
      is answered correctly.

      CLAUDE.md's rule ("prefer a faithful `command not supported` error over a
      half-implemented feature that silently diverges") argues for REFUSING
      `PARTITION BY` outright. That is a behaviour change for anyone currently
      relying on it degrading to a plain table, so it is **Joe's call**, not a
      bug to fix unilaterally. The alternative is implementing partitioning,
      which is a feature rather than a fidelity fix.
- [ ] **`getProcedureColumns` returns nothing for a schema's PROCEDURE.**
      pgjdbc's `getProceduresWithCorrectCatalogAndWithout` asserts
      `getProcedureColumns(null, 'hasprocedures', null, null)` yields 1 row for
      a zero-argument procedure. Still open after the argmodes slice
      (2026-09-20), which fixed the same call for FUNCTIONS — `funcWithDirection`
      / `funcReturningComposite` / `funcReturningTable` all went green while
      this one did not, so the remaining gap is procedure-specific rather than
      shared with the function path.
- [ ] **`pg_proc` carries no built-in functions** — 13 rows against
      PostgreSQL 14's 3,324 (measured 2026-09-19). pgjdbc's
      `getFunctionsWithNullPatterns` / `getFunctionsWithBlankPatterns` assert
      `> 1000` functions, so both are unreachable without cataloguing
      PostgreSQL's builtin set. **Probably should NOT be done**: it is a large
      data table rather than a fix, and it would advertise functions this
      server does not implement. Recorded so the next session does not size
      the "function metadata cluster" as tractable without knowing this — two
      of its tests are blocked on a number, not a bug.
- [ ] **`getColumnPrivileges` on a SYSTEM catalog needs TWO things, and the
      bigger one is that `pg_class` does not self-describe.** Probed 2026-09-28.
      pgjdbc's `columnPrivileges` asks for `getColumnPrivileges(null, null,
      'pg_statistic', null)` and expects at least one row.
      1. **`pg_class` lists no system catalogs at all**: zero `pg_%` rows in an
         empty database, where PostgreSQL 14 has **490** — the catalog tables are
         themselves relations there, with `pg_attribute` rows each. Our
         `pg_class` contains only user objects, so `pg_statistic` does not exist
         to ask about. Making the catalog self-describing is a design decision
         (490 relations plus their columns), not a patch.
      2. **`pg_attribute.attacl` does not exist** (`column "attacl" does not
         exist`). This one is genuinely small AND worth doing on its own merits:
         we already record column grants (`COLUMN_GRANT_COLLECTION`), so `attacl`
         can render the same aclitem form `relacl` does instead of being a NULL
         placeholder. It will not flip the test by itself — (1) is the hard
         blocker — but it closes a real gap.

- [ ] **`pg_type.typcollation` is 0 for every type**, including the collatable
      string types. PostgreSQL 14 reports **100** for `text` / `varchar` /
      `bpchar` (measured 2026-09-19). Left alone deliberately when the
      declared-char-type rows landed: the 0 is uniform across all eight row
      builders in `virtual.py`, so correcting it is its own slice with its own
      blast radius, not a rider on a two-row addition. Nothing measured yet
      reads it — the pgjdbc metadata tests that drove that slice do not.

### 2026-09-03 SQL sweep nine: windows and RETURNING — what is still open

Window functions, DML/`RETURNING` and CTEs probed against PostgreSQL 14.13
(`tests/test_sql_sweep_nine.py` pins the fixes; `changelog.d/sql-sweep-nine.md`
lists them). **CTEs came back clean — 23 of 23**, including `RECURSIVE`,
`MATERIALIZED` / `NOT MATERIALIZED`, a data-modifying CTE and a self-join over
one; that surface needs no work. What is still open:

- [ ] **A subquery in `RETURNING` sees the row the statement just wrote.**
      `INSERT INTO t VALUES (...) RETURNING id, (SELECT count(*) FROM t)`
      counts the new row; Postgres evaluates the subquery against the
      *command's* snapshot, which does not include it. (Before this sweep the
      whole shape crashed with `XX000`, so this is a narrowing, not a
      regression.) Fixing it needs the pre-write snapshot to still be readable
      at RETURNING time, which the executor does not currently keep.
- [ ] **`UPDATE ... RETURNING ... ORDER BY` is accepted.** Postgres has no
      `ORDER BY` on a data-modifying statement's `RETURNING` and raises `42601`;
      we sort and return. Lenient in the harmless direction, but it lets through
      SQL no other server takes.
- [ ] **`UPDATE ... SET (a, b) = (SELECT x, y ...)`** — the row-SUBQUERY form.
      The row-constructor forms landed in this sweep; the subquery one cannot
      use the same expansion (each column would re-run the query) and still
      answers `0A000`.
- [ ] **`abs(-2147483648)` answers 2147483648.** Postgres raises `22003 integer
      out of range` -- the negation does not fit int4. The arithmetic operators
      overflow-check (`SELECT a + 1` at int4 max is `22003` on both servers);
      `abs` does not.
- [ ] **`upper` / `lower` use Python's FULL case mapping, Postgres uses the
      simple one.** `upper('ß')` is `SS` here and `ß` there. **This one cannot
      be settled on this box**: its PostgreSQL runs `lc_ctype=C`, which does not
      case-map non-ASCII AT ALL (`upper('é')` is `é`), so the C-locale answer
      cannot distinguish "simple mapping" from "no mapping". Needs a
      UTF-8-locale PostgreSQL to measure before anything is changed — our
      Unicode-aware answer is right for that server in every case except
      possibly the full-vs-simple ones.
- [ ] **`date_part` should return float8; we return numeric.** In PostgreSQL 14
      `EXTRACT` returns `numeric` but `date_part` still returns `double
      precision` -- for every field, not just `epoch`. The VALUES agree; only
      the oid differs (1700 vs 701). **Not fixed because the two spellings are
      indistinguishable in the AST**: sqlglot folds both into `exp.Extract` with
      the field as a `Var`, and the only difference is that the `EXTRACT`
      keyword form arrives upper-cased. Keying on case would misread
      `date_part('YEAR', ...)`, so this wants a real marker set during parsing
      rather than a heuristic.
- [ ] **`convert_from` / `convert_to` are absent** (`42883`).
- [ ] **A set-returning function beside another column in the SELECT list.**
      `SELECT jsonb_array_length(x), jsonb_object_keys(y)` and
      `SELECT regexp_split_to_array(...), regexp_split_to_table(...)` answer
      `0A000`; Postgres expands the SRF and pairs it with the scalar column.
      The FROM form and the whole-SELECT-list form both work. The error now
      names the actual limit rather than claiming the function does not exist.
- [ ] **`numeric_send` is absent** (`42883`). One of Postgres' internal
      type-I/O functions; nothing but a driver test is likely to call it.
- [ ] **An in-call `ORDER BY` inside a window aggregate is dropped.**
      `array_agg(v ORDER BY x) OVER (...)` aggregates in frame order rather than
      the requested one. The argument is unwrapped from its `exp.Order` and the
      sort keys discarded — the same shape the GROUP BY path handles through
      `_sorted_agg_push`, not yet wired into `window.py`.

### 2026-09-03 SQL sweep eight: what it found and did NOT fix

263 statements against PostgreSQL 14.13, both sides driven by the same
`psycopg` client (`tests/test_sql_sweep_eight.py` pins the fixes). What landed
is in `changelog.d/sql-sweep-eight.md`. What is still open, measured rather
than assumed:

- [ ] **JSON `null` is indistinguishable from SQL NULL**, so
      `jsonb_typeof('null'::jsonb)` answers NULL where PostgreSQL answers the
      string `'null'`, and so does `jsonb_typeof(v -> 'k')` over a JSON null
      member. Both representations are Python `None`. The exposure is narrow --
      the VALUES are right, and `->`, `@>`, `?`, `jsonb_strip_nulls` all agree
      with PostgreSQL; only `jsonb_typeof` / `json_typeof` can see the
      difference, because psycopg decodes a jsonb null to `None` anyway. Fixing
      it means a JSON-null sentinel threaded through the scalar evaluator, the
      storage round-trip and `typemap`, which is why it was left. Same
      missing-vs-null shape this file names three times already.
- [ ] **`make_interval` does not take NAMED arguments.** `make_interval(days =>
      3)` is `0A000 unsupported scalar expression: days => 3` -- the `=>`
      named-argument form is not handled anywhere in the scalar evaluator, so
      this is not specific to `make_interval`. The positional form
      `make_interval(0, 1, 0, 2)` fails for the same reason.
- [ ] **`LATERAL` with no FROM inside it.** `FROM t, LATERAL (SELECT t.id) x`
      answers `0A000 a LATERAL subquery requires a FROM clause`; PostgreSQL
      evaluates it per outer row.
- [ ] **`GROUP BY` does not honour a primary key's functional dependency.**
      `SELECT id, n FROM t GROUP BY id` is legal in PostgreSQL when `id` is the
      primary key (every other column is functionally dependent on it); we raise
      `42803`. Erring toward the error is the safe direction, but it rejects
      valid SQL.
- [ ] **`substring(x similar p escape e)`** does not parse (`42601`). The
      `SIMILAR TO` regex engine that landed in this sweep
      (`scalar.similar_to_regex`) is what it would be built on -- the SQL
      spec's `#"` / `"#` capture markers are the missing half.
- [ ] **`overlaps(a, b, c, d)` as a FUNCTION call** does not parse; only the
      `(a, b) OVERLAPS (c, d)` operator spelling works. PostgreSQL accepts both.
- [ ] **`power(numeric, numeric)` returns one digit too many.**
      `power(2, 0.5)` gives `1.4142135623730951` where PostgreSQL gives
      `1.414213562373095` -- we finish in a float, PostgreSQL in numeric at
      `select_div_scale`'s scale. Same root cause class as the statistical
      aggregates that `typemap.numeric_stat` already fixed.
- [ ] **`ARRAY[1,2,3][2]` is accepted; PostgreSQL rejects it** (`42601 syntax
      error at or near "["` -- an array constructor must be parenthesised
      before it can be subscripted). Lenient in the harmless direction.
- [ ] **A time with a single-digit MINUTE or SECOND does not parse.**
      `TIMESTAMP '2020-01-15 1:2:3.45'` is `22007`; PostgreSQL reads it as
      `01:02:03.45`. `datetimes._widen` pads a single-digit *hour* and the date
      fields but not the minute or second, so the padded string
      `2020-01-15 01:2:3.45` still fails. Identical on 3.10 and 3.12 — it is a
      gap in `_widen`, not the Python-version fractional-seconds bug fixed
      alongside it.
- [ ] **The 42883 message's argument types are approximate.** The new
      unknown-function error names types from the RUNTIME values, so
      `nonexistent_fn('a', 1.5)` says `(text, text)` where PostgreSQL says
      `(unknown, numeric)`. The SQLSTATE and the function name are right, which
      is what a client keys on.

**Two differences that are NOT bugs, recorded so the next sweep does not
re-open them.** (a) `initcap('ECOLE')` with an accented E: this box's
PostgreSQL runs `lc_ctype=C`, which treats a non-ASCII letter as a word
SEPARATOR and answers `ECole`; our Unicode-aware answer matches a UTF-8-locale
PostgreSQL. Check `SHOW lc_ctype` before calling a case-mapping difference a
bug. (b) `GROUP BY GROUPING SETS ((b), ())` ordered by `b NULLS LAST` puts two
rows with a NULL `b` in an unspecified order; the sweep's apparent divergence
was a tie, not a difference.

## 6. Admin UI review punch list

### P2 — polish

- **EXPLICIT SCOPING DECISION, not a gap: the SQL / PostgreSQL-wire server
  has no admin UI.** `client.check_supported_uri` rejects a `postgresql://`
  target with a message saying so. Every admin page is pymongo-driven; a SQL
  admin surface would need its own console.

## SQL / PostgreSQL interface — P0 spike limitations

### pgx gauge — `pgconn` findings and next steps (2026-08-14)

#### Temp tables: residual namespacing caveats

Every session has its own `pg_temp_<n>` namespace
(`tests/test_sql_temp_namespacing.py`). What is left, deliberately: stale `pg_temp_<n>.*` catalog entries from a crashed process are
not purged at startup (the namespace counter is pid-seeded, so re-mint
collisions are vanishingly unlikely, but the dead entries linger invisibly
until a same-named session drops them); RBAC grants and `pg_namespace` don't
know per-session namespaces (`pg_namespace` still lists a single `pg_temp_1`
row).

#### Reproductions

```
# gauge, one test
python -m secantus.sql.pgserver --host 127.0.0.1 --port 15435 --storage-path $(mktemp -d)
cd vendor/pgx && PGX_TEST_DATABASE="host=127.0.0.1 port=15435 dbname=postgres user=postgres" \
  go test -count=1 -run 'TestConnCopyFrom$' ./pgconn/

# whole package (~130s)
... go test -count=1 -timeout=600s ./pgconn/
```

### SQL-gauge re-baseline sweep (2026-08-15, at `36937245`)

What is left from the pgtest campaign (`docs/validation-report-pgtest.md`,
reasons inline in `pgtest_validation/include_paths.py`):

- **`multiple_active_portals/query_timeout` needs row-LAZY portal execution**
  (architectural): `pg_sleep` evaluated per fetched row with a cumulative
  `statement_timeout` across Execute-resumes. Our portals materialise fully at
  first Execute, so the timeout cannot fire mid-drain. Recorded as an expected
  divergence.
- **`portals:1182` is a fidelity-conflict divergence** (the stanza pins crdb's
  CHECK-violation wording; ours is real PostgreSQL's), so **the stanzas AFTER
  :1182 are not exercised**. They cover 42P03 `cursor "p" already exists as
  portal` — a `DECLARE` colliding with an open portal name — which is NOT
  implemented.
- **A comparison against a parameter written as a CAST (`v = $1::uuid`) is
  still unjudged.** The declared-parameter form raises 42883 at Parse via
  `_PARAM_CATEGORY`; the cast path consults `_CATEGORY` only, which does not
  model uuid / bytea / inet.
- [ ] **pgtest: two unexpected failures, `prepare` and `tuple`.**
  `docs/validation-report-pgtest.md` (generated 2026-09-21) reads 47/66 files
  pass, 12 expected divergences, 2 unexpected failures, 5 skipped — the
  campaign had closed at zero unexpected. Not diagnosed here; re-run the
  gauge (below) and read the per-file output.
- **Re-runs:** drive the runner directly —
  `PYTHONPATH=. .venv/bin/python -m pgtest_validation.runner` then
  `... -m pgtest_validation.generate_report .validation/pgtest-raw.json
  docs/validation-report-pgtest.md`. `invoke validate-pgtest` spawns the daemon
  without `PYTHONPATH` and dies with "pg daemon did not become ready within
  15s" (the same wrapper issue the psycopg gauge has).

### SQL / PostgreSQL section — survey (2026-08-14)

- [ ] **Cross-type comparisons: the remaining lenient pairs.** The plan-time
  42883 analysis in `src/secantus/sql/typecheck.py` raises `42883 operator does
  not exist: text = integer` for a comparison between two *confidently-typed*
  operands drawn from two different categories, on declared tables. What is
  still deliberately lenient, and why:
  - **Reflected (schema-on-read) tables are exempt wholesale.** A reflected
    column's type comes from sampling 50 documents, so a heterogeneous BSON
    field can be declared `text` while holding integers; the dual-protocol
    path *wants* the cross-BSON-type comparison. This is not a gap to close —
    it is the correct answer for that table kind.
  - **Categories not modelled**: `bytea`, `uuid`, `money`, `oid`,
    `interval`, `time`/`timetz`, `"char"`, arrays, ranges, geo, network, bit
    (as column types; uuid / bytea / inet are modelled for DECLARED parameters
    only, via `_PARAM_CATEGORY`). Several of those pairs genuinely error in
    Postgres (`bytea = text`, `uuid = text`, `interval = integer`), but the
    implicit-cast graph is subtler and a false 42883 is worse than the lenient
    FALSE. `json` and `jsonb` share one storage tag, so `jsonb = json` stays
    lenient where PG rejects it; splitting that needs the declared oid.
  - **Same-category pairs Postgres still rejects**: `enum = text` (an enum
    column carries the `text` tag here), `citext = name`. Lenient by design.
  - **Operand shapes not typed**: bound parameters, subqueries, arithmetic,
    `||`, `CASE`, and every function outside the small text-preserving /
    int-returning allowlist. Also `IN`-lists and `BETWEEN` (only the six binary
    comparison operators are analysed).
  - **Statement shapes skipped**: CTEs, derived tables, set operations, table
    functions in FROM, views, and any comparison nested inside a subquery
    scope. Also **FROM-less selects** (`SELECT 'a'::text = 1`) — the analysis
    is driven by declared column types, and constant-only expressions are most
    of what the psycopg / SQLAlchemy gauges evaluate, so widening to them wants
    its own gauge run.
  - **Not yet verified against the psycopg / sqllogictest / SQLAlchemy
    gauges** (`invoke validate-psycopg` / `validate-slt` /
    `validate-sqlalchemy`) — the vendored gauges are the only population large
    enough to expose a false 42883.
- [ ] **Wide `numeric` values equal but for trailing zeros split in a join key**
  (measured 2026-09-19). `1e40` and `1e40.0` are one value in Postgres. Both are
  stored as the wide `{__numeric, __numkey}` document when wider than
  Decimal128. `GROUP BY`, `DISTINCT`, `count(DISTINCT)`, set operations,
  `DISTINCT ON`, `PARTITION BY` and ordinary numeric joins all key on the value
  now (`numeric.group_key_expr`, `numeric.eq_key`;
  `tests/test_pg_numeric_wide_grouping.py`). Two shapes remain:
  - **A join whose key is wide.** The wide document reaches `$lookup` as a
    document and is compared whole. Fixing it means the `let` / `pipeline`
    form, which runs per outer doc — so every numeric join would pay O(N*M)
    for a case that needs 35+ digits. Do it only behind a cheap test for
    whether the column actually holds a wide value.
  - **A value stored narrow in one row and wide in another.** `1.5` fits a
    Decimal128; `1.5` written with 39 digits does not, so it stores wide, and
    PostgreSQL says the two are equal (14.24). The group key keeps the two
    forms in different key spaces — a scale-free string for wide rows, the
    number itself for narrow ones — and no MQL expression can derive one from
    the other (a `$toDecimal` of the wide text would round at 34 digits and
    merge genuinely different values). A uniform key needs the value computed
    in Python per row, which the pipeline plan's `pre_eval_fields` hook could
    do for a single table but not for a join.
- [ ] **pgx `TestDeadlineContextWatcherHandler/DeadlineExceeded_with_DeadlineDelay`
  fails intermittently on CI** (2026-09-19, #1509's and #1518's gauge runs;
  passes on re-run). The test runs `select 1, pg_sleep(0.250)` under a 100 ms
  context deadline plus a 500 ms `DeadlineDelay`, so it fails when the Python
  pgserver takes more than ~600 ms to answer: `timeout: context deadline
  exceeded`. On #1518 the Exec at `pgconn_test.go:4726` failed 1.4 s in, while
  the parallel `TestCancelRequestContextWatcherHandler` subtests and
  `TestConnExecBatchHuge` were running. Measured locally (idle, 20 runs): the
  server adds ~15 ms to a 250 ms sleep; a concurrent or abandoned sleep on
  another connection does not serialize with it; three connections running
  2,000-statement pipelines raise the max only to 309 ms. So the stall is not
  reproduced off CI. Next step: a timing trace on the runner itself — log
  per-statement server-side start/end in the pgx lane. Leave open until it
  recurs with a trace or a CI timing run rules the server out.
- [ ] **HAVING on a numeric aggregate compares at Decimal128 precision: joins,
  grouping sets and FILTER only** (Python pgserver, 2026-09-19). For a
  single-table GROUP BY (with or without windows) and for an aggregate with no
  GROUP BY, HAVING takes the per-grouped-row residual and sees the exact folded
  value (`tests/test_pg_having_exact_numeric.py`). Still in-pipeline, where
  `$sum` rounds at 34 digits and skips a wide value:
  - JOIN + GROUP BY (`_join_having_to_match`): there is no residual on the join
    paths.
  - GROUPING SETS / ROLLUP / CUBE: nothing catches the 0A000 there.
  - `sum(v) FILTER (WHERE ...)` in HAVING: the residual evaluator rejects a
    FILTER term (0A000 "unsupported scalar expression"), so the lowerer keeps
    it in the pipeline.
  A Python post-filter after the fold would cover all three. Each path would
  need to carry its hidden `__having_*` fields through the `$project`.
- [ ] **Reject connections to non-existent databases (`test_pgconn_error`,
  `test_connect_bad`).** SecantusDB creates a database on demand (the
  ephemeral-db model shared with the Mongo server), so connecting to
  `dbname=nosuchdb` succeeds where real PG raises. A deliberate divergence —
  rejecting unknown dbs would break the in-process test-db ergonomic.
- [ ] **Write-conflict retry semantics (40001 under contention)**: concurrent
  writers to the same row surface WiredTiger's optimistic write conflict as a
  PG-SERIALIZABLE-style `40001 could not serialize access` instead of real
  PG's READ COMMITTED block-and-proceed. Autocommit statements (and a lone
  statement in a pipeline's implicit transaction) already retry from a fresh
  read (`executor.py`, `pgextended.py`); inside an explicit transaction block
  the 40001 reaches the client. Clients that retry (pgbench 15+
  `--max-tries`, retry-loop applications) behave correctly; ones that treat
  40001 as fatal abort. A server-side fix means statement-level replay after
  waiting out the competing transaction (WT invalidates the whole txn on
  conflict, so the user transaction's prior statements must replay too).
  Until then the sql-stress smoke keeps its write lanes single-client.
- [ ] **pgjdbc gauge — remaining clusters** (`invoke validate-pgjdbc`;
  `docs/validation-report-pgjdbc.md`, generated 2026-09-21: 5732 P / 59 F /
  28 S = 98.9%). Read the per-message counts off the JUnit XML in
  `vendor/pgjdbc/pgjdbc/build/test-results/test/*.xml` rather than eyeballing
  class totals — the report lists test ids only, and attributing a class's
  failures to the wrong cause once already cost a feature that fixed nothing
  here. Re-measure before planning: a stale artifact silently invalidates any
  of it. What the report still lists, with what is known of each:
  - **`DatabaseMetaDataTest`** (16 failures / 10 distinct as of 2026-09-28,
    across the jdbc2 + jdbc4 + jdbc42 copies). **Every one is filed with a
    reason** — see the `json`/`jsonb`, `PARTITION BY`, `columnPrivileges`,
    `getProcedureColumns`, `LANGUAGE internal`, `PARALLEL SAFE` and
    `indoption`/`getIndexInfo` entries in §3. Two are marked deliberately NOT
    worth doing (they need PostgreSQL's ~3,000 builtin `pg_proc` rows); the
    rest need a feature, a self-describing catalog, or a decision from Joe.
  - **`ArrayTest`**: `testNonStandardBounds` (negative array subscripts
    `intarr[-1]` unsupported in the planner) and `testUnknownArrayType` (next
    probe is the exact metadata query). **`EnumTest::enumArrayArray`**: nested
    enum-array resolution returns no rows — needs the exact `TypeInfoCache`
    query captured.
  - **`UpdateableResultTest`**: `testReturnSerial` (serial
    `RETURN_GENERATED_KEYS`) and `testOidUpdatable` (oid-column updatability).
  - **`RefCursorFetchTest`** (`initializationError`): the class's seeding
    INSERT fails because its `string_agg` ARGUMENT is
    `lpad(to_hex(width_bucket(random(),0,1,256)-1),2,'0')` and
    `_agg_arg_to_expr` can't compile `lpad` / `to_hex` / `width_bucket` /
    `random` into pipeline expressions. Either extend the pipeline expression
    vocabulary or evaluate aggregate args per row in Python on the derived
    path.
  - **`StatementTest`**: `parsingSemiColons` (CREATE RULE — rewrite rules);
    `warningsAreAvailableAsap` / `closeInProgressStatement(Protocol32)` /
    `concurrentWarningReadAndClear` (mid-statement NOTICE streaming +
    blocking LOCK TABLE + plpgsql FOR; the Protocol32 variant additionally
    needs NegotiateProtocolVersion for a 3.2 StartupMessage); `updateCount`.
  - **`PreparedStatementTest`**: `testDoubleQuestionMark` (`?-` prefix / `?#`
    infix geometric operators — sqlglot can't tokenize either, needs a parser
    extension, and the `lseg '...'` / `box '...'` typed-literal prefix syntax
    doesn't parse outside `point`); `testUnknownSetObject` (extended-protocol
    strictness: a varchar-typed param inserting into an interval column must
    raise 42804; our insert coercion doesn't see the param's declared OID);
    `testNegativeNumericScale`, `testTrailingSpaces`.
  - Also listed: `AutoSaveTransactionSettingsTest::setLocalTransactionIsolationLevelAsFirstStatement`,
    `CopyTest::lockReleaseOnCancelFailure`, `CursorFetchSqlTransactionTest`
    (×3), `CustomTypeWithBinaryTransferTest::testCustomBinaryTypes`,
    `DatabaseEncodingTest::encoding`,
    `DatabaseMetaDataCacheTest::getSQLTypeQueryCache`,
    `GeometricTest::testPGline`,
    `LoginTimeoutInterruptTest::loginTimeoutInterruptsAuthPluginSleep`,
    `ResultSetTest::testRowResultPositioning`,
    `SearchPathLookupTest::searchPathNormalLookup`, and two `TimestampTest`
    failures — none diagnosed here.
  - **Large objects — known divergences, deliberately accepted:** LO
    descriptors close at session end, not transaction end (PG closes them at
    commit/rollback; no gauge test distinguishes the two); `lo_manage` trigger
    DDL is an inert no-op (replaced large objects are never unlinked; nothing
    vacuums orphans here anyway); `lo_import` / `lo_export` are absent
    (server-side file I/O; nothing drives them over the wire in the gauges).
- [ ] **OPEN — Cross-connection async NOTIFY — WORKS; only the pgjdbc re-run is
  outstanding.** Measured 2026-08-18: with one psycopg connection blocked in
  `notifies()` (both the timeout form and the endless form, which is what
  pgjdbc's `NotifyTest` uses), a `NOTIFY` on a second connection IS delivered.
  `NotifyTest` stays excluded from the pgjdbc gauge
  (`pgjdbc_validation/include_paths.py`) only because it has never been re-run
  since: if it did still hang, a JUnit timeout cannot interrupt that socket
  read and it would wedge the weekly gauge. Closing this is one gauge run —
  drop the `EXCLUDE_CLASSES` entry and run `pgjdbc_validation.runner`.
- [ ] **OPEN — a scalar subquery over a DERIVED table is `42P01 relation ""`
      (residual from sweep 4, 2026-09-01).**
      `SELECT (SELECT max(x) FROM (VALUES (1),(2)) t(x))`. The derived table
      inside a scalar subquery resolves to the empty name, so this is a
      scope-plumbing gap, not a typing one.
- [ ] **`UPDATE … SET serial_col = DEFAULT` is refused (0A000).** A serial's
      default draws from its sequence (`nextval`), which the planner cannot do,
      so it is refused rather than guessed at; PostgreSQL draws the next value.
      Pinned as refused in `tests/test_sql_update_default.py` (the `sq (id
      serial, v int)` case). Plain and GENERATED-column `SET col = DEFAULT` work.
- [ ] **OPEN — a NOT NULL violation does not consume a sequence value; a CHECK
      or UNIQUE violation does (measured 2026-09-01).** PG evaluates DEFAULTs
      (including `nextval`) BEFORE checking NOT NULL, so a failed insert still
      burns a serial value:

          INSERT ok            pg id 1     us id 1
          INSERT NOT NULL fail pg (error)  us (error)
          INSERT ok            pg id 3     us id 2      <-- no gap here

      CHECK and UNIQUE violations match exactly (both consume), so this is an
      inconsistency **within our own server** as much as against PG.

      Cause: the NOT NULL check runs in the PLANNER
      (`planner._insert_doc`, two sites) and the sequence draw runs in the
      EXECUTOR, so the check happens first. Matching PG means moving that
      validation across the planner/executor boundary — deliberately NOT done
      so far: planning is storage-free by design, the write-path validation was
      consolidated on purpose, and PG itself documents sequences as gappy, so a
      client depending on the exact value is already outside PG's contract.
      Worth doing only alongside other write-path work.
- [ ] **OPEN — we are LENIENT where PG raises 42883 on operand type (measured
      2026-09-01).** In the function x value-type matrix (`upper(1)`,
      `upper(true)`, `upper(date '…')` and similar) we RETURN A VALUE where PG
      refuses the call. Across 396 measured shapes the SQLSTATE agreed 52% of
      the time; almost every disagreement is this leniency.

      Same root as the "Cross-type comparisons: the remaining lenient pairs"
      entry — closing it needs declared-operand-type modelling per function,
      and a FALSE 42883 is worse than a lenient answer, so it wants the same
      care that entry describes. One shape went the other way:
      `to_char(time, …)` succeeds in PG and errored here.
- [ ] **OPEN — `GROUPING SETS` rejects ANY computed projection (2026-09-02).**
      `SELECT g, count(*)::text FROM t GROUP BY GROUPING SETS ((g),())` is
      `0A000 expected a column, got: CAST(COUNT(*) AS TEXT)` — nothing to do
      with which aggregate it is. An honest refusal, but PG supports it, and
      the plain (non-computed) GROUPING SETS projection works, so the gap is
      the projection builder rather than the grouping. Pinned as refused by
      `tests/test_sql_nested_array_agg_order.py::TestStillRefused`.
- [ ] **OPEN — an ordered-set aggregate under a cast is refused (2026-09-02).**
      `mode() WITHIN GROUP (ORDER BY n)` works; `(mode() WITHIN GROUP (ORDER BY
      n))::text` is `0A000 unsupported aggregate: MODE()`, as is `+ 1`.
      `_ordered_set_agg` unwraps `exp.Alias` but not any other wrapper, so the
      nested scan reaches the bare `Mode` node — which is not itself the
      aggregate; the `WithinGroup` around it is.
- [ ] **OPEN — `stddev` / `variance` over a FLOAT input differ in the last bit
      (2026-09-02).** `stddev(f)` gives 1.5275252316519465 where PG gives
      1.5275252316519468. PG uses the **Youngs-Cramer** update for float8
      (`float8_accum`), which is a different rounding path from summing the
      squares. Matching it needs the values in order, so a `$push` rather than
      an accumulator — memory-heavy for a last-bit difference. The exact
      (numeric) path matches digit for digit.
- [ ] **OPEN — `avg(DISTINCT x)` and an aggregate with FILTER keep the float
      path (2026-09-02).** `avg(DISTINCT i)::text` gives
      `2.3333333333333335` for PG's `2.3333333333333333`, and
      `avg(i) FILTER (WHERE i > 1)::text` is `0A000 unsupported scalar
      expression`. Both were excluded from the exact-numeric path
      deliberately: DISTINCT reduces a `$addToSet` and FILTER folds a
      condition into the accumulator, so neither reaches
      `_register_numeric_avg` as written.
- [ ] **OPEN — a JSON null sorts as SQL NULL (2026-09-02).** `'null'::jsonb`
      and SQL NULL are both Python `None` in this engine, so a JSON null goes
      to the NULLS FIRST/LAST end rather than into the jsonb order (where PG
      puts it just above a top-level empty array). Needs a distinct
      representation for JSON null, which is a storage-format change.
- [ ] **OPEN — `array_agg(x ORDER BY <mixed jsonb>)` orders inconsistently
      (2026-09-02).** No longer an error, but this path builds its sort key
      inside the Mongo pipeline (`_sorted_agg_push`), where the column type is
      not available — so the value-shaped fallback applies and a column mixing
      scalars with containers comes back in a non-transitive order. A
      partition of homogeneous containers is correct.
- [ ] **OPEN — `date_part()` returns numeric where PG returns float8
      (2026-09-02).** `date_part('epoch', ts)` is `double precision` in PG;
      `extract(epoch from ts)` is `numeric`. sqlglot parses BOTH to
      `exp.Extract`, so the two spellings are indistinguishable at the AST and
      the type cannot be chosen without the source text. Values agree.

- [ ] **OPEN — two-argument `log(b, x)` returns float8, PG returns numeric
      (2026-09-02).** `log(2,8)` is `3.0000000000000000` in PG and `3.0` here.
      The type is easy; the SCALE is not — PG's `log_var` picks it from an
      ESTIMATE of the result's decimal weight, and six probed cases give 16,
      15, 15, 9, 16 and 16 decimal places, which no simple rule reproduces
      (`16 - adjusted(result)` matches only three). Needs a faithful port of
      `log_var`, not a guess.

- [ ] **OPEN — `jsonb_agg(...)` inside a computed projection (2026-09-02).**
      `jsonb_agg(i)` works alone; `jsonb_agg(i)::text` is
      `0A000 function jsonb_agg() is not supported`. sqlglot models it as an
      `Anonymous` call, and the aggregate collector walks
      `find_all(exp.AggFunc)`, which never sees it. `json_agg` has a dedicated
      node and is fine. Widening the collector to named anonymous aggregates
      is the fix, with the regression risk that implies.

- [ ] **OPEN — `to_char(numeric, …)`'s `V` and `TH` answer instead of refusing
      (2026-09-02).** Three numeric template patterns are unimplemented, and
      two of them return a WRONG number rather than an error, which is the bad
      failure mode:

      | template | PG 14.13 | SecantusDB |
      | --- | --- | --- |
      | `to_char(42, '9V99')` | `` ` ###` `` | `` `  42` `` |
      | `to_char(42, '999TH')` | `` `  42ND` `` | `` `  42` `` |
      | `to_char(42, '9.9EEEE')` | `` ` 4.2e+01` `` | `` ` #.#` `` |

      `V` shifts the value by that many digits (and overflows here, hence PG's
      `#`); a NUMERIC `TH` appends the ordinal — the datetime `TH` is
      implemented, this one is not. `EEEE` at least prints `#`, which is wrong
      but not silently plausible. (`src/secantus/sql/numformat.py`'s docstring
      lists all three as out of scope.)

- [ ] **OPEN — a year-less `to_date` / `to_timestamp` template defaults to
      1 BC, and BC dates are unrepresentable (2026-09-02).**
      `to_date('11:30:00 PM','HH12:MI:SS AM')` is `'0001-01-01 BC'` on
      Postgres; SecantusDB answers `'0001-01-01'` (AD). Python's `datetime`
      has a minimum year of 1 AD and no era, so this is not a formatting bug
      to fix in `datetimeformat.py` — the whole engine stores datetimes as
      `datetime.datetime` and cannot represent any BC instant. Fixing it means
      a proleptic date representation of our own, which is a much larger
      change than the two cases it buys. The only two divergences left in the
      314-case datetime sweep.

- [ ] **OPEN — `EXISTS (subquery)` in UPDATE / DELETE (2026-09-02).** Still
      `0A000`. Unlike `IN`, `_expr_to_filter` refuses `exp.Exists` outright:
      the SELECT path handles it as a RESIDUAL predicate evaluated per row, and
      `UpdatePlan` / `DeletePlan` have no residual field. Needs one, plus the
      executor filtering fetched docs through `scalar.evaluate` before the
      write.

- [ ] **OPEN — `UPDATE … FROM (VALUES …)` (2026-09-02).**
      `UPDATE t SET n = s.x FROM (VALUES (1,111)) AS s(k,x) WHERE t.id = s.k`
      is `0A000 unsupported MERGE source: …` — a message naming MERGE for a
      statement the user wrote as an UPDATE. `UPDATE … FROM <table>` works.

- [ ] **OPEN — `sqrt(numeric)` returns float8 (2026-09-02).** PG returns
      NUMERIC at a scale its `sqrt_var` estimator picks — probed:
      sqrt(2)=1.414213562373095 (15 dp), sqrt(10000)=100.0000000000000 (13 dp),
      sqrt(0.5)=0.70710678118654752 (17 dp). No simple `16 - weight` rule fits.
      **Same class as the two-argument `log`**: needs a faithful port of PG's
      scale estimator, not a guess. Do both together.

- [ ] **OPEN — a scalar subquery under GROUP BY is typed text (2026-09-02).**
      `SELECT g, (SELECT max(n) FROM t b WHERE b.g=t.g) FROM t GROUP BY g`.
      The projection is typed with the SYNTHETIC resolver, under which the
      inner table's column does not resolve. The ungrouped form works. Needs
      the catalog at that point.

- [ ] **OPEN — arithmetic in a WHERE clause is still unchecked (2026-09-02).**
      `SELECT * FROM t WHERE i + 1 > 0` returns rows where PG raises `22003`.
      The predicate lowers to a Mongo `$expr` (`planner._to_agg_expr` /
      `_null_guarded_expr_cmp`) and is evaluated by `secantus/expressions.py`
      — the operator engine the **MongoDB server shares**. Range-checking there
      would put PostgreSQL's integer widths into a Mongo operator, which the
      layer rules forbid, and Mongo's expression language has no way to raise.

      Fixing it properly means a PG-specific `$expr` evaluation path, or
      refusing to lower integer arithmetic and running the predicate in Python
      (a real slowdown on a common path for a rare error). Neither is a tweak —
      size it from a probe before starting. Same three shapes reproduce it:
      `WHERE i + 1 > 0`, `WHERE i * 2 > 0`, `WHERE 1e308::float8 * 10 > 0`.

- [ ] **OPEN — `hashtext()` deliberately still unsupported (2026-09-02).**
      `hashtext()` is PG's INTERNAL hash: its values come from `hash_any` and
      cannot be reproduced without porting it, so returning a different number
      would be worse than refusing. Only worth revisiting if a driver or gauge
      actually calls it.

- [ ] **pgx gauge** (`invoke validate-pgx`, `docs/validation-report-pgx.md`):
  the current report (Python server, 2026-09-21) is 377 P / 1 F / 22 S =
  99.7%; `bgreader` / `ctxwatch` / `pgproto3` at 100%, `pgconn` 99.4% with one
  failure, `TestConnExecBatchHuge`. The earlier remaining work was a
  load-sensitivity re-measurement of the `TestDeadlineContextWatcherHandler`
  family on a quiet machine.

- [ ] **OPEN — `char(n)` padding is invisible to `octet_length()` and to array
  elements.** `char(n)` is stored unpadded and padded at render, so
  `octet_length('xy'::char(4))` returns 2 where PG returns 4 (the scalar
  evaluator has no access to the declared width), and `char(n)[]` elements are
  not padded. Reproduce: `SELECT octet_length('xy'::char(4))`.

- **`::STRING` — WON'T FIX.** The crdb type alias parses and behaves as
  `text`, where PG raises `42704 type "string" does not exist`. sqlglot
  normalises `STRING` to `TEXT` at parse time, so detecting it would mean
  sniffing the raw SQL string, which is fragile (a column named `string`, a
  literal containing `::STRING`) for a spelling no PostgreSQL client emits.
  Accepting a non-PG alias is a permissiveness, not a wrong answer.

- [ ] **OPEN — casts to text for geo and other composite targets.** `_eval_cast`
  converts int / float / Decimal / Decimal128 / bool / interval / bit to PG's
  text spellings; geometric and other composite values are still unconverted.
  Reproduce: `SELECT point(1,2)::text` and compare with PG.

- [ ] **OPEN — Multi-way comma-join performance — much improved, `select4`/`select5`
  still over the 300s cap.** Single-table WHERE conjuncts already move to the
  stage that produces their rows, and unqualified cross-table equalities key
  the comma joins (`_push_single_table_predicates` /
  `_key_comma_joins_from_where`).

  **Not sound to push into a LEFT join's lookup** — WHERE runs after the join, so
  a predicate on the right table must delete the outer row; only an alias whose
  `$unwind` is non-preserving is eligible.

  **Remaining:** both files run but exceed the runner's 300s cap. The queries
  are well-constrained — the 6-way one expects 30 values — so what is left is
  the join *strategy*: unkeyed lookups materialise their whole foreign side per
  outer row. Index-backed or streaming lookups are the next step, not more
  predicate motion. They stay out of `slt_validation` INCLUDE until then.

- [ ] **Wire server: simple + extended protocol, trust + SCRAM auth, optional TLS.**
  `pgserver.py` speaks v3 startup, simple `Query`, extended
  `Parse`/`Bind`/`Describe`/`Execute`/`Close`/`Sync`, `SCRAM-SHA-256` auth + TLS,
  binary parameter input and binary result output for the common type-OID set.
  Still missing: channel binding (`SCRAM-SHA-256-PLUS`; `pgauth.py` says it is
  not offered), mTLS client-cert auth, and login users stored in the catalog —
  SCRAM users are constructor config (`users={}`), distinct from the SQL-level
  roles `CREATE ROLE` records. `psql`/JDBC as live gauges still need a libpq
  CLI / a JVM in the dev env.
- [ ] **Reflected tables are now read-write.** A collection with no
  `CREATE TABLE` reflects (sampled schema-on-read) for SELECT, aggregates, JOIN
  and INSERT / UPDATE / DELETE. Remaining gaps: in a JOIN, an *unqualified*
  reference to an *un-sampled* field of a reflected table can't be routed (the
  resolver matches on sampled columns — qualify it as `alias.field`, or it must
  appear in the 50-doc sample); type inference samples 50 docs and picks the
  first non-null type per field (no widening across conflicting types —
  first-seen wins, `reflect.py`).
- [ ] **OPEN — jsonb operator surface landed (one gap + one parser quirk).**
  **Still not modeled:** the lateral `jsonb_each_text` form
  (`FROM t, jsonb_each_text(t.doc)` — the value would need per-row text
  rendering inside the pipeline; `planner._is_jsonb_each_join` accepts only
  `jsonb_each` / `json_each`) and the base-less `SELECT jsonb_each(x)`
  composite form. **Parser quirk:** sqlglot reads a bare `f(a->'k')` arrow as a
  lambda, so a navigated *function argument* must be parenthesised
  (`f((a->'k'))`) or use `#>` (`f(a #> '{k}')`) — bare navigation in WHERE /
  projection is unaffected.
- [ ] **OPEN — Aggregate/JOIN path has gaps (P5 + later slices landed the core).** Still `0A000`: a mixed
  `LEFT`/`RIGHT` or any `FULL` 3+ table chain (a *pure*-`RIGHT` adjacent chain
  works, `_build_right_chain_pipeline`); in the trailing-composite builder
  (`_build_trailing_composite_pipeline`) a composite whose own joins aren't
  adjacent, an unqualified / `o2`-doesn't-touch-composite ON, a non-plain-table
  source, a non-adjacent `RIGHT` `ON`, and a second `FULL` in the tail;
  subqueries; GROUP BY keys using a function the aggregation engine can't
  evaluate (e.g. `substr`). SUM/MIN/MAX result typing is approximate (uses the
  column's tag; AVG → float8; arithmetic → numeric).
- [ ] **OPEN — a whole-table aggregate over an EMPTY table returns no row.**
  PG returns one row (NULL for `sum` / `max` etc.); here only `count` does. The
  same gap makes a bare scalar aggregate over an empty group in an
  `INNER`/`CROSS` `LATERAL` drop the outer row instead of keeping it with a NULL.
  Reproduce: `CREATE TABLE e (x int); SELECT sum(x) FROM e`.
- [ ] **OPEN — jsonpath subset.** `secantus/sql/jsonpath.py` raises
  `feature_not_supported` for arithmetic, item methods (`.size()` etc.),
  recursive `**`, `like_regex`, `exists()` inside a predicate, and `$var`
  bindings. `jsonb_path_query` is set-returning in PG but returns only the
  **first** match in a scalar SELECT (use `jsonb_path_query_array` for the set).
  Reproduce: `SELECT jsonb_path_query('[1,2]', '$.size()')`.
- [ ] **OPEN — `initcap` uses Python `str.title()`.** Matches PG for ASCII
  words; word boundaries on exotic Unicode may differ. No failing case recorded
  — probe against PG before working it.
- [ ] **OPEN — MERGE: an unqualified column ambiguous between target and source
  resolves to the TARGET**, where PG raises `42702`. Reproduce: a MERGE whose
  target and source both have column `v`, with a bare `v` in `ON` or `SET`.
- [ ] **OPEN — join DML: no self-join of the target back into the source list.**
  `UPDATE t … FROM t AS t2 …` / `DELETE FROM t USING t AS t2 …` are not
  modelled (`engine._run_update_from` / `_run_delete_using`).
- [ ] **OPEN — composite primary key edge cases.** Renaming a composite-PK
  column via `ALTER TABLE … RENAME COLUMN` doesn't rewrite the `_id.<name>`
  subdoc key; a SERIAL / identity column inside a composite PK is untested.
  (A reflected collection's `_id` can't be updated — `0A000`, by design: it is
  the real Mongo key.)
- [ ] **OPEN — computed `UPDATE … SET` gaps.** A computed *composite-type*
  subfield is coerced as a scalar (the nested composite value is not rebuilt);
  a SET RHS that is a correlated subquery over another table
  (`SET n = (SELECT … FROM u WHERE u.k = t.k)`) isn't modelled.
- [ ] **OPEN — domain gaps.** Domain-on-domain (`CREATE DOMAIN d2 AS d1`) isn't
  modelled, and `DROP DOMAIN` doesn't check for dependent columns (no
  RESTRICT / CASCADE dependency tracking).
- [ ] **OPEN — no `NULLS NOT DISTINCT` on UNIQUE.** NULLs are always distinct
  (PG's default); the PG 15 `UNIQUE NULLS NOT DISTINCT` form isn't modelled.
- [ ] **OPEN — CHECK columns aren't listed in
  `information_schema.constraint_column_usage`** (the predicate isn't parsed for
  referenced columns). FK / PK / UNIQUE rows are.
- [ ] **OPEN — `ALTER TABLE` constraint gaps.** Unnamed `ADD CHECK (…)` isn't
  accepted (sqlglot needs an explicit `CONSTRAINT name`); no `ALTER CONSTRAINT`
  and no table-level `VALIDATE CONSTRAINT` (only `ALTER DOMAIN … VALIDATE
  CONSTRAINT` exists, as a no-op).
- [ ] **OPEN — materialized-view gaps.** The unpopulated (`55000`) check only
  fires for a matview in the query's primary FROM, not a JOIN position;
  `REFRESH … CONCURRENTLY` recomputes without snapshot isolation; `ALTER
  MATERIALIZED VIEW` subcommands other than `RENAME TO` are `0A000`.
- [ ] **OPEN — FK `MATCH` clause renders as the default.** `MATCH FULL` is not
  modelled (enforcement is MATCH SIMPLE, `executor.py`).
- [ ] **OPEN — sequences: no `ALTER SEQUENCE … OWNED BY`, and `nextval`
  concurrency is unverified.** `nextval` was a read-modify-write with a
  duplicate-value window under concurrent `nextval` from different connections;
  it now pre-allocates a batch (`tests/test_sql_sequences.py:214`) — re-check
  whether that window is closed.
- [ ] **OPEN — SQL roles are a reflection / DDL record only.** They are distinct
  from the wire server's SCRAM users (constructor `users={}`), the password is
  not stored (only a `password_set` flag), and roles live in the connection's db
  rather than cluster-wide.
- [ ] **OPEN — table GRANT/REVOKE limitations.** Additive only — a table grant
  never *restricts* a broader Mongo role (`readWrite` still writes any table);
  `WITH GRANT OPTION` is tolerated but re-grant chains aren't checked;
  `TRUNCATE` / `REFERENCES` / `TRIGGER` recorded but not enforced; no table-owner
  tracking; multi-table / subquery statements get no table-grant fallback.
- [ ] **OPEN — SET ROLE limitations.** SET ROLE to a granted-but-unbound role
  isn't validated against role membership; the Mongo RBAC db-level gate still
  uses the login's `session.roles` (SET ROLE changes the table-grant identity and
  `current_user`, not the db-wide Mongo role bindings).
- [ ] **OPEN — row-level security limitations.** USING injection is
  single-table only (a join doesn't get the base table's policy); policies over
  the pipeline / set-operation / CTE paths aren't injected; no table-owner
  concept (RLS applies to every non-root role; `FORCE` behaves like `ENABLE`);
  RLS DDL needs no privilege.
- [ ] **OPEN — UDF reflection gaps.** No `proargdefaults`;
  `information_schema.routines.is_deterministic` is a fixed `NO`.
- [ ] **OPEN — column-privilege limitations.** Column-grant enforcement is
  single-table only (join SELECTs need a role or table grant); `count(*)` /
  no-column-ref SELECTs can't be authorized by a column grant alone;
  `REFERENCES` / `TRIGGER` column privileges aren't enforced; `is_grantable` is
  always `NO`.
- [ ] **OPEN — identity columns: no `OVERRIDING SYSTEM VALUE`, no `ALTER TABLE
  … ADD / DROP / SET GENERATED`.** A GENERATED ALWAYS column can't be
  force-overridden — the `428C9` error's hint names `OVERRIDING SYSTEM VALUE`
  (`planner.py`), which isn't parsed.
- [ ] **OPEN — enum gaps.** JOIN / GROUP BY / evaluated plans carry
  `out_columns` as `(name, tag)` pairs and may still describe an enum result
  column as `text` 25 rather than the enum oid; no `pg_type` row for the paired
  `_name` array type (only `typarray` on the base row); `mood[]` table columns
  aren't supported; enums live in the connection's db, not schema-scoped.
- [ ] **OPEN — `ALTER TYPE … RENAME VALUE` is `0A000`**
  (`tests/test_sql_alter_type.py:94`), as are composite-type alters.
- [ ] **OPEN — generated-column gaps.** No `ALTER TABLE … ADD COLUMN …
  GENERATED` (the ALTER ADD path doesn't parse the constraint); no guard against
  subqueries / volatile functions in the expression. (A generated column isn't
  re-derived when data is written through the Mongo API — SQL writes only, by
  design.)
- [ ] **OPEN — COPY to / from server-side files.** Only `STDIN` / `STDOUT`
  (the client streams, like `\copy`); the embedded `run_sql` API can't drive
  the streaming COPY sub-protocol — it's wire-only.

- [ ] **OPEN — Partial indexes landed** (b110): `CREATE INDEX … WHERE <predicate>`
  lowers the predicate to a Mongo `partialFilterExpression`. **Still open:**
  `pg_index.indpred` reflects as NULL (`virtual.py`), so SQLAlchemy's
  `get_indexes` won't report the index as partial — rendering the Mongo filter
  back to a SQL predicate for `pg_get_expr` isn't done. `pg_index.indpred IS NOT
  NULL` answers `false` where PG answers `true`.
- [ ] **OPEN — `LATERAL` / `DISTINCT ON` gaps.** `JOIN LATERAL` with a
  non-`TRUE` ON is rejected (`tests/test_sql_lateral_distinct.py:195`; correlate
  in the subquery WHERE instead); a rich lateral encountered outside a
  join-pipeline context raises `0A000`; `DISTINCT ON` doesn't enforce PG's
  "ORDER BY must start with the DISTINCT ON exprs" rule.
- [ ] **OPEN — GROUPING SETS gaps.** A subquery in `HAVING` alongside a window
  over GROUPING SETS → `0A000`; a correlated / per-row WHERE with GROUPING SETS
  over a JOIN → `feature_not_supported`.
- [ ] **OPEN — pipeline ORDER BY gaps.** Still `0A000`: a non-aggregate
  computed ORDER BY expression not in the select list, and ORDER BY an
  unselected aggregate under GROUPING SETS (the union branches don't share
  hidden fields).
- [ ] **OPEN — `ALTER COLUMN … TYPE` doesn't recast existing rows.** It retypes
  the catalog only (`executor.py`, "retype in the catalog"); already-stored BSON
  values keep their old type.
- [ ] **Transactions: single-connection atomicity; savepoints are real.**
  **Limitations:** savepoints are a collection-granularity snapshot
  (`O(rows-in-touched-collection)` per first-write-per-savepoint, not a
  WT-native savepoint); recovery after a storage-engine `WT_ROLLBACK`-class
  error (vs an ordinary constraint violation) may leave the WT txn unusable for
  the restore writes. `DISCARD` remains a no-op (`engine._NOOP_WORDS`).
  Server-side cursors (`DECLARE` / `FETCH` / `MOVE` / `CLOSE`): the cursor is a
  materialized snapshot at DECLARE (later same-txn writes aren't visible
  through it), and it isn't wired into the extended protocol's Portal
  machinery.
- [ ] **OPEN — row-locking clauses are accepted no-ops.** `SELECT … FOR UPDATE
  | FOR SHARE | …` return the rows without locking anything; `FOR UPDATE` on an
  aggregate / `DISTINCT` / set-op / GROUP BY is accepted where PG rejects it;
  `OF <table>` validation runs only on the direct single-table / JOIN
  `_run_select` path.

- [ ] **`TRUNCATE` has no `ONLY` / partition semantics.** `engine._run_truncate`
  empties each table with a bulk `storage.delete_matching(db, coll, {})`, not
  the O(1) file-truncate a real engine does. Reproduce: `TRUNCATE ONLY t`
  against a partitioned table and compare with PostgreSQL.
- [ ] **`pg_get_indexdef` / `pg_indexes.indexdef` detail gaps.**
  Expression/functional index columns reflect only when every key field maps
  to a declared column (an index over a raw field is skipped), and `indexdef`
  carries no opclass or collation detail. Reproduce: `CREATE INDEX ... ((lower(c)))`
  or `... (c COLLATE "C")`, then `SELECT indexdef FROM pg_indexes`.
- [ ] **`pg_locks` shows only this connection's advisory locks.**
  `virtual._pg_locks` reflects the calling session's held advisory locks
  only: no other backends' locks, and no non-advisory lock types
  (relation / tuple / transactionid rows). `objsubid` / `tuple` reflect as
  `int4` (no `int2` type tag). Reproduce: hold `pg_advisory_lock(1)` on one
  connection and `SELECT * FROM pg_locks` on another.
- [ ] **GUC metadata and transaction-end `ParameterStatus`.** When a
  transaction end reverts a *reportable* GUC (`search_path` etc.), no
  compensating `ParameterStatus` is sent. `pg_settings` metadata is coarse:
  generic category, empty short_desc, NULL unit/min/max/enumvals, no per-GUC
  context. Reproduce: `BEGIN; SET LOCAL search_path = x; COMMIT;` and watch
  the client's reported parameters; `SELECT * FROM pg_settings` against
  PostgreSQL.
- [ ] **`pg_stat_database` counters are always 0; `pg_stat_activity`
  omits live fields.** `xact_commit` / `blks_hit` / `tup_*` are `0` (no stats
  collector). `xact_start` / `state_change` / `wait_event*` / `leader_pid` /
  `backend_xid` are NULL. `client_port` is NULL and `client_addr` is text,
  not `inet`. The COPY sub-protocol and the initial handshake don't update
  `state`.
- [ ] **Role membership is recorded but not enforced.** After `GRANT role TO
  member`, the member does not inherit the group role's table grants: the
  authz gate keys off direct grants only, and `list_role_memberships` is read
  only by the catalog views. `CASCADE` / `RESTRICT` on REVOKE are accepted but
  ignored. A membership naming a non-role reflects with `oid 0` in
  `pg_auth_members`. There is no cycle detection. Reproduce: grant SELECT to
  role `g`, `GRANT g TO u`, then SELECT as `u`.
- [ ] **Prepared (2PC) transactions are in-memory and simple-protocol only.**
  `PreparedXactRegistry` does not survive a server restart (PostgreSQL
  persists to `pg_twophase`). `PREPARE TRANSACTION` / `COMMIT PREPARED` work
  only over the simple query protocol: `engine._maybe_two_phase` intercepts
  them before sqlglot in `run_sql`, and the extended Parse/Bind path does not
  reach it. Reproduce: send `PREPARE TRANSACTION 'g'` through a driver's
  prepared-statement path, or restart the server between PREPARE and COMMIT
  PREPARED.
- [ ] **`ORDER BY` on an expression-index expression is not
  index-accelerated.** The hidden `__expr_<name>` field is projected away
  before the pipeline `$sort`, so the sort is correct but runs without the
  index. `ALTER TABLE`-driven index changes were also listed as missing.
- [ ] **Dev-env import shim:** `tests/conftest.py` stubs `wiredtiger` only when the
  extension is absent so the pure SQL/operator tests import without a WT build. Inert in
  CI. Revisit if `secantus/__init__` is made lazy (would let `secantus.sql` import without
  dragging in the WT-backed server).

## Concurrency races (found by the concurrent stress suites, 2026-07-16)

- **DEFERRABLE constraints keep the cross-transaction UNIQUE race (decision).**
  SQL `UNIQUE` / `PRIMARY KEY` are enforced by the storage-level unique-keys
  table (`table:secantus_unique_keys`), so cross-transaction and
  simultaneous-uncommitted duplicates collide on the key itself. DEFERRABLE
  constraints deliberately keep the commit-time check (an every-write index
  would reject legitimately-transient violations, e.g. the value-swap case)
  and therefore keep the old race — a narrow, documented trade.

## `numeric` beyond Decimal128's 34 significant digits (2026-08-01)

**Anything that consumes a numeric value must unwrap it first**
(`typemap.unwrap_numeric`): `Decimal128` implements no Python numeric
protocol, so `int()`, `float()`, arithmetic, comparison and unary minus all
reject it outright.

**Residual: values beyond Decimal128's 34 significant digits.** The pgjdbc
numeric cluster's remaining 4 failures are
`SELECT 0.1000000000000000000000000000000...` and its integer counterpart,
which carry more than 34 significant digits and so round when stored as
Decimal128. Postgres' numeric is arbitrary precision. Fixing it means not
using Decimal128 as the carrier — a bigger change, since Decimal128 is what
the storage layer stores for a `numeric` column — and it is the same limit
already noted against `typemap.coerce`.

## ORDER BY does not sort rows expanded by a set-returning function (2026-08-02)

`ORDER BY` over `unnest` (ordinal, alias, `DESC`) and `unnest` element types
are fixed (`tests/test_sql_pg_expandarray.py`, `TestSrfOrdering` /
`TestSrfElementTypes`). Sort keys for an SRF-produced output come off the
expanded tuple (`EvaluatedSelectPlan.order_srf_output`); `DISTINCT ON`
deliberately stays row-level and must not be conflated with it. Still open:

- **The record-SRF field form** `(information_schema._pg_expandarray(arr)).x`
  plans by a different route the fix does not reach, so `ORDER BY` over it
  keeps array order; and `(...).n AS n ... ORDER BY n` answers `42703`.
- **`pg_typeof(<set-returning function>)` returns ONE row where PG returns N
  (obscure).** `SELECT pg_typeof(generate_series(1,3))` answers one row;
  PG answers three. `planner.rewrite_pg_typeof` folds the call to a literal at
  plan time — right for the *type*, but it discards the argument, and with it
  the SRF's row multiplication. Fixing it means keeping the SRF in the
  projection and emitting a constant per row.

A live PostgreSQL 14 is on this machine (`host=127.0.0.1 port=5432`;
`SECANTUS_PG_ORACLE_DSN` points the sub-ms reference test at it).

## pgjdbc `UpdateableResultTest`: the last 8 failures (2026-08-02)

pgjdbc `UpdateableResultTest`'s last 8 failures (as of the fix) are a mix
rather than a cluster: two more `column ""` (a select shape that still
describes without a base table — likely the pipeline/evaluated path, which
builds its columns from `(name, tag)` pairs with no Column object to carry the
table oid / attnum), a timestamp-precision mismatch, an `INSERT has 2 values
but 3 columns`, and a "resultset tuples, but no field structure" protocol
complaint. Find what the driver sent by logging the statement server-side
during a `validate-pgjdbc` run rather than reconstructing the query by hand.

## Weekly validate.yml failures (2026-08-03 triage)

- [ ] **Don't run the pgjdbc gauge alongside the test suite.** It is
  timing-sensitive, and its wall clock under contention says nothing about the
  server: a run competing with a full pytest suite blew the (then 1h) budget
  while the same code re-run alone finished in **8m16s**. Note also that a
  healthy full run is legitimately long — `CopyLargeFileTest` alone is ~282s,
  inherently, since it streams a large file through COPY (`CopyTest` beside it
  is 1.8s). Check `uptime` before drawing any conclusion from gauge timings;
  this machine has repeatedly carried runaway processes from parallel sessions.
- **Read-only transactions: known divergences from PG**, none exercised by any
  gauge: temp-table writes are also blocked (PG allows them in read-only
  txns), and `SELECT … FOR UPDATE` / `nextval()` are NOT blocked (PG blocks
  both). Note `pg_stat_activity.xact_start` is a hardcoded stub and `state`
  never reports "idle in transaction" — do not use them to rule out open
  transactions.

## `character(n)` blank padding (2026-09-03): four gaps left

`char(n)` stores unpadded and pads on the way out (`typemap.blank_pad`); a
bpchar-to-text conversion strips trailing blanks. The set of functions that pad
(`octet_length` / `concat` / `concat_ws` / `format` / `to_json` / `to_jsonb` /
`::bytea`, and the pattern operators) is MEASURED against PostgreSQL 14.13 —
`length` is deliberately outside it while `octet_length` is inside, so do not
"complete" it by adding string functions on the assumption that they behave
alike. Pinning tests in `tests/test_sql_sweep_thirteen.py`.

Four gaps remain, all recorded rather than guessed:

1. **`array_agg(c)` aggregates the unpadded value** (PostgreSQL gives
   `{"ab   "}`, and types the result `bpchar[]`/1014 where we say `text[]`/
   1009). It belongs in the output-form set by behaviour, but the aggregate
   planner takes a COLUMN, not an expression — wrapping it raised `0A000
   unsupported aggregate argument: RPAD(c, 5)`. Refusing the query is worse
   than the unpadded value, so it deliberately keeps the old answer. Fixing it
   means teaching the aggregate path to accept an expression argument.
2. **The CAST form of the output-form functions is still unpadded** —
   `octet_length('a'::char(3))` is 1 where PostgreSQL says 3, and likewise
   `concat('a'::char(3), '|')` and `to_json('a'::char(3))`. The padding rewrite
   is plan-time and keyed on the CATALOG (`col.decl_oid == BPCHAR_OID`), so a
   cast expression, which has no catalog column behind it, is invisible to it.
   The column form — the one that occurs in real queries — is correct.
3. **A `char(n)` COLUMN inside a record constructor loses its padding** —
   `SELECT (t, c)::text` gives `(ab,ab)` where PostgreSQL gives
   `(ab,"ab   ")`. The record path pads a `char(n)` CAST field
   (`_blank_pad_record_field`), which is all it can see: the scalar layer has
   no catalog, so a column's declared width is not available there.
4. **`'a'::char(3) = 'a  '` answers false**, where PostgreSQL says true.
   `_strip_bpchar_literals` makes the comparison blank-insensitive by trimming
   the literal, but it too only recognises a bpchar COLUMN, not a cast.

Also found by the same sweep: **`record::text` renders the record as JSON** —
`SELECT ('a'::text, 'd'::char(2))::text` gives `{"f1": "a", "f2": "d "}` where
PostgreSQL gives `(a,"d ")`. The record's own text renderer is right
(`typemap.to_pg_text(val, "composite")` produces the PostgreSQL form, and
`tests/test_sql_composite_type.py` pins it); it is the `::text` CAST that does
not route to it. **`to_json` reports oid 3802 (`jsonb`) where PostgreSQL
reports 114 (`json`)** (see the json / jsonb identity entry below).

## Extended query protocol (2026-09-03) — swept, three fixes, three gaps left

Probed with `tools/probes/pg_corpora/params.sql` (run three ways per line —
text parameters, a server-side PREPARED statement, and BINARY result format)
plus the stateful scenarios in `tests/test_sql_sweep_fourteen.py`. Three gaps
remain:

1. **`int[] @> $1` accepts a `smallint[]` parameter** where PostgreSQL raises
   `42883 operator does not exist: integer[] @> smallint[]`. psycopg sends a
   small-int list as `smallint[]`, and we are more permissive than PostgreSQL
   about array element types. Being permissive returns a right answer here, so
   this is an error-surface gap rather than a wrong result.
2. **`EXPLAIN (FORMAT JSON)` reports oid 25** where PostgreSQL reports 114
   (`json`). Describe deliberately matches this server's own Execute, which
   also sends 25 — a Describe that disagrees with its Execute is a worse bug
   than the wrong oid. Fixing it means changing both together.
3. **`EXPLAIN (FORMAT XML)` and `(FORMAT YAML)` are `0A000`.** A scope
   decision, not a defect; recorded so the next sweep does not re-find it.

## COPY (2026-09-03) — swept, FORCE_* added, one deliberate difference

**`COPY ... TO '/path'` stays `0A000`** — PostgreSQL writes a server-side file
there (as a superuser). That is a deliberate scope decision, not a gap: an
in-process test server should not write arbitrary paths on the host.

## LISTEN / NOTIFY (2026-09-04) — swept, three fixes, two gaps left

Delivery semantics match PostgreSQL 14.13 (`tests/test_sql_sweep_sixteen.py`).
Two gaps remain:

1. **`pg_listening_channels()` does not exist** (`42883`). It is a
   set-returning function, and the SRF path (`srf.record_rows`) takes a
   pre-evaluated value with no `Session`, so serving it needs session-aware SRF
   support that does not exist yet. Diagnostic-only, hence recorded rather than
   built.
2. **A parameter in `NOTIFY` / `LISTEN` / `UNLISTEN` is not rejected.**
   PostgreSQL answers `42601 syntax error at or near "$1"` — their grammar has
   no slot for a parameter (`pg_notify(ch, $1)` is the working spelling).
   SecantusDB accepts `NOTIFY ch, $1` and answers `42601` with a different
   message for the other two. A check in the extended-protocol Parse was
   written and REVERTED: it never fired, because these statements reach
   `_maybe_pubsub` from the already-substituted SQL text rather than through
   the path that check sits on. Shipping a guard that does nothing is worse
   than the gap, so the next attempt should start by finding where a pubsub
   statement's ORIGINAL text is still available.

## Advisory locks, 2PC, and a scout (2026-09-04)

**Two-phase commit has not been compared against PostgreSQL.** The local
PostgreSQL ran `max_prepared_transactions = 0` at the time, so `PREPARE
TRANSACTION` answered `55000 prepared transactions are disabled`. Re-run a 2PC
probe against a reference server with that setting raised.

Open items from the scout, in descending value:

1. **`LATERAL <srf>(...)` is unsupported** — `FROM t, LATERAL unnest(t.arr) AS
   x` answers `0A000 unsupported LATERAL source`, and so do the
   `generate_series` / `jsonb_array_elements` / `regexp_split_to_table` forms.
   Corpus: `tools/probes/pg_corpora/lateral_srf.sql` (9 shapes).

   **Attempted on 2026-09-05 and REVERTED**: it got 4 of the 9 shapes
   returning correct VALUES, but with the wrong column TYPE (`text` where
   PostgreSQL says `int`), and a silent type divergence is worse than the
   honest `0A000`. What that attempt established:

   - **The EXECUTOR is already ready.** `_expand_lateral` runs the inner query
     per outer row through `engine.run_inner_select`, and that entry point
     already evaluates a FROM-less SELECT whose projection is a set-returning
     function — measured for `unnest`, `generate_series`,
     `jsonb_array_elements`, `jsonb_array_elements_text` and
     `regexp_split_to_table`. Nothing in the executor needs changing.
   - **Four planner changes are needed, and three of them are known-good.**
     (a) normalise a bare SRF call into the `SELECT <srf> AS <alias>` it is
     shorthand for — and build the `exp.Alias` DIRECTLY, because
     `exp.alias_()` sets the alias ON an `exp.Unnest` instead of wrapping it,
     so the column comes out named `unnest`;
     (b) treat a FROM-less source as "rich" in `_lateral_is_rich` so it takes
     the nested-loop path (there is no collection to lower a `$lookup`
     against);
     (c) `_lateral_literal` must keep a LIST as an `exp.Array` and a DICT as a
     jsonb cast — its string fallthrough turned an `int[]` outer value into
     the literal text `'[1, 2, 3]'`, which `unnest` then returned as ONE row
     holding that string.
   - **The unsolved piece is the SHAPE.** `_plan_rich_lateral` derives output
     columns by planning the subquery with correlations replaced by NULL, and
     `plan_pipeline_select` cannot take a FROM-less SELECT.
     `plan_constant_select` cannot either — it evaluates the projection as a
     SCALAR, and an SRF reaching the scalar evaluator is a deliberate `0A000`.
     Running it through `run_inner_select` DOES give the right column NAMES,
     but `unnest(NULL)` yields zero rows and the tag comes back `any` → text.
     Getting the element type needs the outer column's declared array type,
     which means threading the outer resolver / TableDef into
     `_plan_rich_lateral`. **That is the actual work.**
   - Two shapes stay out of reach even then: `count(*)` over a lateral hits
     the pre-existing "rich LATERAL only in a plain SELECT" restriction, and
     a `WHERE` over the lateral's output column needs the residual filter to
     run after expansion.
2. **`LATERAL (SELECT <expr from outer>) alias` fails** with `0A000 aggregate
   without FROM is not supported` — a FROM-less correlated lateral, same root
   cause as (1).
3. **Hypothetical-set aggregates, MULTI-COLUMN form** (one argument per ORDER
   BY expression) is still `0A000`, refused rather than approximated. The
   single-column `WITHIN GROUP` form of `rank` / `dense_rank` /
   `percent_rank` / `cume_dist` works (`tests/test_sql_sweep_twentythree.py`).
4. **`FILTER (WHERE ...)` on a statistical aggregate is `0A000`** — the twelve
   `corr` / `covar_*` / `regr_*` functions work everywhere else (GROUP BY,
   HAVING, empty input, single pair, degenerate variance), but
   `corr(y, x) FILTER (WHERE ...)` reports `unsupported scalar expression`.
   `_register_regr_stat` already ACCEPTS a filter condition and folds it into
   the both-non-null gate correctly; what is missing is upstream. The
   FILTER-wrapped node never reaches either registration site — the
   computed-projection registrar replaces the inner aggregate and leaves the
   bare `exp.Filter` behind for the scalar evaluator, and adding a check on
   the whole projection in the plain select-list path did NOT fire either, so
   the gate is earlier than both. Start by finding what decides a
   FILTER-wrapped `exp.Corr` is not an aggregate.
5. **`var_pop` / `var_samp` / `variance` over `float8` lose the last digits** —
   `1.2500000000000002` where PostgreSQL says `1.25`. The exact-numeric path in
   `_register_numeric_stat` covers the `numeric` case, not `float8`.

## `string_agg` over a join (2026-09-05)

2. **`string_agg(<expression>, ',')` over a join is `0A000 expected a column`**
   — the join HAVING/select path takes a column, not an expression, for
   `string_agg`. The single-table path accepts one.

## Whole-row references and the json / jsonb identity (2026-09-05)

Corpus: `tools/probes/pg_corpora/json_record.sql`.

**1.** Bare relation aliases in a value position work
(`tests/test_sql_sweep_twentyfour.py`). ONE GAP LEFT: `SELECT t FROM t`
reports the generic `RECORD` oid (2249) where PostgreSQL reports the table's
OWN rowtype oid (a per-table oid minted with the table). The field values are
right, so this is a type identity rather than a wrong answer — psycopg hands
back the parsed fields instead of the record's text — and closing it means
minting and cataloguing per-table rowtype oids.

**2. `json` and `jsonb` share one type tag.** `typemap.PG_OID["json"]` is 3802
(jsonb). A declared `json` COLUMN already reports 114 correctly (there is a
`json_plain` flag for that), so the machinery half-exists; what is missing:

- every json-returning FUNCTION reports 3802 — `to_json`, `json_build_object`,
  `json_build_array`, `json_object`, `array_to_json`, `json_agg`,
  `json_object_agg`, `row_to_json`, `json_strip_nulls`;
- `pg_typeof(json_col)` says `jsonb`;
- `json_col::text` normalises whitespace (`{"k": 1}`) where PostgreSQL replays
  the ORIGINAL text (`{"k":1}`) — `json` stores the text it was given and only
  `jsonb` normalises. That last one is a storage-representation change, not a
  tag change, and is the deep part.

**3. `jsonb` does not normalise key order.** `'{"b":1,"a":2}'::jsonb` keeps
input order where PostgreSQL sorts keys (shorter first, then bytewise). Note
`json` is CORRECT to keep the input order — only `jsonb` sorts, so a fix must
not apply to both.

## pgjdbc `DatabaseMetaDataTest`: 35 failures in 156 tests (2026-09-19)

The Python SQL server fails **33** of `org.postgresql.test.jdbc2.DatabaseMetaDataTest`'s
156 tests. Most **share one signature** — `AssertionFailedError: expected: <true>
but was: <false>`, which is `rs.next()` returning false: the metadata query ran
and returned NO ROWS.

Affected methods include `functionColumns`, `informationAboutArrayTypes`,
`ascDescIndexInfo`, `columnPrivileges`, `droppedColumns`, `escaping`,
`foreignKeysToUniqueIndexes`, `funcReturningComposite`, `funcReturningTable`,
`funcWithDirection`, `getSQLTypeQueryCache`.

One cause is known and fixed (`funcWithoutNames()`: unnamed `CREATE FUNCTION`
parameters recorded as `void`, now resolved by `_function_param_types`); it
fixed that test and nothing else, so do not expect a single remaining
explanation for the other 33.

**What it is NOT** — hypotheses tested and eliminated by measurement, so the
next person does not re-run them:

| hypothesis | measurement |
| --- | --- |
| catalogs empty | populated: `pg_class` 2, `pg_type` 95, `pg_proc` 13, `pg_namespace` 4 |
| the tests' setUp DDL fails | `CREATE FUNCTION` / `INDEX` / `SCHEMA` / `ALTER … DROP COLUMN` / `COMMENT ON` all succeed |
| catalog lookups regressed | **identical** between the 2026-08-16 server (`c07b9487`) and current, across proc-by-name, attrs-of-table, index-of-table, array-elem-type, class-by-name |
| vendored driver moved | `vendor/pgjdbc` unchanged since 2026-08-01 |
| include set widened | only `NotifyTest` was re-enabled (`2d15316d`) |
| JDK mismatch | the runner selects JDK 21 (`_find_jdk21`) and one is installed, same as CI |
| advertised version changed | `15.0` / `150000` on both old and current servers |

So the objects exist, the catalogs hold rows, and the direct lookups behave as
they did a month ago — yet the driver's own metadata calls come back empty. The
gap is somewhere between the catalog contents and what `getFunctionColumns` /
`getIndexInfo` / `getColumns` actually query, not in the server having lost data.

**Do not call these "regressions".** They were surfaced by a baseline comparison
whose populations do not match: the 2026-09-19 run executed **5,819** tests
against the baseline's **5,570** — 249 more, with the same driver, include set
and server version. Where those 249 came from is itself unexplained and worth a
look. The baseline was regenerated on 2026-09-19 (82 failures / 49 entries) so
the regression check compares like with like again.
