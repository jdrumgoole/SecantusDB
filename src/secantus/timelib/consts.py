"""timelib's constants, shared by the scanner, the format parser and
``update_ts`` (MIT; see ``__init__.py``)."""

from __future__ import annotations

#: timelib's "not set" marker.
UNSET = -9_999_999

ZONETYPE_OFFSET = 1
ZONETYPE_ABBR = 2

SPECIAL_WEEKDAY = 0x01
SPECIAL_DAY_OF_WEEK_IN_MONTH = 0x02
SPECIAL_LAST_DAY_OF_WEEK_IN_MONTH = 0x03
SPECIAL_FIRST_DAY_OF_MONTH = 0x01
SPECIAL_LAST_DAY_OF_MONTH = 0x02

#: The error code mongod rewrites the message for.
ERR_TZID_NOT_FOUND = 0x202

I64_MIN = -(2**63)
I64_MAX = 2**63 - 1


def wrap_i64(v: int) -> int:
    """Two's-complement wrap to an int64, as Rust's ``wrapping_*`` does."""
    return ((v + 2**63) % 2**64) - 2**63
