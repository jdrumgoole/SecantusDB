"""A failing run must never print a perfect pass rate.

Every gauge report formatted its rate with ``f"{...:.1f}%"``, which ROUNDS.
`docs/validation-report-php-lib.md` published this, live, on 2026-09-18:

    | **Overall** | **3089** | **1** | **40** | **3130** | **100.0%** |

3089 passed, **one failed**, scored 100.0% — because 3089/3090 is 99.9676% and
`:.1f` rounds it up. The website's driver panels are generated from those
reports, so the claim was on the public site. Any run at >= 99.95% has the same
problem, which is precisely where the healthy gauges now sit: the better the
server gets, the likelier its own report is to overstate it.

These tests pin both halves — the helper's arithmetic, and the fact that a real
generator actually USES it. The second half matters on its own: an earlier fix
in this family was inert because the report never imported the thing it added.
"""

from __future__ import annotations

import pathlib
import subprocess
import sys

import pytest
from validation_summary.rates import pass_rate

REPO = pathlib.Path(__file__).resolve().parent.parent


@pytest.mark.parametrize(
    ("passed", "run", "expected"),
    [
        # The live php-lib case, and the psycopg one found the same day.
        (3089, 3090, "99.9%"),
        (5545, 5546, "99.9%"),
        # One failure in a large run must still not read as perfect.
        (4999, 5000, "99.9%"),
        (999_999, 1_000_000, "99.9%"),
        # Only a genuinely clean run prints 100.0%.
        (1, 1, "100.0%"),
        (3090, 3090, "100.0%"),
        # Ordinary values floor rather than round: 2/3 is 66.67%, not 66.7%.
        (2, 3, "66.6%"),
        (0, 5, "0.0%"),
        # Nothing ran at all — not 0%, which would read as total failure.
        (0, 0, "—"),
    ],
)
def test_pass_rate_floors_and_never_flatters(passed: int, run: int, expected: str) -> None:
    assert pass_rate(passed, run) == expected


def test_one_failure_never_reads_as_a_perfect_score() -> None:
    """The property that matters, stated directly rather than by example."""
    for run in (100, 1_000, 3_090, 10_000, 100_000):
        assert pass_rate(run - 1, run) != "100.0%", f"{run - 1}/{run} printed as perfect"


def _junit(tmp_path: pathlib.Path, passed: int, failed: int) -> pathlib.Path:
    cases = "".join(f'<testcase class="C" name="ok{i}" />' for i in range(passed))
    cases += "".join(
        f'<testcase class="C" name="bad{i}"><failure>boom</failure></testcase>'
        for i in range(failed)
    )
    p = tmp_path / "junit.xml"
    p.write_text(f'<testsuites><testsuite name="s" time="1.0">{cases}</testsuite></testsuites>')
    return p


def test_a_real_generator_renders_the_floored_rate(tmp_path: pathlib.Path) -> None:
    """End-to-end through php-lib's generator, at the exact live counts.

    Asserting the helper alone would not catch a generator that imports it and
    then formats the rate its old way — the failure mode that made the earlier
    expected-failures fix inert until its wiring landed.
    """
    xml = _junit(tmp_path, passed=3089, failed=1)
    out = tmp_path / "report.md"
    subprocess.run(
        [sys.executable, "-m", "php_lib_validation.generate_report", str(xml), str(out)],
        cwd=REPO,
        check=True,
        capture_output=True,
    )
    text = out.read_text()
    overall = next(line for line in text.splitlines() if "**Overall**" in line)
    assert "99.9%" in overall, overall
    assert "100.0%" not in overall, f"a failing run rendered as perfect: {overall}"
