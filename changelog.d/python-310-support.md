### Python 3.10 actually works — and CI actually tests it

The CI test matrix's `python-version` never took effect: `uv sync` honours the
repo's `.python-version` pin (3.12), so every matrix cell — including the
scheduled 3.10–3.13 sweep — was silently testing 3.12. With the interpreter
genuinely pinned per cell (a job-level `UV_PYTHON`, which outranks the pin file
for every `uv` invocation in the job), the first real 3.10 run surfaced three
breakers that the gap had been hiding, all now fixed: the config loader's
module-level `tomllib` import (stdlib only from 3.11) crashed
`secantus.config` / the `secantusd-py` CLI on 3.10; `datetime.UTC` (a 3.11+
alias) in fifteen test call sites; and `datetime.fromisoformat` on 3.10
rejecting Postgres's short UTC offsets (`+00` / `+0000`), which PG text
rendering emits and timestamptz literals carry.

#### Fixed

- `config.py`: fall back to the API-identical `tomli` backport on Python 3.10
  (`tomli>=2.0; python_version < '3.11'` added to the core dependencies).
- `sql/datetimes.py`: new `parse_iso_datetime` — `fromisoformat` fast path
  (a no-op passthrough on 3.11+) that widens a trailing short UTC offset to
  `+HH:MM` only on failure; wired into `scalar._as_datetime`, `intervals`,
  and both `typemap.coerce` timestamp branches.
- `.github/workflows/test.yml`: the three matrix jobs set a job-level
  `UV_PYTHON: ${{ matrix.python-version }}` so `uv sync` and every `uv run`
  agree on the matrix interpreter (a sync-only `--python` flag is not enough —
  a later bare `uv run` re-resolves against `.python-version` and recreates
  the venv without the dev extras).
- Tests: `datetime.UTC` → `datetime.timezone.utc` in
  `test_indexes` / `test_expressions` / `test_crud`.
