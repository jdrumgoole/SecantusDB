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

**Trajectory: 694 -> 746 -> 853 -> 899 -> 900 -> 904 -> 945 -> 965 -> 984 -> 1043.**

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
