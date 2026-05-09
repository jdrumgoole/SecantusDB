"""Sampler + hub: pure delta logic and a single-tick integration."""

from __future__ import annotations

import asyncio

import pytest

from secantus.admin.sampler import (
    Hub,
    Sampler,
    build_sample,
    compute_delta,
)

# ---- compute_delta ---------------------------------------------------------


def test_compute_delta_first_tick_returns_zeros() -> None:
    out = compute_delta(None, {"opcounters": {"insert": 5, "query": 10}})
    assert all(v == 0 for v in out.values())


def test_compute_delta_subtracts_per_bucket() -> None:
    prev = {
        "opcounters": {"insert": 5, "query": 10, "update": 2},
        "network": {"numRequests": 100},
    }
    curr = {
        "opcounters": {"insert": 8, "query": 14, "update": 2},
        "network": {"numRequests": 120},
    }
    out = compute_delta(prev, curr)
    assert out["insert"] == 3
    assert out["query"] == 4
    assert out["update"] == 0
    assert out["requests"] == 20


def test_compute_delta_floors_negative_at_zero() -> None:
    """A server restart resets counters; we floor at 0 instead of going negative."""
    prev = {"opcounters": {"insert": 1000}, "network": {"numRequests": 9999}}
    curr = {"opcounters": {"insert": 5}, "network": {"numRequests": 0}}
    out = compute_delta(prev, curr)
    assert out["insert"] == 0
    assert out["requests"] == 0


# ---- build_sample ----------------------------------------------------------


def test_build_sample_projects_wire_shape() -> None:
    snap = {
        "uptime": 42,
        "connections": {"current": 3, "totalCreated": 7},
        "opcounters": {
            "insert": 5,
            "query": 10,
            "update": 1,
            "delete": 0,
            "getmore": 2,
            "command": 8,
        },
        "network": {"numRequests": 50},
    }
    delta = {
        "insert": 1,
        "query": 2,
        "update": 0,
        "delete": 0,
        "getmore": 0,
        "command": 1,
        "requests": 4,
    }
    s = build_sample(123.45, snap, delta)
    assert s["ts"] == 123.45
    assert s["uptime"] == 42
    assert s["connections"] == {"current": 3, "totalCreated": 7}
    assert s["opcounters"]["insert"] == 5
    assert s["delta"]["insert"] == 1
    assert s["requests"] == 50


# ---- Hub -------------------------------------------------------------------


@pytest.fixture
def anyio_backend():
    return "asyncio"


pytestmark = pytest.mark.anyio


async def test_hub_broadcasts_to_subscribers() -> None:
    hub = Hub()
    q = hub.subscribe()
    hub.broadcast({"hello": 1})
    received = await asyncio.wait_for(q.get(), timeout=1.0)
    assert received == {"hello": 1}


async def test_hub_drops_oldest_for_slow_subscriber() -> None:
    hub = Hub(queue_size=2)
    q = hub.subscribe()
    for i in range(5):
        hub.broadcast({"i": i})
    # Queue holds the most-recent two; older were dropped.
    a = await q.get()
    b = await q.get()
    assert a["i"] == 3
    assert b["i"] == 4


async def test_hub_unsubscribe_stops_delivery() -> None:
    hub = Hub()
    q = hub.subscribe()
    hub.unsubscribe(q)
    hub.broadcast({"x": 1})
    # Nothing should arrive within a brief timeout.
    with pytest.raises(asyncio.TimeoutError):
        await asyncio.wait_for(q.get(), timeout=0.1)


# ---- Sampler integration ---------------------------------------------------


async def test_sampler_tick_once_pushes_to_hub() -> None:
    hub = Hub()
    q = hub.subscribe()
    snapshots = iter(
        [
            {
                "uptime": 1,
                "connections": {"current": 1, "totalCreated": 1},
                "opcounters": {
                    "insert": 0,
                    "query": 0,
                    "update": 0,
                    "delete": 0,
                    "getmore": 0,
                    "command": 0,
                },
                "network": {"numRequests": 0},
            },
            {
                "uptime": 2,
                "connections": {"current": 1, "totalCreated": 1},
                "opcounters": {
                    "insert": 3,
                    "query": 5,
                    "update": 0,
                    "delete": 0,
                    "getmore": 0,
                    "command": 1,
                },
                "network": {"numRequests": 9},
            },
        ]
    )

    def fake_snapshot() -> dict:
        return next(snapshots)

    loop = asyncio.get_running_loop()
    sampler = Sampler(fake_snapshot, hub=hub, loop=loop, interval_seconds=10.0)

    # First tick: zeros (no prior).
    sampler.tick_once()
    first = await asyncio.wait_for(q.get(), timeout=1.0)
    assert first["delta"]["insert"] == 0
    # Second tick: deltas reflect the difference.
    sampler.tick_once()
    second = await asyncio.wait_for(q.get(), timeout=1.0)
    assert second["delta"]["insert"] == 3
    assert second["delta"]["query"] == 5
    assert second["delta"]["requests"] == 9


async def test_sampler_history_capped() -> None:
    hub = Hub()
    snapshots = iter(
        [
            {
                "uptime": i,
                "connections": {"current": 0, "totalCreated": 0},
                "opcounters": {
                    "insert": i,
                    "query": 0,
                    "update": 0,
                    "delete": 0,
                    "getmore": 0,
                    "command": 0,
                },
                "network": {"numRequests": 0},
            }
            for i in range(10)
        ]
    )
    loop = asyncio.get_running_loop()
    sampler = Sampler(
        lambda: next(snapshots), hub=hub, loop=loop, interval_seconds=10.0, history_size=3
    )
    for _ in range(10):
        sampler.tick_once()
    history = sampler.history()
    assert len(history) == 3
    # The 3 newest samples (uptime 7, 8, 9) survive.
    assert [s["uptime"] for s in history] == [7, 8, 9]


async def test_sampler_tick_swallows_snapshot_errors() -> None:
    hub = Hub()
    q = hub.subscribe()

    def boom() -> dict:
        raise RuntimeError("server unreachable")

    loop = asyncio.get_running_loop()
    sampler = Sampler(boom, hub=hub, loop=loop, interval_seconds=10.0)
    out = sampler.tick_once()
    assert out is None
    # Hub got nothing.
    with pytest.raises(asyncio.TimeoutError):
        await asyncio.wait_for(q.get(), timeout=0.1)
