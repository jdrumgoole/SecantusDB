"""Sanity test for display_uri."""

from secantus.admin.client import display_uri


def test_display_uri_loopback_default() -> None:
    assert display_uri("mongodb://127.0.0.1:27017/") == "mongodb://127.0.0.1:27017"


def test_display_uri_strips_password_keeps_user() -> None:
    out = display_uri("mongodb://alice:s3cret@host:27017/?authSource=admin")
    assert out == "mongodb://alice@host:27017"
    assert "s3cret" not in out
    assert "authSource" not in out


def test_display_uri_no_userinfo_pass_through() -> None:
    assert display_uri("mongodb://host/") == "mongodb://host"


def test_display_uri_invalid_returns_input() -> None:
    # Some pathological strings aren't URIs at all; we don't want to
    # raise — just hand the value back so the badge shows whatever
    # the operator typed.
    assert display_uri("") == ""
    assert display_uri("garbage") == "garbage"
