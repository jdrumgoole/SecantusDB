"""The servers a differential probe compares, in one place.

Every probe here answers the same question -- does SecantusDB match mongod? --
and SecantusDB is now TWO servers. Five probes only ever asked the Python one,
which is how the Rust server came to be 219 divergent shapes on the aggregation
stage corpus with nobody the wiser; adding a single column to that probe is what
surfaced it, three of them silent wrong answers.

So the Rust column is not optional decoration. A probe that omits it proves half
of what it claims.

Usage::

    from _servers import probe_targets

    with probe_targets() as (mongod, targets):
        for label, client in targets:
            ...

`targets` is `[("python", client)]` plus `("rust", client)` when the Rust server
can be started -- the embedded `_secantus_server` extension, or a URI in
`PROBE_SERVER`. It is skipped with a loud note rather than an error when the
extension is not built, so the probe still runs in a checkout that has not built
it; the note is there so a clean run is never mistaken for a compared one.
"""

from __future__ import annotations

import contextlib
import os
import sys
import tempfile
from collections.abc import Iterator

import pymongo

DEFAULT_MONGOD = "mongodb://127.0.0.1:27041"


@contextlib.contextmanager
def probe_targets(
    *, mongod_uri: str | None = None, replica_set: str | None = None
) -> Iterator[tuple[pymongo.MongoClient, list[tuple[str, pymongo.MongoClient]]]]:
    """`(mongod_client, [(label, client), ...])`, cleaned up on exit."""
    from secantus import SecantusDBServer

    mongod = pymongo.MongoClient(
        mongod_uri or os.environ.get("PROBE_MONGOD", DEFAULT_MONGOD),
        directConnection=True,
        serverSelectionTimeoutMS=8000,
    )
    python_server = SecantusDBServer(
        port=0, storage_path=tempfile.mkdtemp(), replica_set_name=replica_set
    )
    python_server.start()
    host, port = python_server.address
    targets: list[tuple[str, pymongo.MongoClient]] = [
        ("python", pymongo.MongoClient(host, port, directConnection=True))
    ]

    rust_server = None
    if os.environ.get("PROBE_SERVER"):
        targets.append(("rust", pymongo.MongoClient(os.environ["PROBE_SERVER"])))
    else:
        try:
            import _secantus_server
        except ImportError:
            print(
                "  NOTE: the Rust server is NOT being compared -- build it with\n"
                "        uv pip install --no-build-isolation-package secantus-server-py \\\n"
                "            ./crates/secantus-server-py",
                file=sys.stderr,
            )
        else:
            rust_server = _secantus_server.RustServer(tempfile.mkdtemp(), 0)
            rhost, rport = rust_server.address
            targets.append(("rust", pymongo.MongoClient(rhost, rport, directConnection=True)))

    try:
        yield mongod, targets
    finally:
        for _, client in targets:
            client.close()
        mongod.close()
        python_server.stop()
        if rust_server is not None:
            rust_server.stop()


def report(name: str, total: int, divergent: dict[str, int]) -> int:
    """One headline line per probe, and the exit code to use."""
    summary = ", ".join(f"{label} {n}" for label, n in divergent.items())
    print(f"\n=== {name}: {total} shapes -- {summary} divergent ===")
    return 1 if any(divergent.values()) else 0
