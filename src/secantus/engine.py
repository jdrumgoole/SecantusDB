"""Engine selection — historical shim, now pure-Python only.

The Python server (the ``secantus`` package) is **entirely pure Python and
depends on no Rust components** — it never imports ``_secantus_core``. The
original in-process engine-swap (``SECANTUS_ENGINE=rust``, where each operator
module delegated to the Rust core) has been **retired** in favour of the
two-separate-servers model (see CLAUDE.md "Engines"): the Rust engines now live
only in the standalone Rust server and in the ``tests/test_rust_*_parity.py``
oracle, which imports ``_secantus_core`` directly rather than through this
package.

This module is kept as an inert compatibility stub so existing call sites keep
working:

    import secantus.engine as engine
    engine.available()        # always False — the package never loads Rust
    engine.enabled("query")   # always False — no component delegates to Rust
    engine.selected()         # echoes SECANTUS_ENGINE / set_engine(), but inert
    engine.set_engine(...)    # accepted and recorded, but has no effect

``SecantusDBServer(engine=...)`` still accepts the argument for backwards
compatibility; it no longer changes behaviour.
"""

from __future__ import annotations

import os
import threading

# The Python server is pure Python and depends on **no** Rust components: it
# never imports ``_secantus_core``. The in-process engine-swap (the original
# ``SECANTUS_ENGINE=rust`` accelerator) has been retired from the Python server
# per the two-separate-servers direction (see CLAUDE.md "Engines"). The Rust
# engines now live only in the standalone Rust server and the
# ``tests/test_rust_*_parity.py`` oracle, which import ``_secantus_core``
# directly — not through this package. ``available()`` / ``enabled()`` therefore
# always report Python; ``selected()`` / ``set_engine()`` remain inert API stubs
# so callers that still pass ``engine=`` keep working.
_AVAILABLE = False

# The components the Rust core can accelerate (each pure-Python module has a
# shim that consults ``enabled(<component>)``).
COMPONENTS = (
    "sortkey",
    "query",
    "update",
    "expressions",
    "projection",
    "diff",
    "aggregate",
)

_VALID = ("python", "rust", "auto")

_lock = threading.Lock()
_override: str | None = None  # set by set_engine(); None => read the env var


def available() -> bool:
    """Always ``False``: the Python server is pure Python and never loads the
    Rust core. (The Rust engines live in the standalone Rust server.)"""
    return _AVAILABLE


def set_engine(name: str | None) -> None:
    """Set the process-wide engine selection.

    ``name`` is ``"python"`` / ``"rust"`` / ``"auto"``, or ``None`` to revert
    to the ``SECANTUS_ENGINE`` environment variable (default ``"python"``).
    """
    global _override
    if name is not None:
        name = name.lower()
        if name not in _VALID:
            raise ValueError(f"engine must be one of {_VALID} or None, got {name!r}")
    with _lock:
        _override = name


def selected() -> str:
    """The current global engine selection (``'python'`` / ``'rust'`` / ``'auto'``)."""
    if _override is not None:
        return _override
    return os.environ.get("SECANTUS_ENGINE", "python").lower()


def enabled(component: str) -> bool:
    """Always ``False`` — the Python server is pure Python and never delegates
    to a Rust component. Retained so the operator modules' (now historical)
    shims and any external callers keep importing cleanly. ``component`` is
    accepted and ignored."""
    return False
