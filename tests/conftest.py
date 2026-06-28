"""Shared pytest setup.

When the WiredTiger extension isn't built — e.g. a network-restricted dev box
where the ``vendor/wiredtiger`` submodule can't be fetched — stand in a stub
``wiredtiger`` module so the **pure-Python** parts of ``secantus`` (the operator
engines, the SQL layer) still import. ``secantus``'s package ``__init__`` pulls
in the WiredTiger-backed server, so without this any ``import secantus.*`` fails
at collection time.

This is strictly a no-op when a real WiredTiger is present (the normal case, and
always the case in CI): the stub is installed only when the module genuinely
can't be found, so a real build is never shadowed. Tests that exercise the real
``Storage`` require the real extension and are unaffected.
"""

from __future__ import annotations

import importlib.machinery
import importlib.util
import sys
import types

if "wiredtiger" not in sys.modules and importlib.util.find_spec("wiredtiger") is None:
    _stub = types.ModuleType("wiredtiger")
    # A real ModuleSpec (rather than None) so later importlib.find_spec calls
    # against the now-present module don't raise "__spec__ is None".
    _stub.__spec__ = importlib.machinery.ModuleSpec("wiredtiger", loader=None)
    sys.modules["wiredtiger"] = _stub
