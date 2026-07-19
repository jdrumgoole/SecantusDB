# WiredTiger build-cache plan

Goal: stop every CI job rebuilding vendored WiredTiger from source, worth
~100 s on each of 13 jobs. Written after the CI work that took the `Tests`
workflow from 18m00 to 7m49 (#486, #501, #503, #507); this is the largest
remaining lever and also the riskiest, which is why it is a plan rather than a
patch.

Numbers below were measured, not estimated, unless flagged `(est)`.

## Guardrail (non-negotiable)

**A stale WiredTiger is a data-corruption bug, not a slow-CI bug.** The patches
we apply to the vendored tree cover musl `off64_t`, strict-flag `-Werror`
suppression, and the Windows `.pyd` suffix. A cache that silently pairs
already-patched stamps with unpatched source produces a *building, passing,
subtly wrong* artifact. Every design below is judged first on whether it can
fail that way, and only then on speed. Prefer a cold rebuild over a clever hit.

## Where the time goes (measured)

Windows `storage-engine`, the workflow's pole before #503:

```
  +56s   wheel build starts
  +66s   cmake configure
 +166s   first cargo "Compiling"     -> C / WiredTiger build ~100s
 +611s   last cargo "Compiling"      -> cargo ~445s, 469 crates
 +616s   wheel done
```

Cargo was the larger half and is already cached (#503, `SECANTUS_CARGO_TARGET`),
taking that job 770 s cold -> 395 s warm. What remains is the ~100 s C build,
which also appears as the `Sync dependencies` step in every test lane:

| lane | test exec | `Sync dependencies` |
| --- | --- | --- |
| `test-windows` | 363 s | **102 s** |
| `test` (Linux) | 356 s | **96 s** |

12 test cells + `storage-engine` all pay it independently.

## Prerequisite (done — PR "cache the cargo target dir" follow-up)

ExternalProject re-ran the WT build on *every* build, even locally with a warm
build dir, despite `BUILD_ALWAYS OFF`. Two separate causes, both the same shape:

1. `PATCH_COMMAND` named `${Python3_EXECUTABLE}`, which under a PEP 517 build is
   the isolated build env's interpreter (`~/.cache/uv/builds-v0/.tmpXXXX/bin/
   python`) — a fresh temp path every build. ExternalProject records the command
   verbatim and re-runs the step when the text changes.
2. Fixing that alone was not enough: the `-patch` stamp then held, but
   `-configure` re-ran instead, because `CMAKE_ARGS` carried the same volatile
   path in `-DPython3_EXECUTABLE=`.

Fixed by passing the interpreter to the patch step via a file, and by resolving
`REALPATH` (uv's temp venv -> `~/.pyenv/.../python3.12`, stable) for the CMake
argument. A venv has no headers of its own and shares its base interpreter's
ABI, so this is equivalent, not a compromise.

Measured: local rebuild **37 s -> 1.3 s**, `libwiredtiger.a` untouched.

Without this, *any* cache is invalidated on contact — so it lands first,
independently, on its own merits (it fixes every developer's local rebuild).

Known gap: on Windows a venv python is a copy, not a symlink, so `REALPATH` is a
no-op and Windows rebuilds still churn. Needs a different stabilisation (e.g.
resolving via `sys._base_executable`) before a Windows cache can work.

## The central hazard: patches mutate the source tree

The patch step runs with `work_dir=vendor/wiredtiger` — it edits the **source**,
while the stamps live in the **build dir**. A CI job restores a build dir whose
stamps say "patched" onto a **freshly checked out, unpatched** submodule.

If nothing recompiles, the cached objects are fine (they were built from patched
source). But anything that triggers a recompile then compiles *unpatched*
source. That is the corruption path, and it fails quietly.

## Options

### 1. Patch into a build-dir copy, not in place *(recommended)*

Copy `vendor/wiredtiger` into the build dir, patch the copy, build there. Source
tree stays pristine; everything cacheable lives in one directory; the stamp/
source coupling disappears by construction.

- Cost: largest change. `SOURCE_DIR` moves; the copy adds a few seconds and disk.
- Benefit: the hazard cannot occur — there is no unpatched-source state to pair
  with a patched stamp.

### 2. Make patch scripts byte-idempotent and always re-run

Force the patch step on every restore and ensure each script rewrites nothing
(and touches no mtime) when the patch is already applied.

- Cost: cheap to implement; requires verifying all four scripts individually.
- Risk: correctness rests on all four staying mtime-stable forever. One future
  script that rewrites unconditionally silently reintroduces full rebuilds — or
  worse, half-patched state.

### 3. Cache the patched source alongside the build dir

Works, but caching a submodule working tree is easy to get subtly wrong and
interacts badly with `git submodule update`.

### 4. Do nothing

Honest baseline. ~100 s per job stays. Everything else in the workflow is
already balanced; this is the last structural cost.

## Cache key

Any input change must produce a cold rebuild:

- OS + arch
- wheel tag (python version) — the build dir is `build/{wheel_tag}`
- **WT submodule SHA** — bindgen output and the C source both derive from it
- hash of `CMakeLists.txt` + `cmake/*.py` (the patch scripts)

Restore-keys are acceptable for the *cargo* cache (partial hits still seed
dependency crates) but **not** here: a near-miss WT cache is exactly the stale
state the guardrail forbids. Exact key or cold build.

## Verification (required, not optional)

1. After a cache hit, assert the extension imports and reports the expected
   WiredTiger version (`WiredTiger 11.2.0` today).
2. Run a storage-focused subset on the cache-hit path specifically — not just
   the general suite, which may not recompile anything.
3. One-off: build cold and warm from the same SHA and confirm identical
   behaviour on the storage suite.
4. Land with the cache **write-only for one run** (populate, never read), then
   enable reads, so the first hit is observed deliberately rather than in
   traffic.

## Expected payoff

~100 s off 12 test cells and `storage-engine`. Pole 464 s -> ~350 s (est), so
roughly **7m49 -> ~6m** (est).

That is the smallest win of any CI change made so far and carries the highest
correctness risk. It is worth doing only with option 1 and the full verification
above; a quick `actions/cache` on `build/` would be a bad trade.

## Sequence

1. Land the `REALPATH` / patch-file fix (independent value: local rebuilds).
2. Fix the Windows interpreter-stabilisation gap.
3. Implement option 1 (patch into a build-dir copy) — no caching yet, prove the
   build still works everywhere.
4. Add the cache, write-only, one run.
5. Enable reads + the verification steps.
6. Measure; keep only if the warm number justifies the machinery.

Steps 1-3 are useful on their own merits even if 4-6 are never done.
