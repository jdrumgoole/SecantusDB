# Plan: a Rust PostgreSQL server

> **Status: P0 SPIKE RUN AND PASSED, 2026-08-31. Phases P1+ not started.**
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

### 0.5 What the spike did NOT prove

- **Rows came from memory, not `secantus-storage`.** Deliberate: storage is the
  already-proven half (the Mongo Rust server runs on it) and linking WT would
  have added build noise to a probe about the front half. Low risk, but it is
  untested here.
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
