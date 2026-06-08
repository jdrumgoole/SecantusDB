# secantus-storage (the `_secantus_storage` extension)

PyO3 bindings exposing SecantusDB's **Rust storage layer** (`secantus-storage`,
Phase 4) to Python.

Documents cross the boundary as **BSON bytes** (matching `secantus.storage`'s
"documents are opaque BSON blobs" design); `_id` values are passed wrapped as
`bson.encode({"v": <id>})` (the same one-key envelope the sort-key seam uses).
The exposed `RustStorage` class covers the CRUD core: `insert_one`,
`find_by_id`, `scan_collection`, `replace_by_id`, `delete_by_id`,
`collection_exists`, `list_collections`.

```python
import bson, tempfile
import _secantus_storage as ss

st = ss.RustStorage(tempfile.mkdtemp())
st.insert_one("app", "users", bson.encode({"_id": 1, "name": "alice"}))
doc = st.find_by_id("app", "users", bson.encode({"v": 1}))
print(bson.decode(doc))            # {'_id': 1, 'name': 'alice'}
```

## The wheel-matrix gate

Unlike `secantus-core`, this extension **links the vendored WiredTiger C
library** (via `secantus-storage` → `secantus-wt`), so it does **not** build in
maturin's plain manylinux container. Shipping it across the wheel matrix
(cp310–313 × manylinux / musllinux / macOS-arm64 / Windows) is Phase 4's
go/no-go gate — likely solved by building through the same scikit-build CMake
path that already vendors + builds WiredTiger for the main `secantus` wheel,
rather than maturin. Until then it builds where WiredTiger is present.

## Build / smoke

`build.rs` (in `secantus-wt`) resolves WiredTiger via `SECANTUS_WT_INCLUDE` /
`SECANTUS_WT_LIB` or a probed `build/*/wt-build` / `/tmp/wt-build`; bindgen needs
libclang (`LIBCLANG_PATH`). Then:

```bash
invoke rust-storage-py      # build the wheel + run the Python smoke test
```

This is **not** yet wired into `secantus.engine`'s storage selection — that (and
running `test_crud.py` against the Rust storage under `SECANTUS_ENGINE=rust`)
needs the rest of the `Storage` surface (indexes, oplog) ported. See
`tasks/rust-rewrite-phase4-scoping.md`.
