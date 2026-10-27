"""`name` values against PostgreSQL 15.19 (found while speeding up the
`information_schema` views, RP-131).

`nameconcatoid(name, oid)` is behind `information_schema`'s `specific_name`.
It was rewritten into `(name::text || '_' || oid::text)::name`, which kept a
long name whole and so overran `NAMEDATALEN`; PostgreSQL's `name.c` cuts the
NAME part (at a character boundary) so the oid suffix always fits in 63
bytes. A cast to `name` itself does not cut a value to 63 bytes either
(`namein` does, with `pg_mbcliplen`). Every literal was measured on
PostgreSQL 15.19 with read-only queries.
"""

from __future__ import annotations

from typing import Any

import pytest

from tests.backlog.conftest import pg_answer


@pytest.mark.backlog_fixed("RP-305")
def test_nameconcatoid_cuts_the_name_not_the_suffix(pg: Any) -> None:
    """A 60-byte name and the 6-byte `_12345` keep 57 bytes of the name; 40
    two-byte characters and `_4000000000` keep 26 characters, never half of
    one. The result is a `name`, NULL for a NULL argument, and an `int4` is
    an oid by its bits."""
    got = pg_answer(
        pg,
        "select right(nameconcatoid(repeat('a', 60)::name, 12345)::text, 8),"
        " octet_length(nameconcatoid(repeat('a', 60)::name, 12345)),"
        " nameconcatoid(repeat('é', 40)::name, 4000000000::oid)::text,"
        " octet_length(nameconcatoid(repeat('é', 40)::name, 4000000000::oid)),"
        " nameconcatoid('abc', -1)::text, nameconcatoid(null, 1),"
        " pg_typeof(nameconcatoid('a', 1))::text",
    )
    assert got == (
        "ok",
        [("aa_12345", 63, "é" * 26 + "_4000000000", 63, "abc_4294967295", None, "name")],
    )


@pytest.mark.backlog_fixed("RP-304")
def test_a_cast_to_name_keeps_namedatalen_bytes(pg: Any) -> None:
    """`namein` keeps at most 63 bytes, cut back to a character boundary:
    70 ASCII letters keep 63, 40 two-byte characters keep 31 (62 bytes).
    This server kept the whole text (70 and 80 bytes)."""
    got = pg_answer(
        pg,
        "select octet_length(repeat('a', 70)::name),"
        " octet_length(repeat('é', 40)::name),"
        " octet_length(cast(repeat('a', 70)::text as name))",
    )
    assert got == ("ok", [(63, 62, 63)])
