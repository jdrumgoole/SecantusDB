"""One pass-rate formatter, so a failing run cannot print a perfect score.

Every gauge report formatted its rate with ``f"{...:.1f}%"``, which ROUNDS.
`3089 / 3090` is 99.9676%, and that rounds **up** to ``100.0%`` — which is what
`docs/validation-report-php-lib.md` published: 3089 passed, **1 failed**, scored
100.0%. The website's driver panels are generated from those reports, so the
claim was live. Any run at ≥ 99.95% has the same problem, and that is exactly
where the healthy gauges now sit — the better the server gets, the more likely
its report is to lie in the flattering direction.

`pass_rate` floors to the displayed precision instead, so only a genuinely clean run
can print ``100.0%``. Raising the precision instead was considered and rejected:
``99.98%`` still reads as "perfect" to someone skimming, whereas ``99.9%`` next
to a visible failure count does not.

Originally written by the session fixing the psycopg gauge; kept here so the
nineteen report generators share one definition rather than nineteen copies
that can drift.
"""

from __future__ import annotations

import math


def pass_rate(passed: int, run: int) -> str:
    """``passed`` of ``run`` as a percentage string, floored, or ``—``.

    ``run`` is the number of tests that actually RAN (passed + failed), not the
    collected total — a skipped test is not a failure and must not be counted
    against the rate.
    """
    if not run:
        return "—"
    if passed == run:
        return "100.0%"
    return f"{math.floor(passed / run * 1000) / 10:.1f}%"
