"""`$toDate` on a string it cannot parse, on the RUST server.

The failure path used to `Conv::Failed`, which on this server surfaces as
`2 BadValue: aggregation pipeline uses a stage or operator not supported by the
Rust server` -- **false**, because `$toDate` is supported and the STRING was at
fault, and a different code from mongod's `241 ConversionFailure`.

It now carries 241 always, and mongod's exact message for the two shapes whose
text is reproducible:

* an EMPTY string names a literal NUL;
* everything else that reaches here gets the incomplete-string text.

WHITESPACE-ONLY is not empty for mongod (`''` is "Empty string" but `'  '` is
the incomplete message), which the Python server had wrong -- it tested the
STRIPPED text. Fixed on both servers here.

**Still not reproduced, on either server:** mongod's per-position timelib
diagnostic (`'abc'` names the offending character and where its scanner stopped).
That needs timelib's own lexer, its timezone abbreviation tables and its
per-position error accumulation; inventing a position would look authoritative
and be wrong. Both servers give the same message, so they agree with each other
while the shared gap stays documented in `tasks/backlog.md`.

Measured against mongod 8.2.11 on 2026-09-08: 9 exact of 25 strings, 16
message-only, and **0 with the wrong code** (it was 25).

Gated on the `_secantus_server` extension, like `test_rust_server_smoke.py`.
"""

from __future__ import annotations

import pytest

_server = pytest.importorskip("_secantus_server")
pymongo = pytest.importorskip("pymongo")


@pytest.fixture(scope="module")
def rs(tmp_path_factory):
    srv = _server.RustServer(str(tmp_path_factory.mktemp("rs_todate") / "wt"), 0)
    try:
        yield srv
    finally:
        srv.stop()


@pytest.fixture
def db(rs):
    host, port = rs.address
    cli = pymongo.MongoClient(host, port, directConnection=True, serverSelectionTimeoutMS=5000)
    d = cli["todate"]
    d.c.drop()
    d.c.insert_one({"_id": 1})
    try:
        yield d
    finally:
        cli.close()


def _to_date(db, value):
    return list(db.c.aggregate([{"$addFields": {"x": {"$toDate": {"$literal": value}}}}]))


# Strings whose message mongod and both servers agree on exactly.
EXACT = [
    ("", "Error parsing date string ''; 0: Empty string '\x00'"),
    ("  ", 'an incomplete date/time string has been found, with elements missing: "  "'),
    ("a", 'an incomplete date/time string has been found, with elements missing: "a"'),
    ("z", 'an incomplete date/time string has been found, with elements missing: "z"'),
    ("Z", 'an incomplete date/time string has been found, with elements missing: "Z"'),
    ("T", 'an incomplete date/time string has been found, with elements missing: "T"'),
    ("GMT", 'an incomplete date/time string has been found, with elements missing: "GMT"'),
]


@pytest.mark.parametrize("text,message", EXACT, ids=[repr(t) for t, _ in EXACT])
def test_message_matches_mongod_exactly(db, text, message):
    with pytest.raises(pymongo.errors.OperationFailure) as e:
        _to_date(db, text)
    assert e.value.code == 241
    assert message in str(e.value)


def test_whitespace_only_is_not_an_empty_string(db):
    """`''` and `'  '` take DIFFERENT branches on mongod. Testing the stripped
    text conflated them and gave `'  '` the empty-string message."""
    with pytest.raises(pymongo.errors.OperationFailure) as empty:
        _to_date(db, "")
    with pytest.raises(pymongo.errors.OperationFailure) as blank:
        _to_date(db, "  ")
    assert "Empty string" in str(empty.value)
    assert "Empty string" not in str(blank.value)
    assert "incomplete date/time string" in str(blank.value)


# Strings where mongod says more than we can reproduce. The CODE must still be
# mongod's, and the reply must never claim the operator is unsupported.
UNREPRODUCED = [
    "abc",
    "xyz",
    "$s",
    "12-",
    "1",
    "12",
    "123",
    "ab",
    "-",
    "a1",
    "1a",
    "!",
    "junk here",
]


@pytest.mark.parametrize("text", UNREPRODUCED)
def test_code_is_mongods_even_where_the_text_is_not(db, text):
    with pytest.raises(pymongo.errors.OperationFailure) as e:
        _to_date(db, text)
    assert e.value.code == 241, "mongod's ConversionFailure, not the generic BadValue"
    assert "not supported by the Rust server" not in str(e.value), (
        "$toDate IS supported; the string is what failed"
    )


@pytest.mark.parametrize("text", ["2020-01-01", "2020-01-01T00:00:00Z", "2020-01-01T00:00:00.123Z"])
def test_the_success_path_is_untouched(db, text):
    assert _to_date(db, text)[0]["x"] is not None


def test_on_error_still_covers_a_parse_failure(db):
    """A named failure must remain catchable by `$convert`'s `onError`."""
    got = list(
        db.c.aggregate(
            [{"$addFields": {"x": {"$convert": {"input": "abc", "to": "date", "onError": "bad"}}}}]
        )
    )
    assert got[0]["x"] == "bad"
