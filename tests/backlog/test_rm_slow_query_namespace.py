"""The namespace a `Slow query` line names, against mongod 8.2's (found
while working RM-361, 2026-11-07, and RM-378, 2026-11-09)."""

from __future__ import annotations

import contextlib
import json
from collections.abc import Callable
from typing import Any

import pytest
from pymongo.errors import OperationFailure

from tests.backlog.opdebug import fresh


def _lines(
    client: Any,
    is_mongod: bool,
    run: Callable[[Any, Any], None],
    profiled: tuple[str, ...] = (),
) -> tuple[list[tuple[str, str | None]], dict[str, list[str]]]:
    """Each `Slow query` line `run(client, db)` wrote, as `(ns, kind)` with
    the database's name as `<db>`, and the profile entries of `<db>` and the
    `profiled` databases, as their `ns`. `run` tags its commands with
    `comment: "probe"`."""
    db = client[fresh("linens")]
    db.c.insert_one({"_id": 1})
    watched = [db, *(client[name] for name in profiled)]
    last = client.admin.command("getLog", "global")["log"][-1]
    for d in watched:
        d.command("profile", 2, slowms=-1 if is_mongod else 0)
    try:
        run(client, db)
    finally:
        for d in watched:
            d.command("profile", 0, slowms=100)
    log = client.admin.command("getLog", "global")["log"]
    found = []
    for raw in log[log.index(last) + 1 :]:
        entry = json.loads(raw)
        command = entry.get("attr", {}).get("command", {})
        if (
            entry.get("msg") == "Slow query"
            and isinstance(command, dict)
            and command.get("comment") == "probe"
        ):
            attr = entry["attr"]
            found.append((attr.get("ns", "").replace(db.name, "<db>"), attr.get("collectionType")))
    entries = {
        ("<db>" if d.name == db.name else d.name): [
            e.get("ns", "").replace(db.name, "<db>")
            for e in d.system.profile.find({"command.comment": "probe"})
        ]
        for d in watched
    }
    client.drop_database(db.name)
    for d in watched[1:]:
        d.system.profile.drop()
    return found, entries


def _both(run: Callable[[Any, Any], None], rust: Any, mongod: Any, **kw: Any) -> dict[str, Any]:
    return {"rust": _lines(rust, False, run, **kw), "mongod": _lines(mongod, True, run, **kw)}


def _command(client: Any, db: Any, on: str, command: dict) -> None:
    """Run `command`, tagged; a refusal is part of what is measured."""
    with contextlib.suppress(OperationFailure):
        client[on if on != "<db>" else db.name].command({**command, "comment": "probe"})


def _rename(client: Any, db: Any) -> None:
    _command(client, db, "admin", {"renameCollection": f"{db.name}.c", "to": f"{db.name}.c2"})


@pytest.mark.backlog_fixed("RM-378")
def test_a_rename_line_names_the_collection_it_renames(rust_mongo: Any, mongod_rs: Any) -> None:
    """mongod 8.2.1's line for a `renameCollection`, which runs against
    `admin`, names the collection it renames (`<db>.c`); this server's
    named `admin.<db>.c`, the command's database joined to its argument."""
    answers = _both(_rename, rust_mongo, mongod_rs)
    assert answers["mongod"][0] == [("<db>.c", "normal")]
    assert answers["rust"][0] == answers["mongod"][0]


def _failed_renames(client: Any, db: Any) -> None:
    db.d.insert_one({"_id": 1})
    for source, target in (("nope", "c3"), ("c", "d"), ("c", "c"), ("c", "$x")):
        _command(
            client,
            db,
            "admin",
            {"renameCollection": f"{db.name}.{source}", "to": f"{db.name}.{target}"},
        )


@pytest.mark.backlog_fixed("RM-378")
def test_a_failed_rename_line_names_its_own_namespace(rust_mongo: Any, mongod_rs: Any) -> None:
    """mongod 8.2.1's line for a `renameCollection` that fails -- a source
    that does not exist, a target that does, a rename to itself, a bad
    target name -- names the command's own `admin.$cmd`; this server's named
    `admin.<db>.<source>`."""
    answers = _both(_failed_renames, rust_mongo, mongod_rs)
    assert [ns for ns, _ in answers["mongod"][0]] == ["admin.$cmd"] * 4
    assert [ns for ns, _ in answers["rust"][0]] == [ns for ns, _ in answers["mongod"][0]]


@pytest.mark.backlog("RM-478")
def test_a_line_on_an_internal_database_names_its_kind(rust_mongo: Any, mongod_rs: Any) -> None:
    """mongod 8.2.1's line for an operation on `admin`, `local` or `config`
    -- a `ping`, a `find` -- says `collectionType: "admin"` (`"local"`,
    `"config"`); this server's says `"normal"`."""

    def run(client: Any, db: Any) -> None:
        for on in ("admin", "local", "config"):
            _command(client, db, on, {"ping": 1})
            _command(client, db, on, {"find": "c"})

    answers = _both(run, rust_mongo, mongod_rs)
    assert [kind for _, kind in answers["mongod"][0]] == [
        "admin",
        "admin",
        "local",
        "local",
        "config",
        "config",
    ]
    assert answers["rust"][0] == answers["mongod"][0]


@pytest.mark.backlog("RM-479")
def test_a_failed_rename_is_not_profiled(rust_mongo: Any, mongod_rs: Any) -> None:
    """mongod 8.2.1 profiles a `renameCollection` that renamed in its
    source's database, and one that failed nowhere (its namespace is
    `admin.$cmd`, and `admin` profiles no command); this server profiles a
    failed one in the source's database too."""
    answers = _both(_failed_renames, rust_mongo, mongod_rs, profiled=("admin",))
    assert answers["mongod"][1] == {"<db>": [], "admin": []}
    assert answers["rust"][1] == answers["mongod"][1]


@pytest.mark.backlog("RM-480")
def test_a_data_size_line_names_its_own_namespace(rust_mongo: Any, mongod_rs: Any) -> None:
    """mongod 8.2.1's line for a `dataSize`, whose argument is a full
    namespace, names `<db>.$cmd`; this server's names `<db>.<db>.c`, the
    command's database joined to its argument."""

    def run(client: Any, db: Any) -> None:
        _command(client, db, "<db>", {"dataSize": f"{db.name}.c"})

    answers = _both(run, rust_mongo, mongod_rs)
    assert [ns for ns, _ in answers["mongod"][0]] == ["<db>.$cmd"]
    assert [ns for ns, _ in answers["rust"][0]] == [ns for ns, _ in answers["mongod"][0]]


@pytest.mark.backlog("RM-481")
def test_a_command_that_fails_to_parse_writes_no_line(rust_mongo: Any, mongod_rs: Any) -> None:
    """mongod 8.2.1 writes no `Slow query` line for a command refused as it
    parses -- a mistyped or missing field (14, 40414), an unknown one
    (40415), a collection name that is not a string (73), an unknown
    pipeline stage (40324, measured 8.2.11 on a collection and on a view,
    2026-11-10) -- since the operation never ran; this server writes one for
    each."""

    def run(client: Any, db: Any) -> None:
        _command(client, db, "admin", {"renameCollection": 5, "to": f"{db.name}.c9"})
        _command(client, db, "admin", {"renameCollection": f"{db.name}.c"})
        _command(client, db, "<db>", {"find": "c", "bogus": 1})
        _command(client, db, "<db>", {"find": 5})
        _command(client, db, "<db>", {"count": 5})
        _command(
            client, db, "<db>", {"aggregate": "c", "pipeline": [{"$bogusStage": {}}], "cursor": {}}
        )

    answers = _both(run, rust_mongo, mongod_rs)
    assert answers["mongod"][0] == []
    assert answers["rust"][0] == answers["mongod"][0]
