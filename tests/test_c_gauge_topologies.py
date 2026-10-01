"""The C gauge's two-topology split stays coherent.

`c_validation` runs libmongoc's suite against a single-node replica set, then
re-runs the few tests that assert standalone semantics (`STANDALONE_ONLY`)
against a `--standalone` daemon and merges the two. Running only the
standalone pass cost 20 tests to self-skips -- most of `/change_stream` -- so
the list has to stay short, in scope, and out of the out-of-scope skip list.
"""

from __future__ import annotations

import fnmatch
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from c_validation.include_paths import INCLUDE, SKIP_TESTS, STANDALONE_ONLY  # noqa: E402


def test_standalone_tests_are_in_scope_and_not_skipped() -> None:
    for name in STANDALONE_ONLY:
        assert any(fnmatch.fnmatchcase(name, pat) for pat in INCLUDE), name
        assert name not in SKIP_TESTS, name


def test_the_standalone_list_is_exact_names() -> None:
    # Exact names only: a glob here would quietly move a whole suite off the
    # replica-set pass, which is the coverage loss the split exists to avoid.
    assert all("*" not in name for name in STANDALONE_ONLY)
    assert len(STANDALONE_ONLY) == len(set(STANDALONE_ONLY))
