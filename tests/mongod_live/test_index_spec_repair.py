"""A `--repair` that salvages index files writes each index's spec again in
creation order (RM-789's neighbour: `reIndex`), as mongod 8.2.11 does."""

from __future__ import annotations

from pathlib import Path

from tests.backlog.test_rm_index_spec_fidelity import _mongod_after, _ours_after


def test_repair_writes_a_collmodded_spec_in_creation_order(tmp_path: Path) -> None:
    """`--repair` over index files it has to salvage builds every index
    again from its spec: an option `collMod` added after `name` is written
    before `key`, as at creation (mongod 8.2.11, 2026-11-24)."""
    theirs = _mongod_after(tmp_path, "repair")
    assert [f for f, _ in theirs["a_1"][0][1]] == ["v", "hidden", "key", "name"]
    ours = _ours_after(tmp_path, "repair")
    assert list(ours) == list(theirs)
    assert ours == theirs
