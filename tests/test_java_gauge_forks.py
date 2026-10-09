"""The Java gauge runs ONE test JVM at a time unless told otherwise.

The driver's fixture drops the shared ``JavaDriverTest`` database when each
test JVM exits. Twelve forks against one server meant a finishing fork deleted
the data under a running one, and the gauge published 495 of 496 for a server
that passes all 496. See ``java_validation.runner.default_forks``.
"""

from __future__ import annotations

from pathlib import Path

from java_validation.runner import default_forks

REPO = Path(__file__).resolve().parent.parent


def test_one_fork_by_default() -> None:
    assert default_forks({}) == 1
    assert default_forks({"SECANTUS_GAUGE_PARALLEL_FORKS": ""}) == 1


def test_the_override_is_honoured() -> None:
    assert default_forks({"SECANTUS_GAUGE_PARALLEL_FORKS": "12"}) == 12


def test_the_init_script_does_not_fall_back_to_every_core() -> None:
    script = (REPO / "java_validation" / "init.gradle.kts").read_text()
    assert "availableProcessors" not in script
    assert "toIntOrNull() ?: 1" in script
