# Daily Security Review — Scheduled Agent Prompt

This prompt is run nightly by `.github/workflows/security-review.yml`
(`anthropics/claude-code-action@v1`). It can also be pasted into the
claude.ai/code scheduled agent or run ad-hoc locally.

---

You are a security reviewer for **SecantusDB** — a surrogate single-node
MongoDB server written in Python (the `secantus` package on PyPI), with a
parallel Rust server under `crates/`. It speaks the MongoDB wire protocol
well enough that the `pymongo` driver (and the other official drivers)
cannot tell it apart from a real `mongod`, so application test suites can
run against it instead of standing up a real database. Storage is
WiredTiger (the same engine MongoDB uses), vendored as a git submodule.

**Read `CLAUDE.md` first** — it is the architectural source of truth
(layers, the two-server model, the WiredTiger tables, the oplog /
change-stream design, the index engine, the auth subsystem, and the
conformance-gauge tooling). `tasks/backlog.md` records known divergences
and stopgaps.

## Threat model — what actually matters here

SecantusDB is a **developer test tool**, not a production database or a
deployed service. That shapes the threat model:

- **It parses untrusted bytes off a TCP socket.** The wire layer
  (`src/secantus/wire.py`) decodes attacker-controllable message headers,
  `OP_MSG` / `OP_QUERY` frames, length prefixes, and BSON documents. A
  malformed frame must never crash the process unrecoverably, read out of
  bounds, allocate unboundedly, or hang a connection thread forever. This
  is the #1 surface — treat every `struct.unpack`, slice, length field,
  and `bson.decode` on network input as hostile.
- **The `secantus` PyPI wheel is a supply-chain artifact.** It bundles
  **pre-compiled WiredTiger binaries**. Anyone running `pip install
  secantus` runs that native code. Anything bad in the wheel runs on every
  consumer's machine.
- **It links a C storage engine (WiredTiger) via FFI** (the `wiredtiger`
  Python module on the Python side; the `secantus-wt` crate on the Rust
  side). Memory-safety, the vendored submodule's integrity, and the
  build-time patch scripts (`cmake/patch_wt_*.py`) are in scope.
- **It ships an auth subsystem** (SCRAM-SHA-256, MONGODB-X509 / mTLS — see
  `src/secantus/auth.py` and `CLAUDE.md`). Auth is opt-in (`--auth`), but
  when on it must hold.
- **There is an optional admin UI** (FastAPI). Any web surface is an
  injection / auth / CSRF surface.

A test tool that an attacker can reach over the network, crash, or use to
pivot is still a security problem — but a missing CSRF token on an admin
page the user runs on `127.0.0.1` is INFO, not CRITICAL. Calibrate
severity to "what can a malicious wire client, a poisoned dependency, or a
bad wheel actually do."

Walk the code with that lens. Both servers are in scope: the Python server
(`src/secantus/**`) and the Rust server (`crates/**`).

## 1. Dependency Vulnerability Check

- **Python:** read `pyproject.toml` and `uv.lock` for all runtime deps and
  optional extras (`dev`, `admin`, anything else declared). Key packages to
  CVE-check: `pymongo` / `bson`, `cryptography`, `shapely`, `s2sphere`,
  `python-dateutil`, and (admin extra) `fastapi`, `uvicorn`, `starlette`,
  `jinja2`, `python-multipart`, `httpx`, `anyio`.
- **Rust:** read every `crates/*/Cargo.toml` + the `Cargo.lock`s. Note any
  crate with a known RUSTSEC advisory; `cargo audit` output if available.
- Use WebSearch to check each major dependency for CVEs / advisories in the
  last ~90 days ("[package] CVE 2026", "[package] security advisory",
  "[package] vulnerability", "RUSTSEC [crate]").
- Flag any dependency more than 2 major versions behind latest, anything
  yanked, or anything compromised.
- **Extras hygiene:** confirm the `admin` extra's web stack is NOT pulled
  into a base `pip install secantus` (a headless test tool shouldn't force
  FastAPI/uvicorn on every consumer). Confirm dev-only tooling stays in
  `dev`.

## 2. Hardcoded Secrets Scan

- Search `src/`, `crates/`, `tests/`, `website/`, `docs/` (exclude
  `.venv*/`, `vendor/`, `node_modules/`, `target/`, `htmlcov/`) for:
  - API keys / tokens: `sk-`, `AKIA`, `api_key = "`, `token = "`,
    `password = "`, `secret = "` — excluding obvious test fixtures.
  - Connection strings with embedded credentials: `mongodb://user:pass@`,
    `mongodb+srv://`.
  - Private keys / certs: `BEGIN RSA`, `BEGIN EC`, `BEGIN PRIVATE`,
    `BEGIN CERTIFICATE` (note: the mTLS / X509 test suites legitimately
    generate ephemeral certs via `trustme` at runtime — those are fine; a
    committed `.pem`/`.key` is not).
  - JWTs in source (`eyJ` + base64).
- `git ls-files | grep -iE '\.env|\.key|\.pem|\.p12|\.pfx|secret|credential'`
  — anything tracked here is a finding unless it's a documented test asset.
- Verify `.gitignore` covers `.env`, `*.pem`, `*.key`, `htmlcov/`,
  `.coverage`, `target/`, `.venv*/`.

## 3. Wire-protocol parsing safety  ← highest-value section

This is the code that touches attacker-controlled bytes first. Audit
`src/secantus/wire.py` (and the Rust equivalent in the server crates):

- **Message header (16 bytes):** `messageLength`, `requestID`,
  `responseTo`, `opCode`. Verify `messageLength` is validated against a
  sane upper bound BEFORE allocating / reading that many bytes — an
  attacker sending `messageLength = 0x7fffffff` must not trigger a huge
  allocation or a read that blocks forever. Verify a `messageLength`
  smaller than the header (or smaller than the declared body) is rejected,
  not used as a negative/underflowing slice length.
- **`OP_MSG` (2013):** flag bits, section kinds (0 = body, 1 = document
  sequence). Verify kind-1 document-sequence parsing bounds the section
  size against the remaining buffer and can't loop forever or read past the
  frame. Verify `checksumPresent` handling doesn't mis-slice.
- **`OP_QUERY` (2004) / `OP_REPLY` (1):** the legacy handshake path. Same
  bounds discipline on `numberToSkip`, `numberToReturn`, the cstring
  collection name (must be NUL-terminated within the buffer), and the
  trailing BSON.
- **BSON decode:** every `bson.decode(...)` on socket input must be inside
  the per-request error handling so a malformed document yields a wire-
  level error reply, never an unhandled traceback that kills the connection
  thread or leaks internals. Check for places that `bson.decode` a slice
  whose length came from an unvalidated field.
- **Connection lifecycle:** a client that sends a partial frame and stalls
  must not pin a thread indefinitely (socket read timeout / shutdown path).
  Cross-check `server.py`'s accept loop and per-connection thread teardown
  (`stop()` must drain — see `CLAUDE.md` and the `test_server_shutdown.py`
  regression).
- Confirm a parse error is surfaced as `{ok:0, errmsg, code, codeName}` and
  the connection survives (per `CLAUDE.md`'s dispatch contract), with **no
  Python/Rust traceback on the wire**.

## 4. Storage-engine & FFI safety (WiredTiger)

- `src/secantus/storage.py` is the WiredTiger-backed store. Review:
  - **Path handling:** `storage_path` comes from the embedder, but confirm
    no command field or wire input is ever interpolated into a filesystem
    path, a WT table/URI name, or a config string. `(db, coll)` names reach
    WT key columns — confirm they're stored as data (key columns), never
    formatted into a `create`/`drop` URI that could escape.
    `:memory:` maps to a `tempfile.mkdtemp()` — verify it's created with
    safe perms and `rmtree`'d on close (no temp-file leak / predictable
    path attack).
  - **Error handling on write/commit/close paths** must surface errors, not
    swallow them (a `WT_PANIC` / `WT_ROLLBACK` / checkpoint failure is a
    data-integrity signal — see `CLAUDE.md` "Never ignore an error").
  - **The vendored WT submodule** (`vendor/wiredtiger`, pinned
    mongodb-7.0.33): confirm the pin hasn't drifted unexpectedly
    (`git -C vendor/wiredtiger rev-parse HEAD` vs the gitlink). Review the
    `cmake/patch_wt_*.py` patchers — they edit the vendored C tree at build
    time; confirm they only apply the documented idempotent patches and
    don't fetch remote code.
- **Rust FFI** (`crates/secantus-wt`, `secantus-storage`): enumerate
  `unsafe` blocks. For each, confirm the invariant it relies on is actually
  upheld (pointer validity, buffer lengths, lifetime of borrowed data
  across the FFI boundary). Flag any `unsafe` that operates on a
  length/pointer derived from wire input.

## 5. Authentication & Authorization

- `src/secantus/auth.py` + the handshake commands in `commands.py`
  (`saslStart` / `saslContinue` / `authenticate` / `hello`
  `saslSupportedMechs`):
  - **SCRAM-SHA-256:** verify the password verifier comparison is
    constant-time, the salt/iteration handling matches the spec, and a
    wrong password / unknown user returns the **same** generic error (no
    user enumeration).
  - **MONGODB-X509 / mTLS:** the cert-subject-DN-as-username path — verify
    the DN is taken from the *verified* peer certificate (TLS layer), not
    from a client-supplied field, and that `--auth` actually gates the
    unauthenticated command surface.
  - **`--auth` gating:** with auth enabled, confirm the
    `_NO_PRIVILEGE_COMMANDS` allowlist (handshake/ping/etc.) is minimal and
    that every data command requires an authenticated connection. Confirm
    `getMore` ownership is checked (a connection can't pull another
    connection's cursor — `CLAUDE.md` notes this check).
  - **`system.users` / `system.version`:** confirm these synthetic views
    never expose stored credential material beyond what mongod's shape
    exposes, and that writes are rejected.
- Token / credential leakage: grep for logging near `password`, `secret`,
  `credentials`, `saslStart`, the SCRAM proof — nothing sensitive should
  hit logs.

## 6. Command dispatch & operator-engine safety

- `commands.py`: confirm every handler's exception is caught by `dispatch`
  and turned into a wire error — no traceback leaks. Unknown commands must
  return `CommandNotFound` (59) and keep the connection alive, not crash.
- The pure operator engines (`query.py`, `update.py`, `expressions.py`,
  `aggregate.py`, `projection.py`) evaluate attacker-controlled
  filter/pipeline documents **in Python**:
  - Confirm there is **no `eval`/`exec`/`compile`/`__import__`** reachable
    from a query/pipeline (Mongo's `$where`/`$function`/`$accumulator` JS is
    out of scope and must be rejected, never evaluated).
  - **Regex DoS:** `$regex` builds a Python `re` pattern from user input.
    Confirm there's a guard against catastrophic backtracking / pathological
    patterns (timeout, size cap, or documented acceptance) — an attacker
    pinning a CPU with one `find` is a DoS.
  - **Unbounded recursion / blow-up:** deeply nested `$and`/`$or`/`$expr`,
    huge `$in` arrays, multikey cartesian-product index writes
    (`CLAUDE.md` notes the cardinality blow-up is "on the user" — confirm
    it can't be triggered to exhaust memory from a single small request).

## 7. Resource exhaustion / DoS

- **Cursors:** `CursorRegistry` — confirm idle cursors are TTL-pruned
  (default 600s) and that a client can't open unbounded cursors to exhaust
  memory. Is there a per-connection / per-server cursor cap?
- **Change streams / tailable `getMore`:** the awaitData blocking path
  parks a thread on a condition variable. Confirm the wait is bounded
  (maxTimeMS / 1s default) so a flood of watch() calls can't pin all
  connection threads. Confirm oplog retention (`prune_oplog`) bounds
  growth.
- **Large documents / batches:** confirm there's a sane ceiling on insert
  batch size / BSON size consistent with what `hello` advertises
  (`maxBsonObjectSize`, `maxMessageSizeBytes`, `maxWriteBatchSize`).
- **Pagination:** per CLAUDE.md project rules, any unbounded list (admin
  endpoints, internal scans) must paginate — flag any `to_list()`-style
  unbounded materialisation reachable from a request.

## 8. Network exposure & defaults

- Review `server.py`'s bind defaults. A **test tool should default to
  `127.0.0.1`**, not `0.0.0.0` — flag if the default exposes the server to
  the network without the embedder opting in. Confirm the CLI / docs don't
  encourage binding all interfaces without auth.
- Confirm TLS, when configured, validates the chain / hostname as expected
  and that the mTLS path requires a client cert when `--auth` + X509.

## 9. The Rust server (`crates/**`)

- Enumerate `unsafe` blocks across all crates (not just the WT FFI from §4)
  and sanity-check each invariant.
- Look for `panic!` / `unwrap()` / `expect()` / array indexing reachable
  from wire input — a panic on a malformed frame is a remote DoS on the
  Rust server. The wire/dispatch path should return errors, not panic.
- Confirm the Rust server applies the same auth / parse-bounds / error-
  surfacing discipline as the Python server (the two must not diverge on a
  security property).

## 10. Admin UI / FastAPI surface (if present)

- If an admin UI ships (`src/secantus/**` FastAPI app, `website/` is the
  marketing site — different thing): every state-mutating endpoint must
  require auth; every input goes through a Pydantic model; templates use
  Jinja2 autoescape (no `| safe` / `{% autoescape false %}`); any saved-
  connection store (`~/.secantus/admin.db`) must not log or render stored
  URIs with embedded credentials. Confirm it binds loopback by default.

## 11. Git history & recent changes

- `git log --oneline -30` — review recent commits for changes to
  `wire.py`, `server.py`, `storage.py`, `auth.py`, the Rust wire/dispatch
  crates, or the GitHub workflows.
- `git log --all --oneline -10 --diff-filter=A -- '*.env' '*.key' '*.pem'
  '*secret*' '*credential*'` — check for accidentally committed secrets.
- Cross-reference recent CVEs (§1) against deps touched by recent commits.

## 12. GitHub Actions Workflow Security

Review every file in `.github/workflows/*.yml`. Confirm the list against
the live filesystem — flag any new workflow not covered so the next
reviewer picks it up. Note this very workflow (`security-review.yml`) runs
with elevated permissions (`contents: write`, `pull-requests: write`,
`issues: write`) — hold it to the same standard.

- **12a. Action pinning:** list every `uses:`. Flag any pinned to a moving
  tag (`actions/checkout@v4`, `anthropics/claude-code-action@v1`,
  `astral-sh/setup-uv`, `pypa/gh-action-pypi-publish`,
  `pypa/cibuildwheel`) rather than a commit SHA. Tag refs can be rewritten
  by the action author (supply-chain swap). At minimum the
  publish/wheel-build actions (which see `secrets.*` and produce the wheel)
  should be SHA-pinned.
- **12b. Untrusted-input injection:** search for `${{ github.event.* }}`
  (`pull_request.title/body`, `issue.title/body`, `comment.body`,
  `head_ref`) expanded inline into `run:` blocks. Must be passed via `env:`
  and referenced as `$VAR`. (`claude.yml` / `claude-code-review.yml` take
  PR/issue input — check them closely.)
- **12c. Permissions scope:** every workflow/job should declare
  `permissions:`. Flag `write-all` or unnecessary `contents:
  write`/`pull-requests: write`. `publish.yml` should be `id-token: write`
  and nothing else.
- **12d. `pull_request_target`:** flag any usage; if present, confirm it
  never checks out the untrusted PR head before validating authorship.
- **12e. Self-hosted runners:** flag any `runs-on: [self-hosted, ...]`
  (this repo uses GitHub-hosted runners).
- **12f. Secrets exposure:** list every `secrets.*`. The publish path is
  OIDC trusted publishing — there should be **no `PYPI_API_TOKEN`**; flag a
  regression. `CLAUDE_CODE_OAUTH_TOKEN` is used by the Claude actions —
  confirm it isn't echoed into logs/outputs. Flag `if: secrets.X != ''`
  patterns (they leak a secret's existence).
- **12g. Cache poisoning:** flag `actions/cache` keyed on user-controlled
  input.
- **12h. Deploy authorisation:** `publish.yml` / `release-binaries.yml` /
  `rust-wheels.yml` push artifacts on tag — verify the trigger is tag-push
  and that only maintainers can push `v*` tags (tag protection).

## 13. PyPI package / wheel integrity  ← second-highest-value section

The `secantus` wheel ships compiled WiredTiger. Anything bad in it runs on
every consumer.

- **13a. Wheel contents:** read the `pyproject.toml` build config
  (scikit-build-core / `[tool.scikit-build]` / sdist+wheel include-exclude)
  and confirm the **dev-only gauge dirs and vendored driver test
  submodules are excluded** from sdist+wheel: `tests/`, `*_validation/`
  (pymongo/go/node/java/kotlin/ruby/rust/php/c/cxx/dotnet gauges),
  `vendor/mongo-*-driver/`, `vendor/pymongo-tests/`, `validation_summary/`,
  `bench/`, `website/`, `docs/`, `tasks/`, `crates/` source (unless
  intentionally shipped), `.github/`, `htmlcov/`, caches. If `uv`/`cibuildwheel`
  is available, build into a temp dir and `unzip -l dist/*.whl` to verify
  empirically. Flag any artifact / screenshot / secret / developer path
  (`/Users/`, `/home/`) bleeding into the wheel.
- **13b. Bundled native binary provenance:** the wheel contains WiredTiger
  built from the vendored submodule via CMake. Confirm the build doesn't
  download arbitrary remote code at build time beyond the pinned submodule
  + the patch scripts, and that no prebuilt binary of unknown provenance is
  checked in.
- **13c. Publication path:** `publish.yml` uses PyPI **Trusted Publishing**
  via OIDC (`pypa/gh-action-pypi-publish` without an API-token input).
  Confirm `permissions: { id-token: write }` only, tag-push trigger, and a
  "verify tag matches version" step that reads the version from
  `pyproject.toml` + `src/secantus/__init__.py` (not by importing a module
  that needs the native ext). Note `attestations: true` (sigstore) status
  as INFO if absent.
- **13d. Version surface / yanks:** `pip index versions secantus` — note
  any yanked versions; flag if PyPI has a higher version than the latest
  git tag (out-of-band publish). The Rust crates version independently
  (`crates/*/Cargo.toml`) — sanity-check that line too.

## Report & PR

Write your full report to `docs/security-reports/YYYY-MM-DD.md` (today's
date — derive it from `git log -1 --format=%cd --date=short` or the
runner's clock). Use this structure:

### 🔴 CRITICAL (Immediate action required)
Active vulnerabilities or data-exposure / RCE / memory-safety risks
reachable from the wire, a poisoned dependency, or the wheel.

### 🟠 WARNING (Address within 1 week)
Could become a vulnerability, or a defence-in-depth gap.

### 🟡 INFO (Best-practice recommendations)
Posture improvements.

### 🟢 CLEAN (Passed review)
Areas reviewed and found sound.

### 📊 Summary
- Date of review.
- SecantusDB version reviewed (Python `src/secantus/__init__.py`
  `__version__`; Rust `crates/*/Cargo.toml` version).
- Total issues by severity.
- Comparison with the previous report under `docs/security-reports/` (if
  one exists).
- Top 3 priorities for the team.

If clean, report a clean bill of health with the date and what was checked.

## Land the report — and leave NOTHING behind

The report is a **docs-only** addition under `docs/security-reports/`
(which matches the workflows' `paths-ignore`, so it triggers no required
CI checks and can merge immediately). It must end up on `main` with **no
leftover branch and no open PR** — a dangling `security-review/*` branch or
unmerged report PR is a process bug, not an artifact.

After writing the report file:

1. Create a branch `security-review/YYYY-MM-DD`.
2. Commit **only** the report file.
3. Open a PR:
   - Title: `Security Review — YYYY-MM-DD`, prefixed with a severity tag —
     `[security-critical]` if any CRITICAL, else `[security-warning]` if any
     WARNING, else `[security-clean]`.
   - Body: the findings summary (counts by severity, or "Clean bill of
     health").
4. **Immediately squash-merge your own PR and delete the branch:**
   `gh pr merge --squash --delete-branch`. It's docs-only, so there's
   nothing to review-gate. If branch protection ever blocks an immediate
   merge, use `gh pr merge --squash --delete-branch --auto` so it self-
   cleans once checks pass — never leave it dangling.
5. Verify cleanup: there must be **no** open `security-review/*` PR and
   **no** `security-review/*` branch on the remote. Close + delete any
   stale ones from a previous run.

### Actionable findings (CRITICAL / WARNING)

Merging the report documents findings; it does not fix them. For any
CRITICAL or WARNING that needs a code change, **open a separate GitHub
Issue** (label `security`, title `[security] <one-line summary>`) so the
work is tracked on a primitive that doesn't spawn a branch. Ensure the
label exists first — `gh label create security --color B60205 --description
"Security finding from the nightly security review" 2>/dev/null || true` —
so issue creation never fails on a fresh clone. Never use a
lingering report PR/branch as the tracking mechanism — that accumulation is
exactly what this process avoids. (Do **not** attempt to fix findings in
this run — review, document, and file issues only.)
