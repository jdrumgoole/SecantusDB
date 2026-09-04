"""The comparison every `test_rust_*_parity.py` suite uses.

A parity suite is only as sharp as its comparator, and a bare ``==`` is blunt in
two ways that matter for BSON:

* ``nan != nan``, so two engines that both correctly answer NaN look like a
  divergence;
* ``-0.0 == 0.0``, so two engines that answer DIFFERENT values look identical.

The second is not hypothetical. mongod keeps IEEE's signed zero -- ``$ceil(-0.5)``
is ``-0.0`` -- and the Rust engine kept it while the pure engine dropped it. The
expressions suite had ``-0.0`` in its fuzz pool and exercised ``$ceil`` 5,000
times a run, and passed every time, because ``==`` could not see the difference.
Its corpus was never the gap; the comparator was (fixed 2026-09-03, measured
against mongod 8.2.11).

This module exists so that fix is not one file's private property. It is a
plain helper, not a test module -- the name deliberately lacks the ``test_``
prefix so pytest does not collect it.
"""

from __future__ import annotations

import math
from typing import Any


def same(a: Any, b: Any) -> bool:
    """Parity equality: NaN equals NaN, signed zeros are DISTINCT, and both
    rules apply inside nested arrays and documents.

    Recursion matters as much as the scalar rule: a signed zero nested in an
    array (``$map``, ``$range``, ``$slice``, a ``$push``ed element) is just as
    wrong as a bare one, and ``==`` on the container hides it.
    """
    if isinstance(a, float) and isinstance(b, float):
        if a != a and b != b:
            return True
        if a == 0.0 and b == 0.0:
            return math.copysign(1.0, a) == math.copysign(1.0, b)
    if isinstance(a, list) and isinstance(b, list):
        return len(a) == len(b) and all(same(x, y) for x, y in zip(a, b, strict=True))
    if isinstance(a, dict) and isinstance(b, dict):
        # Key ORDER is behaviour in BSON -- a driver renders it -- so it is
        # compared, not just the key set.
        return list(a) == list(b) and all(same(a[k], b[k]) for k in a)
    return a == b
