---
name: backlog
description: The SecantusDB backlog protocol — how an item is filed, verified, worked and closed, and how the whole backlog is checked against the running servers. Fires when adding to or editing `tasks/backlog-*.md`, when you stub something, defer a slice or find a limitation, when you start or finish a backlog item, when an item needs a decision or one has been made, when you fold, retire or file an item found in passing, on "verify the backlog", "is this item still open", "check the backlog", and before quoting a backlog item as fact.
---

# The backlog is a set of failing tests with prose attached

The backlog is the only record of where the Rust servers differ from the
real products, and it rotted every time it was prose alone: 107 of 178
"open" items were finished work in one audit, and on 2026-10-14 the single
21,000-line file held 526 finished records beside 259 open ones. The fix is
mechanical: **every open item names a test that fails while the item is
open.** Then "is it still open?" is a command, not a reading.

## Where items live

| file | IDs | |
| --- | --- | --- |
| `tasks/backlog-rust-mongodb.md` | `RM-nnn` | the Rust MongoDB server, the Rust storage both Rust servers use, CI / build / release items no one server owns |
| `tasks/backlog-rust-postgres.md` | `RP-nnn` | the Rust PostgreSQL server |
| `tasks/backlog-python-mongodb.md`, `tasks/backlog-python-postgres.md` | none | the DEPRECATED Python servers: a record, not worked, nothing added |

`tasks/backlog.md` is the index; section numbers (`7.077`, `§3.1`) are the
ones the single file used and code comments cite them. An item about both
Rust servers goes in both files, each copy with its own ID and test.

## An item

```markdown
- [ ] **One-line headline: what is wrong, from the user's side.** What is
  there now, what the real product does instead, and how to reproduce it.
  The constraint or decision that shapes the fix, if any.
  Backlog-ID: RM-142
  Verify: tests/backlog/test_rm_explain.py::test_or_branch_index
```

- **`- [ ]` only.** There are no ticked items: a finished item is deleted
  (`tests/test_backlog_markers.py` fails on `- [x]`, on a headline that
  reads as finished unless it leads with `**OPEN —`, and on a Rust item
  without `Backlog-ID` or `Verify`).
- **`Backlog-ID`** is permanent and never reused. `python tools/backlog.py
  ids` gives the next free one to every item without.
- **`Verify:`** names the gap test(s), comma separated -- or `none --
  <reason>` when no test can check it: a performance budget, a CI or
  tooling behaviour, a crash needing a hard reboot, a design decision. Not
  for an item that is merely awkward to test: anything that names a wrong
  answer, an error code or a missing option can be tested.
- **Scope decisions** are plain `- **Scope decision — …**` bullets, with
  the reason; they are not open items and carry no ID.

## A gap test

A test in `tests/backlog/`, marked `@pytest.mark.backlog("RM-142")`, that
asserts what **the real product** does -- mongod 8.2.11, PostgreSQL 15 --
for exactly the behaviour the item says is missing. So it FAILS while the
item is open, and passes the day the item is fixed. `tests/backlog/conftest.py`
has the fixtures (`rust_mongo`, `mongod`, `rust_pg` / `pg`, `pg15`) and
the helpers (`mongo_answer`, `pg_answer`) that turn an error into a value.

- **Every departure fails on an `assert`.** Compare answers, errors
  included; never let the server's exception escape. A test that fails any
  other way is reported BROKEN, because it is not measuring the gap.
- **The expected value is the real product's**, measured: a literal with
  the version in the docstring where it is stable, the live reference
  fixture where it is not. Never the other SecantusDB server's answer.
- **Narrow and fast**: the gap the item names, not its neighbours; seconds.
- Gap tests and regression tests are excluded from the default suite by
  their markers (gap tests fail by design, and both need the reference
  servers) and run through the verifier.

## Filing an item

1. Reproduce it against the real product and the Rust server -- a probe,
   a query (`differential-probe` has the method).
2. Write the gap test and run it; it must fail on an assertion:
   `python -m pytest -o addopts= -m backlog tests/backlog/<file>::<test>`.
3. Add the item to the server's file, run `python tools/backlog.py ids`,
   then add the `Verify:` line.
4. `python tools/backlog.py verify --id RM-142` says `OPEN`.

## Working an item

1. Claim it (a pushed branch; see `CLAUDE.md`), then
   `python tools/backlog.py verify --id RM-142`. `FIXED?` means it was
   done already: confirm and delete the item instead of building anything.
2. Fix it. The gap test now passes.
3. **The gap test becomes the regression test**: change its marker to
   `@pytest.mark.backlog_fixed("RM-142")` and leave it where it is. It keeps
   asserting the real product's answer, the verifier runs it on every
   unscoped run, and it reports `REGRESSED` if the behaviour ever breaks
   again. For the Rust PG backlog, CI's `pg-oracle` job also runs every
   `backlog_fixed` test in `tests/backlog/test_rust_postgres_gaps.py` and
   `tests/backlog/test_rp_*.py`, and `verify --file backlog-rust-postgres.md`,
   on every PR: an RP item a change fixes without closing turns CI red
   (`FIXED?`), so close it in that PR. (A test that needs no reference server can instead move into the
   default suite; keep the ID in its docstring.)
4. **Delete the item** in the same change, and name it in the
   `changelog.d/` fragment with the test that now pins it:
   `Fixes RM-142 (tests/backlog/test_rm_explain.py::test_or_branch_index).`
   That line, and the ID on the test, are how a later reader gets from a fix
   back to its proof.
5. A fix that closes only part of an item: trim the item, keep its ID,
   re-mark the passing tests `backlog_fixed`, and leave only the failing
   ones on its `Verify:` line.

## Other ways an item ends

- **Folded into another**: when two items have one cause and one fix
  (RP-043's cursor scroll and RP-028's tie order were both the planner's
  cost model), move the prose into the surviving item with `(This was
  RP-043.)`, move its tests onto the survivor's `Verify:` line, re-mark
  them with the survivor's ID, and delete the folded item. Its ID stays
  retired.
- **Nothing left that is a divergence**: when what remains is not a wrong
  answer (RP-015's last bullet was XML output that differed only because the
  two databases hold different objects), delete the item, keep its
  regression tests, and write any rule the work settled as a `Scope
  decision` bullet.
- **A remainder no test can check**: point `Verify:` at `none -- <what is
  left>`, never at a test that now passes -- a passing gap test on the line
  reads as `FIXED?` and hides the remainder.

## Decisions

Most items are work; some are a choice that belongs to Joe. It is a
decision when the fix would:

- stand in for the real product's mechanism rather than reproduce it
  (CLAUDE.md: "We always choose the Postgres model"), or answer only part
  of what the real product answers (a sign where PostgreSQL gives a
  platform-dependent magnitude);
- commit a generated artifact (baked data, a profile, a vendored blob);
- change a default that tests, fixtures or users rely on (trusting unknown
  users);
- pick between two answers the real product itself gives (two OpenSSL
  versions, two platforms).

Then:

1. **Size it from a probe first** -- what it would cost, what it would fix,
   what it would not -- and write that into the item under `**Decision
   needed (date):**`, with the options. A decision asked without the
   measurement gets the wrong answer.
2. **Ask Joe one decision at a time** (`AskUserQuestion`, two to four
   options, a recommendation first and why), not as a list in prose.
3. **Record the answer in the item**: replace the paragraph with
   `**Decided (Joe, date): …**` saying what to build. The item is then
   ordinary work. If the decision changes what "correct" means (sign
   only), restate the gap test to assert exactly that, against the real
   product -- that is the decision applied, not the test weakened.
4. **Measure before building what was decided.** A decision made on an
   estimate can still be cheaper or dearer than it looked: RP-005's
   generated data fixed seven languages and not the three that turned out
   to be ICU4X's collator, not its data.

## Found while working

A divergence found in passing -- by a probe, a matrix, a corpus -- is filed
the moment it is confirmed, with its gap test, even if it is fixed in the
same batch: the gap test is the proof, and an unfiled bug fixed silently
leaves no trail. Fix it in the batch when it is a silent wrong answer and
small (jsonb `ORDER BY`, found while building `jsonb_cmp`); otherwise file
it and move on (RP-044, notice deduplication).

## Measuring against the reference

- **Never change the shared reference servers.** A gap test or probe that
  needs objects creates them inside a rolled-back transaction; a corpus
  that needs none uses `VALUES` lists and a `.setup.sql` of `SELECT 1`.
- **Prove a new corpus or matrix is not vacuous**: run it against a build
  WITHOUT the fix and see it diverge (`jsonb_order.sql`: 9 of 9 diverging
  before, 0 after). Two sides failing alike reads as agreement.
- **The reference's answer can depend on how it was built.** CI's
  PostgreSQL 15.19 links OpenSSL 3.0.13 and the macOS one 3.6.3, and a
  root-issued scoped CRL is refused by one and accepted by the other. Find
  the cause (`openssl verify` with each version reproduced it), make the
  probe use a shape both builds agree on, and record in the item which
  behaviour this server follows.
- **Do not edit the backlog or `tests/backlog/` while a verifier or suite
  run is going** -- it reads them -- and do not commit under a run that
  checks provenance: the binary goes stale and every probe aborts.

## Verifying the backlog

```bash
./inv rust-binary-build && ./inv rust-pgserver-build   # debug, from HEAD
python tools/backlog.py verify                         # or --file / --id
```

Needs mongod on `PATH` and PostgreSQL 15 on port 5415
(`SECANTUS_PG_ORACLE_DSN_15`). The binaries' provenance is checked, so a
stale build is refused rather than measured. Each item comes back:

| status | means | do |
| --- | --- | --- |
| `OPEN` | every gap test fails on an assertion | nothing: the item is real |
| `FIXED?` | a gap test passes | confirm with a probe, then delete the item and keep the test as a regression test (above) |
| `BROKEN` | a gap test errored, was skipped, failed off an assertion, or is not marked with the item's ID | repair the test; until then the item is unverified |
| `UNCHECKED` | `Verify: none -- …`, or no Verify line | the first is by design; the second is a defect: write a test |
| `REGRESSED` | a `backlog_fixed` regression test failed | a fixed behaviour broke: re-open the item (same ID) or fix the regression |

Run it before a release, after a large batch, and whenever you are about to
quote an item as true. A `FIXED?` is how a stale item is found; a stale item
is how the same thing gets built twice.

## Rules that do not bend

- **Never weaken a gap test to change its verdict** -- not to make it fail
  (so an item "stays open") and not to make it pass. It asserts the real
  product's answer or it is wrong.
- **Never tick, never keep a finished record.** History lives in git and
  `changelog.d/`.
- **Correct a superseded item; do not add a newer one beside it.**
- **An ID is never reused.** A closed item's ID lives on in its regression
  test's marker and the changelogs; `tools/backlog.py ids` counts those
  (it once took the highest OPEN ID and handed out a closed RP-029 again).
  Check `git log -S'RP-nnn'` when in doubt.
- **Edit the backlog by anchored spans and read the removed lines**
  (`git diff … | grep '^-'`) before committing: slicing between two
  markers has silently deleted neighbouring items.
- The Python servers' files are deprecated: no new items, no IDs, not
  verified.
