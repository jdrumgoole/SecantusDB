# Plan: a Rust PostgreSQL server

> **Status: P0 PASSED, P1 LANDED, P5 STARTED, P7 LANDED, 2026-08-31.**
> P7 (extended protocol) is done — see §0.8. It was pulled forward ahead of the
> rest of P5 because it was not a missing feature but a WRONG ANSWER, and
> because no SQL gauge can run without it.
>
> P5's first batch added ORDER BY / LIMIT / OFFSET, UPDATE, DELETE, the
> three-valued predicates and the count/sum/min/max aggregates with GROUP BY,
> pinned by a 109-case differential against a live PostgreSQL (§0.7). P2
> (session/auth) is still not started.
>
> P1 shipped `secantus-pgcatalog` / `secantus-pgplan` / `secantus-pgserver` and
> the `secantusd-pg` binary: CREATE TABLE, INSERT and single-table SELECT over
> **real `secantus-storage`**, with the cross-server catalog contract proven in
> both directions (§0.6). That closes the first two gaps §0.5 listed.
>
> The premise held on every question the spike could reach — see
> §0 immediately below, which is measurement, not proposal. The phasing from §9
> onward remains a proposal.
>
> This is the effort `tasks/sql-postgres-plan.md` §10 deferred: *"a Rust port
> mirrors the existing pattern … a separate, later effort with its own crate
> version line."* The Python SQL layer it names as the precondition is now
> shipped, gauged and stable, so the precondition is met.

## 0. P0 spike results (2026-08-31) — the premise holds

Run on this branch; the throwaway probe workflow has been deleted. Everything
here was executed, not reasoned about.

### 0.1 The build gate: PASSED on all four wheel platforms

`pg_query` 6.2.0 compiles the vendored PostgreSQL C with the `cc` crate — **no
cmake, no autotools, no configure**. macOS arm64 builds clean in **31.7s**
(53.3s release, whole spike). CI matrix result:

| platform | result |
|---|---|
| linux-gnu x86_64 | **pass** |
| **windows-msvc x86_64** | **pass** — the feared unknown |
| macos arm64 | **pass** |
| linux-musl x86_64 | **pass**, with one flag (below) |

Two build facts worth carrying into P1:

- **`protoc` is optional.** Protobuf bindings are pre-generated and regenerated
  only if `protoc` happens to be on PATH. manylinux/musllinux containers do not
  need it.
- **`bindgen` is required, and this repo already solves it.** pg_query needs
  libclang exactly as `secantus-wt` does, and `pyproject.toml`'s existing
  per-platform `before-build` recipes (symlink whichever `libclang.so` exists
  into a fixed prefix, point `LIBCLANG_PATH` at it) cover it unchanged. **No new
  build infrastructure.**
- **musl needs `RUSTFLAGS=-C target-feature=-crt-static`.** Rust's musl host
  defaults to `crt-static`, so pg_query's *build script* is statically linked
  and cannot `dlopen` libclang at all — independent of whether libclang is
  installed. Standard musl+bindgen interaction; one flag. (Two earlier probe
  failures on this leg were the probe's own fault — Alpine's cargo 1.78 is too
  old for `edition2024` — and are recorded so nobody re-derives them.)

### 0.2 The parser: it fixes a currently-RED gauge

Every shape the backlog records sqlglot mangling parses natively into the
correct node type: `MOVE FORWARD 2 IN c` → `FetchStmt`, `LISTEN`/`NOTIFY` →
`ListenStmt`/`NotifyStmt`, `DROP TABLE a, b, c` → `DropStmt`,
`BEGIN ISOLATION LEVEL …` → `TransactionStmt` — each of which the Python server
reaches only through a **regex pre-pass** in `planner.parse`.

**G7 (`sql-stress`, 0/6 RED) traced to a concrete sqlglot defect.** Reproduced
against the running Python server:

    copy pgbench_accounts from stdin with (freeze on)   -> ERROR: syntax error at or near "on"
    copy pgbench_accounts from stdin with (freeze)      -> OK

The mechanism, measured on both parsers — note it is **not** that `planner.parse`
rejects the statement; it returns a `Copy` node, and the failure lands
downstream on mis-structured options:

| parser | result |
|---|---|
| sqlglot | **two** params: `CopyParameter(Var(freeze))`, `CopyParameter(Var(on))` |
| libpg_query | **one** option: `defname="freeze", arg=String("on")` — correct |

sqlglot splits the boolean option from its value and invents a phantom option
named `on`, which the COPY layer then rejects. libpg_query gets it right by
construction. That is the whole reason `pgbench -i` cannot load.

### 0.3 The lowering: PG parse tree → MQL → `secantus-core`, end to end

**~130 lines** lower a PostgreSQL `SelectStmt` to a Mongo filter, which the
**existing** `secantus_core::query::matches` evaluates unchanged:

| SQL | lowered filter |
|---|---|
| `WHERE x = 1` | `{x: 1}` |
| `WHERE n > 15` | `{n: {$gt: 15}}` |
| `WHERE n >= 20 AND x <> 3` | `{$and: [{n: {$gte: 20}}, {x: {$ne: 3}}]}` |
| `WHERE n <= 20 AND (x = 1 OR name = 'bob')` | `{$and: [{n: {$lte: 20}}, {$or: [{x: 1}, {name: "bob"}]}]}` |

**All five probe queries returned exactly what live PostgreSQL 14 returns** for
the same rows — the oracle, not the Python server, adjudicated.

### 0.4 A real `psql` renders the rows

`psql` 17.6 → `pgwire` 0.31 → libpg_query → lowering → `secantus-core`:

    $ psql "host=127.0.0.1 port=25433 ..." -c "SELECT id, name FROM t WHERE n <= 20 AND (x = 1 OR name = 'bob')"
     id | name
    ----+-------
      1 | alice
      2 | bob
    (2 rows)

    $ psql ... -c "SELECT count(*) FROM t GROUP BY x"
    ERROR:  pgspike cannot lower this yet: only bare column targets are lowered

The whole spike — parse, lower, wire, evaluate — is **267 lines**. The second
result matters as much as the first: an unsupported construct returns an honest
`0A000`, which is the §6 discipline working as intended.

Note for P1: `pgwire`'s **default features pull `aws-lc-rs`**, a second C crypto
build. `default-features = false, features = ["server-api"]` avoids it; match
whatever rustls backend the Mongo server already uses rather than adding one.

### 0.8 P7 (2026-08-31): the extended protocol, pulled forward

**Found by asking "what would a gauge need?", not by reading the plan.** A
parameterised query — the moment psycopg, pgjdbc or pgx binds a value — went
down pgwire's default extended path and returned **command status OK with zero
rows**. Not an error, not a `0A000`: a silent empty result for a query that
should return data. Every differential case up to then used literal SQL, so
none of them could see it.

Implemented: `Parse` / `Bind` / `Describe` / `Execute` / `Close`, text AND
binary parameter decoding, `Describe`-before-`Bind` (planned against NULL
placeholders, since the result SHAPE does not depend on the values), and
`Execute`'s `max_rows`. `$N` resolves in the planner via `plan_with_params`, so
a bound value flows through exactly the same NULL rules as a literal instead of
needing a parallel set. **Both protocols share one `run()`** so they cannot
drift.

**It immediately exposed a wrong answer unrelated to parameters.** `WHERE n =
NULL` is never true in SQL — only `IS NULL` matches — but MQL's `{n: null}`
matches, so the server returned the NULL row. The same was true of `<>`, `>`,
`<=` and the rest. **The literal form was equally broken and equally untested**;
binding a NULL parameter is simply what made it obvious enough to write down.
Lowering now short-circuits any comparison against NULL to match-nothing.

Differential: **141 cases**, 28 of them over the extended protocol, including
bound NULLs and the literal `= NULL` forms.

### 0.9 The first gauge number (2026-08-31)

`SECANTUS_GAUGE_SERVER=rust` now drives `secantusd-pg` in
`psycopg_validation/runner.py`, mirroring the MongoDB gauges' variable. The raw
JSON is written per-server so a Rust run cannot overwrite the Python baseline.

**psycopg 3's unmodified suite against the Rust server: 694 passed, 3026
failed, 417 errors, 76 skipped of 4,238 (~16%).** Low, and that is the point —
everything before this was measured against a differential written by the same
person who wrote the server. This is the first number produced by someone
else's tests, and the ranked failure list is worth more than the percentage:

| n | blocker |
|---:|---|
| 732 | `AExpr` — expressions in a target list (`SELECT 1+1`, `a \|\| b`) |
| 520 | `TypeCast` — `'1'::int`, `$1::text` |
| 502 | `VariableSetStmt` — `SET`/`RESET` |
| 331 | statement input (multi-statement / empty) |
| 316 | **`DropStmt` — `DROP TABLE`**, which is cheap |
| 223 | `CopyStmt` |
| 71 | `DeclareCursorStmt` |
| 59 | `set_config()` |
| 56 | `AArrayExpr` — array literals |
| 40 | `RangeFunction` — `FROM generate_series(...)` |

Two connection-level requirements were found only by attempting this, and no
amount of self-written differential testing would have surfaced either: the
runner refuses any daemon whose `select version()` does not name SecantusDB
(the server could not do `SELECT` without `FROM` at all), and psycopg wraps
setup in `BEGIN`/`COMMIT` (transaction statements did not exist). Both are now
implemented — transactions with real `UserTransactionHandle`s, so `ROLLBACK`
actually discards rather than reporting success.

### 0.10 DROP TABLE + TypeCast (2026-08-31): 694 -> 746

Both blockers closed; **neither appears in the ranking any more**. The gauge
moved **694 -> 746 passed, errors 417 -> 333**.

**+52, not +836, and that is the lesson.** The blocker counts are per-test
occurrences of the FIRST error each test hits. A test blocked by `DROP TABLE`
usually needs several other things as well, so removing one obstacle mostly
reveals the next. Read the ranking as "what to build", never as "how many tests
this will win".

Updated ranking:

| n | blocker |
|---:|---|
| **737** | `AExpr` — expressions in a target list. Now the clear #1. |
| 503 | `VariableSetStmt` — `SET`/`RESET` |
| 274 | `CopyStmt` |
| 83 | `AArrayExpr` — array literals |
| 72 | `DeclareCursorStmt` |
| 66 | casts to `interval` |
| 59 | `set_config()` |

### 0.11 Expressions (2026-08-31): 746 -> 853

`AExpr` closed; **gone from the ranking**. The largest jump yet, **+107**.

Arithmetic (`+ - * / %`), `||`, comparisons and unary minus, over literals and
bound parameters. 14 of 14 shapes match PG 14 on value AND type oid, including
`7/2 = 3` (integer division truncates) and `5/0` -> `22012`.

**Decimal arithmetic is deliberately refused.** PostgreSQL types `1 + 1.5` as
`numeric` (oid 1700) with its own scale rules, not `float8`. Returning a double
would be the right value under the wrong declared type -- the same class as the
`$1::int`-decoded-as-string bug. Explicit `::float8` casts work, because then
the type genuinely is float8.

The static-typing rule earned its keep again: `SELECT $1 + 1` is planned
against a NULL placeholder at Describe time, so the column type comes from the
OPERATOR (int4), never from the value (which would say text).

Ranking now:

| n | blocker |
|---:|---|
| 503 | `VariableSetStmt` — `SET`/`RESET` |
| 325 | `FuncCall` — functions in a target list |
| 274 | `CopyStmt` |
| 179 | `AArrayExpr` — array literals |
| 80 | casts to `interval` |

### 0.12 Session settings (2026-09-01): 853 -> 899

`SET`/`SHOW`/`RESET` plus `current_setting()` / `set_config()`, held per
connection. `VariableSetStmt` closed; 11 of 11 shapes match PG 14.

Three details that decide whether a client is satisfied, all probed:

* **`SHOW datestyle` answers a column named `DateStyle`.** Lookups are
  case-insensitive, the REPORTED name is not, and clients match on it.
* `current_setting('nope')` is **42704**; `current_setting('nope', true)` is
  **NULL**.
* **`RESET` restores the DEFAULT, it does not delete.** A client reading the
  setting back afterwards must see the default, not an error.

The GUC functions resolve at EXECUTION, not while planning -- settings live on
the connection and the planner is stateless. That is why `ConstCol` exists.

Ranking now: `FuncCall` 325, `CopyStmt` 274, `AArrayExpr` 179,
`DeclareCursorStmt` 72, casts to `interval` 80.

### 0.13 COPY FROM STDIN (2026-09-01): 899 -> 900

`COPY ... FROM STDIN` in text format: `\N` nulls, escaped tab/newline/
backslash, optional column list, and chunk boundaries that fall mid-row (buffer
until CopyDone, parse then).

**+1, and the reason matters more than the number.** `CopyStmt` was 274, but
psycopg's copy tests ROUND-TRIP: roughly 6 `TO STDOUT` against 6 `FROM STDIN`,
so most still fail on the half that is missing. Another instance of the rule
from 0.10 -- the ranking says what to build, never how many tests it wins.

The value is elsewhere: this is how `pgbench -i` loads its tables, so it is a
prerequisite for the `sql-stress` gauge that has been 0/6.

**`COPY TO STDOUT` is blocked by pgwire, not by us.** It sends the
CopyOutResponse header and sets CopyInProgress, but `SimpleQueryHandler::
do_query` has no `Sink` bound, so there is no way to push the CopyData rows
that must follow. The extended handler's `do_query` DOES have the sink, so an
asymmetric implementation is possible but would be worse than refusing.

**This is the SECOND concrete cost of choosing the pgwire crate over
hand-rolling** (the first: `ErrorInfo` cannot carry `constraint_name`, §0.6).
Both are recoverable by upstreaming, and both should be weighed if a third
appears.

Also worth noting: pgwire sends `ReadyForQuery` after `on_copy_done` but NOT
`CommandComplete` -- the handler must send that itself, or the client waits for
a result that never arrives (psycopg: "not enough values to unpack").

### 0.14 pgwire 0.31 -> 0.40 (2026-09-01): 900 -> 904

**A review of the "pgwire vs hand-roll" decision found that both recorded costs
of the crate were actually costs of OUR PIN.** We were on 0.31.1; current was
0.40.7, released three weeks earlier and actively maintained. `pgwire = "0.31"`
is pre-1.0, so cargo would never cross the minor boundary.

| recorded limitation | reality |
|---|---|
| `ErrorInfo` cannot carry `constraint_name` (§0.6) | **fixed in 0.39.0** -- 18 fields now, incl. schema/table/column/datatype/constraint |
| `COPY TO STDOUT` impossible, no `Sink` (§0.13) | **fixed in 0.38.0** -- CopyEncoder + full copy-out API; `SimpleQueryHandler::do_query` now takes a Sink |

Both are now closed in our code: a 23505 carries constraint/table/schema
identically to PG 14 (`column` stays UNSET, because PostgreSQL leaves it unset
and populating it looked helpful but was wrong), and `COPY TO STDOUT` emits
BYTE-IDENTICAL text that round-trips through `COPY FROM`.

Migration cost: 12 compile errors. `Response` lost its lifetime parameter,
`QueryParser` gained `get_parameter_types` / `get_result_schema`, `parse_sql`
takes `&[Option<Type>]` (unspecified is `None`, not oid 0), `CopyResponse::new`
takes a data stream, and `DataRowEncoder::finish` is deprecated in favour of
`take_row`. All 200 PG tests passed unchanged afterwards -- the upgrade is
behaviour-neutral.

**Run `./inv sync` BEFORE the final full suite, not after it fails.** Stale
`_secantus_core` produced phantom parity failures THREE times in this session
alone -- each costing a ~15-minute suite run -- because parallel sessions keep
merging engine changes to `main` while a long PG batch is in flight. The trap is
documented in `CLAUDE.md`; what is new is how often it fires when sessions
overlap. Sync is cheap; a wasted suite run is not.

**The standing lesson: a sub-1.0 pin silently stops receiving compatible
updates.** Two batches were spent writing careful notes about limitations that
`cargo search pgwire` would have disproved in seconds. Check the upstream
version BEFORE recording a limitation as permanent.

Verdict on the original decision: **keep pgwire.** It is 9,545 lines against
the Python server's 4,092 for less capability (no SCRAM, TLS or binary COPY),
and it already covers everything ahead -- NoticeResponse, ParameterStatus,
NotificationResponse, CancelRequest, SASL/SCRAM, SSL, PortalSuspended.

### 0.15 date + time (2026-09-01)

`date` / `time` as column types AND cast targets. Stored as canonical TEXT --
the representation the Python server writes, and therefore a contract -- but
reported with oids **1082 / 1083**, not varchar. That is what makes a client
return a `date` object instead of a string; psycopg decodes varchar to `str`
either way, so only an OID comparison catches it.

**`timestamp` LANDED in the same batch after all** (see below); the scoping
note is kept because the reasoning still applies to anything touching the
companion.

**Original scoping note:** The Python server
stores it as a BSON `Date` truncated to milliseconds plus a hidden
`__us_<field>` companion for the microseconds BSON cannot hold, and
`subms.py` warns: *"Every write of a timestamp field must set or clear its
companion. A stale companion is worse than truncation: it silently reports a
time that was never stored."* That is cross-server silent corruption, and the
companion is PER-COLUMN so it cannot live in `cast_value`, which has no field
name. It needs its own batch with the INSERT/UPDATE paths done carefully.

**The batch's real find was not about dates.** Testing `date` as a COLUMN type
rather than a cast exposed that assigned values were never coerced to the
column's declared type: `INSERT INTO t(d) VALUES ('2026-9-1')` stored the
literal verbatim and the client then failed with `can't parse date '2026-9-1'`.
PostgreSQL coerces on assignment. Now fixed for INSERT and UPDATE, for EVERY
type. **All 23 cast-level differential cases passed while the storage path was
broken** -- casts and column types are different paths and need separate tests.

22007 (not a date) and 22008 (a date that cannot exist, e.g. `2026-02-30`) are
distinct codes and are kept distinct.

### 0.16 timestamp + the sub-millisecond companion (2026-09-01): 904 -> 945

BSON's `Date` is a MILLISECOND count; PostgreSQL's `timestamp` carries
microseconds. The Python server stores the truncated date plus the lost 0-999us
in a hidden `__us_<field>` companion (`subms.py`); this server now writes the
identical representation, because both share one store.

**THE INVARIANT, and it is the whole risk:** every write must SET or CLEAR the
companion. `subms.py`: *"A stale companion is worse than truncation -- it
silently reports a time that was never stored."* The dangerous sequence is
covered by a test that drives it directly: insert `.789012`, **overwrite with a
whole-millisecond value** (companion must be REMOVED), then set microseconds
again. Clearing needs an `$unset` in the update path, not just an `$set` --
which is why `Update` grew an `unset` field.

**Three-way agreement verified:** Rust writes -> Python server reads at full
precision -> both match real PG 14, oid 1114 throughout.

Two details worth keeping:

* `split_subms` uses `div_euclid`/`rem_euclid`, not `/` and `%`. Rust truncates
  toward zero, so a PRE-EPOCH timestamp would get a negative remainder and
  reconstruct to the wrong time.
* The read path VALIDATES the stored remainder (integer, 1-999) instead of
  trusting it, mirroring `subms.py::merge`, so a hand-edited or foreign
  document cannot produce a time nobody wrote.

Caught by the differential: `'...'::timestamp::text` fell through to a BSON
debug dump rather than PostgreSQL's rendering -- casts to text are a separate
path from the row encoder and needed their own case.

### 0.17 numeric (2026-09-01): 945 -> 965

`numeric` as a column type, cast target, and the type of a DECIMAL LITERAL --
`SELECT 1.5` is oid 1700 in PostgreSQL, not float8. Stored as BSON
`Decimal128`, which preserves SCALE exactly as PostgreSQL does: `1.50` stays
`1.50`, `-0.30` stays `-0.30`, `2.5000000000000000` round-trips. Scale is part
of the VALUE, not formatting. 8 of 8 shapes match PG 14.

**This retires a refusal carried since §0.11.** Decimal arithmetic and `avg()`
were deferred precisely because returning a double under a `numeric` oid is the
wrong-type bug that made `$1::int` arrive as a string. Now the type exists, the
refusal can be lifted incrementally.

**The limit is honest and enforced:** Decimal128 holds 34 significant digits,
PostgreSQL's `numeric` is arbitrary precision. A value needing more is **22003**,
not a silent rounding -- a quietly shortened number is indistinguishable from a
correct one. `'x'::numeric` stays 22P02, a different failure.

**Still deferred: decimal ARITHMETIC.** `bson`'s Decimal128 is a container with
no arithmetic, so `1 + 1.5` needs `rust_decimal` (28-29 digits, FEWER than
Decimal128's 34) plus PostgreSQL's scale-derivation rules (`+`/`-` take
max(s1,s2), `*` takes s1+s2, `/` gets scale ~16 -- `10.0/4` is
`2.5000000000000000`). That is its own increment with its own precision
tradeoff, and it stays refused until then.

### 0.18 arrays: SHIPPED after the regression was root-caused (2026-09-01)

**Arrays first measured as a 16-test REGRESSION (965 -> 949), were bisected to a
single missing comparison arm, and shipped at 984 -- the best number so far.**

The park was right and the diagnosis in it was wrong. Recorded here because the
wrong diagnosis is the more useful half.

**The bisect took two runs, not the four the park expected.** Applying
`type_name_of` ALONE reproduced 949 exactly, and applying everything EXCEPT
`type_name_of` gave 963. So the entire cost sat on the one change that was not
array-local -- as the park guessed -- but not for the reason it assumed.

**The actual cause: `eval_binary` had no `Bson::Array` arm.** psycopg asks
`select %s::text[] = %s::text[]`. While a cast to `text[]` quietly degraded to
`text`, BOTH sides rendered to strings and string comparison gave the right
answer by accident. Typing the cast correctly turned them into real arrays, and
`compare_constants` returned `None` for those -- so 16 tests that had been
passing on a coincidence started failing on a missing feature. **Making the
types right exposed a gap that wrong types had been hiding.** Adding array
comparison took it to 970, and a third missed `type_name` call site in
`static_type` (the one that decides the REPORTED wire type, so `ARRAY[1,2,3]`
came back as a string) took it to 984.

`type_name` now has exactly ONE caller, inside `type_name_of`. There is no
longer a way to read a type name without its brackets, which is what let this
hide in three places at once.

**Array NULL rules are not scalar NULL rules**, all four probed against a live
PG 14 rather than reasoned out: inside an array two NULLs are EQUAL, a NULL
sorts AFTER every non-NULL, a common prefix makes the shorter array smaller,
and empty equals empty. Scalar `NULL = NULL` is NULL, so an elementwise
comparison written by analogy with the scalar path gets every one of these
wrong.

**Nested arrays are REFUSED (0A000), not flattened.** rust-postgres encodes one
dimension only, so the typed path turned `{{1,2},{3,4}}` into two elements whose
text was `{1,2}` and `{3,4}` -- a wrong answer a client cannot distinguish from
a real one. Smuggling the literal through as text by guessing the client's
format code is the same trade somewhere less visible. **Refusing cost zero gauge
tests** (984 both ways), so the wrong answer was never buying anything.

**The lesson, and it is the batch's real output: a feature verified correct
against the oracle can still make the gauge worse, and the drop can mean the
feature EXPOSED a bug rather than caused one.** Nine probed shapes proved the
implementation and said nothing about interactions. Measure before and after,
treat a drop as a blocker -- and when bisecting, run the complement (everything
except the suspect) as well as the suspect alone: that pair is what turned four
candidates into one answer in two runs.

**Still open:** multidimensional arrays over the wire (needs hand-built array
encoding, since rust-postgres will not do it); `test_array.py` is 34/124, so
psycopg's array corpus goes much deeper than 1-D round-trips.

### 0.19 multi-statement, and a gauge that had been lying (2026-09-02)

**986 -> ~1040.** Multi-command simple queries, `DEALLOCATE ALL`, `pg_typeof`,
and two cast bugs the probes turned up.

**The instrument was broken before any of this.** Two runs of the SAME build
gave 1014 and 993 — a 21-test swing that looked like noise and was not. psycopg
issues `DEALLOCATE ALL` to reset its prepared-statement cache, but only when a
connection happens to have one, so refusing it failed a scattered set of tests
depending on execution order. **A bimodal gauge is a missing feature until
proven otherwise**; supporting `DEALLOCATE ALL` collapsed the spread to 3
(1011 / 1013 / 1010). Every number recorded in 0.18 was measured on the
unstable instrument, which is why the arrays batch re-measured as +36 against a
`main` that had itself moved.

**Multi-statement is mostly a transaction feature.** The splitting is one call
to libpg_query. What cannot be added afterwards is that a batch runs as ONE
implicit transaction: a failure in the third command discards what the first
two wrote, and an explicit `COMMIT` inside the batch ends the transaction so
its work survives a later failure. Both probed against PG 14. Both fall out of
reusing the session's own transaction slot rather than tracking a second one —
`BEGIN` inside a transaction was already a no-op and `COMMIT` already took the
handle, so the composition was free.

**Naming the function in the error is what made the rest rankable.** `FuncCall
is not supported yet` was the single largest failure signature at 367 and said
nothing about which function to build. Naming it split into `pg_typeof` 225,
`chr` 47, `generate_series` 43, `set_byte` 22, the range constructors ~90.
`pg_typeof` alone was two thirds of it. **A diagnostic change can be the
highest-value change on the board** — it cost four lines and re-ranked
everything behind it.

**Two cast bugs found by the pg_typeof probe, not by the gauge.** `1.5::float8`
failed outright, and so did `1.5::int`: decimal literals became `numeric` in the
previous batch and neither cast path was taught about `Decimal128`. Then
`2.5::float8::int` answered 3 — PostgreSQL rounds float-to-integer HALF TO EVEN
and numeric-to-integer HALF AWAY FROM ZERO, and one rule was doing both.
Writing a probe for the feature you are adding keeps finding bugs in the
feature you added last.

**Next, re-ranked at 1043:** `DROP <non-table>` 207 (and its message leaks Rust
debug formatting: `DROP of Ok(ObjectType) is not supported yet`), `interval`
126, COPY binary 119, `timestamptz` 99, cursors 73, `CREATE SCHEMA` 71,
`json`/`jsonb` 68 each, binary-format parameters 54.

### 0.20 binary-format parameters (2026-09-02)

**~1040 -> 1215, the largest single jump so far, and stable across two runs.**

**Clients do not send parameters as text.** psycopg sends numbers, dates,
timestamps and arrays in the BINARY format by default and falls back to text
only where it must. This server decoded int / float / bool / text that way and
refused everything else — so binding a `Decimal`, a `date`, a `datetime` or a
list failed while the identical value written as a SQL literal worked. That one
mechanism was blocking 242 tests spread across `test_numeric`, `test_datetime`
and `test_json`, which is why it beat every per-type item on the board.

**Decode to canonical TEXT, then reuse the text path.** Every new decoder
(`numeric` 1700, `date` 1082, `time` 1083, `timestamp` 1114, and all the array
oids through the ELEMENT's own decoder) produces the same string a literal
would have. The alternative — a parallel set of binary-specific conversions —
is precisely how two formats drift into disagreeing about one value.

**Two bugs found while building it, neither visible on the gauge:**

* A `numeric` parameter sent as TEXT was parsed with `parse::<f64>()`. A client
  binding `Decimal("1.50")` got a float that had already lost the exactness and
  the scale that make it a different value from `1.5`. The binary work is what
  made anyone look at the text arm.
* `SELECT '2026-01-01 12:00'::timestamp` answered **NULL**. A stored timestamp
  is reassembled from its column plus a hidden sub-millisecond companion; a
  CONSTANT never passes through a row, so it reached the encoder as that
  composite with no arm to match. The same value via a column, or cast to text,
  was correct — three routes to one value and only the least-used one empty.

**The probe was 21 values x 2 formats compared against PG 14, and went from 6
divergences to 0.** Building the probe as a matrix over both formats is what
caught the text-format numeric bug; a binary-only probe would have passed.

**Next, re-ranked at 1215:** `DROP <non-table>` 207 — but that is really
user-defined types (`test_enum.py` 197, `test_composite.py` 48), which needs
`CREATE TYPE` plus `pg_catalog` queries, so it is a campaign rather than a
slice. Cheaper: `interval` 126, COPY binary 119, `timestamptz` 99, `CREATE
SCHEMA` 71, cursors 70, `json`/`jsonb` 68 each, `chr` 47, `generate_series` 43.

### 0.21 timestamptz, timetz and the session TimeZone (2026-09-02)

**1215 -> ~1295.**

**`timestamptz` is an instant, not a timestamp with an offset attached**, and
what a client sees is the SESSION's view of it. That makes the session
`TimeZone` part of what a statement MEANS: the same literal read under two
zones names two different moments, and the same instant prints differently in
each. So the zone is passed into the planner rather than defaulted.

**Two sign conventions meet here and run opposite ways** — both probed, because
either one backwards is invisible under UTC and wrong by hours everywhere else:

* `SET TimeZone TO '+02:00'` is POSIX: positive is WEST, and it renders `-02`.
* `'2026-01-01 12:00+02'` is ordinary: two hours EAST.

**`chrono-tz` was already in the tree** for the MongoDB side's date operators,
so named zones with real DST rules cost one dependency line rather than a
campaign. `Europe/Rome` gives +01 in January and +02 in July; a fixed offset
gives the same all year.

**The session zone reaches the lowering code through a thread-local**, installed
by `plan_with_session` around a SYNCHRONOUS call with no `await` inside, so no
other task can observe it — the same shape as the Python server's `maxTimeMS`
deadline. Threading a session argument through every intermediate signature to
reach two leaves buys nothing.

**A comment written in this batch was contradicted by the next probe.** It said
no zone in use carries seconds in its offset; psycopg's corpus has `+01:02:03`
in 16 tests. Fixed in both directions. That is the third instance in this file
of the "comment justifying behaviour by something other than the oracle" shape
CLAUDE.md warns about, and this one was self-inflicted within the hour.

**A wrong answer inside the new feature, caught by checking a claim.** The
backlog entry written for this batch asserted that a `timestamptz` COLUMN was
refused. Nothing had tested that, and it was false: the column was ACCEPTED, and
a row written under UTC read back as `12:00:00+00` under `Europe/Rome` where
PostgreSQL answers `13:00:00+01` — the right instant printed in the wrong zone,
undetectable by any client. `timestamptz` is stored as canonical TEXT (the
choice `date` and `time` already make) and a timestamptz renders in the SESSION
zone, so that text is only correct for the session that wrote it. Columns of
both tz types are now refused with `0A000`, which cost **zero** gauge tests —
the wrong answer was buying nothing, exactly as with nested arrays in 0.18.

**The lesson: verify the claims in the write-up, not just the code.** This one
was invented while documenting and would have shipped as a false statement
about a real wrong answer.

**Still failing in `test_datetime.py`, with counts:** `interval` 128, `inf` /
`-inf` timestamps 60, BC dates 30, `'epoch'` 6.

### 0.22 interval (2026-09-02)

**~1298 -> ~1372.** The last big single-type item on the board.

**An interval refuses to be one number, and that is the whole design.**
PostgreSQL keeps months, days and microseconds separately because none converts
to another without a calendar: a month is 28-31 days depending on where you
start, and `2026-01-31 + '1 mon'` is `2026-02-28` -- a result no count of
microseconds expresses. `+ '30 days'` on the same date gives `2026-03-02`, and
both are right for what was asked.

**Comparison goes the other way**, which is the part that would be missed by
anyone implementing from the arithmetic: PostgreSQL FLATTENS the parts for
ordering using 30-day months and 24-hour days, so `'1 mon' = '30 days'` is TRUE
while adding them lands on different dates. Ordering goes through
`comparable_micros`; arithmetic keeps the parts apart.

**Three input grammars, all of which combine**: verbose (`1 year 2 months`, with
abbreviations, and `week` -> 7 days), a bare time (`02:03:04.5`, own sign, may
exceed 24 hours), and ISO 8601 (`P1Y2M3D`, where `M` is months before the `T`
and minutes after it). Each component keeps its own sign: `1 day -02:03:04`.

**A silent factor-of-60,000 bug, caught by the differential.** Depluralising a
unit with `trim_end_matches('s')` turned `s` (seconds) into the empty string and
`ms` (milliseconds) into `m` (minutes). The first showed up as a parse failure;
the second would have been a wrong ANSWER, and no probe in the first batch
covered `ms`. The fix strips the `s` only when what remains is still a unit, and
`interval_units_survive_depluralisation` pins all seven spellings.

**Probe: 46 shapes plus 10 bound values across both wire formats, 0
divergences.**

**The BACKLOG predicted a bug in code that did not exist yet.** An entry from
2026-08-30 recorded, for the Python server, that a bare unknown literal beside
an interval coerces to an INTERVAL rather than a timestamp -- so
`'2020-01-01' + interval '1 day'` is `22007`, not date arithmetic. The Rust
implementation written today reproduced the identical bug, and the entry is
what caught it: the differential probe had not covered the shape. Reading the
backlog for the AREA you are working, not just for the item you claimed, is
worth the minute it costs.

The discrimination has to be made on the AST NODE rather than the value,
because a `::date` cast is also a string at that point -- so a bare `AConst`
string is what marks an unresolved literal. `+` / `-` only: for `*` / `/`
PostgreSQL resolves the unknown to a NUMBER, which is why
`interval '1 day' * '2'` is two days.

**Scaling spills fractions downward** (`'1 mon' * 1.5` is `1 mon 15 days`,
`'1 year' * 0.5` is `6 mons`), and `/ 0` is `22012` rather than an infinity.

**A new differential lane: shared REFUSALS.** `test_error_sqlstate_matches_
postgres` compares SQLSTATEs for statements both servers must reject -- a row
comparison passes trivially when both sides raise, so wrong error CODES were
invisible to the existing lane. Its guard immediately earned its keep by
rejecting two cases drafted for it that PostgreSQL actually ACCEPTS (nested
arrays, a 35-digit numeric): those are this server's documented limitations,
not shared refusals. It also caught an unseeded-oracle bug in its own first
draft, where a missing fixture table answered 42P01 there against 42703 here.

**Next, re-ranked at ~1372:** `DROP <non-table>` 207 (really user-defined types:
`test_enum.py` + `test_composite.py`, needing `CREATE TYPE` and `pg_catalog` --
a campaign), `comparing these operands with =` 120, COPY binary 119, `CREATE
SCHEMA` 71, cursors 70, `json`/`jsonb` 68 each, `chr` 47, `generate_series` 43.

### 0.23 comparison, and a regression the gauge never showed (2026-09-02)

**~1372 -> 1388.** A small number in front of the largest correctness find of
the session.

**Naming the types in one error split it into five causes.** "comparing these
operands with =" was the second largest signature at 120 and named neither
operand. Naming them gave `text vs text` 71, `numeric vs numeric` 18,
`float8 vs float8` 3 -- which was still wrong, because `inferred_type` collapses
several BSON kinds onto `text`. Naming the BSON kind instead gave the real list:
`interval vs string` 32, `document vs string` 30, `array vs string` 28,
`decimal128 vs decimal128` 18, `datetime vs string` 9, `double vs double` 3.
**Two rounds of diagnostic sharpening, each of which changed the answer.**

**Four of the five were ONE rule, already implemented for the wrong half.** In
0.22 the unknown-literal coercion was written for `+` / `-` only. It applies to
COMPARISON identically -- `interval '1 day' = '1 day'` is true,
`ARRAY[1,2] = '{1,2}'` is true, and `interval '1 day' = '2020-01-01'` is 22007
rather than false. Implementing a PostgreSQL resolution rule for one operator
family and not the other is a shape worth remembering.

**The find that matters: ALL DECIMAL ARITHMETIC WAS BROKEN.** `1.5 + 1.5` was an
error. This regressed in 0.19 when decimal literals became `Decimal128` and no
arithmetic path learned the type. It went unnoticed for four batches because no
test covered it and the gauge barely moved -- the psycopg corpus tests decimals
through parameters far more than through literal arithmetic. **A feature that
"only" costs 16 gauge tests can still be a total break of a basic operation**;
the gauge measures a client's corpus, not this server's surface.

Now exact, on `i128` digits with PostgreSQL's measured scale rules: add and
subtract take `max(s1, s2)`, multiply takes `s1 + s2`, so `1.50 + 1.5` is `3.00`
and `1.50 * 1.50` is `2.2500`. **Division stays refused** -- its result scale
depends on operand weights in a way not measured here, and a plausible number of
decimal places that is not PostgreSQL's would be a wrong answer.

**Two more real gaps:** decimals compared through an `f64` reported
`12345678901234567890.1` and `...2` as EQUAL (34 digits versus 15); and NaN got
"cannot compare" because IEEE says every NaN comparison is false, where
PostgreSQL orders it TOTALLY -- NaN equals itself and sorts above infinity.

**Probe: 28 shapes, 0 divergences.**

### 0.24 the text parameter path, and a message that blamed the wrong layer (2026-09-02)

**1386 -> ~1485, +99.**

**The error named comparison; the bug was in decoding.** After 0.23 the residual
`comparing array with string` (36), `comparing interval with string` (32) and
`comparing document with string` (30) looked like remaining comparison gaps.
They were not: the TEXT parameter path had no arms for arrays, intervals or
timestamps, so those values fell through to `sniff_text` and became plain
strings. The binary path had learned all of them in 0.20 and the text path never
did. **A sharpened message localises a symptom, not a cause** -- 0.23's
diagnostic was still pointing one layer past the defect.

**Both formats now go through the same conversions**, and the array-oid ->
element-type map is shared between them, so the two cannot drift again. A
parameter's meaning must not depend on the format a client happened to pick.

**The second half is the resolution rule again.** psycopg leaves the type
UNSPECIFIED for lists and datetimes and lets the server infer -- so the value
arrives as text in EITHER format, and must be resolved from the operand beside
it. 0.22 implemented that for literals and 0.23 extended it to comparison; this
extends it to `ParamRef`. Three batches to cover one PostgreSQL rule across
literal / parameter and arithmetic / comparison.

**A divergence found and filed rather than papered over:** psycopg dumps a list
of small ints as `smallint[]`, and PostgreSQL has NO `integer[] = smallint[]`
operator -- array operators require identical element types and do not widen -- so
`array[1,2,3] = %s` is 42883 there and `true` here. Reproducing that needs
PostgreSQL's operator-resolution table for arrays. The case was removed from the
differential list (where it had been added asserting OUR answer) and written
into the backlog.

**Probe: 9 shapes x 2 formats, 0 divergences.**

### 0.25 json and jsonb (2026-09-02)

**~1487 -> ~1615, +128.** The predicted ceiling was 136 (json 68 + jsonb 68), so
nearly all of it.

**Two rules, both measured, neither derivable.** `json` keeps its input text
verbatim -- whitespace, key order, duplicate keys. `jsonb` normalises: keys
sorted, last duplicate wins, canonical spacing. And `jsonb` sorts keys by BYTE
LENGTH first, then bytewise: `z` before `é` (one byte against two), `b` before
`aa`. Not alphabetical, not by character count.

**Numbers are what rule out a library.** A `jsonb` number is a `numeric` and
prints as one: the exponent expands (`-1.5e10` -> `-15000000000`) AND the
literal's trailing zero survives (`1.10` stays `1.10`, it is the scale). Every
general-purpose JSON parser reads numbers into `f64` by default, which gets the
first right and silently loses the second. So the parser here keeps number
tokens as TEXT and normalises them the way `numeric` renders.

**A sniffing bug found by the malformed-input half of the probe.** `'01'::json`
was ACCEPTED, because an unspecified-type parameter is guessed from its text and
`01` parsed as the integer 1 -- so invalid JSON became valid before the cast
ran. **A guess made on the client's behalf must never make a value more
acceptable than the client wrote it.** Sniffing now requires the number to
round-trip to the same text. Probing the REFUSALS as well as the accepted shapes
is what surfaced it; a probe of valid documents alone passes.

**Probe: 38 shapes -- 29 valid, 9 malformed -- 0 divergences.**

### 0.26 scalar built-ins, of which there were NONE (2026-09-03)

**1615 -> ~1633, +18 on the gauge and far more than that in surface.**

**A survey of 37 common scalar functions found 37 missing.** `upper`, `length`,
`abs`, `round`, `coalesce` -- the Rust planner had no scalar function table at
all, only a zero-argument `session_function` for `current_user` and friends.
(The Python server's `src/secantus/sql/scalar.py` has had them for months; the
two servers share no code here.)

**+18 is the honest number and it understates the work**, the same way the
decimal-arithmetic batch did: the gauge measures psycopg's corpus, not this
server's surface. Forty functions that did not exist now work.

**A probe can hide the bug it was written to find.** Every case in the first
probe was `select (f(...))::text` -- and a cast routes through `const_value`,
where the new table was wired. A BARE `select upper('a')` goes through the
target list, which was not. All 71 cases passed while the plainest possible
call still failed. A unit test written from the same list caught it, because it
did not carry the cast. **Vary the SHAPE of the call, not just the arguments.**

**Result TYPES were where the remaining divergences were**, and only appeared
once the probe compared result oids as well as values:

* `sign(-3)` is `float8` (-1.0), not `int4`.
* `div(7,3)` is `numeric`, not `int8` -- it is defined on numeric.
* `round` splits by argument type exactly as the integer casts do: half away
  from zero for numeric, half to even for float8.
* `nullif(1,1)` is `int4` -- it answers its LEFT operand's type even when the
  value is NULL. Typing from the value gave `text`, because a NULL cannot
  report a type. Fixed generally: a literal now types from its own AST node.

**NULL handling is not uniform.** Most built-ins propagate; `concat` /
`concat_ws` SKIP nulls, and `greatest` / `least` IGNORE them
(`greatest(1, NULL)` is 1). A comment claiming the last two were handled "by
the filter below rather than by name" was contradicted by the code one line
above it -- the third instance this session of a comment asserting something
the code does not do.

**Probe: 60 shapes, values AND result oids, 0 divergences.**

### 0.27 range types (2026-09-03)

**1634 -> ~1692, +58.**

**Ranking by ERROR signature had been hiding this.** The blocker list put the
range constructors at 15 tests each; ranking by FILE put `test_range.py` at 238
and `test_multirange.py` at 182 -- 420 together, the largest coherent group
left. The same failure reaches the list under many different messages, so an
error-signature ranking undercounts anything with more than one way to fail.
**Rank both ways.**

**The whole design is one split: discrete versus continuous.** PostgreSQL
rewrites every bound of a DISCRETE range to `[)` -- `[1,5]` becomes `[1,6)`,
`(1,5)` becomes `[2,5)` -- so that two spellings of one range are one value.
Over a CONTINUOUS type there is no rewrite, because there is no next value to
move a bound to. `int4range` / `int8range` / `daterange` are discrete;
`numrange` / `tsrange` / `tstzrange` are not. Getting it wrong makes
`'[1,5]'::int4range = '[1,6)'::int4range` answer false.

**Details a real server tells you and reasoning does not:** an absent bound
prints as NOTHING (`(,5)`, not `(-infinity,5)`); bounds that meet without
including each other make the range EMPTY, so `int4range(1,1)` is `empty`; and a
bound is quoted when its text would be ambiguous inside the brackets, which a
timestamp always is.

**Three mistakes, three SQLSTATE classes**, all of which refuse the query and
none of which mean the same thing: a crossed bound is `22000` (data), a
malformed literal `22P02` (invalid text), bad bound flags `42601` (syntax). The
first two were both `22P02` until the probe compared codes.

**Probe: 32 shapes -- 27 valid, 5 refused -- 0 divergences.**

**Still open here:** multirange (`int4multirange` etc.), and the range OPERATORS
(`@>`, `<@`, `&&`), which `test_range.py` exercises heavily.

### 0.28 ranges as parameters -- and a bug 0.27 shipped (2026-09-03)

**1693 -> ~1790, +96.** Two thirds of it was fixing what the previous batch
broke.

**0.27's probe covered only LITERAL ranges, and the gauge said so within the
hour.** `malformed range literal: "bounds must be text"` appeared 120 times --
this server's own internal placeholder string, quoted back at the client as
though it were the user's input. Every `int4range(%s, %s, %s)` failed.

**The cause is Describe running before Bind.** At plan time every parameter is
NULL, and a NULL flags argument IS an error in PostgreSQL
(`int4range(1,5,null)` -> 22000). Rejecting it at describe time rejected every
parameterised constructor. The two have to be told apart by the AST: a `null`
WRITTEN in the query is an error, a not-yet-bound `ParamRef` is not. **This is
the third time this session that Describe-before-Bind has produced a bug** (the
first typed `$1::int` as varchar, the second typed a NULL result as text) --
it is worth checking every new plan-time refusal against it.

**+42 from that fix alone**, before any new feature.

**Then binary range parameters (+~50):** the wire form is a flags byte and each
present bound length-prefixed in the ELEMENT's binary format, so it reuses the
element decoder rather than needing a range-specific one.

**And a gap underneath both:** a `tsrange` must ORDER its bounds to
canonicalise, and two timestamp COMPOSITES (the sub-millisecond form) had no
comparison arm -- so the error said "comparing timestamp range bounds", naming
ranges for a hole in timestamp comparison. Fixed generally: anything that names
an instant now compares as one.

**Probe: 12 constructor shapes + 18 value shapes across both formats, 0
divergences.** The lesson from 0.27 restated: **probe the PARAMETER form as well
as the literal form.** They are different code paths in this server, and a
literal-only probe passed everything while the parameter form was broken for
every single case.

### 0.29 multiranges (2026-09-03)

**1789 -> ~1845, +56.** `test_multirange.py` was the second-largest file at 182.

**One normal form: sorted, empties dropped, members that MEET folded together.**
The last is the rule worth stating precisely, because it is not "overlapping":
`{[1,5),[5,8)}` collapses to `{[1,8)}` even though the two share no point --
nothing lies between them. `{[1,5),[6,8)}` stays apart because 5 does. So the
test is "does the next member start at or before this one ends", and AT the
meeting point it comes down to the bounds: over a continuous type
`[1.0,2.0)`+`[2.0,3.0)` join while `[1.0,2.0)`+`(2.0,3.0)` do not.

Members canonicalise BEFORE merging, so `{[1,5]}` is `{[1,6)}` and the merge
sees the bounds a client would.

**The literal parser splits on brackets, not on commas** -- a range contains
commas of its own, so a naive split cuts every member in half.

**Probed the PARAMETER form alongside the literal form this time**, which is the
correction 0.28 forced: 26 literal shapes and 14 parameter shapes across both
wire formats, 0 divergences on the first run. The previous batch shipped a type
whose literal form was right in every case and whose parameter form was broken
in every case, because the probe covered only literals.

### 0.30 cursors (2026-09-03)

**1845 -> ~1848. A small number in front of a complete feature and a
protocol-level bug**, and the number is explained: the cursor test files are
blocked behind `generate_series` (96) and `RangeFunction` (54), not behind
cursors.

**The enum campaign was SIZED first and deferred on evidence.** psycopg's
`TypeInfo.fetch` needs `pg_type` + `pg_enum`, a LEFT JOIN, a subquery in FROM
and `array_agg` -- four separate features, all measured as absent. That is a
campaign, not a batch, and it is now recorded as such rather than re-guessed
every time it tops the file ranking.

**PostgreSQL's cursor position has two ends beyond the rows.** The cursor sits
ON a 1-based row, and can also sit at 0 (before the first) or len+1 (after the
last). Fetching past the end parks it at len+1, so `MOVE BACKWARD 2` afterwards
lands on the LAST row. A "next index to read" model is off by one exactly
there -- which is the case anyone tries first.

Three more measured rules: a BACKWARD fetch returns rows NEAREST-FIRST;
`RELATIVE`/`ABSOLUTE` fetch ONE row where `FORWARD`/`BACKWARD` fetch a run; and
`FETCH ALL` arrives as `i64::MAX`, so every arithmetic step must saturate --
adding to it overflowed and killed the connection.

**Two bugs of my own, both found by probing rather than reading:**

* `block_on` inside the sync `execute` HUNG the connection outright. Collecting
  a row stream needs to await, so DECLARE runs in the async `run` instead --
  beside transaction control, which is there for the same reason.
* The command tag must be `FETCH`, not `FETCH 2`: the wire layer appends the
  row count, so building it here produced `FETCH 2 2`.

**And one that was not cursor-specific at all.** `describe_fields` had no arm
for `Fetch`, so a PREPARED fetch described ZERO columns -- and psycopg prepares
any statement it runs six times. A cursor read in a loop worked five times and
then sent rows with no description: a protocol violation, in the ordinary way
of using a cursor. **It was invisible to a short probe**; the sequence had to
be long enough to cross the prepare threshold, which is why bisecting the long
probe rather than trusting the short one is what found it.

**Probe: 29 cursor operations, 0 divergences.**

### 0.31 generate_series (2026-09-03)

**~1848 -> ~1870, +22.**

**A FINISHED FEATURE CAN BE UNREACHABLE.** Cursors landed in 0.30 matching
PostgreSQL on 29 probed operations, and the gauge moved 3 -- because the usual
way to give a cursor rows to scroll over, without inventing a table, is
`generate_series`. 0.30 measured that and filed it rather than guessing; this
batch collects it.

**A SOURCE, not a statement.** `Select` and `Aggregate` grew an optional
`series` field, so ORDER BY / LIMIT / OFFSET / count / sum / min / max all work
on generated rows unchanged. The alternative -- a `SelectSeries` statement --
would have needed every one of those reimplemented.

**Four places needed the source, not one**, and each was found by re-probing
rather than by reading: the SELECT execution path, the AGGREGATE execution
path, `describe_fields` for both, and the schema built inside `execute` for the
aggregate. Three of the four surfaced as `relation "" does not exist` -- the
empty table name leaking out -- which is a useful shape to recognise: it means
a code path that still expects a table.

**Two rules that read backwards:** counting up towards a smaller stop is EMPTY,
not reversed (`generate_series(5,1)` gives nothing; counting down needs a
negative step); and a zero step is `22023` (invalid_parameter_value), which
PostgreSQL keeps distinct from the general `22000` data class.

**A stale-binary near miss.** After adding the 22023 error I read `grep -c
'^error'` as a count of *lines* and moved straight to measuring -- but it was
2 compile errors, and the gauge ran against the PREVIOUS binary. The +23 it
reported was fiction. **Check that the build succeeded before believing a
measurement**, not merely that the command exited.

**A WHERE over a series is REFUSED**, not ignored: the filter language is built
against stored columns, and dropping a predicate silently would return rows the
client asked to exclude.

**Probe: 17 shapes, 0 divergences.**

### 0.32 COPY in every format (2026-09-05)

**~1870 -> ~1933, +63.**

**Every bug in this batch was NULL versus empty string.** The three formats
spell NULL differently -- text `\N`, CSV an UNQUOTED empty field (so the empty
STRING must be quoted `""`), binary a length of -1 (empty string is length 0) --
and each distinction broke at least once. The failure is always silent, and it
round-trips within one format while corrupting across formats.

**The binary one had an ordinary cause worth repeating:** a catch-all match arm
sat ABOVE the NULL arm and swallowed it, rendering NULL as text and writing a
zero-length field. Arms are tried in order; a catch-all must come after every
case it must not absorb.

**pgwire's encoder could not be used for the textual formats.** It decides
nullness by asking the value, and an `Option` of the wrong type answered "not
null" with no bytes -- so NULL came out empty. The rules are short and already
probed, so text and CSV are written here; binary keeps the encoder, where the
per-type byte layout is the hard part.

**+63, and 72 of it came from `copy (select 1) to stdout`** -- a query with no
FROM, which clients use to check how a server reports a BAD query. Refusing it
failed a whole file that was not about COPY. `COPY (query)` reuses the ordinary
select path, so ordering, limits and a generated series all work inside a COPY.

**The remaining COPY failures are NOT COPY.** 195 are a cascading
`relation "copy_in" does not exist`. `serial`, range and multirange columns were
each checked directly and all work, so the fixture is failing for some other
reason -- filed rather than guessed at, because three plausible causes were
eliminated by measurement and the fourth is not yet known.

**Probe: 11 output shapes byte-for-byte (including binary) and 3 input shapes,
0 divergences.**

### 0.33 DDL inside a transaction (2026-09-05)

**~1933 -> ~2117, +184.** The cascade 0.32 filed, chased to its cause.

**Planning read the catalog OUTSIDE the open transaction.** Resolving a table
name happens at plan time; the catalog is an ordinary table; so an uncommitted
`CREATE TABLE` was invisible and the next statement said the relation did not
exist. EXECUTION already ran inside the transaction, which is exactly why this
hid so well: anything using a PRE-EXISTING table worked perfectly. Only
create-then-use, uncommitted, failed -- the ordinary shape of a test fixture.

**Two wrong fixes before the right one, both instructive:**

1. Wrapping the PLAN in a second `with_user_transaction` worked for plain
   statements and DEADLOCKED COPY, which opens its own transaction context.
   Nesting that call is not safe.
2. Recording created tables in a per-connection map, but asking `txn` whether a
   transaction was open -- from inside `execute`, which `run` calls while
   HOLDING that mutex. Non-reentrant, so it deadlocked the moment it mattered.
   The working version reads a lock-free `AtomicBool` instead.

**And a third bug underneath, never before reachable:** COPY inside a
transaction wrote its rows OUTSIDE it, blocking against the transaction's own
locks and hanging the connection. Nobody had seen it because resolving the
table failed first whenever a transaction was open. **Fixing one bug exposed a
worse one that had been hiding behind it.**

**DDL IS NOT TRANSACTIONAL, and that is now filed rather than assumed.** A
`CREATE TABLE` survives ROLLBACK (with its rows); a `DROP TABLE` stays dropped.
PostgreSQL rolls back both. Measured in both directions. The tests assert what
this server actually does and say so; the divergence is in `tasks/backlog.md`
as a STORAGE-level item, since `create_collection` / `drop_collection` are not
part of the user transaction.

**Also: `ORDER BY <position>`** -- an ordinal into the SELECT LIST, not the
table, so `select b, a from t order by 1` orders by `b`; out of range is 42P10.
Worth +4 on its own and needed by the same fixtures.

### 0.34 set-returning functions in the SELECT LIST (2026-09-05)

**2119 -> ~2181, +62.**

**`generate_series` was back at 102 in the ranking after 0.31 implemented it.**
Not a regression: 0.31 built the `FROM generate_series(...)` form, and the
corpus mostly writes `select generate_series(1, 10)` -- the function in the
SELECT LIST, no FROM. **Implementing a feature does not retire its ranking
entry; check WHICH SHAPE the remaining tests use.**

Planned as a select over a generated source, so it is the same statement the
FROM form produces and ORDER BY / LIMIT / OFFSET came along unchanged. A SRF
BESIDE another column (`select 1, generate_series(1,3)`, which repeats the
constant per row) is refused: it needs the other columns carried into each
generated row, and nothing in the corpus asks for it.

**Multirange binary parameters (+22)**, finishing 0.29: a count of ranges then
each one in the RANGE's own binary form, reusing the range decoder rather than
restating the layout.

**Also confirmed by re-ranking the binary-parameter oids:** what remains there
(uuid, inet, cidr, bytea, json arrays) is types this server does not have, not
decoder gaps. That entry is now exhausted as a source of cheap wins.

**Probe: 8 select-list shapes and 10 multirange parameter shapes, 0
divergences.**

### 0.35 binary result columns, and a cursor is a portal (2026-09-05)

**2118 -> 2146 on a same-branch baseline, +29 / -1.** Measured off `main`
BEFORE the select-list-SRF batch (0.34) landed, so it does not stack with that
number arithmetically; the two are independent.

**Both fixes were sized from the failure ranking and both came in UNDER it,
for the same reason: a ranking counts the FIRST error a test hits, not the
only one.** "Portal not found" was 44 tests; fixing it gained 12, because the
other 32 go on to hit `transaction_status` or a missing feature. The result
format was 46; fixing it gained 13. **Read a ranking entry as an UPPER BOUND
on what one fix can buy, never as the figure.**

**Result format.** Every `FieldInfo` this server built said TEXT, so a cursor
opened with `binary=True` got text rows -- with the RIGHT VALUES, because the
format travels per column in the RowDescription and psycopg decoded what it
was told. Nothing was wrong except the thing the client asked for, which is
why no row comparison had ever caught it. Now honoured for the types that can
be encoded exactly (bool, the int and float widths, the string types,
`numeric`, arrays of those); everything else is still described as text, which
the client reads correctly. Backlog carries the gap.

The trap worth keeping: a value is encoded from the BSON it is STORED as, and
the column's DECLARED type is a separate thing. An `int8` column holding a BSON
`Int32` renders `1` either way in text, but in binary it would put four bytes
where the client reads eight -- so the binary path encodes through the DECLARED
type, and the text path was left exactly as it was.

`numeric`'s binary layout is base-10000 groups aligned on the DECIMAL POINT,
not on the digit string: `0.00001` is one group of `1000` at weight -2, not a
group straddling the point.

**Server cursors.** PostgreSQL exposes a DECLAREd cursor as a portal of the
same name, and psycopg describes that portal straight after the DECLARE,
before any FETCH. Our cursors are not pgwire portals, so pgwire answered
"portal not found" and every server cursor died on its first row. `on_describe`
now answers from the cursor of that name.

**Probes: 44 format shapes (both formats) and 5 server-cursor shapes, 0
divergences** -- written and RUN against PostgreSQL 14 BEFORE either fix, which
is what made both diagnoses take minutes rather than a reading of the source.

**Next, from the re-ranking:** `transaction_status` reports IDLE where
PostgreSQL reports INTRANS / INERROR (32 tests, protocol-level, and it is what
the other 32 cursor tests hit once the describe works).

### 0.36 the transaction status, and a failed block that refuses (2026-09-05)

**2181 -> 2219, +51 / -12.**

Every connection reported IDLE whatever the transaction state, because the
status that rides on `ReadyForQuery` is computed by pgwire from the RESPONSE
VARIANT and this server answered `Response::Execution` for BEGIN / COMMIT /
ROLLBACK. `TransactionStart` / `TransactionEnd` is the whole fix for the status
itself -- pgwire already tracks the transitions, including the error state.

The half that was a WRONG ANSWER rather than a cosmetic one: PostgreSQL aborts a
block at the first error and refuses everything after it (`25P02`) until the
block ends, and a `COMMIT` there is a rollback whose TAG says `ROLLBACK`. This
server carried on executing, so a client that shrugged off a mid-transaction
error committed work PostgreSQL would have discarded.

**The first measurement of this batch was -11, and the cause was mine.** The
implicit transaction around a multi-statement simple query cleared
`in_transaction` but not the new failed flag, so one failed batch refused every
later statement on that connection FOREVER -- 47 of the losses were psycopg
FIXTURES failing at setup. Clearing it in `commit_implicit` / `rollback_implicit`
turned -11 into +38. **A new piece of session state has to be cleared on every
path that ends the session's transaction, not just the one that reads well.**

**A same-branch baseline is what caught it.** `main` had moved (0.34 merged, plus
two other PRs), so the previous run's 2181 was not a baseline for this branch;
measuring the branch WITH and WITHOUT the change, minutes apart, is the only
comparison that means anything.

**A correct status makes every OTHER gap more expensive.** Two
`test_transaction.py` tests that used to pass now fail on SAVEPOINT: psycopg
reaches for one when a `conn.transaction()` block nests inside an open
transaction, which it could not see before. That is PostgreSQL's own behaviour
and the right trade, but it means an unsupported statement inside a block now
poisons the rest of the block. Savepoints are the next blocker there
(`tasks/backlog.md`).

**Also fixed while in there:** `START TRANSACTION` answered with `BEGIN`'s
command tag, and `AND CHAIN` left the connection IDLE instead of opening the
next block -- a chained client's next statements were autocommitted one by one.

**Probes: 11 status shapes, 18 failed-block shapes (including whether the rows
SURVIVE), 5 chain/spelling shapes -- 0 divergences.**

**A WT_PANIC that was the HARNESS, not the server.** The ad-hoc probe pattern
used one fixed storage path and started each run with
`pkill …; rm -rf /tmp/probe-pg-home; mkdir …`. `pkill` returns when the SIGNAL
IS DELIVERED, not when the process has finished closing WiredTiger -- so the
`rm -rf` deleted `WiredTigerHS.wt` out from under a server that was still
checkpointing, and it panicked: *"the checkpoint failed, the system must
restart"*, the exact shape of a real storage bug. What identified it was the
FIRST line of the log (`file-size: stat: No such file or directory`) and the
fact that the current server was listed by `lsof` as healthy on its port
throughout. `scratchpad/pgprobe.sh` now gives every probe run its own `mktemp`
directory and waits for the port to be free. **A harness that can manufacture a
storage panic will eventually hide a real one.**

### 0.37 a series whose bounds are parameters (2026-09-05)

**2233 -> 2256, +28 / -5** (the five are the `__del__` unraisable-warning churn
in the cursor tests, which flips between runs on identical code).

`generate_series(1, $1)` was refused with *"generate_series over text is not
supported yet"*. An untyped bound parameter arrives as TEXT and PostgreSQL
resolves it against the function's SIGNATURE; this server checked the decoded
BSON type and gave up. **A ranking entry that names a TYPE (`over text`) is
usually about where the value CAME FROM, not about what the user wrote** -- no
client sends a text series bound on purpose.

Two more from the same 19-shape probe:

* A NULL bound is an EMPTY result on PostgreSQL, not an error.
* `generate_series(1, 3::float8)` matches NO overload there (`42883`) and this
  server TRUNCATED the bound and answered rows -- more permissive than the
  oracle, which is the same class of wrong answer as any other. Fixed by
  refusing, which needed a new `UndefinedFunction` error class.

**Left divergent, both recorded:** all-`int2`-parameter bounds
(`generate_series(%s,%s,%s)`), which PostgreSQL refuses as AMBIGUOUS (`42725`)
and needs parameter OIDS in the planner to reproduce; and the `numeric`
overload, which needs decimal bounds.

**Two harness traps in one sitting, both of which produced a confident wrong
answer:** the probe pattern reused a fixed storage path (see the WT_PANIC note
in 0.36), and then the FIRST run of the fixed probe reported 19 divergences of
19 -- because an older server was still listening on the port, so every
measurement was of the previous binary. `scratchpad/pgprobe.sh` now kills by
PORT, waits for it, and REFUSES TO RUN unless the process it started is the one
answering.

### 0.38 savepoints (2026-09-05)

**2273 -> 2347, +77 / -3** (the three are the `__del__` unraisable-warning churn
in the cursor tests). Largest single batch since 0.29.

**Why it was worth doing even though no client writes the word:** every client
builds a NESTED transaction block out of savepoints, so `with
conn.transaction():` inside another one failed. 158 of the failures were
psycopg's own `OutOfOrderTransactionNesting`, not the 79 that named a savepoint.

**Design, ported from the PYTHON SQL layer** (`src/secantus/sql/engine.py`,
which has had savepoints for a while): WiredTiger has no savepoint, so one is a
set of PRE-IMAGES. Before a statement writes a table, every open savepoint that
has not yet captured that table captures it; `ROLLBACK TO` puts the captured
contents back, `RELEASE` merges the released frame's captures DOWN into the
enclosing savepoint (oldest wins) so an outer rollback can still undo them.
Lazy capture is what makes it affordable. **Reading the existing implementation
first turned a design problem into a port.**

**The bug that ate the most time, and the shape to remember:** the first
version restored NOTHING while reporting success, because the captures and
restores went through `self.storage` directly while the block's writes were
inside the WT user transaction. A read outside the transaction cannot see the
block's uncommitted rows and a write outside it lands in another snapshot ->
`in_open_transaction` now wraps both. **Anything that touches storage from
OUTSIDE the statement path has to be told about the open transaction.**

**A differential lane caught what a standalone probe missed.** The probe ended
its "unknown savepoint name" scenario with a `ROLLBACK`; the differential
followed it with another statement, and PostgreSQL answered `25P02` where we
answered `42703`. Two findings from one line: a FAILED savepoint statement
poisons the block, and **PostgreSQL's abort check runs BEFORE the planner** --
in an aborted block `select nosuchcolumn` is `25P02`, not `42703`. A SYNTAX
error is the exception (`42601` still surfaces), because the parser runs first
there too.

**Also fixed, found by a psycopg FIXTURE rather than a test:** `CREATE TABLE IF
NOT EXISTS` raised `42P07` on an existing table. It only surfaced once
savepoints let the suite get far enough to run that fixture twice -- a
pre-existing bug that a passing test had been hiding.

**Probes: 22 savepoint shapes, 21 failed-block shapes, 11 status shapes -- 0
divergences.**

### 0.39 the type a parameter was SENT as (2026-09-05)

**2347 -> 2589, +255 / -13.** The largest batch of the campaign. (The 13 are
`test_leak` object-count tests, which differ by ~2 between two runs of the same
binary.)

A bound parameter carries a type the CLIENT declared, and this server threw it
away and re-derived each parameter's meaning from its decoded value. Where the
two disagree the answers were wrong in ways that PRINT CORRECTLY:

* `pg_typeof(%s)` with a small integer said `integer`; psycopg declared `int2`,
  so PostgreSQL says `smallint`. 69 tests were on that one line.
* A parameter with NO declared type has no type to report at all -- PostgreSQL
  answers `42P18`, this server guessed `text`.
* A range parameter kept the client's spelling: `int4range(10, 20, '[]') = $1`
  bound to that very range was FALSE, while both sides printed `[10,21)`.

**The declared types ride a thread-local**, as the session zone already does --
they are needed deep in the expression walk, and threading them through every
planner signature would touch every function to reach two of them.

**They have to reach the DESCRIBE path too, and that is where an hour went.**
The types were arriving (a debug print confirmed `Some(Int2)`), the execute path
had them, and the client still saw `42P18` -- because psycopg DESCRIBES before
it executes, and the describe planned the same statement with no types at all.
**When a fix "does not take" on a statement a client prepares, check the
describe: it runs first and its error is the one the client sees.**

**Ranges needed two routes, not one.** A range parameter that arrives WITH its
type now decodes through the same cast a literal takes (it was falling through
to the untyped sniffer); one that arrives UNTYPED takes its type from the
operand beside it, read off the EXPRESSION -- a range value is carried as its
rendered text, so by the time two operands are values there is nothing left to
tell `'[10,21)'` from any other string.

**A test case written by hand was wrong where the probe was right**:
`int4range(11, 20, '(]') = Range(10, 20, '(]')` is FALSE on PostgreSQL too
(`[12,21)` vs `[11,21)`). The probe had the pairing right because it was
checked against the oracle; the hand-written test was not.

**Probes: 22 `pg_typeof` shapes, 26 range-parameter shapes (both formats) -- 0
divergences.**

**Standing rule earned three times over: after resolving a KEEP-BOTH rebase
conflict, run `cargo test -p secantus-pgplan` and `ruff format`.** Concatenating
the two sides of a conflict is right for files where both sides only APPEND,
but it drops the blank line or the closing brace between them: twice it left
`tests.rs` with an unclosed delimiter (which the Python suite cannot see,
because it does not compile the Rust unit tests) and once it failed CI's
`ruff format --check` on two missing blank lines.

**Left divergent and recorded:** `pg_typeof` answers `text` rather than
`regtype`, so the NAME matches but a binary cursor gets the name where
PostgreSQL sends the four-byte oid.

### 0.39a the numbers in 0.34-0.39 were taken with a BROKEN INVOCATION (2026-09-05)

**Every figure in sections 0.34 through 0.39 is ~152 too low**, and the cause is
how the gauge was invoked, not anything about the server.

psycopg's `tests/test_typing.py` (125 tests) shells out to a bare `mypy`.
`pyproject.toml` carries mypy in the dev extras precisely so those tests count,
and the comment there says why: **`uv run` puts `.venv/bin` on PATH**. Running
the gauge as `.venv/bin/python -m psycopg_validation.runner` does NOT -- the
subprocess resolves `mypy` through pyenv, finds nothing, and 119 tests fail with
`pyenv: mypy: command not found`. `invoke validate-psycopg` runs it through
`uv run` and they pass.

Measured on the same commit, minutes apart:

| invocation | passed |
| --- | --- |
| `.venv/bin/python -m psycopg_validation.runner` | 2589 |
| `PATH=.venv/bin:$PATH …` (what `uv run` does) | **2741** |

**The DELTAS in those sections still hold** -- both sides of every comparison
were measured the same way, and the same-branch baselines were taken with the
same broken invocation. The ABSOLUTE numbers are not comparable with figures
taken by the invoke task, which is what earlier sections used.

**Rule: run the gauge through `uv run` (or `invoke validate-psycopg`), never
`.venv/bin/python -m …` directly.** A gauge that shells out to a tool is
measuring the PATH as much as the server, and the failure looks like 119 server
failures rather than a missing binary.

### 0.40 an array takes its type from its ELEMENTS (2026-09-05)

**2739 -> 2886, +169 / -22** (all 22 are `test_leak` churn), on the CANONICAL
scale of 0.39a -- this and everything after it is measured through `uv run`.

`array[%s::float4]` came back as an array of STRINGS, as did every array built
over a parameter. The values were computed correctly and DESCRIBED wrongly: the
array's type was read off the values, and **the describe pass sees no values at
all** -- every parameter is NULL there -- so it settled on `text[]`, and the
client decoded floats as text because the row description is what it believes.
Same lesson as 0.39, one layer up: **anything typed from a VALUE is wrong on
the describe path.**

The type now comes from the elements' EXPRESSIONS. Mixed numerics widen in
PostgreSQL's order -- **measured, not assumed**: `int2 < int4 < int8 < numeric
< float4 < float8`, so `array[1, 1.5]` is `numeric[]` and `array[1::float4,
1.5]` is `float4[]` (the float wins over the numeric). A bare NULL literal
contributes no type.

**Getting the type right exposed the next bug in the same path**: `array[1,
1.5]` then returned `[Decimal('1'), None]`, because the text encoder chose its
element conversion from the FIRST element rather than from the column's type.
Arrays now go through the typed encoder in text as well as binary.

**A one-word fix worth 34 tests:** `array_element` treated any element starting
with `{` as a nested array, including a QUOTED one, so `'{"{"}'::text[]`
answered "malformed array literal". `{` is an ordinary member of any corpus
that walks the ASCII range, so it failed every text-array round-trip.

**Also:** arrays of date / time / timestamp / interval / json had no wire type
and were described as `varchar`.

**And a SILENT DATA LOSS found by widening the probe's alphabet from 1..60 to
1..256:** `array_element` trimmed with Rust's `str::trim`, which strips U+0085
and U+00A0. PostgreSQL keeps them, so an unquoted element that was one of those
two characters came back as the EMPTY STRING -- a character in, nothing out,
and invisible to any test whose alphabet is ASCII. `trim_matches(char::
is_ascii_whitespace)` is the fix. **When a corpus test disagrees on a few
elements out of hundreds, diff the ELEMENTS rather than reading the assertion:
the two that differed named the bug immediately.**

**And a SILENT DATA LOSS found by widening the probe's alphabet from 1..60 to
1..256:** `array_element` trimmed with Rust's `str::trim`, which strips U+0085
and U+00A0. PostgreSQL keeps them, so an unquoted element that was one of those
two characters came back as the EMPTY STRING -- a character in, nothing out,
invisible to any test whose alphabet is ASCII. `trim_matches(char::is_ascii_
whitespace)` is the fix. **When a corpus test disagrees on a few elements out
of hundreds, DIFF THE ELEMENTS rather than reading the assertion**: the two
that differed named the bug immediately.

**Probe: 80 array shapes across both formats -- 0 divergences.**

**Killing a gauge run leaves psycopg's SEGFAULT SENTINEL behind.** Their
`conftest.pytest_sessionstart` sets `.pytest_cache/v/segfault` at session start
and clears it at the end, so an interrupted run makes the NEXT one refuse
everything with an `INTERNALERROR` and *"Previous run resulted in segfault! Not
running any test"* -- which reads like a catastrophic regression and is a stale
file. `rm vendor/psycopg/.pytest_cache/v/segfault`.

**Do not run the gauge and the full pytest suite at the same time.** Tried it
here to save wall-clock: load average hit 8, and the gauge -- normally 90
seconds -- was at 59% after FORTY MINUTES, because its per-test timeout is 20s
and everything was starved. Same failure mode CLAUDE.md records for
`validate-all --jobs 8`, on one machine with two jobs.

**Trajectory: 694 -> 746 -> 853 -> 899 -> 900 -> 904 -> 945 -> 965 -> 984 -> 1043 -> 1215 -> 1295 -> 1372 -> 1388 -> 1485 -> 1615 -> 1633 -> 1692 -> 1790 -> 1845 -> 1848 -> 1870 -> 1933 -> 2117 -> 2181 -> 2219 -> 2256 -> 2347 -> 2589 | 2739 -> 2886 (everything before the bar is on the no-mypy scale of 0.39a; the same commit measures 2741 through `uv run`. 0.35 measured +29 on a pre-0.34 base and is not in this line).**

**Re-measured after rebasing onto a `main` that had gained seven parallel
pgserver PRs: that `main` scores 946 on its own and 982 with this batch, so the
batch is +36 there.** The 965 and 984 figures above were taken against the
older `main` WITH this batch's numeric half already applied, which is why they
do not subtract to the same number. **A gauge figure is only meaningful next to
the baseline it was measured against**, and with several sessions landing in
these crates the baseline moves within a day.

**A type-system trap worth remembering.** `Describe` runs BEFORE `Bind`, so a
column's type cannot be inferred from its value — at that point `$1::int` has
no value. Inferring typed it `varchar`, and the client then decoded a correct
integer as a string. Types must come from the expression that declares them.
The same fix caught `text` being reported as `varchar` (oid 25 vs 1043):
psycopg decodes both to `str` so no value comparison notices, but pgjdbc and
pgx read the oid.

### 0.7 P5, first batch (2026-08-31): three-valued logic is where the bugs are

Added: `ORDER BY` (direction + `NULLS FIRST/LAST`), `LIMIT`, `OFFSET`,
`UPDATE`, `DELETE`, `IS [NOT] NULL`, `IN`, `NOT IN`, `BETWEEN`, `NOT BETWEEN`,
`NOT`. Pinned by `tests/test_rust_pgserver_differential.py` — **83 statements
run against a live PostgreSQL 14 and ours, compared verbatim**.

**Every bug in this batch was a NULL bug, and every one returned wrong ROWS
rather than an error:**

| construct | MQL's answer | PostgreSQL's | fix |
|---|---|---|---|
| `ORDER BY n` (ASC) | null sorts LOW → first | NULLs **LAST** | sort in Rust, do not push down |
| `ORDER BY n DESC` | null sorts LOW → last | NULLs **FIRST** | same |
| `n <> 1` | `$ne` matches null → row returned | NULL excluded | explicit `$ne: null` guard |
| `n NOT IN (1)` | `$nin` matches null | NULL excluded | same guard |
| `n NOT IN (1, NULL)` | matches rows | **nothing** matches | short-circuit to match-nothing |

`NOT` is lowered by **pushing the negation into the leaves**, not by wrapping:
MQL has no operator meaning SQL's `NOT` (`$nor` matches missing-or-null). De
Morgan is valid in Kleene logic, so the descent is exact, and every leaf it
reaches is already NULL-correct.

**The magic-number lesson repeated itself and cost real time.** P1 recorded
"match named enums, never protobuf integers" after the `BoolExpr` bug — and this
batch then wrote `AExprKind` as integers anyway, with **two** consequences:
`AEXPR_OP` is 1 not 0, so every plain `=` was refused; and `BETWEEN` is 11 not
10, so `BETWEEN` silently ran the `NOT BETWEEN` arm and returned the complement.
All enum handling is now `try_from` on the named type. If a third instance
appears, make it a lint rather than a comment.

**Aggregates landed in the same batch**, with PostgreSQL's rules probed rather
than assumed: `count(*)` counts ROWS while every other aggregate skips NULLs;
over an empty input `count` is 0 but `sum`/`min`/`max` are **NULL**; NULL forms
its own GROUP BY group; and the result types are `int8` for count/sum but the
INPUT type for min/max. `avg` is refused — PostgreSQL returns `numeric` with its
own scale (`avg(int4)` over {1,3} is `2.0000000000000000`), and approximating it
would be a wrong answer.

Two aggregate bugs, both from keying output rows by NAME:

* `SELECT count(*), count(n)` produces two columns both called `count`; the
  name-keyed row let the second overwrite the first, so `count(*)` reported
  `count(n)`'s answer.
* `GROUP BY s ORDER BY s` with `s` unprojected lost the sort key entirely.

Both are fixed by making the output POSITIONAL — `OutputCol::Group(i)` /
`OutputCol::Agg(i)` — and resolving ORDER BY to a group INDEX rather than an
output name. Worth remembering as its own shape: **SQL output columns are
positional and their names are not unique**, so any structure keyed by name will
eventually drop one.

**Method note:** the differential harness was written before most of these
fixes, and it found all of them. Write the oracle comparison first; it is
cheaper than reasoning about three-valued logic and it does not talk itself into
a wrong answer.

**The harness had a bug of its own, and only the FULL suite found it.** It
created a fixed-name table `d` in the shared `public` schema of the single local
PostgreSQL, so xdist workers raced on `CREATE TABLE d` — 35 failures under
`-n auto`, every one of which passed serially. Each worker now gets its own
schema. A second run then surfaced a connection leak in the same file: the
`skipif` probe opened a connection at import and never closed it, one per worker
for the whole session. **Running only the new test file proves nothing about
either.**

### 0.6 P1 vertical slice (2026-08-31): the seam holds on real storage

`secantusd-pg` serves CREATE TABLE / INSERT / single-table SELECT from a real
`secantus-storage` home. 17 tests in `tests/test_rust_pgserver_slice.py`.

**The cross-server contract works in both directions** — this is the headline,
because §5 called it the one constraint that is silent data loss if wrong:

- Rust creates a table and inserts → the **Python** server reads it, and writes
  a further row → the Rust server reads Python's row.
- Python creates a table and inserts → the **Rust** server reads it.

**Two real bugs the slice surfaced, both worth recording:**

1. **Acknowledged writes did not survive SIGTERM.** The first cut had no signal
   handler, so the process died with no WiredTiger checkpoint: after
   CREATE TABLE + INSERT the client had been told both succeeded, and reopening
   the store found the catalog document *and* the rows gone. `secantusd-rs`
   already installs a SIGINT/SIGTERM handler for exactly this reason; `-pg` now
   does too, with a named regression test.
2. **A duplicate key leaked the MongoDB persona.** Storage answers
   `E11000 duplicate key error collection: postgres.t index: _id_ ...`. Real
   PostgreSQL 14 answers `duplicate key value violates unique constraint
   "t_pkey"` with `DETAIL: Key (id)=(1) already exists.`; ours now matches it
   byte for byte.

**A `pgwire` 0.31 limitation to carry forward:** its `ErrorInfo` exposes
severity / code / message / detail / hint / position / where / file / line /
routine, but **not** the protocol's schema / table / column / constraint fields.
Real PostgreSQL sends `constraint_name` and `table_name` on a 23505, and pgjdbc
surfaces them via `getServerErrorMessage().getConstraint()`. If a gauge starts
asserting them, either upstream the fields or hand-roll the codec — this is the
first concrete cost of the crate-over-hand-roll decision.

Also worth knowing: an early `BoolExpr` lowering matched the wire integer
(`0 => $and`) instead of the named enum (`AND = 1`), silently turning every
`AND` into an `OR`. A unit test caught it. Match named enums, never protobuf
integers.

### 0.5 What the spike did NOT prove

- ~~**Rows came from memory, not `secantus-storage`.**~~ **CLOSED by P1** — the
  slice runs on real WiredTiger; see §0.6.
- **Nothing beyond single-table SELECT with comparison/boolean predicates.**
  Joins, aggregates, subqueries, DML and DDL — the actual bulk of P5 — are
  untouched. The spike shows the seam is real; it says nothing about how long
  walking it takes.
- **The §8 test-transfer problem is unaddressed** and remains the most
  under-estimated cost in this plan.

---
## 1. What this is, and what it is not

A **third server**, alongside the two `tasks/rust-server-plan.md` establishes:

| server | wire | request path |
|---|---|---|
| Python server | MongoDB | pure Python |
| Rust server | MongoDB | pure Rust |
| **Rust PG server (this plan)** | **PostgreSQL v3** | **pure Rust** |

The same rule applies verbatim: **no Python in the request path, no PyO3 in the
hot path, no fallback into Python operators.** The Python surface is a thin
lifecycle handle (`start` / `stop` / `.address`), exactly as `secantus-server-py`
is for the Mongo server. The Python `SecantusPGServer` stays first-class and
permanent — it is the reference implementation and the behavioural oracle.

**This is not a port of `src/secantus/sql/`.** It is a reimplementation of the
same *contract*, measured against the same gauges. That distinction decides
several choices below, most importantly the parser.

## 2. The finding that makes this tractable: SQL compiles to MQL

Measured 2026-08-31. This is the load-bearing fact of the whole plan.

`planner._expr_to_filter` lowers a SQL predicate to a **Mongo query filter
dict**. `executor.execute_pipeline_select` runs a **literal Mongo aggregation
pipeline** through `secantus.aggregate.apply_pipeline` (7 call sites). Joins,
GROUP BY and window sources execute as `$lookup` / `$unwind` / `$group` /
`$project` — 71 pipeline-stage literals in `planner.py` alone. A SQL table is a
collection; a row is a BSON document; the catalog is documents in a per-db
`__sql_catalog__` collection.

So the SQL engine's *back half is the Mongo engine*, and that already exists in
Rust and runs at 99.4% across thirteen driver gauges:

| layer | Rust today | lines | reuse |
|---|---|---:|---|
| WT storage, byte-identical on disk | `secantus-storage` | 15,194 | **~100%** — SQL calls the same 20 `Storage` methods |
| query filters + aggregation stages | `secantus-core` | 17,161 | **high** — this *is* the planner's compile target |
| SCRAM-SHA-256 | `secantus-auth` | 458 | direct |
| WT-free `Storage` trait seam | `secantus-commands` | — | the pattern to copy |

**The naive estimate is "rewrite 46,750 lines of Python". That is wrong in the
expensive direction** — the same error `CLAUDE.md` records four times under
"estimates from READING code have been unreliable". The back half is built. What
is missing is the front half: wire, parse, plan, catalog, types.

## 3. The wall, measured

The front half is not uniformly hard. sqlglot's AST is the de-facto IR, and its
coupling is sharply concentrated:

| file | lines | `exp.` refs | distinct node types |
|---|---:|---:|---:|
| `planner.py` | 12,367 | **1,339** | 185 |
| `engine.py` | 6,379 | 384 | 68 |
| `scalar.py` | 4,549 | 318 | 121 |
| `executor.py` | 2,980 | 27 | 23 |
| `pgserver.py` | 1,218 | 4 | — |
| `pgwire.py`, `catalog.py`, `session.py`, `virtual.py`, all 13 type modules | ~13,000 | **0** | 0 |

Totals: **235 distinct node types, 2,815 references, ~528 untyped
`.args[...]` bag accesses.**

Two consequences:

- **~13,000 lines port independently** with no parser question at all — the
  wire codec, catalog, session, `information_schema`/`pg_catalog` virtual
  tables, and every type module.
- **`planner.py` is the schedule.** Not `engine.py`, not the line count. Any
  estimate that does not decompose the planner is not an estimate.

There is **no seam to swap the parser at**. Plan dataclasses are an IR for
*statement shape* only; every expression slot inside them is raw
`exp.Expression` (`CorrelatedSelectPlan.where`, `EvaluatedSelectPlan.out_exprs`,
`UpdatePlan.computed`, `AlterTablePlan.actions`), and `Prepared.stmt` /
`Portal.bound_stmt` hold raw ASTs — parameter binding is AST rewriting. Do not
plan around a seam that does not exist.

## 4. The parser: use PostgreSQL's own, not a reimplementation

**Decision: `pg_query` (pganalyze/pg_query.rs), which statically links
libpg_query — the real `gram.y` from the PostgreSQL server.** `pg_parse`
(paupino) is the protobuf-free variant and is the fallback if the protobuf
dependency is unwelcome. `sqlparser-rs` is rejected: it is a generic
multi-dialect parser, i.e. the same class of tool as sqlglot, and would import
the same class of problem.

**This inverts the risk.** sqlglot is not merely a dependency here, it is a
*defect source*, and the repo has the receipts:

- an interval mis-parse patched at runtime
  (`planner._patch_sqlglot_interval_continuation`) — filed twice, and the
  backlog notes "the cause was one level deeper than filed";
- sqlglot **rewrites a numeric token into a string literal** inside a
  multi-part interval;
- `O(N**2)` parameter binding, because sqlglot re-parents every sibling;
- regex **pre-passes** in `planner.parse` for `MOVE`, `BEGIN … characteristics`,
  `LISTEN`/`NOTIFY`/`UNLISTEN` and multi-name `DROP TABLE`, because sqlglot
  mis-parses them; a segment-parse fallback for batches it rejects whole;
- constructs it "can't tokenize either, needs a parser extension";
- silent normalisations (`STRING` → `TEXT`, collapsed quoted spellings).

Roughly **40 comment sites are compensations for parser bugs rather than
Postgres semantics.** A Rust server on libpg_query does not re-derive any of
them — it parses what PostgreSQL parses, including error positions.

**G7 is the proof case, and the spike confirmed it** (§0.2). `invoke sql-stress`
is **0/6 lanes** because `pgbench -i` cannot COPY: sqlglot splits
`with (freeze on)` into two parameters and invents an option named `on`.
libpg_query returns the correct single `freeze="on"` option. That gauge is a
green-field win for a real-parser implementation, not a regression risk.

**Risk, and it is real: a second vendored C dependency.** WiredTiger already
costs this project meaningful cross-build pain (`cmake/patch_wt_*.py`, musl
`off64_t`, four wheel platforms across cp310–cp313). libpg_query adds another C
build to manylinux2014 / musllinux / macOS arm64 / **Windows AMD64**. Windows is
the exposure — libpg_query gained Windows support only at PG16 and it is the
least-travelled path. **This is the single most likely reason the plan dies, and
P0 exists to find out in days rather than months.**

## 5. Non-negotiable constraint: catalog byte-compatibility

The Python catalog persists `TableDef` as BSON documents in a per-db
`__sql_catalog__` collection inside the *shared* `Storage`. Byte-identical
on-disk layout across servers is an established project invariant (it is what
makes cross-server backup and PITR work).

**A Rust PG server MUST read and write that catalog format exactly**, or the
three servers cannot share a data directory — which forfeits the main reason to
build it. Treat this with the discipline `tasks/rust-perf-findings.md` demands
of the RecordId re-keying: *"a wrong `id_key→RecordId` hop is silent data
loss."* A catalog written subtly wrong by one server and read by another is the
same failure mode. Golden-vector tests on the serialised catalog documents, in
`cargo test`, from P3 onward.

## 6. No fallback — divergence must be an error

The Mongo engines could return `Fallback` and defer to Python. **The two-server
model forbids that here**, and that is correct: an unsupported construct must
answer PostgreSQL's `0A000 feature_not_supported` (or the specific SQLSTATE),
never a wrong answer. This matches the project's standing rule — *"prefer
returning a faithful 'command not supported' error over a half-implemented
feature that silently diverges."*

Practically: the Rust planner ships a **narrowing** unsupported set, and every
narrowing step is gauge-measurable.

## 7. Oracles, and which one wins

Two, and the precedence matters:

1. **A live PostgreSQL 14** (`SECANTUS_PG_ORACLE_DSN`, already used by six test
   files) — the oracle for **correctness**. Where Python-SecantusDB and real PG
   disagree, **PG is right by definition**, exactly as mongod is on the Mongo
   side. A Rust/Python parity test that pins a Python bug is worse than no test.
2. **The Python SQL server** — the oracle for **behaviour and coverage**, i.e.
   what the contract currently is.

`CLAUDE.md` states the trap directly: *"Parity is not correctness. The Rust
parity suites pin the two engines to each other, so they are equally satisfied
by both being wrong — that has happened."* Build the PG-oracle differential
first, and pin Rust↔Python only where PG has already adjudicated.

## 8. The acceptance gate — and a problem with it

External gauges, all Python-server-only today (2026-08-30 baselines):

| gauge | task | baseline to hold |
|---|---|---|
| G1 sqllogictest | `validate-slt` | 52/60 files, 0 unexpected |
| G2 psycopg 3 | `validate-psycopg` | per-category; `test_hstore` 61.5% is the floor |
| G3 pgtest (CockroachDB corpus) | `validate-pgtest` | 49/66, 0 unexpected |
| G4 pgx | `validate-pgx` | **378/378 = 100.0%** |
| G5 pgjdbc | `validate-pgjdbc` | 5711/80/28 = **98.6%** |
| G6 SQLAlchemy | `validate-sqlalchemy` | 978/0/435 = **100.0%** |
| G7 pgbench/psql | `sql-stress` | **0/6 — currently RED** |

**The in-tree suite does not transfer, and this is the plan's most
under-appreciated cost.** There are 3,596 SQL tests across 198 files and 44,636
lines — but roughly **two thirds drive `secantus.sql.run_sql` embedded**, with an
explicit `Session` and `Storage`, not the wire. The Rust server has no embedded
Python entry point *by design*. So:

- **~33 wire-level files** (pg8000 ×16, psycopg ×10, protocol ×23 overlapping)
  transfer by re-pointing a DSN. Do this first — it is the cheap half.
- The embedded majority transfers only by rewriting tests against the wire,
  which is a large, low-glamour, easily-underestimated task. **Budget it
  explicitly or it will be discovered at P7.**

Do not reuse the Mongo `validate-lanes.json` assumption that a gauge runs twice
against both servers: these gauges have never run against a Rust server, and
`tasks.py` says so in every SQL gauge task.

## 9. Phasing

Each phase is independently testable. **P0 is a kill gate.**

- **P0 — spike (days, not weeks).** Prove the premise end to end and nothing
  more: `pg_query` parses, a `SELECT * FROM t WHERE x = 1` lowers to a
  `secantus-core` filter, `secantus-storage` answers it, and a real `psql`
  renders the rows. **Simultaneously build libpg_query on all four wheel
  platforms, Windows included.** Two outcomes justify stopping: libpg_query
  will not cross-build, or the MQL lowering does not survive contact with
  PG's parse tree. Report both as findings.
- **P1 — `secantus-pgwire`.** v3 codec: startup, simple query, extended query,
  COPY. Hand-rolled, mirroring `secantus-wire` (859 lines for the Mongo
  equivalent; `pgwire.py` is 725). Evaluate the `pgwire` crate at P0 but expect
  to hand-roll for consistency with the existing seam.
- **P2 — `secantus-pgserver`: accept loop, handshake, session.** Thread-per-
  connection like `secantus-server`; SCRAM via `secantus-auth`; TLS via rustls.
  `Session` is ~60 fields — port it wholesale, it has zero parser coupling.
- **P3 — catalog + `information_schema`/`pg_catalog`.** `catalog.py` (1,601) and
  `virtual.py` (3,478), both parser-free. **Golden vectors for the on-disk
  catalog format from day one** (§5).
- **P4 — types.** `typemap.py` (1,840) plus 13 type modules (~4,258), all
  parser-free. The BSON↔PG boundary, including `subms.py`'s microsecond
  timestamps. Fuzz against live PG's text and binary renderings.
- **P5 — the planner.** The wall. **Sub-slice by statement shape**, mirroring
  R2's sub-slicing: constant SELECT → single-table SELECT → filters/pushdown →
  joins → aggregates/GROUP BY → subqueries/CTEs → window functions → DML →
  DDL. Each slice moves a gauge number; if it does not, the slice was wrong.
- **P6 — the scalar evaluator.** `scalar.py` (4,549; 121 node types) — the
  per-row interpreter for everything that cannot be pushed down. Largely
  mechanical once P5 fixes the AST shape.
- **P7 — extended protocol.** `pgextended.py` (2,149): Parse/Bind/Describe/
  Execute/Close, prepared statements, portals, binary codecs, PG's "cached plan
  must not change result type" rule. **The wire-test re-pointing from §8 lands
  here.**
- **P8 — gauge parity gate.** Every gauge in §8 runs against both PG servers;
  neither may regress. G7 going 0/6 → green is the headline this effort earns.

## 10. Crate layout

Mirror the WT-free seam exactly — it is the structural lesson of the Mongo
server, and the one whose violation keeps causing red CI:

    secantus-pgwire        pure Rust   v3 codec
    secantus-pgcatalog     pure Rust   catalog + virtual tables
    secantus-pgtypes       pure Rust   BSON <-> PG type map
    secantus-pgplan        pure Rust   pg_query AST -> MQL plans   <- the wall
    secantus-pgserver      pure Rust   accept loop, session, dispatch
    secantus-pgserver-py   PyO3        thin lifecycle handle (excluded)
    secantusd-pg           bin         standalone binary (excluded)

Everything above `secantus-pgserver` talks to a **`Storage` trait with bytes at
the seam**, so only the adapter links WiredTiger. **Heed the recorded trap:
excluded crates are never compiled by `cargo clippy -p …`** — `rust_tasks.py`'s
own docstring warns *"Cargo does not warn about an excluded crate, so this task
reports success having never compiled them."* Extend `rust-gate` in the same
commit that adds each crate.

## 11. Versioning

A third independent version line, per the established rule: feature PRs bump
nothing; the release stamps it. Crate version `0.1.0-beta.N` at its own pace,
lockstep across the `secantus-pg*` crates. It is not tied to either existing
line.

## 12. Honest sizing

Rust equivalents of the front half, at the ~1.3–1.8× line ratio the existing
crates show against their Python counterparts, land around **35,000–50,000 lines
of Rust** — comparable to the entire existing Mongo Rust server (≈39,000 across
`core` + `commands` + `server` + `wire`), which reached 99.4% over many months
and many sessions.

**So: a second effort of the same magnitude as the first, from a materially
better starting position** — storage and the execution engines already exist and
are proven, and the parser is an upgrade rather than a reimplementation.

Do not start P1 before P0 answers the libpg_query cross-build question. And per
the standing rule, **reproduce before working any slice**: this plan is written
from measurement taken on 2026-08-31, and the file that taught this repo to
distrust its own plans is `tasks/remaining-work-plan.md`, which was wrong about
an item in this very session.
