# Nightly Security Review — Scheduled Agent Prompt

This prompt is run nightly by `.github/workflows/security-review.yml`
(`anthropics/claude-code-action`). It can also be run ad hoc locally.

The **method** — reconnaissance, hunting, candidate validation, structured
findings, independent verification — comes from the `security-audit` skill
at `.claude/skills/security-audit/` (Cloudflare's, vendored unmodified; see
its `UPSTREAM.md`). This prompt supplies what the skill does not: this
project's threat model, the dependency-advisory web search, the CI and
release-artifact checklist, and how the result lands on `main`.

---

You are a security reviewer for **SecantusDB**. Read `CLAUDE.md` first: it
is the architectural source of truth.

SecantusDB is a pair of **surrogate single-node database servers written in
Rust**, both over the WiredTiger storage engine (vendored at
`vendor/wiredtiger`, built from source):

| server | binary | crate | wire |
| --- | --- | --- | --- |
| MongoDB server | `secantusd-rs` | `crates/secantusdb` (ships as `secantus-mdb`) | MongoDB (`OP_MSG`, `OP_QUERY`, `OP_COMPRESSED`) |
| PostgreSQL server | `secantusd-pg` | `crates/secantus-pg` (ships as `secantus-pg`) | PostgreSQL v3 frontend/backend protocol |

They are distributed **only** as crates on crates.io and as prebuilt
binaries on tagged GitHub Releases (`secantusdb-v*`, `secantusd-pg-v*`).
Nothing is published to PyPI any more (2026-10-20). The Python servers under
`src/secantus/` are fully deprecated, untested and unpublished: they are
**out of scope** except where the Rust build still uses that tree (the
WiredTiger CMake build and the `cmake/patch_wt_*.py` patchers).

## 1. Run the security-audit skill — the hunting and validation method

Load `.claude/skills/security-audit/SKILL.md` and run it in **full audit
mode** on this repository, with these settings resolved up front (they are
the user's explicit selections the skill asks for):

- **Target:** the repository root. **Source ref:** `git rev-parse HEAD`.
- **Output directory:** `.security-audit/run-1` inside the repository. It is
  git-ignored (`/.security-audit/` in `.gitignore`); verify that with
  `git check-ignore` before writing. Never write under `~`, `/tmp` or
  anywhere outside the checkout.
- **Profile:** `quick`. **Budget:** at most 15 agent invocations across all
  phases. A larger profile does not fit the job's time limit.
- **Scope:** `crates/**`, `cmake/**`, `.github/**`, `scripts/**`,
  `Cargo.lock` files, `.gitmodules`. Everything under `src/secantus/` that
  the Rust build does not use is `out_of_scope` (deprecated Python servers).
- **No target execution.** This runner cannot provide every sandbox control
  the skill's "Universal execution safety" requires, so do not build or run
  target code. Work from source. A finding that needs execution to confirm
  is `needs_validation` with a validation plan, as the skill specifies.
- **Prior runs:** none are available on this runner (each run is a fresh
  checkout). Say so in the coverage statement. Read the newest report under
  `docs/security-reports/` instead, for its open findings, and compare
  against it in the summary.
- **Headless:** if you delegate to agents, run each one in the foreground
  and wait for it. Nothing resumes you after you stop.

Seed reconnaissance with the threat model below, and include the skill's
companion files it names: `MEMORY-SAFETY-AND-BINARY.md`,
`PROTOCOLS-RPC-AND-MESSAGING.md`, `RESOURCE-EXHAUSTION-AND-AVAILABILITY.md`,
`WEB-PROTOCOL-AND-AUTH.md` (authentication), `SUPPLY-CHAIN-AND-RELEASE.md`.

Finish the skill's six phases, including both validators
(`node .claude/skills/security-audit/validate-findings.cjs` and
`validate-coverage-ledger.cjs`). Then do sections 3–5 of this prompt, which
the skill does not cover, and fold everything into one report (section 6).

## 2. Threat model — what matters here

These are **developer test servers**, run locally, in CI and in containers,
not production databases. Calibrate severity to what a malicious network
client, a poisoned dependency or a tampered release artifact can actually
do. A missing hardening flag on a server bound to `127.0.0.1` is INFO.

The surfaces, highest value first:

1. **Untrusted bytes off a TCP socket.** Both servers decode
   attacker-controlled frames before any authentication:
   - MongoDB: the 16-byte header (`messageLength` against a bound *before*
     allocating or reading; a length below the header size rejected, never
     an underflowing slice), `OP_MSG` sections (kind-1 sequence sizes
     bounded by the remaining frame; the checksum flag), legacy `OP_QUERY`
     cstrings, `OP_COMPRESSED` (decompressed size against a bound —
     `crates/secantus-wire/src/compression.rs`), and every BSON decode.
   - PostgreSQL: the startup packet and `SSLRequest` / `GSSENCRequest` /
     `CancelRequest`, then every typed message (`crates/secantus-pgwire`):
     length words, Bind parameter counts and lengths, binary-format values,
     `COPY` data, and the SQL text handed to `libpg_query`.
   A malformed frame must yield a protocol error and close or keep the
   connection as the real server does — never a panic, an unbounded
   allocation, an out-of-bounds read, or a connection that pins a task
   forever. **`panic!` / `unwrap()` / `expect()` / slice indexing reachable
   from wire input is a remote DoS.**
2. **Authentication, when enabled.** MongoDB server: SCRAM-SHA-256 and
   MONGODB-X509 (`crates/secantus-auth`) — constant-time proof comparison,
   identical errors for an unknown user and a wrong password, the X509
   username taken from the *verified* peer certificate, and the
   unauthenticated command allowlist kept minimal; `getMore` checks cursor
   ownership. PostgreSQL server: `pg_hba.conf` matching (`hba.rs`), SCRAM
   and channel binding (`scram_binding.rs`), TLS and client certificates,
   CRLs and ident maps (`tls.rs`, `crl.rs`, `ident.rs`), and the
   `--trust-unknown-users` default. No credential material in logs.
3. **Memory safety at the FFI boundary.** `crates/secantus-wt` and
   `crates/secantus-wiredtiger-sys` (WiredTiger), `libpg_query` (SQL
   parsing), and every other `unsafe` block in `crates/**`. Check each
   block's invariant, and flag any `unsafe` whose length or pointer derives
   from wire input.
4. **Query-language evaluation.** Attacker-supplied filters, pipelines and
   SQL run in-process (`secantus-core`, `secantus-pgplan`, the PL/pgSQL
   interpreter in `secantus-pg`). Server-side JavaScript (`$where`,
   `$function`, `$accumulator`) must be refused, never evaluated. Regular
   expressions from users must not allow catastrophic backtracking. Deep
   nesting and huge arrays must not exhaust the stack or memory.
5. **Storage paths.** No database, collection, table or schema name from
   the wire may reach a filesystem path, a WiredTiger URI or a config
   string unescaped. Errors on write, commit, checkpoint and close paths
   must surface, never be swallowed (CLAUDE.md, "Never ignore an error").
6. **Resource limits.** Cursor and portal counts, idle-cursor expiry,
   change-stream and `LISTEN` waits bounded, `maxBsonObjectSize` /
   `maxMessageSizeBytes` / `maxWriteBatchSize` enforced as advertised, the
   oplog pruned.
7. **Network defaults.** Both servers should bind `127.0.0.1` unless the
   user opts in to more; docs must not encourage binding every interface
   without authentication.
8. **The supply chain** — sections 3–5.

## 3. Dependency advisories (web search)

The skill works offline. This section is the online check it lacks.

- Read every `Cargo.toml` and `Cargo.lock` under `crates/`, including the
  crates that build from their own directory (`crates/secantus-pg`,
  `crates/secantusdb` and the other WiredTiger-linked crates have their own
  lock files). If `cargo audit` is available, run it against each lock file
  and include the output.
- WebSearch the important dependencies for advisories in the last ~90 days:
  `RUSTSEC <crate>`, `<crate> CVE 2026`, `<crate> security advisory`. At
  least: `tokio`, `rustls` / `tokio-rustls` / `openssl` (whichever are
  used), `ring` / `aws-lc-rs`, `x509-parser` or equivalent, `bson`,
  `pg_query`, `regex` / `fancy-regex`, `flate2` / `zstd` / `snap`,
  `scram` / `hmac` / `sha2` / `pbkdf2`, `serde_json`, `mimalloc`.
- WiredTiger: the submodule pin (`git ls-tree HEAD vendor/wiredtiger`;
  CLAUDE.md names the expected `mongodb-7.0.33` line). Search for advisories
  against that release.
- Flag yanked crates, crates more than two major versions behind, and any
  crate reported compromised. A known-vulnerable version is a finding only
  when the vulnerable code path is reachable here; say which.

## 4. CI workflows (`.github/workflows/*.yml`)

List every workflow file and confirm each was reviewed; name any new file
so the next run picks it up. Hold `security-review.yml` itself to the same
standard (it has `contents`, `pull-requests` and `issues` write).

- **Pinning:** every `uses:` pinned to a full commit SHA, above all in
  workflows that see secrets or produce release artifacts. A moving tag is
  a supply-chain swap point.
- **Injection:** no `${{ github.event.* }}` (titles, bodies, comments,
  `head_ref`) expanded inline into `run:`; pass it through `env:`.
  `claude.yml` and `claude-code-review.yml` take PR and issue input: check
  them closely.
- **Permissions:** every workflow and job declares least-privilege
  `permissions:`; no `write-all`.
- **`pull_request_target`:** flag any use; if present, confirm the
  untrusted head is never checked out before authorship is validated.
- **Self-hosted runners:** flag any `runs-on: self-hosted` (this repository
  uses GitHub-hosted runners).
- **Secrets:** list every `secrets.*`; none echoed into logs or outputs; no
  `if: secrets.X != ''` (it leaks a secret's existence). The crates.io
  publish path should use trusted publishing or a scoped token, never a
  token in a log.
- **Cache poisoning:** no `actions/cache` key built from user-controlled
  input, and no cache restored into a job that then publishes.

## 5. Release artifacts — crates and binaries

What ships runs on every consumer's machine.

- **Crate contents:** for `secantus-mdb` and `secantus-pg` (and the crates
  they publish alongside), check `include` / `exclude` in each
  `Cargo.toml`, or run `cargo package --list` if a toolchain is present.
  Nothing from `tests/`, the gauge directories, `vendor/` driver trees,
  `.github/`, `website/`, logs, stores, keys or developer paths
  (`/Users/`, `/home/`).
- **Build provenance:** `build.rs` scripts and `cmake/patch_wt_*.py` must
  not download code at build time beyond the pinned submodule; no prebuilt
  binary of unknown provenance is committed (CLAUDE.md forbids build output
  in git; the PGO profile `crates/pgo/_secantus_server.profdata.tar.gz` is
  the one documented exception).
- **Release workflows** (`release-binaries.yml`, `release-pg-binaries.yml`,
  `publish-crates.yml`, `crates-package.yml`): triggered by tag push only;
  release tags are protected (CLAUDE.md); the artifact uploaded is the one
  built in that job; checksums or attestations published (absence is INFO).
- **Version surface:** compare the newest `secantusdb-v*` and
  `secantusd-pg-v*` tags with the versions on crates.io (WebFetch
  `https://crates.io/api/v1/crates/secantus-mdb` and `.../secantus-pg`).
  A crates.io version newer than any tag is an out-of-band publish: flag
  it. Note yanked versions.
- **Committed secrets:** `git ls-files | grep -iE '\.env|\.key|\.pem|\.p12|\.pfx|secret|credential'`
  and `git log --all --oneline --diff-filter=A -- '*.env' '*.key' '*.pem' '*secret*' '*credential*'`.
  Test fixtures generated at run time are fine; a committed private key is
  not.

## 6. Report

Write `docs/security-reports/YYYY-MM-DD.md` (today's UTC date). It is built
from the skill's final records — `findings.json` and `REPORT.md` in the
output directory, after both validators pass — plus sections 3–5. Do not
copy the output directory into the repository; the report must stand on
its own.

Map the skill's severities onto this report's headings:

| heading | contents |
| --- | --- |
| 🔴 **CRITICAL** (immediate action) | skill `confirmed` records of overall severity `critical` or `high`; a reachable advisory with a known exploit |
| 🟠 **WARNING** (within a week) | `confirmed` records of severity `medium`; reachable advisories; CI or release findings that let an outsider influence a published artifact |
| 🟡 **INFO** | `confirmed` `low` / `informational`; checklist gaps with no reachable boundary violation |
| 🔍 **NEEDS VALIDATION** | the skill's `needs_validation` records, each with its blocker and validation plan, and no severity |
| 🟢 **CLEAN** | the areas reviewed and found sound |

Each confirmed finding keeps the skill's evidence: the source trace
(repository-relative `file:line`), the conditions, the affected principal
or resource, and the smallest effective fix.

End with **📊 Summary**: the date; the commit reviewed; the Rust versions
(`crates/secantusdb/Cargo.toml` and `crates/secantus-pg/Cargo.toml`); the
skill's profile, budget used and coverage statement (it is partial: say
what was `out_of_scope` or `deferred`); counts by heading; a comparison
with the previous report under `docs/security-reports/`; and the top three
priorities.

A clean run still writes a report, saying what was checked.

## 7. Land the report and leave nothing behind

The report is a docs-only change (`docs/security-reports/**` matches the
workflows' `paths-ignore`, so no required checks run). It must end up on
`main` with no leftover branch and no open PR.

1. Create branch `security-review/YYYY-MM-DD` and commit **only** the
   report file (never `.security-audit/`).
2. Open a PR titled `Security Review — YYYY-MM-DD`, prefixed
   `[security-critical]` if any CRITICAL, else `[security-warning]` if any
   WARNING, else `[security-clean]`. Body: the counts by heading.
3. Squash-merge it at once: `gh pr merge --squash --delete-branch` (add
   `--auto` only if branch protection blocks an immediate merge).
4. Verify: no open `security-review/*` PR and no `security-review/*` branch
   on the remote. Close and delete stale ones from earlier runs.

**Issues for actionable findings.** For each CRITICAL or WARNING finding,
open a GitHub issue labelled `security`, titled `[security] <one-line
summary>`, with the evidence and the fix. First make sure the label exists:
`gh label create security --color B60205 --description "Security finding from the nightly security review" 2>/dev/null || true`.
Before filing, search open `security` issues and comment on an existing one
instead of filing a duplicate.

**Do not fix anything in this run.** Review, report and file issues only.
