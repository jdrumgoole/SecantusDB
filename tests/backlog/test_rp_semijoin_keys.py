"""RP-306: `element_types`' row IN, written as the join to a DISTINCT
subquery that PostgreSQL's `convert_ANY_sublink_to_join` makes of it.

The statements are `tests/backlog/rp306_element_types_semijoin.sql`. The
subquery's columns are `information_schema.sql_identifier` /
`character_data` domains, and the comma join's keys took only columns of
a plain integer or string type, so the subquery joined the other six
relations as a cross product: about 5 GB of rows, and the server was
killed after 55 s. A domain now hashes as its base type does.

278 is PostgreSQL 15.19's count in a fresh `initdb -U postgres` cluster's
`postgres` database (2026-11-04), for both forms. The statement timeout
bounds a regression to seconds instead of the runner's memory.
"""

from __future__ import annotations

from pathlib import Path
from typing import Any

import psycopg
import pytest

from tests.backlog.test_rp_batch26 import _dsn

SQL = Path(__file__).with_name("rp306_element_types_semijoin.sql")


def _statements() -> list[str]:
    return [s for s in SQL.read_text().split(";;") if s.strip().lower().startswith("select")]


@pytest.mark.backlog_fixed("RP-306")
@pytest.mark.parametrize("form", ["row IN", "join to the DISTINCT subquery"])
def test_element_types_semijoin_answers(form: str, rust_pg: Any) -> None:
    sql = _statements()[0 if form == "row IN" else 1]
    with psycopg.connect(_dsn(rust_pg), autocommit=True) as c:
        c.execute("set statement_timeout = '60s'")
        assert c.execute(sql).fetchall() == [(278,)]
