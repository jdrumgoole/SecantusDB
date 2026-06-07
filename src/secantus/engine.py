"""Engine selection — SecantusDB keeps **both** implementations as first-class,
permanently-supported modes.

The pure-Python engines (the original implementation) are always present and
are the **default**. The Rust core (``_secantus_core``, an optional compiled
extension) is an accelerator that reproduces the pure-Python behaviour exactly —
pinned, operator by operator, by the ``tests/test_rust_*_parity.py`` suites.
Neither replaces the other: the Python version is not going away.

Selecting an engine (process-wide):

    SECANTUS_ENGINE=python   # default — original pure-Python engines
    SECANTUS_ENGINE=rust     # use the Rust core wherever a component is ported,
                             #   transparently falling back to Python where it
                             #   isn't (or when the extension isn't installed)
    SECANTUS_ENGINE=auto     # rust if the extension is importable, else python

Per-component overrides take precedence (for debugging / bisection):

    SECANTUS_RUST_QUERY=1    # force the Rust query matcher on
    SECANTUS_RUST_QUERY=0    # force it off

Programmatic control mirrors the env var and wins over it::

    import secantus.engine as engine
    engine.set_engine("rust")        # process-wide
    engine.available()               # is the Rust extension importable?
    engine.selected()                # 'python' | 'rust' | 'auto'

``SecantusDBServer(engine="rust")`` is the same as ``engine.set_engine("rust")``.

The selection is **process-wide**, not per-server: the Rust extension is a
single shared module and the pure engines read the same global. Running two
servers in one process with different engines isn't supported (the last
``set_engine`` / env value wins).
"""

from __future__ import annotations

import logging
import os
import threading

logger = logging.getLogger(__name__)

try:  # the Rust core is optional — a pure-Python install works without it
    import _secantus_core  # noqa: F401

    _AVAILABLE = True
except ImportError:
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
_warned_unavailable = False


def available() -> bool:
    """True if the Rust core extension (``_secantus_core``) is importable."""
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
    """Whether the Rust implementation should be used for ``component``.

    Resolution order: per-component override (``SECANTUS_RUST_<COMPONENT>``),
    then the global selection. Always ``False`` when the extension isn't
    importable — selecting ``rust`` without the extension transparently falls
    back to the pure-Python engines (with a one-time warning).
    """
    override = os.environ.get(f"SECANTUS_RUST_{component.upper()}")
    want = override == "1" if override is not None else selected() in ("rust", "auto")
    if want and not _AVAILABLE:
        _warn_unavailable_once()
        return False
    return want


def _warn_unavailable_once() -> None:
    global _warned_unavailable
    if not _warned_unavailable:
        _warned_unavailable = True
        logger.warning(
            "Rust engine requested but the _secantus_core extension is not "
            "installed; using the pure-Python engines instead."
        )
