### The test suite is warning-free again

starlette's `TestClient` prefers `httpx2` and warns on every construction when
only `httpx` is importable. Six of those warnings came from the admin websocket
tests, and they were the last ones left in the default suite.

Adding `httpx2` to the dev extra silences them. It is test-only on purpose:
nothing under `src/secantus` imports `httpx` at runtime.

#### Changed

- `httpx2>=2.12` added to the `dev` extra. Refreshing the lock also pulled in its
  dependencies (`httpcore2`, `httpx2-jsfetch`, `truststore`) and moved `idna`
  3.13 → 3.19.
