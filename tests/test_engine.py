"""Engine-selection resolver tests (WT-independent).

`secantus.engine` is the single source of truth for choosing between the
pure-Python engines (default, always present) and the optional Rust core. It
has no intra-package imports, so it loads standalone by path — these tests run
with or without the WiredTiger extension and with or without the Rust core.
"""

from __future__ import annotations

import importlib.util
import os
import pathlib

import pytest

_PATH = pathlib.Path(__file__).resolve().parents[1] / "src" / "secantus" / "engine.py"
_spec = importlib.util.spec_from_file_location("secantus_engine_under_test", _PATH)
engine = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(engine)

_COMPONENT_ENVS = [f"SECANTUS_RUST_{c.upper()}" for c in engine.COMPONENTS]


@pytest.fixture(autouse=True)
def _reset_engine():
    """Reset global override + relevant env vars around each test."""
    saved = {k: os.environ.get(k) for k in ["SECANTUS_ENGINE", *_COMPONENT_ENVS]}
    for k in saved:
        os.environ.pop(k, None)
    engine.set_engine(None)
    yield
    engine.set_engine(None)
    for k, v in saved.items():
        if v is None:
            os.environ.pop(k, None)
        else:
            os.environ[k] = v


def test_default_is_python():
    assert engine.selected() == "python"
    for c in engine.COMPONENTS:
        assert engine.enabled(c) is False  # Python default, regardless of availability


def test_set_engine_rust_and_auto():
    engine.set_engine("rust")
    assert engine.selected() == "rust"
    for c in engine.COMPONENTS:
        assert engine.enabled(c) == engine.available()
    engine.set_engine("auto")
    assert engine.selected() == "auto"
    for c in engine.COMPONENTS:
        assert engine.enabled(c) == engine.available()


def test_env_var_selection():
    os.environ["SECANTUS_ENGINE"] = "rust"
    assert engine.selected() == "rust"
    assert engine.enabled("query") == engine.available()


def test_set_engine_overrides_env():
    os.environ["SECANTUS_ENGINE"] = "rust"
    engine.set_engine("python")  # programmatic wins over env
    assert engine.selected() == "python"
    assert engine.enabled("query") is False


def test_per_component_override_wins():
    engine.set_engine("python")
    os.environ["SECANTUS_RUST_QUERY"] = "1"
    assert engine.enabled("query") == engine.available()  # forced on (if available)
    assert engine.enabled("update") is False  # others stay python
    os.environ["SECANTUS_RUST_QUERY"] = "0"
    engine.set_engine("rust")
    assert engine.enabled("query") is False  # forced off even under rust
    assert engine.enabled("update") == engine.available()


def test_invalid_engine_rejected():
    with pytest.raises(ValueError):
        engine.set_engine("go")


def test_case_insensitive():
    engine.set_engine("RUST")
    assert engine.selected() == "rust"
