"""Profile a single-thread insert workload to find the real bottleneck.

Phase-2 spike step 1 (see ``tasks/wt-concurrency-plan.md``). The
``bench/concurrency.py`` benchmark shows write throughput stops scaling
past one writer; we suspected the pure-Python encoding (``_pack_entry``,
``_index_key_variants``, ``encode_value_directed``) was the GIL-bound
bottleneck. This profile makes that quantitative: it runs the same
``Storage.insert`` workload the benchmark drives, in-process, under
``cProfile``, and prints the top cumulative-time entries.

If pure-Python encoding dominates → C-rewriting it could lift the GIL
ceiling (proceed to step 2).
If BSON encode/decode (already C) dominates → already gets GIL release;
no point porting more to C.
If WT cursor calls dominate → bottleneck is below Python; storage-side
concurrency unlock is impossible without changing the WT integration.
"""

from __future__ import annotations

import cProfile
import io
import pstats
import sys
import tempfile
from pathlib import Path

import bson

from secantus.storage import Storage


_PAYLOAD = b"x" * 8192


def _make_doc(n: int) -> dict:
    return {"_id": n, "n": n, "payload": _PAYLOAD}


def main() -> int:
    storage_path = Path(tempfile.mkdtemp(prefix="profile-insert-"))
    n_docs = 30_000
    batch_size = 100

    storage = Storage(str(storage_path))
    try:
        # Pre-create the collection so the first insert doesn't pay
        # one-time setup costs we don't want in the profile.
        storage.insert("profile", "docs", [{"_id": -1, "warmup": True}])

        profiler = cProfile.Profile()
        profiler.enable()

        for batch_start in range(0, n_docs, batch_size):
            docs = [_make_doc(batch_start + i) for i in range(batch_size)]
            storage.insert("profile", "docs", docs)

        profiler.disable()

        # Top entries by cumulative time, then again by tottime.
        stream = io.StringIO()
        stats = pstats.Stats(profiler, stream=stream)
        stats.sort_stats("cumulative")
        stats.print_stats(40)
        sys.stdout.write("\n=== TOP 40 BY CUMULATIVE TIME ===\n")
        sys.stdout.write(stream.getvalue())

        stream2 = io.StringIO()
        stats2 = pstats.Stats(profiler, stream=stream2)
        stats2.sort_stats("tottime")
        stats2.print_stats(40)
        sys.stdout.write("\n=== TOP 40 BY TOTAL TIME (excl. callees) ===\n")
        sys.stdout.write(stream2.getvalue())
    finally:
        storage.close()
        import shutil
        shutil.rmtree(storage_path, ignore_errors=True)

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
