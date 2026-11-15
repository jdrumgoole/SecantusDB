"""Gap test for what was found on 2026-11-15 while working RM-456: an
insert into a time-series bucket the TTL monitor is removing fails.

The reference is mongod 8.2.11 (`mongod`); the Rust server is
`secantusd-rs` (`rust_mongo`).
"""

from __future__ import annotations

import datetime as dt
import threading
import time
import uuid
from typing import Any

import pytest


def _insert_into_expiring_buckets(client: Any, seconds: float) -> list:
    """Insert, from four threads for `seconds` or until one fails with the TTL monitor
    running every second, measurements into buckets that have already
    expired; the errors."""
    client.admin.command("setParameter", 1, ttlMonitorSleepSecs=1)
    errors: list = []
    try:
        db = client["rm_found_rm456"]
        name = "ts" + uuid.uuid4().hex[:8]
        db.create_collection(
            name, timeseries={"timeField": "t", "metaField": "m"}, expireAfterSeconds=60
        )
        old = dt.datetime.now(dt.timezone.utc).replace(tzinfo=None) - dt.timedelta(hours=3)
        deadline = time.monotonic() + seconds

        def insert(meta: int) -> None:
            i = 0
            while time.monotonic() < deadline and not errors:
                try:
                    db[name].insert_one({"t": old + dt.timedelta(milliseconds=i), "m": meta})
                except Exception as e:  # noqa: BLE001 -- collected and asserted on
                    errors.append(str(e)[:120])
                i += 1

        threads = [threading.Thread(target=insert, args=(m,)) for m in range(4)]
        for t in threads:
            t.start()
        for t in threads:
            t.join()
    finally:
        client.admin.command("setParameter", 1, ttlMonitorSleepSecs=60)
    return errors


@pytest.mark.backlog("RM-702")
def test_an_insert_into_a_bucket_the_ttl_monitor_removes_succeeds(
    rust_mongo: Any, mongod: Any
) -> None:
    """mongod 8.2.11 retries an insert whose bucket the TTL monitor removed
    under it, into a new bucket: no insert fails. This server returns the
    write error 112, "the time-series bucket was removed while it was
    written"."""
    assert _insert_into_expiring_buckets(mongod, 10) == []
    # The race is a window in each TTL pass: up to 90 passes to find it.
    assert _insert_into_expiring_buckets(rust_mongo, 90) == []
