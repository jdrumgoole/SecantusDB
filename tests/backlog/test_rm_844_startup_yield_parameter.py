"""RM-844: `--setParameter internalQueryExecYieldPeriodMS=...` at startup.

mongod 8.2.11 (measured 2026-11-28) takes the parameter on its command line
(the `mongod_rs_no_yield` fixture starts one that way) and `getParameter`
reads it back as the number given. This server (0.5.3-beta.199) exits at
startup: "secantusd-mdb does not take --setParameter
'internalQueryExecYieldPeriodMS' at startup", though `setParameter` takes it
at run time.
"""

from __future__ import annotations

from typing import Any

import pytest

from tests.backlog.conftest import rust_mongo_server

_PARAM = "internalQueryExecYieldPeriodMS"


@pytest.mark.backlog("RM-844")
def test_the_yield_period_is_a_startup_parameter(
    mongod_rs_no_yield: Any,
    tmp_path: Any,
) -> None:
    expected = mongod_rs_no_yield.admin.command({"getParameter": 1, _PARAM: 1})[_PARAM]
    assert expected == 86400000, expected
    with rust_mongo_server(tmp_path, "--setParameter", f"{_PARAM}=86400000") as client:
        assert client.admin.command({"getParameter": 1, _PARAM: 1})[_PARAM] == expected
