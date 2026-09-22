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

import atexit
import contextlib
import os
import shutil
import sys
import tempfile
from collections.abc import Iterator

import pymongo

DEFAULT_MONGOD = "mongodb://127.0.0.1:27041"

#: Prefix for a probe's throwaway WiredTiger home.
#:
#: Probe stores used to be bare ``tempfile.mkdtemp()`` dirs that nothing ever
#: deleted -- the helper below stopped the servers and walked away from their
#: data. Each run leaked ~260 MB (two servers, a ~130 MB WT home each), and
#: since a probe is the thing you run in a loop while chasing a divergence, one
#: session left 385 of them: ~50 GiB, invisible to pytest's numbered-dir
#: janitor, which only knows about ``pytest-of-<user>/``.
#:
#: They are cleaned up on exit now. The PID in the name is for the run that is
#: NOT clean -- a probe killed mid-flight, which is routine -- so
#: ``_sweep_stale_probe_tmp`` can tell an abandoned store from a live one
#: instead of guessing from mtime.
PROBE_TMP_PREFIX = "secantus-probe-"


def probe_store() -> str:
    """A fresh WiredTiger home for a probe server, deleted when this exits.

    The delete is an ``atexit`` hook rather than a teardown line in each probe
    on purpose: there are ~17 probes here and they all create their store
    inline, most at module scope, with no common shutdown path to hang a
    ``finally`` on. Registering the cleanup with the store itself makes the
    substitution a one-liner per probe and cannot be forgotten by the next one
    written.

    ``ignore_errors`` because this is housekeeping running at interpreter
    shutdown: a probe that left a WiredTiger connection open still holds its
    files (on Windows an open file cannot be deleted at all), and failing to
    reclaim disk must never turn a clean probe run into a non-zero exit.
    """
    path = tempfile.mkdtemp(prefix=f"{PROBE_TMP_PREFIX}{os.getpid()}-")
    atexit.register(shutil.rmtree, path, ignore_errors=True)
    return path


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
    stores: list[str] = []
    python_store = probe_store()
    stores.append(python_store)
    python_server = SecantusDBServer(
        port=0, storage_path=python_store, replica_set_name=replica_set
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
            rust_store = probe_store()
            stores.append(rust_store)
            rust_server = _secantus_server.RustServer(rust_store, 0)
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
        # The servers are stopped, so nothing holds these open any more and
        # the delete cannot race WiredTiger's background threads the way a
        # mid-session tmp_path delete does (see tests/conftest.py).
        for store in stores:
            shutil.rmtree(store, ignore_errors=True)


def report(name: str, total: int, divergent: dict[str, int]) -> int:
    """One headline line per probe, and the exit code to use."""
    summary = ", ".join(f"{label} {n}" for label, n in divergent.items())
    print(f"\n=== {name}: {total} shapes -- {summary} divergent ===")
    return 1 if any(divergent.values()) else 0
