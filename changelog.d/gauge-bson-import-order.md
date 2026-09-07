### The pymongo gauge measured the two servers against different `bson` versions

The gauge is the project's headline MongoDB-compatibility number and it is
routinely used to compare the two servers. It was not comparing like with like:

```
python mode   pymongo -> vendor/pymongo-tests/pymongo   (4.17.0)
              bson    -> site-packages/bson             (4.18.0)   <- MIXED
rust mode     pymongo -> vendor/pymongo-tests/pymongo
              bson    -> vendor/pymongo-tests/bson
```

The split is import **order**, not configuration. `_start_server("python")` does
`from secantus import SecantusDBServer`, and `secantus` imports `bson` — at a
point before pytest has inserted the vendored tree into `sys.path`, so the name
binds to site-packages and stays bound for the rest of the run. Rust mode never
imports Python `bson` at that moment (`_secantus_server` is a compiled
extension), so `bson` resolves later, to the vendored copy.

That produced a **phantom server difference**: `test_default_exports::test_bson`
failed on the Python server and passed on the Rust one, because vendored `bson`
takes `Generator` from `typing` (which the test skips) and site-packages `bson`
takes it from `collections.abc` (which it does not). Nothing to do with either
server.

The plugin now puts `vendor/pymongo-tests` on `sys.path` in its
`pytest_load_initial_conftests` hook, before the server import. Both modes load
the same vendored `bson` / `pymongo` pair, and the two servers' numbers became
identical:

| server | before | after |
| --- | --- | --- |
| rust | 1277 passed / 5 failed | 1277 / 5 (unchanged — it already used the vendored pair) |
| python | 1276 passed / **6** failed | 1277 / **5** |

Both failure lists are now byte-identical and are exactly the five
known-standing ones (`test_index_hashed`, `test_index_text`, `test_where`, all
out of scope; `test_maxtime_ms_message` / `test_to_list_csot_applied`, pymongo's
client-side CSOT formatting). The Python server's headline figure moves 99.53%
to 99.61% by removing a harness artifact, not by any change in behaviour.

#### Fixed

- `pymongo_validation/plugin.py`: the vendored test tree goes on `sys.path`
  before the embedded server is imported, so `bson` cannot bind to
  site-packages first.
