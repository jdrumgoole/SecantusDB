"""Guard against double ``pytestmark`` assignment in test modules.

``pytestmark`` is a plain module attribute — a second bare assignment silently
OVERWRITES the first (last write wins). ``test_rust_binary_pitr.py`` lost its
``timeout(1200, method="signal")`` mark exactly this way, putting its
disk-bound tests back under the global 600s thread-method timeout whose expiry
``os._exit``s the whole xdist worker ("Not properly terminated", no signal
trace, no faulthandler dump) — the worker-death cluster's root cause.
"""

from __future__ import annotations

import ast
import pathlib

TESTS_DIR = pathlib.Path(__file__).parent


def test_no_module_assigns_pytestmark_twice() -> None:
    offenders: list[str] = []
    for path in sorted(TESTS_DIR.glob("*.py")):
        tree = ast.parse(path.read_text(encoding="utf-8"), filename=str(path))
        count = sum(
            1
            for node in tree.body
            if isinstance(node, ast.Assign)
            and any(isinstance(t, ast.Name) and t.id == "pytestmark" for t in node.targets)
        )
        if count > 1:
            offenders.append(f"{path.name} ({count} assignments)")
    assert not offenders, (
        "modules assigning pytestmark more than once (the later assignment"
        f" silently discards the earlier marks): {offenders}"
    )
