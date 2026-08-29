"""Deselects for the SQLAlchemy compliance gauge.

Same model as the other gauges: the suite runs unmodified; divergence lives
here, one nodeid per line with a reason. Prefer closing a capability in
``requirements.py`` (which skips a whole feature family honestly) over
deselecting individual tests — deselects are for tests that hang or take the
whole run down, not for ordinary failures (those are the signal).
"""

#: nodeids (relative to the suite dir) excluded from the run entirely.
DESELECT_TESTS: list[str] = []
