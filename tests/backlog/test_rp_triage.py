"""Gap tests for `tasks/backlog-rust-postgres.md`, "Open from the 2026-08-26 triage"."""

from __future__ import annotations

import threading
import time
from typing import Any

import pytest

from tests.backlog.conftest import pg_answer


@pytest.mark.backlog_fixed("RP-001")
def test_set_transaction_outside_a_block_warns(pg: Any) -> None:
    """PostgreSQL 15.19: `WARNING: SET TRANSACTION can only be used in
    transaction blocks` (sent as a notice, the statement succeeds)."""
    notices: list[str] = []
    pg.add_notice_handler(lambda d: notices.append(f"{d.severity}: {d.message_primary}"))
    pg.execute("SET TRANSACTION ISOLATION LEVEL SERIALIZABLE")
    assert notices == ["WARNING: SET TRANSACTION can only be used in transaction blocks"]


@pytest.mark.backlog_fixed("RP-002")
def test_cancel_during_parse_is_honoured_at_once(pg: Any) -> None:
    """PostgreSQL 15.19: a cancel sent 0.5 s into this 1.2 MB statement
    answers 57014 at 0.515 s, during the parse (it takes ~2 s to parse
    here in a debug build, and the cancel waits for it)."""
    sql = "select pg_sleep(3), 1 in (" + ",".join(str(i) for i in range(200_000)) + ")"
    delay = 0.5
    timer = threading.Timer(delay, pg.cancel)
    t0 = time.monotonic()
    timer.start()
    answer = pg_answer(pg, sql)
    elapsed = time.monotonic() - t0
    timer.join()
    assert answer[:2] == ("err", "57014")
    assert elapsed - delay < 0.5, f"cancel honoured {elapsed - delay:.2f} s after it was sent"


@pytest.mark.backlog_fixed("RP-003")
def test_national_literal_under_nonconforming_strings(pg: Any) -> None:
    """PostgreSQL 15.19: with `standard_conforming_strings = off`, `N'a\\\\b'`
    reads its backslash as an escape, like a plain literal: `a\\b`."""
    pg.execute("set standard_conforming_strings = off")
    assert pg_answer(pg, r"select N'a\\b'") == ("ok", [("a\\b",)])


@pytest.mark.backlog_fixed("RP-003")
def test_backslash_quote_off_refuses_backslash_quote(pg: Any) -> None:
    """PostgreSQL 15.19: with `standard_conforming_strings = off` and
    `backslash_quote = off`, `'a\\'b'` is 22P06 `unsafe use of \\' in a
    string literal`."""
    pg.execute("set standard_conforming_strings = off")
    pg.execute("set backslash_quote = off")
    assert pg_answer(pg, r"select 'a\'b'") == (
        "err",
        "22P06",
        "unsafe use of \\' in a string literal",
    )


@pytest.fixture(scope="module")
def _revoked_abs(rust_pg: Any) -> str:
    """A role with no EXECUTE on `abs(int)`, and a view, a DEFAULT and a
    rule that call it. Returns the role's name."""
    with rust_pg.connect() as conn:
        for sql in (
            "create role rp004_r",
            "revoke execute on function abs(int) from public",
            "create table rp004_t (a int default abs(-1), b int)",
            "create view rp004_v as select abs(-1) as x",
            "create table rp004_log (a int)",
            "create table rp004_ruled (a int)",
            "create rule rp004_rule as on insert to rp004_ruled"
            " do also insert into rp004_log values (abs(-1))",
            "grant select, insert on rp004_t to rp004_r",
            "grant select on rp004_v to rp004_r",
            "grant insert on rp004_ruled, rp004_log to rp004_r",
        ):
            conn.execute(sql)
    return "rp004_r"


@pytest.mark.backlog_fixed("RP-004")
@pytest.mark.parametrize(
    "sql",
    [
        "select x from rp004_v",
        "insert into rp004_t (b) values (1)",
        "insert into rp004_t (b) values (default)",
        "insert into rp004_t default values",
        "insert into rp004_ruled values (1)",
        "select abs(length('ab'::text))",
    ],
    ids=["view", "column-default", "values-default", "default-values", "rule", "nested-call"],
)
def test_revoked_builtin_refused_through_view_and_default(
    pg: Any, _revoked_abs: str, sql: str
) -> None:
    """PostgreSQL 15.19 (measured in a rolled-back transaction): each is
    42501 `permission denied for function abs` for the restricted role."""
    pg.execute(f"set role {_revoked_abs}")
    assert pg_answer(pg, sql) == ("err", "42501", "permission denied for function abs")


@pytest.mark.backlog_fixed("RP-004")
@pytest.mark.parametrize(
    "sql",
    [
        "select abs(x) from (values (-1)) v(x)",
        "with w(x) as (select -1) select abs(x) from w",
        "select abs(length('ab'))",
    ],
    ids=["values-column", "cte-column", "untyped-literal-argument"],
)
def test_revoked_builtin_refused_where_the_overload_is_unanalysed(
    pg: Any, _revoked_abs: str, sql: str
) -> None:
    """PostgreSQL 15.19 (measured in a rolled-back transaction): each calls
    `abs(int4)` and is 42501 for the restricted role: the overload is
    chosen from a VALUES column, a CTE column and an untyped literal."""
    pg.execute(f"set role {_revoked_abs}")
    assert pg_answer(pg, sql) == ("err", "42501", "permission denied for function abs")


@pytest.mark.backlog_fixed("RP-005")
def test_pg_collation_lists_locale_icu_collations(pg: Any) -> None:
    """PostgreSQL 15.19 (907 ICU collations here) lists `de-x-icu`,
    `fr-FR-x-icu` and `sv-x-icu` beside `en-x-icu` and `und-x-icu`."""
    sql = (
        "select collname::text from pg_collation where collprovider = 'i' and collname in "
        "('de-x-icu', 'en-x-icu', 'fr-FR-x-icu', 'sv-x-icu', 'und-x-icu') order by 1"
    )
    assert pg_answer(pg, sql) == (
        "ok",
        [("de-x-icu",), ("en-x-icu",), ("fr-FR-x-icu",), ("sv-x-icu",), ("und-x-icu",)],
    )


@pytest.mark.backlog_fixed("RP-005")
def test_tailored_icu_collation_orders_as_postgres(pg: Any) -> None:
    """PostgreSQL 15.19: `haw-x-icu` (Hawaiian) sorts the vowels before the
    consonants, `a,e,o,b`. ICU4X's compiled data has no Hawaiian tailoring;
    it comes from the generated data (`secantus-pgplan/src/icu_data.rs`)."""
    sql = (
        "select string_agg(w, ',' order by w collate \"haw-x-icu\") "
        "from (values ('a'), ('e'), ('b'), ('o')) v(w)"
    )
    assert pg_answer(pg, sql) == ("ok", [("a,e,o,b",)])


@pytest.mark.backlog_fixed("RP-005")
def test_script_form_icu_collation_orders_as_its_language(pg: Any) -> None:
    """PostgreSQL 15.19: `az-Cyrl-x-icu` orders as `az` (`x` after `h`,
    dotless `ı` before `i`), and `uz-Arab-AF-x-icu` as `uz` (`ch` last).
    ICU4C drops the script and the region; ICU4X fell back to the root."""
    words = "('i'), ('ı'), ('h'), ('x'), ('q'), ('k'), ('ch'), ('c'), ('ç'), ('d')"
    answers = []
    for name in ("az-Cyrl-x-icu", "uz-Arab-AF-x-icu"):
        sql = f"select string_agg(w, ',' order by w collate \"{name}\") from (values {words}) v(w)"
        answers.append(pg_answer(pg, sql))
    assert answers == [
        ("ok", [("c,ch,ç,d,h,x,ı,i,k,q",)]),
        ("ok", [("c,ç,d,h,i,ı,k,q,x,ch",)]),
    ]


@pytest.mark.backlog("RP-005")
def test_marathi_icu_collation_orders_as_postgres(pg: Any) -> None:
    """PostgreSQL 15.19: `mr-x-icu` (Marathi) sorts the conjunct `क्ष` after
    `ह`. Here the collation is refused (42704): ICU4X's compiled data has no
    such rule, so it ordered `क्ष` beside `क` and is not listed."""
    sql = (
        "select string_agg(w, ',' order by w collate \"mr-x-icu\") "
        "from (values ('क'), ('क्ष'), ('ख'), ('ह')) v(w)"
    )
    assert pg_answer(pg, sql) == ("ok", [("क,ख,ह,क्ष",)])
