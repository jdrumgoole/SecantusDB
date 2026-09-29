"""Unit tests for ``secantus.serverparams`` — the ``setParameter`` rules.

Every expectation here was measured against mongod 8.2.11 on 2026-09-29 and is
pinned against a live server by ``tests/test_mongod_differential.py``. These
unit tests exist so the rules are also checked on a box with no mongod, where
that gate skips.
"""

from __future__ import annotations

import pytest

from secantus.serverparams import ServerParams, SetParameterError, is_generic_arg

DEFAULTS = {"logLevel": 0, "quiet": False, "enableTestCommands": False}


def _apply(command: dict, store: ServerParams | None = None) -> dict:
    return (store or ServerParams()).apply(command, DEFAULTS)


def test_reports_the_previous_value_and_sticks() -> None:
    store = ServerParams()
    assert _apply({"setParameter": 1, "logLevel": 3}, store)["was"] == 0
    assert _apply({"setParameter": 1, "logLevel": 4}, store)["was"] == 3
    assert store.get("logLevel") == 4


@pytest.mark.parametrize(
    ("value", "stored"),
    [
        (3, 3),
        (1.9, 1),  # truncates toward zero
        (-0.5, 0),  # ... so a small negative lands on zero
        (True, 1),
        (False, 0),
        (6, 5),  # clamped, not refused
        (99, 5),
    ],
)
def test_log_level_coerces_and_clamps(value: object, stored: int) -> None:
    """An "int" parameter is far more permissive than the name suggests."""
    store = ServerParams()
    _apply({"setParameter": 1, "logLevel": value}, store)
    assert store.get("logLevel") == stored


@pytest.mark.parametrize(
    ("value", "stored"),
    [
        (True, True),
        (False, False),
        (1, True),
        (0, False),
        (0.0, False),
        ("yes", True),
        ("", True),  # an EMPTY STRING is true, unlike Python's own truthiness
        (None, False),
        ([1, 2], True),
        ({"a": 1}, True),
    ],
)
def test_bool_parameter_accepts_everything(value: object, stored: bool) -> None:
    store = ServerParams()
    _apply({"setParameter": 1, "quiet": value}, store)
    assert store.get("quiet") is stored


@pytest.mark.parametrize(
    ("command", "code", "message"),
    [
        (
            {"setParameter": 1, "notARealParameter": 1},
            72,
            "attempted to set unrecognized parameter [notARealParameter], "
            "use help:true to see options ",
        ),
        ({"setParameter": 1}, 72, "no option found to set, use help:true to see options "),
        (
            {"setParameter": 1, "enableTestCommands": True},
            20,
            "not allowed to change [enableTestCommands] at runtime",
        ),
        (
            {"setParameter": 1, "logLevel": "nope"},
            2,
            'Invalid value for logLevel: logLevel: "nope"',
        ),
        (
            {"setParameter": 1, "logLevel": [1, 2]},
            2,
            "Invalid value for logLevel: logLevel: [ 1, 2 ]",
        ),
        (
            {"setParameter": 1, "logLevel": {"a": 1}},
            2,
            "Invalid value for logLevel: logLevel: { a: 1 }",
        ),
        ({"setParameter": 1, "logLevel": None}, 2, "Invalid value for logLevel: logLevel: null"),
        ({"setParameter": 1, "logLevel": -1}, 2, "Invalid value for logLevel: logLevel: -1"),
    ],
)
def test_refusals_carry_mongods_code_and_message(command: dict, code: int, message: str) -> None:
    """A startup-only parameter is 20, not the 72 an unknown NAME gets.

    72 would claim the parameter does not exist, which ``getParameter``
    reporting it immediately contradicts.
    """
    with pytest.raises(SetParameterError) as caught:
        _apply(command)
    assert caught.value.code == code
    assert str(caught.value) == message


def test_a_bad_name_in_a_batch_changes_nothing() -> None:
    """mongod validates the whole batch first, so a partial apply cannot happen."""
    store = ServerParams()
    with pytest.raises(SetParameterError):
        _apply({"setParameter": 1, "logLevel": 4, "notARealParameter": 1}, store)
    assert store.get("logLevel") is None


def test_multiple_parameters_report_the_first_ones_previous_value() -> None:
    store = ServerParams()
    reply = _apply({"setParameter": 1, "logLevel": 3, "quiet": True}, store)
    assert reply["was"] == 0  # logLevel's, not quiet's
    assert store.get("logLevel") == 3
    assert store.get("quiet") is True


@pytest.mark.parametrize(
    "key",
    ["lsid", "$db", "$clusterTime", "txnNumber", "writeConcern", "readConcern", "maxTimeMS"],
)
def test_the_command_envelope_is_not_a_parameter(key: str) -> None:
    """pymongo attaches ``lsid`` to every command it sends.

    A handler that filters only ``$``-prefixed keys rejects every real driver
    call with "unrecognized parameter [lsid]" while a unit test built from a
    bare dict passes — which is exactly what happened here before this list.
    """
    assert is_generic_arg(key)
    store = ServerParams()
    _apply({"setParameter": 1, "logLevel": 2, key: {"x": 1}}, store)
    assert store.get("logLevel") == 2


def test_an_envelope_only_call_still_reports_no_option_found() -> None:
    with pytest.raises(SetParameterError) as caught:
        _apply({"setParameter": 1, "lsid": {"id": "x"}})
    assert caught.value.code == 72
    assert str(caught.value) == "no option found to set, use help:true to see options "
